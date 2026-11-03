//! Limits and helpers shared by the image engines.

use std::path::Path;

use super::Encoded;
use crate::error::{AppError, ErrorCode, human_bytes};

/// Images are decoded in memory; these bound how much RAM one job can take.
const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PIXELS: u64 = 150_000_000;

pub fn read(path: &Path) -> Result<Vec<u8>, AppError> {
    let size = std::fs::metadata(path).map_err(|e| AppError::io(&e))?.len();
    if size > MAX_IMAGE_BYTES {
        return Err(AppError::new(
            ErrorCode::ImageTooLarge,
            format!("Images can be at most {} ({} uploaded).", human_bytes(MAX_IMAGE_BYTES), human_bytes(size)),
        ));
    }
    std::fs::read(path).map_err(|e| AppError::io(&e))
}

pub fn check_pixels(width: u64, height: u64) -> Result<(), AppError> {
    if width * height > MAX_PIXELS {
        return Err(AppError::new(
            ErrorCode::ImageTooLarge,
            format!(
                "This image is {width} × {height} pixels ({} megapixels). The limit is {} megapixels.",
                width * height / 1_000_000,
                MAX_PIXELS / 1_000_000
            ),
        ));
    }
    Ok(())
}

pub fn corrupt(format: &str) -> AppError {
    AppError::new(ErrorCode::CorruptInput, format!("This {format} is damaged or incomplete, so it can't be read."))
}

pub struct Candidate {
    pub bytes: Vec<u8>,
    pub note: String,
}

/// Writes the smallest candidate to `output`.
pub fn write_smallest(output: &Path, candidates: Vec<Candidate>) -> Result<Encoded, AppError> {
    for c in &candidates {
        tracing::debug!(size = c.bytes.len(), note = %c.note, "candidate");
    }
    let best = candidates
        .into_iter()
        .min_by_key(|c| c.bytes.len())
        .expect("at least one candidate");
    std::fs::write(output, &best.bytes).map_err(|e| AppError::io(&e))?;
    Ok(Encoded::Written { note: best.note })
}

/// " · metadata removed" or " · metadata kept", for notes.
pub fn metadata_note(strip: bool) -> &'static str {
    if strip { " · metadata removed" } else { " · metadata kept" }
}
