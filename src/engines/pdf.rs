//! PDF: qpdf (Apache-2.0) optimizes losslessly; at Medium/High/Extra, Ghostscript (AGPL-3.0)
//! also shrinks images. Both run as separate programs. The smaller result wins.
//!
//! Ghostscript rewrites the whole document and drops fillable forms, accessibility tags and
//! encryption, so those PDFs only get the lossless qpdf pass. Signed PDFs are not touched.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

use super::image::metadata_note;
use super::{Encoded, Options, engine_error};
use crate::error::{AppError, ErrorCode};
use crate::level::Level;
use crate::storage::remove_if_exists;

/// shortcut: one fixed limit for every PDF, make it a setting if huge scans need longer.
const GHOSTSCRIPT_TIMEOUT_SECS: &str = "3600";

pub fn check_tools() -> Result<(), String> {
    for (tool, purpose) in [("qpdf", "PDF optimization"), ("gs", "PDF image compression (Ghostscript)"), ("timeout", "job time limits")] {
        match Command::new(tool).arg("--version").output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => return Err(format!("{tool} --version failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
            Err(e) => return Err(format!("{tool} is not installed or not on PATH ({e}); it is needed for {purpose}")),
        }
    }
    Ok(())
}

/// Ghostscript's standard presets: images are downsampled to roughly 300 / 150 / 72 dpi.
fn ghostscript_preset(level: Level) -> Option<(&'static str, &'static str)> {
    match level {
        Level::Low => None,
        Level::Medium => Some(("/printer", "300 dpi")),
        Level::High => Some(("/ebook", "150 dpi")),
        Level::Extra => Some(("/screen", "72 dpi")),
    }
}

pub fn compress(input: &Path, output: &Path, opts: Options) -> Result<Encoded, AppError> {
    let doc = Inspection::of(input)?;
    if doc.signed {
        return Ok(Encoded::Unchanged {
            reason: "Kept unchanged: this PDF is digitally signed, and any change would invalidate the signature.".into(),
        });
    }
    let meta = metadata_note(opts.strip_metadata);
    let lossless = output.with_extension("qpdf.pdf");
    optimize(input, &lossless, opts.strip_metadata)?;

    let Some((preset, dpi)) = ghostscript_preset(opts.level) else {
        return finish(&lossless, output, format!("qpdf, lossless{meta}"));
    };
    if let Some(reason) = doc.ghostscript_would_remove() {
        let note = format!("qpdf, lossless only: images weren't shrunk because that would remove its {reason}{meta}");
        return finish(&lossless, output, note);
    }

    let shrunk = match shrink(input, output, preset, opts.strip_metadata) {
        Ok(path) => path,
        Err(e) => {
            remove(&lossless)?;
            return Err(e);
        }
    };
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).map_err(|e| AppError::io(&e));
    if size(&shrunk)? < size(&lossless)? {
        remove(&lossless)?;
        finish(&shrunk, output, format!("Ghostscript (images ≤ {dpi}) + qpdf{meta}"))
    } else {
        remove(&shrunk)?;
        finish(&lossless, output, format!("qpdf, lossless (shrinking images didn't help){meta}"))
    }
}

/// Ghostscript, a page-count sanity check, then the lossless qpdf pass on its output.
fn shrink(input: &Path, output: &Path, preset: &str, strip: bool) -> Result<std::path::PathBuf, AppError> {
    let gs_out = output.with_extension("gs.pdf");
    let result = ghostscript(input, &gs_out, preset).and_then(|()| {
        if page_count(input)? != page_count(&gs_out)? {
            return Err(AppError::new(ErrorCode::EngineFailed, "Ghostscript lost pages from this PDF. Try the Low level."));
        }
        let optimized = output.with_extension("gs.qpdf.pdf");
        optimize(&gs_out, &optimized, strip)?;
        Ok(optimized)
    });
    remove(&gs_out)?;
    result
}

