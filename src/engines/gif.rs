//! GIF (still and animated) via gifsicle (GPL-2.0, run as a separate program).

use std::path::Path;
use std::process::Command;

use super::image::{self, corrupt, metadata_note};
use super::{Encoded, Options, engine_error};
use crate::error::AppError;
use crate::level::Level;
use crate::storage::remove_if_exists;

pub fn check_gifsicle() -> Result<(), String> {
    match Command::new("gifsicle").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!("gifsicle --version failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
        Err(e) => Err(format!("gifsicle is not installed or not on PATH ({e}); it is needed for GIF compression")),
    }
}

/// Low/Medium are lossless; High/Extra add gifsicle's lossy LZW compression.
fn args(level: Level) -> (&'static [&'static str], &'static str) {
    match level {
        Level::Low => (&["-O2"], "lossless"),
        Level::Medium => (&["-O3"], "lossless"),
        Level::High => (&["-O3", "--lossy=30"], "lossy 30"),
        Level::Extra => (&["-O3", "--lossy=80"], "lossy 80"),
    }
}

pub fn compress(input: &Path, output: &Path, opts: Options) -> Result<Encoded, AppError> {
    let mut head = [0u8; 10];
    std::io::Read::read_exact(&mut std::fs::File::open(input).map_err(|e| AppError::io(&e))?, &mut head)
        .map_err(|_| corrupt("GIF"))?;
    let (width, height) = screen_size(&head).ok_or_else(|| corrupt("GIF"))?;
    image::check_pixels(width, height)?;

    let (level_args, label) = args(opts.level);
    let tmp = output.with_extension("gifsicle");
    let mut cmd = Command::new("gifsicle");
    cmd.args(level_args).arg("--no-warnings");
    if opts.strip_metadata {
        // Comments, frame names and application extensions (e.g. XMP). The loop count is kept.
        cmd.args(["--no-comments", "--no-names", "--no-extensions"]);
    }
    let out = cmd.arg("-o").arg(&tmp).arg("--").arg(input).output().map_err(|e| engine_error("gifsicle", &e))?;
    if !out.status.success() {
        remove_if_exists(&tmp).map_err(|e| AppError::io(&e))?;
        tracing::warn!("gifsicle failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        return Err(corrupt("GIF"));
    }
    std::fs::rename(&tmp, output).map_err(|e| AppError::io(&e))?;
    Ok(Encoded::Written { note: format!("gifsicle, {label}{}", metadata_note(opts.strip_metadata)) })
}

/// Logical screen size from the GIF header.
fn screen_size(data: &[u8]) -> Option<(u64, u64)> {
    if !(data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return None;
    }
    let w = u16::from_le_bytes(data.get(6..8)?.try_into().ok()?);
    let h = u16::from_le_bytes(data.get(8..10)?.try_into().ok()?);
    Some((w as u64, h as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_screen_size() {
        assert_eq!(screen_size(b"GIF89a\x40\x01\xC8\x00"), Some((320, 200)));
        assert_eq!(screen_size(b"PNG....."), None);
    }
}
