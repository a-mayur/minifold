//! JPEG: re-encode with mozjpeg. When stripping metadata, also try a lossless copy with
//! the metadata removed, and keep whichever is smaller. The ICC color profile and the
//! orientation are always kept so the photo looks the same.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use mozjpeg::{ALL_MARKERS, ColorSpace, Compress, Decompress, Marker};

use super::image::{self, Candidate, corrupt, metadata_note};
use super::{Encoded, Options, engine_error, exif};
use crate::error::AppError;
use crate::level::Level;

const ICC: &[u8] = b"ICC_PROFILE\0";
const EXIF: &[u8] = b"Exif\0\0";

fn quality(level: Level) -> f32 {
    match level {
        Level::Low => 90.0,
        Level::Medium => 85.0,
        Level::High => 75.0,
        Level::Extra => 60.0,
    }
}

pub fn compress(input: &Path, output: &Path, opts: Options) -> Result<Encoded, AppError> {
    let data = image::read(input)?;

    // mozjpeg reports libjpeg errors by unwinding.
    let reencoded = catch_unwind(AssertUnwindSafe(|| reencode(&data, opts))).map_err(|payload| {
        let reason = payload.downcast_ref::<String>().map_or("unknown", String::as_str);
        tracing::warn!("JPEG decode/encode error: {reason}");
        corrupt("JPEG")
    })??;

    let Some(reencoded) = reencoded else {
        return cmyk_without_adobe_marker(&data, output, opts);
    };
    let q = quality(opts.level);
    let mut candidates = vec![Candidate {
        bytes: reencoded,
        note: format!("mozjpeg, quality {q}{}", metadata_note(opts.strip_metadata)),
    }];
    if opts.strip_metadata {
        candidates.push(Candidate {
            bytes: strip_lossless(&data)?,
            note: "Metadata removed; image data unchanged (re-encoding wouldn't make it smaller)".into(),
        });
    }
    image::write_smallest(output, candidates)
}

/// Re-encoding could invert the colors, so at most the metadata is removed.
fn cmyk_without_adobe_marker(data: &[u8], output: &Path, opts: Options) -> Result<Encoded, AppError> {
    const WHY: &str = "this CMYK JPEG has no Adobe color marker, so re-encoding could invert its colors";
    if opts.strip_metadata {
        let stripped = strip_lossless(data)?;
        if stripped.len() < data.len() {
            let note = format!("Metadata removed; image data unchanged ({WHY})");
            return image::write_smallest(output, vec![Candidate { bytes: stripped, note }]);
        }
    }
    Ok(Encoded::Unchanged { reason: format!("Kept unchanged: {WHY}.") })
}

/// `None` when the image can't be re-encoded safely (CMYK without an Adobe marker).
fn reencode(data: &[u8], opts: Options) -> Result<Option<Vec<u8>>, AppError> {
    let mut d = Decompress::builder()
        .with_markers(ALL_MARKERS)
        .from_mem(data)
        .map_err(|_| corrupt("JPEG"))?;
    image::check_pixels(d.width() as u64, d.height() as u64)?;
    let markers = markers_to_keep(&d, opts.strip_metadata);
    let adobe = d.markers().any(|m| matches!(m.marker, Marker::APP(14)) && m.data.starts_with(b"Adobe"));
    let density = d.pixel_density();

    let (space, width, height, pixels) = match d.image().map_err(|_| corrupt("JPEG"))? {
        mozjpeg::decompress::Format::RGB(mut s) => {
            let (w, h) = (s.width(), s.height());
            let px = s.read_scanlines::<u8>().map_err(|_| corrupt("JPEG"))?;
            s.finish().map_err(|_| corrupt("JPEG"))?;
            (ColorSpace::JCS_RGB, w, h, px)
        }
        mozjpeg::decompress::Format::Gray(mut s) => {
            let (w, h) = (s.width(), s.height());
            let px = s.read_scanlines::<u8>().map_err(|_| corrupt("JPEG"))?;
            s.finish().map_err(|_| corrupt("JPEG"))?;
            (ColorSpace::JCS_GRAYSCALE, w, h, px)
        }
        // Adobe CMYK is stored inverted; the encoder always writes an Adobe marker, so only
        // files that already have one keep their meaning when re-encoded.
        mozjpeg::decompress::Format::CMYK(_) if !adobe => return Ok(None),
        mozjpeg::decompress::Format::CMYK(mut s) => {
            let (w, h) = (s.width(), s.height());
            let px = s.read_scanlines::<u8>().map_err(|_| corrupt("JPEG"))?;
            s.finish().map_err(|_| corrupt("JPEG"))?;
            (ColorSpace::JCS_CMYK, w, h, px)
        }
    };

    let mut c = Compress::new(space);
    if space == ColorSpace::JCS_CMYK {
        c.set_color_space(ColorSpace::JCS_YCCK); // compresses like YCbCr; the reader converts back
    }
    c.set_size(width, height);
    c.set_quality(quality(opts.level));
    if let Some(density) = density {
        c.set_pixel_density(density);
    }
    let mut started = c.start_compress(Vec::new()).map_err(|e| engine_error("mozjpeg", &e))?;
    for (marker, bytes) in &markers {
        started.write_marker(*marker, bytes);
    }
    started.write_scanlines(&pixels).map_err(|e| engine_error("mozjpeg", &e))?;
    started.finish().map(Some).map_err(|e| engine_error("mozjpeg", &e))
}

