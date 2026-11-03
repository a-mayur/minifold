//! WebP: re-encode with libwebp. The container (RIFF) is rebuilt by hand so the ICC color
//! profile is always kept, and EXIF/XMP only when metadata is kept. When stripping, a
//! lossless copy without metadata is also tried, and the smaller result wins.

use std::path::Path;

use ::webp::{BitstreamFeatures, Decoder, Encoder};

use super::image::{self, Candidate, corrupt, metadata_note};
use super::{Encoded, Options, exif};
use crate::error::AppError;
use crate::level::Level;

const IMAGE_CHUNKS: [&[u8; 4]; 3] = [b"ALPH", b"VP8 ", b"VP8L"];

fn quality(level: Level) -> f32 {
    match level {
        Level::Low => 90.0,
        Level::Medium => 80.0,
        Level::High => 70.0,
        Level::Extra => 50.0,
    }
}

/// Animated WebP goes through libvips instead (see `vips.rs`).
pub fn is_animated(input: &Path) -> Result<bool, AppError> {
    let data = image::read(input)?;
    Ok(BitstreamFeatures::new(&data).ok_or_else(|| corrupt("WebP"))?.has_animation())
}

/// True when any frame of an animated WebP is lossless (VP8L), so the animation stays lossless.
pub fn animation_is_lossless(input: &Path) -> Result<bool, AppError> {
    let data = image::read(input)?;
    let all = chunks(&data).ok_or_else(|| corrupt("WebP"))?;
    // Each ANMF chunk holds a 16-byte frame header followed by the frame's own chunks.
    Ok(all
        .iter()
        .filter(|c| &c.id == b"ANMF")
        .filter_map(|c| frame_chunks(c.data.get(16..)?))
        .any(|frame| frame.iter().any(|c| &c.id == b"VP8L")))
}

pub fn compress(input: &Path, output: &Path, opts: Options) -> Result<Encoded, AppError> {
    let data = image::read(input)?;
    let original = chunks(&data).ok_or_else(|| corrupt("WebP"))?;
    let features = BitstreamFeatures::new(&data).ok_or_else(|| corrupt("WebP"))?;
    let (w, h) = (features.width(), features.height());
    image::check_pixels(w as u64, h as u64)?;

    let find = |id: &[u8; 4]| original.iter().find(|c| &c.id == id).map(|c| c.data);
    let icc = find(b"ICCP");
    let (exif, xmp) = if opts.strip_metadata {
        let orientation = find(b"EXIF").and_then(|e| exif::orientation(e.strip_prefix(b"Exif\0\0").unwrap_or(e)));
        (orientation.map(exif::orientation_only), None)
    } else {
        (find(b"EXIF").map(<[u8]>::to_vec), find(b"XMP ").map(<[u8]>::to_vec))
    };
    let meta = Meta { icc, exif: exif.as_deref(), xmp: xmp.as_deref() };

    // Lossless sources stay lossless (screenshots, graphics); lossy ones are re-encoded at the level's quality.
    let lossless = matches!(features.format(), Some(::webp::BitstreamFormat::Lossless));
    let decoded = Decoder::new(&data).decode().ok_or_else(|| corrupt("WebP"))?;
    let encoder = Encoder::new(&decoded, decoded.layout(), w, h);
    let encoded = if lossless { encoder.encode_lossless() } else { encoder.encode(quality(opts.level)) };
    let method = if lossless { "lossless".to_string() } else { format!("quality {}", quality(opts.level)) };
    let encoded_chunks = chunks(&encoded).expect("libwebp writes valid WebP");
    let encoded_alpha = BitstreamFeatures::new(&encoded).expect("libwebp writes valid WebP").has_alpha();

    let mut candidates = vec![Candidate {
        bytes: assemble(&image_chunks(&encoded_chunks), &meta, encoded_alpha, w, h),
        note: format!("libwebp, {method}{}", metadata_note(opts.strip_metadata)),
    }];
    if opts.strip_metadata {
        candidates.push(Candidate {
            bytes: assemble(&image_chunks(&original), &meta, features.has_alpha(), w, h),
            note: "Metadata removed; image data unchanged (re-encoding wouldn't make it smaller)".into(),
        });
    }
    image::write_smallest(output, candidates)
}

struct Chunk<'a> {
    id: [u8; 4],
    data: &'a [u8],
}

struct Meta<'a> {
    icc: Option<&'a [u8]>,
    exif: Option<&'a [u8]>,
    xmp: Option<&'a [u8]>,
}

fn chunks(file: &[u8]) -> Option<Vec<Chunk<'_>>> {
    if file.get(0..4)? != b"RIFF" || file.get(8..12)? != b"WEBP" {
        return None;
    }
    frame_chunks(&file[12..])
}

