//! PNG: lossless optimization with oxipng. At High/Extra, colors are first reduced with
//! pngquant (run as a separate program; it is GPL-licensed and not linked in).

use std::path::Path;
use std::process::Command;

use oxipng::{PngError, StripChunks};

use super::image::{self, Candidate, corrupt, metadata_note};
use super::{Encoded, Options, engine_error};
use crate::error::AppError;
use crate::level::Level;
use crate::storage::remove_if_exists;

/// pngquant exit codes meaning "quality would be too low" / "result is larger": not errors.
const PNGQUANT_SKIPPED: [i32; 2] = [98, 99];

pub fn check_pngquant() -> Result<(), String> {
    match Command::new("pngquant").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!("pngquant --version failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
        Err(e) => Err(format!("pngquant is not installed or not on PATH ({e}); it is needed for PNG compression")),
    }
}

fn oxipng_preset(level: Level) -> u8 {
    match level {
        Level::Low => 2,
        Level::Medium => 3,
        Level::High => 4,
        Level::Extra => 6,
    }
}

fn pngquant_quality(level: Level) -> Option<&'static str> {
    match level {
        Level::Low | Level::Medium => None,
        Level::High => Some("65-85"),
        Level::Extra => Some("40-70"),
    }
}

pub fn compress(input: &Path, output: &Path, opts: Options) -> Result<Encoded, AppError> {
    let data = image::read(input)?;
    let info = Info::read(&data)?;
    image::check_pixels(info.width, info.height)?;
    let meta = metadata_note(opts.strip_metadata);

    let Some(lossless) = optimize(&data, opts)? else {
        return Ok(Encoded::Unchanged {
            reason: "Kept unchanged: this PNG carries a content-authenticity signature (C2PA) that any change would break."
                .into(),
        });
    };
    let mut candidates = vec![Candidate { bytes: lossless, note: format!("oxipng, lossless{meta}") }];

    if let Some(quality) = pngquant_quality(opts.level).filter(|_| info.pngquant_safe(opts.strip_metadata))
        && let Some(quantized) = pngquant(input, output, quality)?
        && let Some(bytes) = optimize(&quantized, opts)?
    {
        candidates.push(Candidate { bytes, note: format!("pngquant (quality {quality}) + oxipng{meta}") });
    }
    image::write_smallest(output, candidates)
}

/// `None` when the file must not be changed (signed content).
fn optimize(data: &[u8], opts: Options) -> Result<Option<Vec<u8>>, AppError> {
    let mut o = oxipng::Options::from_preset(oxipng_preset(opts.level));
    o.strip = if opts.strip_metadata { StripChunks::Safe } else { StripChunks::None };
    match oxipng::optimize_from_memory(data, &o) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(PngError::C2PAMetadataPreventsChanges | PngError::ChunkPreventsChanges(_)) => Ok(None),
        Err(
            PngError::NotPNG
            | PngError::CRCMismatch(_)
            | PngError::TruncatedData
            | PngError::InvalidData
            | PngError::ChunkMissing(_)
            | PngError::IncorrectDataLength(..)
            | PngError::InvalidDepthForType(..)
            | PngError::APNGOutOfOrder,
        ) => Err(corrupt("PNG")),
        Err(e) => Err(engine_error("oxipng", &e)),
    }
}

/// Runs pngquant; `None` when it declined (quality too low or no gain).
/// Metadata stripping is left to oxipng, which keeps the chunks that affect display.
fn pngquant(input: &Path, output: &Path, quality: &str) -> Result<Option<Vec<u8>>, AppError> {
    let tmp = output.with_extension("pngquant");
    let out = Command::new("pngquant")
        .args(["--quality", quality, "--speed", "3", "--force", "--output"])
        .arg(&tmp)
        .arg("--")
        .arg(input)
        .output()
        .map_err(|e| engine_error("pngquant", &e))?;
    let result = match out.status.code() {
        Some(0) => std::fs::read(&tmp).map(Some).map_err(|e| AppError::io(&e)),
        Some(code) if PNGQUANT_SKIPPED.contains(&code) => Ok(None),
        _ => Err(engine_error("pngquant", &String::from_utf8_lossy(&out.stderr).trim())),
    };
    remove_if_exists(&tmp).map_err(|e| AppError::io(&e))?;
    result
}

struct Info {
    width: u64,
    height: u64,
    chunks: Vec<[u8; 4]>,
}

impl Info {
    /// Reads IHDR and the list of chunk types.
    fn read(data: &[u8]) -> Result<Self, AppError> {
        if !data.starts_with(b"\x89PNG\r\n\x1a\n") || data.get(12..16) != Some(b"IHDR") {
            return Err(corrupt("PNG"));
        }
        let be = |at: usize| -> Result<u32, AppError> {
            Ok(u32::from_be_bytes(data.get(at..at + 4).ok_or_else(|| corrupt("PNG"))?.try_into().expect("4 bytes")))
        };
        let (width, height) = (be(16)? as u64, be(20)? as u64);
        let mut chunks = Vec::new();
        let mut i = 8;
        while let (Ok(len), Some(kind)) = (be(i), data.get(i + 4..i + 8)) {
            chunks.push(kind.try_into().expect("4 bytes"));
            i += 12 + len as usize;
        }
        Ok(Self { width, height, chunks })
    }

    fn has(&self, kind: &[u8; 4]) -> bool {
        self.chunks.contains(kind)
    }

    /// pngquant (as packaged by Debian) has no color-profile support: it drops iCCP without
    /// converting colors, flattens animations and drops some metadata chunks. Only use it
    /// when none of that changes the result.
    fn pngquant_safe(&self, strip: bool) -> bool {
        let loses_metadata = [b"eXIf", b"iTXt", b"zTXt"].iter().any(|c| self.has(c));
        !self.has(b"acTL") && !self.has(b"iCCP") && (strip || !loses_metadata)
    }
}