/// Markers copied into the re-encoded file. JFIF/Adobe headers are written by the encoder,
/// and multi-picture (MPF) markers would point at data that re-encoding drops.
fn markers_to_keep<R>(d: &Decompress<R>, strip: bool) -> Vec<(Marker, Vec<u8>)> {
    let mut keep = Vec::new();
    for m in d.markers() {
        let is_icc = matches!(m.marker, Marker::APP(2)) && m.data.starts_with(ICC);
        let is_exif = matches!(m.marker, Marker::APP(1)) && m.data.starts_with(EXIF);
        match m.marker {
            Marker::APP(0) | Marker::APP(14) => {}
            _ if is_icc => keep.push((m.marker, m.data.to_vec())),
            Marker::APP(2) => {}
            _ if strip => {
                if is_exif && let Some(o) = exif::orientation(&m.data[EXIF.len()..]) {
                    keep.push((Marker::APP(1), [EXIF, &exif::orientation_only(o)].concat()));
                }
            }
            _ => keep.push((m.marker, m.data.to_vec())),
        }
    }
    keep
}

/// Copies the JPEG without metadata segments, keeping the image data byte for byte.
/// Kept: JFIF header, ICC profile, Adobe color header, orientation. Anything after the end
/// of the image (e.g. an embedded "motion photo" video) is dropped too.
fn strip_lossless(data: &[u8]) -> Result<Vec<u8>, AppError> {
    let bad = || corrupt("JPEG");
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err(bad());
    }
    let mut segments: Vec<(u8, &[u8])> = Vec::new();
    let mut i = 2;
    let scan = loop {
        if data.get(i) != Some(&0xFF) {
            return Err(bad());
        }
        while data.get(i + 1) == Some(&0xFF) {
            i += 1; // fill bytes
        }
        let marker = *data.get(i + 1).ok_or_else(bad)?;
        if marker == 0xDA {
            let rest = &data[i..];
            let end = rest.windows(2).position(|w| w == [0xFF, 0xD9]).ok_or_else(bad)? + 2;
            break &rest[..end];
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            segments.push((marker, &data[i..i + 2]));
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([*data.get(i + 2).ok_or_else(bad)?, *data.get(i + 3).ok_or_else(bad)?]) as usize;
        let end = i + 2 + len;
        if len < 2 || end > data.len() {
            return Err(bad());
        }
        segments.push((marker, &data[i..end]));
        i = end;
    };

    let payload = |seg: &[u8]| -> Vec<u8> { seg[4..].to_vec() };
    let orientation = segments
        .iter()
        .filter(|(m, seg)| *m == 0xE1 && seg[4..].starts_with(EXIF))
        .find_map(|(_, seg)| exif::orientation(&seg[4 + EXIF.len()..]));

    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&[0xFF, 0xD8]);
    let mut orientation_written = orientation.is_none();
    for (marker, seg) in &segments {
        let p = payload(seg);
        let keep = match *marker {
            0xE0 => p.starts_with(b"JFIF\0"),
            0xE2 => p.starts_with(ICC),
            0xEE => p.starts_with(b"Adobe"),
            0xE1 | 0xE3..=0xED | 0xEF | 0xFE => false,
            _ => true,
        };
        // JFIF must come first; put the orientation right after it (or first if there's no JFIF).
        if !orientation_written && *marker != 0xE0 {
            push_exif_orientation(&mut out, orientation.expect("checked above"));
            orientation_written = true;
        }
        if keep {
            out.extend_from_slice(seg);
        }
    }
    if !orientation_written {
        push_exif_orientation(&mut out, orientation.expect("checked above"));
    }
    out.extend_from_slice(scan);
    Ok(out)
}

