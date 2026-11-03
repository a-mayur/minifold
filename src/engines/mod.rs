mod bmp;
mod exif;
mod generic;
mod gif;
mod image;
mod jpeg;
mod pdf;
mod png;
mod video;
mod vips;
mod webp;

pub use video::{Hw, detect_hw};

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicU8;

use crate::error::{AppError, ErrorCode};
use crate::level::Level;
use crate::storage::remove_if_exists;

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub level: Level,
    pub strip_metadata: bool,
}

/// What an engine did with the file.
#[derive(Debug)]
pub enum Encoded {
    /// A result was written to the output path.
    Written { note: String },
    /// Written, and delivered even if not smaller: the original still holds metadata that must go.
    Stripped { note: String },
    /// The file must stay as it is (e.g. a signature would break); nothing was written.
    Unchanged { reason: String },
}

/// How the download is named, relative to the uploaded file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rename {
    Keep,
    /// e.g. ".zst": `logs.txt` → `logs.txt.zst`
    Append(&'static str),
    /// e.g. "png": `scan.bmp` → `scan.png`
    Extension(&'static str),
}

impl Rename {
    pub fn apply(self, name: &str) -> String {
        match self {
            Self::Keep => name.to_string(),
            Self::Append(suffix) => format!("{name}{suffix}"),
            Self::Extension(ext) => {
                let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
                format!("{stem}.{ext}")
            }
        }
    }
}

/// What the job produced.
#[derive(Debug)]
pub struct Outcome {
    pub output_size: u64,
    /// The original was kept as the output.
    pub no_gain: bool,
    pub rename: Rename,
    /// Shown to the user: how the file was compressed, or why the original was kept.
    pub note: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Jpeg,
    Png,
    Webp,
    Gif,
    Heic,
    Avif,
    Tiff,
    Bmp,
    OtherImage(&'static str),
    Pdf,
    Video(&'static str),
    Generic,
}

/// Fails startup when a required external tool is missing.
pub fn check_tools() -> Result<(), String> {
    png::check_pngquant()?;
    gif::check_gifsicle()?;
    pdf::check_tools()?;
    video::check_ffmpeg()?;
    vips::check_vips()
}

/// Compresses `input` into `output`. Blocking: call from `spawn_blocking`.
pub fn process(input: &Path, output: &Path, opts: Options, hw: Hw, progress: &AtomicU8) -> Result<Outcome, AppError> {
    let input_size = std::fs::metadata(input).map_err(|e| AppError::io(&e))?.len();
    let (encoded, rename) = match detect(input)? {
        Kind::Jpeg => (jpeg::compress(input, output, opts)?, Rename::Keep),
        Kind::Png => (png::compress(input, output, opts)?, Rename::Keep),
        Kind::Webp if webp::is_animated(input)? => (vips::compress(input, output, opts, vips::Target::AnimatedWebp)?, Rename::Keep),
        Kind::Webp => (webp::compress(input, output, opts)?, Rename::Keep),
        Kind::Gif => (gif::compress(input, output, opts)?, Rename::Keep),
        Kind::Heic => (vips::compress(input, output, opts, vips::Target::Heic)?, Rename::Keep),
        Kind::Avif => (vips::compress(input, output, opts, vips::Target::Avif)?, Rename::Keep),
        Kind::Tiff => (vips::compress(input, output, opts, vips::Target::Tiff)?, Rename::Keep),
        Kind::Bmp => (bmp::compress(input, output, opts)?, Rename::Extension("png")),
        Kind::OtherImage(mime) => {
            return Err(AppError::new(
                ErrorCode::UnsupportedType,
                format!(
                    "This image format ({mime}) isn't supported yet. Supported images: JPEG, PNG, WebP, GIF, HEIC, AVIF, TIFF and BMP."
                ),
            ));
        }
        Kind::Pdf => (pdf::compress(input, output, opts)?, Rename::Keep),
        Kind::Video(mime) => {
            let Some(container) = video::Container::from_mime(mime) else {
                return Err(AppError::new(
                    ErrorCode::UnsupportedType,
                    format!("This video format ({mime}) isn't supported yet. Supported videos: MP4, MOV, MKV and WebM."),
                ));
            };
            (video::compress(input, output, opts, container, hw, progress)?, Rename::Keep)
        }
        Kind::Generic => (generic::compress(input, output, opts.level)?, Rename::Append(generic::SUFFIX)),
    };

    let note = match encoded {
        Encoded::Unchanged { reason } => return keep_original(input, output, input_size, reason),
        Encoded::Written { note } => note,
        Encoded::Stripped { note } => {
            let output_size = std::fs::metadata(output).map_err(|e| AppError::io(&e))?.len();
            return Ok(Outcome { output_size, no_gain: false, rename, note });
        }
    };
    let output_size = std::fs::metadata(output).map_err(|e| AppError::io(&e))?.len();
    if output_size >= input_size {
        return keep_original(input, output, input_size, "Compressing made it bigger, so the original was kept.".into());
    }
    Ok(Outcome { output_size, no_gain: false, rename, note })
}

fn keep_original(input: &Path, output: &Path, size: u64, reason: String) -> Result<Outcome, AppError> {
    remove_if_exists(output).map_err(|e| AppError::io(&e))?;
    std::fs::rename(input, output).map_err(|e| AppError::io(&e))?;
    Ok(Outcome { output_size: size, no_gain: true, rename: Rename::Keep, note: reason })
}

/// Detects the file type from its content (magic bytes), not its name.
fn detect(path: &Path) -> Result<Kind, AppError> {
    let mut head = [0u8; 8192];
    let mut file = File::open(path).map_err(|e| AppError::io(&e))?;
    let read = file.read(&mut head).map_err(|e| AppError::io(&e))?;
    let Some(found) = infer::get(&head[..read]) else {
        return Ok(Kind::Generic);
    };
    Ok(match found.mime_type() {
        "image/jpeg" => Kind::Jpeg,
        "image/png" => Kind::Png,
        "image/webp" => Kind::Webp,
        "image/gif" => Kind::Gif,
        "image/heif" => Kind::Heic,
        "image/avif" if vips::is_avif_sequence(&head[..read]) => {
            return Err(AppError::new(ErrorCode::UnsupportedType, "Animated AVIF isn't supported yet."));
        }
        "image/avif" => Kind::Avif,
        "image/tiff" => Kind::Tiff,
        "image/bmp" => Kind::Bmp,
        "application/pdf" => Kind::Pdf,
        mime if mime.starts_with("image/") => Kind::OtherImage(mime),
        // infer reports every Matroska file as WebM; the header's DocType says which it is.
        "video/webm" | "video/x-matroska" => {
            Kind::Video(if video::is_webm_doc(&head[..read]) { "video/webm" } else { "video/x-matroska" })
        }
        mime if mime.starts_with("video/") => Kind::Video(mime),
        _ => Kind::Generic,
    })
}

/// An engine failure (as opposed to a storage failure).
fn engine_error(engine: &str, err: &dyn std::fmt::Display) -> AppError {
    tracing::error!("{engine} failed: {err}");
    AppError::new(ErrorCode::EngineFailed, format!("{engine} couldn't compress this file."))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("minifold-test-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    const LOW: Options = Options { level: Level::Low, strip_metadata: true };

    #[test]
    fn detects_by_content_not_name() {
        let pdf = temp_file("looks-like.txt", b"%PDF-1.7\n%....");
        assert_eq!(detect(&pdf).unwrap(), Kind::Pdf);
        let text = temp_file("looks-like.pdf", b"just some plain text");
        assert_eq!(detect(&text).unwrap(), Kind::Generic);
        let gif = temp_file("anim.gif", b"GIF89a\x01\x00\x01\x00\x00\x00\x00;");
        assert_eq!(detect(&gif).unwrap(), Kind::Gif);
        let ico = temp_file("x.ico", b"\x00\x00\x01\x00\x01\x00\x10\x10\x00\x00\x01\x00\x20\x00");
        assert_eq!(detect(&ico).unwrap(), Kind::OtherImage("image/vnd.microsoft.icon"));
    }

    #[test]
    fn compresses_text_and_round_trips() {
        let data = "minifold log line\n".repeat(10_000);
        let input = temp_file("in.txt", data.as_bytes());
        let output = input.with_extension("zst");
        let outcome = process(&input, &output, Options { level: Level::Medium, ..LOW }, Hw::Off, &AtomicU8::new(0)).unwrap();
        assert!(!outcome.no_gain);
        assert_eq!(outcome.rename, Rename::Append(".zst"));
        let restored = zstd::decode_all(File::open(&output).unwrap()).unwrap();
        assert_eq!(restored, data.as_bytes());
    }

    #[test]
    fn keeps_original_when_no_gain() {
        // Random bytes (xorshift) do not shrink.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let noise: Vec<u8> = (0..65_536)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 56) as u8
            })
            .collect();
        let input = temp_file("noise.bin", &noise);
        let size = std::fs::metadata(&input).unwrap().len();
        let output = input.with_extension("out");
        let outcome = process(&input, &output, LOW, Hw::Off, &AtomicU8::new(0)).unwrap();
        assert!(outcome.no_gain);
        assert_eq!(outcome.output_size, size);
        assert!(!input.exists());
    }

    #[test]
    fn rejects_unsupported_images_clearly() {
        let ico = temp_file("y.ico", b"\x00\x00\x01\x00\x01\x00\x10\x10\x00\x00\x01\x00\x20\x00");
        let err = process(&ico, &ico.with_extension("out"), LOW, Hw::Off, &AtomicU8::new(0)).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedType);
        assert!(err.message.contains("image/vnd.microsoft.icon"));
    }

    #[test]
    fn renames_downloads() {
        assert_eq!(Rename::Keep.apply("a.jpg"), "a.jpg");
        assert_eq!(Rename::Append(".zst").apply("logs.txt"), "logs.txt.zst");
        assert_eq!(Rename::Extension("png").apply("scan.final.bmp"), "scan.final.png");
        assert_eq!(Rename::Extension("png").apply("noext"), "noext.png");
    }
}