fn ghostscript(input: &Path, output: &Path, preset: &str) -> Result<(), AppError> {
    let out = Command::new("timeout")
        .args(["--kill-after=30", GHOSTSCRIPT_TIMEOUT_SECS, "gs", "-q", "-dSAFER", "-dBATCH", "-dNOPAUSE", "-sDEVICE=pdfwrite"])
        .args(["-dCompatibilityLevel=1.7", "-dDetectDuplicateImages=true"])
        .arg(format!("-dPDFSETTINGS={preset}"))
        .arg(format!("-sOutputFile={}", output.display()))
        .arg(input)
        .output()
        .map_err(|e| engine_error("Ghostscript", &e))?;
    match out.status.code() {
        Some(0) => Ok(()),
        Some(124) | Some(137) => Err(AppError::new(
            ErrorCode::Timeout,
            "Shrinking this PDF took longer than an hour, so it was stopped. Try the Low level.",
        )),
        _ => {
            tracing::warn!("Ghostscript failed: {}{}", String::from_utf8_lossy(&out.stderr).trim(), String::from_utf8_lossy(&out.stdout).trim());
            Err(AppError::new(
                ErrorCode::EngineFailed,
                "Ghostscript couldn't process this PDF. Try the Low level, which only optimizes losslessly.",
            ))
        }
    }
}

/// Lossless: object streams, recompressed streams, unused objects dropped. With `strip`, the
/// document info (author, creator, dates) and the catalog's XMP metadata are removed too.
fn optimize(input: &Path, output: &Path, strip: bool) -> Result<(), AppError> {
    let patch = output.with_extension("patch.json");
    let mut args: Vec<&OsStr> = ["--object-streams=generate", "--compress-streams=y", "--recompress-flate", "--compression-level=9"]
        .iter()
        .map(OsStr::new)
        .collect();
    let update;
    if strip {
        let (header, mut trailer, root_key, mut catalog) = trailer_and_root(input)?;
        trailer.as_object_mut().map(|t| t.remove("/Info"));
        catalog.as_object_mut().map(|c| c.remove("/Metadata"));
        let body = json!({ "qpdf": [header, { "trailer": { "value": trailer }, root_key: { "value": catalog } }] });
        std::fs::write(&patch, body.to_string()).map_err(|e| AppError::io(&e))?;
        update = format!("--update-from-json={}", patch.display());
        args.push(OsStr::new(&update));
    }
    let result = qpdf(args.into_iter().chain([input.as_os_str(), output.as_os_str()]));
    remove(&patch)?;
    result.map(|_| ())
}

struct Inspection {
    signed: bool,
    form: bool,
    tagged: bool,
    encrypted: bool,
}

impl Inspection {
    fn of(input: &Path) -> Result<Self, AppError> {
        let out = qpdf(["--json=2", "--json-key=encrypt", "--json-key=acroform"].map(OsStr::new).into_iter().chain([input.as_os_str()]))?;
        let json: Value = serde_json::from_slice(&out.stdout).map_err(|e| engine_error("qpdf", &e))?;
        let fields = json["acroform"]["fields"].as_array().cloned().unwrap_or_default();
        let (_, _, _, catalog) = trailer_and_root(input)?;
        Ok(Self {
            signed: fields.iter().any(|f| f["fieldtype"] == "/Sig" && !f["value"].is_null()),
            form: !fields.is_empty(),
            tagged: catalog.get("/StructTreeRoot").is_some(),
            encrypted: json["encrypt"]["encrypted"] == true,
        })
    }

    fn ghostscript_would_remove(&self) -> Option<&'static str> {
        if self.encrypted {
            Some("password protection")
        } else if self.form {
            Some("fillable form fields")
        } else if self.tagged {
            Some("accessibility tags")
        } else {
            None
        }
    }
}

