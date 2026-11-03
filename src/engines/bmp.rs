//! BMP has no useful compression of its own, so it is converted to PNG (lossless) and then
//! optimized by the PNG engine. This is the one case where the format changes.

use std::io::Cursor;
use std::path::Path;

use super::image::{self, corrupt};
use super::{Encoded, Options, png};
use crate::error::AppError;
use crate::storage::remove_if_exists;

pub fn compress(input: &Path, output: &Path, opts: Options) -> Result<Encoded, AppError> {
    let data = image::read(input)?;
    let (width, height) = dimensions(&data).ok_or_else(|| corrupt("BMP"))?;
    image::check_pixels(width, height)?;

    let decoded = ::image::load_from_memory_with_format(&data, ::image::ImageFormat::Bmp).map_err(|e| {
        tracing::warn!("BMP decode failed: {e}");
        corrupt("BMP")
    })?;
    let mut png_bytes = Vec::new();
    decoded
        .write_to(&mut Cursor::new(&mut png_bytes), ::image::ImageFormat::Png)
        .map_err(|e| super::engine_error("PNG encoder", &e))?;

    // The PNG engine works from a file (pngquant needs one).
    let converted = output.with_extension("bmp.png");
    std::fs::write(&converted, &png_bytes).map_err(|e| AppError::io(&e))?;
    let result = png::compress(&converted, output, opts);
    remove_if_exists(&converted).map_err(|e| AppError::io(&e))?;
    match result? {
        Encoded::Written { note } => Ok(Encoded::Written { note: format!("Converted to PNG · {note}") }),
        unchanged => Ok(unchanged),
    }
}

/// Width and height from the BMP info header (height is negative for top-down images).
fn dimensions(data: &[u8]) -> Option<(u64, u64)> {
    if !data.starts_with(b"BM") {
        return None;
    }
    let w = i32::from_le_bytes(data.get(18..22)?.try_into().ok()?);
    let h = i32::from_le_bytes(data.get(22..26)?.try_into().ok()?);
    Some((w.unsigned_abs() as u64, h.unsigned_abs() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dimensions_including_top_down() {
        let mut h = b"BM".to_vec();
        h.resize(26, 0);
        h[18..22].copy_from_slice(&640i32.to_le_bytes());
        h[22..26].copy_from_slice(&(-480i32).to_le_bytes());
        assert_eq!(dimensions(&h), Some((640, 480)));
        assert_eq!(dimensions(b"GIF89a"), None);
    }

    #[test]
    fn converts_to_png_losslessly() {
        let img = ::image::RgbImage::from_fn(40, 30, |x, y| ::image::Rgb([x as u8 * 6, y as u8 * 8, 90]));
        let mut bmp = Vec::new();
        img.write_to(&mut Cursor::new(&mut bmp), ::image::ImageFormat::Bmp).unwrap();
        let input = crate::engines::tests::temp_file("in.bmp", &bmp);
        let output = input.with_extension("out");
        let opts = Options { level: crate::level::Level::Medium, strip_metadata: true };
        assert!(matches!(compress(&input, &output, opts).unwrap(), Encoded::Written { .. }));
        let back = ::image::load_from_memory(&std::fs::read(&output).unwrap()).unwrap().to_rgb8();
        assert_eq!(back, img);
    }
}
