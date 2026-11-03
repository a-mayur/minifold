//! HEIC, AVIF, TIFF and animated WebP via the libvips command-line tools (LGPL-2.1, run as
//! separate programs). HEIC/AVIF encoding goes through libheif with x265 / libaom.

use std::path::Path;
use std::process::Command;

use super::image::{self, corrupt, metadata_note};
use super::{Encoded, Options, engine_error};
use crate::error::{AppError, ErrorCode};
use crate::level::Level;
use crate::storage::remove_if_exists;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Heic,
    Avif,
    Tiff,
    AnimatedWebp,
}

impl Target {
    fn name(self) -> &'static str {
        match self {
            Self::Heic => "HEIC image",
            Self::Avif => "AVIF image",
            Self::Tiff => "TIFF image",
            Self::AnimatedWebp => "animated WebP",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Heic => "heic",
            Self::Avif => "avif",
            Self::Tiff => "tif",
            Self::AnimatedWebp => "webp",
        }
    }
}

/// HEIC/AVIF quality (libheif Q).
fn heif_quality(level: Level) -> u8 {
    match level {
        Level::Low => 80,
        Level::Medium => 65,
        Level::High => 50,
        Level::Extra => 35,
    }
}

fn webp_quality(level: Level) -> u8 {
    match level {
        Level::Low => 90,
        Level::Medium => 80,
        Level::High => 70,
        Level::Extra => 50,
    }
}

/// TIFF stays lossless (it's usually an archival or print format).
fn tiff_compression(level: Level) -> (&'static str, &'static str) {
    match level {
        Level::Low => ("compression=lzw,predictor=horizontal", "LZW"),
        Level::Medium => ("compression=deflate,predictor=horizontal,level=6", "Deflate"),
        Level::High | Level::Extra => ("compression=deflate,predictor=horizontal,level=9", "Deflate, max"),
    }
}

/// libvips with loaders that haven't been fuzzed for untrusted input disabled.
fn vips(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("VIPS_BLOCK_UNTRUSTED", "1");
    cmd
}

/// Fails startup unless vips can actually write HEIC and AVIF (the encoders are plugins).
pub fn check_vips() -> Result<(), String> {
    let dir = std::env::temp_dir();
    for (label, ext, opts) in [("HEIC (x265)", "heic", "compression=hevc"), ("AVIF (libaom)", "avif", "compression=av1")] {
        let file = dir.join(format!("minifold-startup-check-{}.{ext}", std::process::id()));
        let out = vips("vips")
            .arg("black")
            .arg(format!("{}[{opts}]", file.display()))
            .args(["16", "16"])
            .output()
            .map_err(|e| format!("libvips (the `vips` command) is not installed or not on PATH ({e})"))?;
        let _ = std::fs::remove_file(&file);
        if !out.status.success() {
            return Err(format!(
                "libvips can't write {label}; is the libheif encoder plugin installed? {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(())
}

pub fn compress(input: &Path, output: &Path, opts: Options, target: Target) -> Result<Encoded, AppError> {
    let bad = || corrupt(target.name());
    let width: u64 = header(input, "width", target)?.parse().map_err(|_| bad())?;
    let height: u64 = header(input, "height", target)?.parse().map_err(|_| bad())?;
    let pages: u32 = header(input, "n-pages", target)?.parse().map_err(|_| bad())?;
    image::check_pixels(width, height)?;

    if matches!(target, Target::Heic | Target::Avif) && pages > 1 {
        return Err(AppError::new(
            ErrorCode::UnsupportedType,
            format!("{}s that contain several images aren't supported yet.", target.name()),
        ));
    }

    let keep = if opts.strip_metadata { "keep=icc" } else { "keep=all" };
    let (save, note) = match target {
        Target::Heic => {
            let q = heif_quality(opts.level);
            (format!("compression=hevc,Q={q},{keep}"), format!("libheif (HEVC), quality {q}"))
        }
        Target::Avif => {
            let q = heif_quality(opts.level);
            (format!("compression=av1,Q={q},{keep}"), format!("libheif (AV1), quality {q}"))
        }
        Target::Tiff => {
            let (c, label) = tiff_compression(opts.level);
            (format!("{c},{keep}"), format!("libvips, lossless {label}"))
        }
        Target::AnimatedWebp if super::webp::animation_is_lossless(input)? => {
            (format!("lossless=true,{keep}"), "libvips (libwebp), lossless, all frames".to_string())
        }
        Target::AnimatedWebp => {
            let q = webp_quality(opts.level);
            (format!("Q={q},{keep}"), format!("libvips (libwebp), quality {q}, all frames"))
        }
    };

    // Multi-frame files are loaded whole. A single-page TIFF is rotated upright when its
    // orientation tag is about to be stripped; HEIC/AVIF rotation is applied by libheif.
    // fail_on=error: by default libvips decodes damaged files as best it can and the output
    // would silently be a broken image.
    let multi = pages > 1 || target == Target::AnimatedWebp;
    let load = format!("{}[{}fail_on=error]", input.display(), if multi { "n=-1," } else { "" });
    let operation = if target == Target::Tiff && opts.strip_metadata && !multi { "autorot" } else { "copy" };

    let tmp = output.with_extension(format!("vips.{}", target.extension()));
    let out = vips("vips")
        .arg(operation)
        .arg(load)
        .arg(format!("{}[{save}]", tmp.display()))
        .output()
        .map_err(|e| engine_error("libvips", &e))?;
    if !out.status.success() {
        remove_if_exists(&tmp).map_err(|e| AppError::io(&e))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if is_read_error(&stderr) {
            tracing::warn!("libvips could not read input: {}", stderr.trim());
            return Err(bad());
        }
        return Err(engine_error("libvips", &stderr.trim()));
    }
    std::fs::rename(&tmp, output).map_err(|e| AppError::io(&e))?;
    Ok(Encoded::Written { note: format!("{note}{}", metadata_note(opts.strip_metadata)) })
}

/// One header field via `vipsheader`; a file it can't read is reported as damaged.
fn header(input: &Path, field: &str, target: Target) -> Result<String, AppError> {
    let out = vips("vipsheader")
        .args(["-f", field])
        .arg(input)
        .output()
        .map_err(|e| engine_error("libvips", &e))?;
    if !out.status.success() {
        tracing::warn!("vipsheader failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        return Err(corrupt(target.name()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// libvips reports decode failures through the loader ("tiffload: …", "tiff2vips: read error",
/// "heif: Invalid input …", "TIFFFillStrip: Read error …"); anything else is an encoder failure.
fn is_read_error(stderr: &str) -> bool {
    ["load", "2vips", "Invalid input", "read error", "Read error", "truncated", "Premature end"]
        .iter()
        .any(|m| stderr.contains(m))
}

/// AVIF image sequences (brand `avis`) would lose all but the first frame.
pub fn is_avif_sequence(head: &[u8]) -> bool {
    head.get(4..8) == Some(b"ftyp") && head.get(8..12) == Some(b"avis")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_libvips_errors() {
        assert!(is_read_error("TIFFFillStrip: Read error at scanline 398\ntiff2vips: read error"));
        assert!(is_read_error("heif: Invalid input: Unexpected end of file"));
        assert!(!is_read_error("heifsave: Unsupported compression"));
    }

    #[test]
    fn detects_avif_sequences() {
        assert!(is_avif_sequence(b"\0\0\0\x1cftypavis\0\0\0\0"));
        assert!(!is_avif_sequence(b"\0\0\0\x1cftypavif\0\0\0\0"));
    }
}