/// A run of RIFF chunks (the file body, or the inside of an ANMF frame).
fn frame_chunks(body: &[u8]) -> Option<Vec<Chunk<'_>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= body.len() {
        let id: [u8; 4] = body[i..i + 4].try_into().ok()?;
        let size = u32::from_le_bytes(body[i + 4..i + 8].try_into().ok()?) as usize;
        let data = body.get(i + 8..i + 8 + size)?;
        out.push(Chunk { id, data });
        i += 8 + size + (size & 1);
    }
    Some(out)
}

fn image_chunks<'a>(all: &'a [Chunk<'a>]) -> Vec<&'a Chunk<'a>> {
    all.iter().filter(|c| IMAGE_CHUNKS.contains(&&c.id)).collect()
}

/// Builds a WebP file: simple format when possible, extended (VP8X) when needed.
fn assemble(image: &[&Chunk], meta: &Meta, alpha: bool, width: u32, height: u32) -> Vec<u8> {
    let has_alph = image.iter().any(|c| &c.id == b"ALPH");
    let extended = meta.icc.is_some() || meta.exif.is_some() || meta.xmp.is_some() || has_alph;
    let mut body = b"WEBP".to_vec();
    if extended {
        let mut flags = 0u8;
        if meta.icc.is_some() {
            flags |= 0x20;
        }
        if alpha {
            flags |= 0x10;
        }
        if meta.exif.is_some() {
            flags |= 0x08;
        }
        if meta.xmp.is_some() {
            flags |= 0x04;
        }
        let mut vp8x = vec![flags, 0, 0, 0];
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        push_chunk(&mut body, b"VP8X", &vp8x);
    }
    if let Some(icc) = meta.icc {
        push_chunk(&mut body, b"ICCP", icc);
    }
    for c in image {
        push_chunk(&mut body, &c.id, c.data);
    }
    if let Some(exif) = meta.exif {
        push_chunk(&mut body, b"EXIF", exif);
    }
    if let Some(xmp) = meta.xmp {
        push_chunk(&mut body, b"XMP ", xmp);
    }
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    file
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// A lossy WebP with an ICC profile, EXIF (orientation 3 + "SECRET") and XMP.
    fn sample_webp() -> Vec<u8> {
        let (w, h) = (80u32, 60u32);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 3) as u8, (i / w * 4) as u8, 90]).collect();
        let plain = Encoder::from_rgb(&px, w, h).encode(98.0);
        let c = chunks(&plain).unwrap();
        let mut exif_tiff = exif::orientation_only(3);
        exif_tiff.extend_from_slice(b"SECRET-GPS");
        let meta = Meta { icc: Some(b"fake-icc"), exif: Some(&exif_tiff), xmp: Some(b"<x:xmpmeta>SECRET-XMP</x:xmpmeta>") };
        assemble(&image_chunks(&c), &meta, false, w, h)
    }

    #[test]
    fn sample_is_valid_webp() {
        let data = sample_webp();
        assert!(Decoder::new(&data).decode().is_some());
    }

    #[test]
    fn stripping_keeps_icc_and_orientation_only() {
        let input = crate::engines::tests::temp_file("strip.webp", &sample_webp());
        let output = input.with_extension("out");
        compress(&input, &output, Options { level: Level::Medium, strip_metadata: true }).unwrap();
        let out = std::fs::read(&output).unwrap();
        assert!(Decoder::new(&out).decode().is_some(), "still a valid WebP");
        assert!(!contains(&out, b"SECRET"));
        let c = chunks(&out).unwrap();
        assert_eq!(c.iter().find(|c| &c.id == b"ICCP").unwrap().data, b"fake-icc");
        assert_eq!(exif::orientation(c.iter().find(|c| &c.id == b"EXIF").unwrap().data), Some(3));
    }

    #[test]
    fn lossless_stays_lossless() {
        let (w, h) = (64u32, 40u32);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 7 * 30) as u8, 200, (i / w * 6) as u8]).collect();
        let src = Encoder::from_rgb(&px, w, h).encode_lossless();
        let input = crate::engines::tests::temp_file("lossless.webp", &src);
        let output = input.with_extension("out");
        compress(&input, &output, Options { level: Level::Extra, strip_metadata: true }).unwrap();
        let out = std::fs::read(&output).unwrap();
        assert!(matches!(BitstreamFeatures::new(&out).unwrap().format(), Some(::webp::BitstreamFormat::Lossless)));
        assert_eq!(&*Decoder::new(&out).decode().unwrap(), &px[..], "pixels identical");
    }

    #[test]
    fn keeping_metadata_keeps_exif_and_xmp() {
        let input = crate::engines::tests::temp_file("keep.webp", &sample_webp());
        let output = input.with_extension("out");
        compress(&input, &output, Options { level: Level::Medium, strip_metadata: false }).unwrap();
        let out = std::fs::read(&output).unwrap();
        assert!(Decoder::new(&out).decode().is_some());
        assert!(contains(&out, b"SECRET-GPS"));
        assert!(contains(&out, b"SECRET-XMP"));
    }
}