fn push_exif_orientation(out: &mut Vec<u8>, orientation: u16) {
    let body = [EXIF, &exif::orientation_only(orientation)].concat();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn sample_cmyk(adobe: bool) -> Vec<u8> {
        let (w, h) = (48usize, 32usize);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 5) as u8, 40, (i / w * 7) as u8, 20]).collect();
        let mut c = Compress::new(ColorSpace::JCS_CMYK);
        c.set_size(w, h);
        c.set_quality(98.0);
        let mut s = c.start_compress(Vec::new()).unwrap();
        s.write_marker(Marker::COM, b"SECRET");
        s.write_scanlines(&px).unwrap();
        let data = s.finish().unwrap();
        if adobe {
            return data;
        }
        let at = data.windows(7).position(|w| w == b"\xFF\xEE\x00\x0EAdo").expect("encoder writes Adobe marker");
        [&data[..at], &data[at + 16..]].concat()
    }

    #[test]
    fn adobe_cmyk_is_reencoded_with_close_values() {
        let data = sample_cmyk(true);
        let input = crate::engines::tests::temp_file("adobe-cmyk.jpg", &data);
        let output = input.with_extension("out");
        compress(&input, &output, Options { level: Level::Low, strip_metadata: true }).unwrap();
        let read = |d: &[u8]| -> Vec<u8> {
            match Decompress::new_mem(d).unwrap().image().unwrap() {
                mozjpeg::decompress::Format::CMYK(mut s) => s.read_scanlines::<u8>().unwrap(),
                _ => panic!("expected CMYK"),
            }
        };
        let out = std::fs::read(&output).unwrap();
        let (a, b) = (read(&data), read(&out));
        let max_diff = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(max_diff < 40, "CMYK values preserved (max diff {max_diff})");
        assert!(!contains(&out, b"SECRET"));
    }

    #[test]
    fn cmyk_without_adobe_marker_is_not_reencoded() {
        let input = crate::engines::tests::temp_file("plain-cmyk.jpg", &sample_cmyk(false));
        let output = input.with_extension("out");
        match compress(&input, &output, Options { level: Level::Low, strip_metadata: true }).unwrap() {
            Encoded::Written { note } => assert!(note.contains("no Adobe color marker"), "{note}"),
            other => panic!("comment should have been stripped: {other:?}"),
        }
        match compress(&input, &output, Options { level: Level::Low, strip_metadata: false }).unwrap() {
            Encoded::Unchanged { reason } => assert!(reason.contains("no Adobe color marker")),
            other => panic!("must not re-encode: {other:?}"),
        }
    }

    /// A tiny gradient JPEG with EXIF (orientation 6 + fake GPS text), a comment and an ICC marker.
    pub(crate) fn sample_jpeg() -> Vec<u8> {
        let (w, h) = (64usize, 48usize);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 4) as u8, (i / w * 5) as u8, 128]).collect();
        let mut c = Compress::new(ColorSpace::JCS_RGB);
        c.set_size(w, h);
        c.set_quality(98.0);
        let mut s = c.start_compress(Vec::new()).unwrap();
        let mut exif_tiff = exif::orientation_only(6);
        exif_tiff.extend_from_slice(b"GPS 51.5007N 0.1246W SECRET");
        s.write_marker(Marker::APP(1), &[EXIF, &exif_tiff].concat());
        s.write_marker(Marker::COM, b"taken at home");
        s.write_marker(Marker::APP(2), &[ICC, b"\x01\x01fake-profile"].concat());
        s.write_scanlines(&px).unwrap();
        s.finish().unwrap()
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    fn opts(strip: bool) -> Options {
        Options { level: Level::Medium, strip_metadata: strip }
    }

    #[test]
    fn stripping_removes_private_data_but_keeps_icc_and_orientation() {
        let input = crate::engines::tests::temp_file("strip.jpg", &sample_jpeg());
        let output = input.with_extension("out");
        compress(&input, &output, opts(true)).unwrap();
        let out = std::fs::read(&output).unwrap();
        assert!(!contains(&out, b"SECRET"));
        assert!(!contains(&out, b"taken at home"));
        assert!(contains(&out, b"fake-profile"), "ICC kept");
        let d = Decompress::builder().with_markers(ALL_MARKERS).from_mem(&out).unwrap();
        let o = d
            .markers()
            .find(|m| m.data.starts_with(EXIF))
            .and_then(|m| exif::orientation(&m.data[EXIF.len()..]));
        assert_eq!(o, Some(6));
    }

    #[test]
    fn keeping_metadata_keeps_it() {
        let input = crate::engines::tests::temp_file("keep.jpg", &sample_jpeg());
        let output = input.with_extension("out");
        compress(&input, &output, opts(false)).unwrap();
        let out = std::fs::read(&output).unwrap();
        assert!(contains(&out, b"SECRET"));
        assert!(contains(&out, b"taken at home"));
    }

    #[test]
    fn lossless_strip_keeps_image_data_identical() {
        let data = sample_jpeg();
        let stripped = strip_lossless(&data).unwrap();
        assert!(stripped.len() < data.len());
        assert!(!contains(&stripped, b"SECRET"));
        let sos = |d: &[u8]| d.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
        assert_eq!(&data[sos(&data)..], &stripped[sos(&stripped)..], "scan data untouched");
        Decompress::new_mem(&stripped).unwrap().rgb().unwrap().read_scanlines::<u8>().unwrap();
    }

    #[test]
    fn damaged_jpeg_is_reported_as_corrupt() {
        let mut data = sample_jpeg();
        data.truncate(data.len() / 3);
        data[200..260].fill(0xAA);
        let input = crate::engines::tests::temp_file("broken.jpg", &data);
        let err = compress(&input, &input.with_extension("out"), opts(true)).unwrap_err();
        assert_eq!(err.code, ErrorCode::CorruptInput);
    }
}
