use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use super::{Encoded, engine_error};
use crate::error::AppError;
use crate::level::Level;

pub const SUFFIX: &str = ".zst";

fn zstd_level(level: Level) -> i32 {
    match level {
        Level::Low => 3,
        Level::Medium => 9,
        Level::High => 15,
        Level::Extra => 19,
    }
}

/// Compresses any file with zstd.
pub fn compress(input: &Path, output: &Path, level: Level) -> Result<Encoded, AppError> {
    let err = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::StorageFull { AppError::io(&e) } else { engine_error("zstd", &e) }
    };
    let mut src = BufReader::new(File::open(input).map_err(|e| AppError::io(&e))?);
    let dst = BufWriter::new(File::create(output).map_err(|e| AppError::io(&e))?);
    let mut encoder = zstd::stream::Encoder::new(dst, zstd_level(level)).map_err(err)?;
    std::io::copy(&mut src, &mut encoder).map_err(err)?;
    encoder.finish().map_err(err)?.flush().map_err(err)?;
    Ok(Encoded::Written { note: format!("zstd level {}", zstd_level(level)) })
}