/// The JSON header, the trailer and the catalog (document root), as qpdf's JSON v2 shows them.
fn trailer_and_root(input: &Path) -> Result<(Value, Value, String, Value), AppError> {
    let object = |name: &str| -> Result<Vec<Value>, AppError> {
        let selector = format!("--json-object={name}");
        let out = qpdf(["--json=2", "--json-key=qpdf", &selector].map(OsStr::new).into_iter().chain([input.as_os_str()]))?;
        let json: Value = serde_json::from_slice(&out.stdout).map_err(|e| engine_error("qpdf", &e))?;
        json["qpdf"].as_array().cloned().filter(|a| a.len() == 2).ok_or_else(|| engine_error("qpdf", &"unexpected JSON"))
    };
    let [header, objects] = <[Value; 2]>::try_from(object("trailer")?).expect("length checked");
    let trailer = objects["trailer"]["value"].clone();
    // "/Root": "1 0 R" → object selector "1,0"
    let root_ref = trailer["/Root"].as_str().ok_or_else(corrupt)?.to_string();
    let mut parts = root_ref.split_whitespace();
    let (id, generation) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let root = object(&format!("{id},{generation}"))?;
    let root_key = format!("obj:{root_ref}");
    let catalog = root[1][&root_key]["value"].clone();
    if !catalog.is_object() {
        return Err(corrupt());
    }
    Ok((header, trailer, root_key, catalog))
}

fn page_count(path: &Path) -> Result<u64, AppError> {
    let out = qpdf([OsStr::new("--show-npages"), path.as_os_str()])?;
    String::from_utf8_lossy(&out.stdout).trim().parse().map_err(|_| corrupt())
}

/// Runs qpdf. Exit 3 means "succeeded with warnings" (e.g. a repaired file) and is fine.
fn qpdf<'a>(args: impl IntoIterator<Item = &'a OsStr>) -> Result<Output, AppError> {
    let out = Command::new("qpdf").args(args).output().map_err(|e| engine_error("qpdf", &e))?;
    if matches!(out.status.code(), Some(0 | 3)) {
        return Ok(out);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("invalid password") {
        return Err(AppError::new(
            ErrorCode::PasswordProtected,
            "This PDF needs a password to open, so it can't be compressed. Remove the password and try again.",
        ));
    }
    tracing::warn!("qpdf failed: {}", stderr.trim());
    Err(corrupt())
}

fn corrupt() -> AppError {
    super::image::corrupt("PDF")
}

fn finish(from: &Path, output: &Path, note: String) -> Result<Encoded, AppError> {
    std::fs::rename(from, output).map_err(|e| AppError::io(&e))?;
    Ok(Encoded::Written { note })
}

fn remove(path: &Path) -> Result<(), AppError> {
    remove_if_exists(path).map_err(|e| AppError::io(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal one-page PDF with an author; the xref is left for qpdf to rebuild.
    const PDF: &[u8] = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\n\
4 0 obj << /Author (SECRET AUTHOR) >> endobj\n\
trailer << /Root 1 0 R /Info 4 0 R /Size 5 >>\n%%EOF\n";

    fn run(level: Level, strip: bool) -> (Encoded, Vec<u8>) {
        let input = crate::engines::tests::temp_file(&format!("doc-{level:?}-{strip}.pdf"), PDF);
        let output = input.with_extension("out");
        let encoded = compress(&input, &output, Options { level, strip_metadata: strip }).unwrap();
        (encoded, std::fs::read(&output).unwrap())
    }

    #[test]
    fn strips_document_info_and_keeps_pages() {
        let (encoded, out) = run(Level::Low, true);
        assert!(matches!(encoded, Encoded::Written { .. }));
        assert!(!out.windows(13).any(|w| w == b"SECRET AUTHOR"));
        let dir = std::env::temp_dir().join(format!("minifold-pdf-check-{}.pdf", std::process::id()));
        std::fs::write(&dir, &out).unwrap();
        assert_eq!(page_count(&dir).unwrap(), 1);
    }

    #[test]
    fn ghostscript_path_produces_a_valid_pdf() {
        let (encoded, out) = run(Level::High, false);
        assert!(matches!(encoded, Encoded::Written { .. }));
        assert!(out.starts_with(b"%PDF-"));
    }
}
