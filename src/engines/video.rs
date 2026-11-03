//! Video via FFmpeg (run as a separate program). The container stays the same (MP4, MOV, MKV,
//! WebM). H.264 is used unless the source is HEVC or HDR (then HEVC, 10-bit kept), WebM uses
//! VP9. Optional hardware encoding (NVENC, Quick Sync, VA-API) for H.264/HEVC.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};

use serde_json::Value;

use super::image::metadata_note;
use super::{Encoded, Options, engine_error};
use crate::error::{AppError, ErrorCode};
use crate::level::Level;
use crate::settings::HwAccel;
use crate::storage::remove_if_exists;

/// shortcut: one fixed limit for every video, make it a setting if long 4K files need more.
const VIDEO_TIMEOUT_SECS: &str = "21600";
const REQUIRED_ENCODERS: [&str; 5] = ["libx264", "libx265", "libvpx-vp9", "aac", "libopus"];

/// Hardware acceleration as resolved at startup from the setting and what was detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hw {
    Off,
    Use(HwAccel),
    /// Selected in the settings but not found at startup: video jobs fail until it's changed.
    Missing(HwAccel),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    Webm,
}

impl Container {
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "video/mp4" | "video/x-m4v" => Some(Self::Mp4),
            "video/quicktime" => Some(Self::Mov),
            "video/x-matroska" => Some(Self::Mkv),
            "video/webm" => Some(Self::Webm),
            _ => None,
        }
    }

    fn muxer(self) -> &'static [&'static str] {
        match self {
            Self::Mp4 => &["-f", "mp4", "-movflags", "+faststart"],
            Self::Mov => &["-f", "mov", "-movflags", "+faststart"],
            Self::Mkv => &["-f", "matroska"],
            Self::Webm => &["-f", "webm"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    H264,
    Hevc,
    Vp9,
}

fn choose_codec(container: Container, source_codec: &str, hdr: bool) -> Codec {
    match container {
        Container::Webm => Codec::Vp9,
        // H.264 can't carry HDR or 10-bit in a widely playable way, so those stay HEVC.
        _ if hdr || source_codec == "hevc" => Codec::Hevc,
        _ => Codec::H264,
    }
}

/// CRF / quality value per codec and level (lower = better quality, bigger file).
fn quality(codec: Codec, level: Level) -> u8 {
    let table = match codec {
        Codec::H264 => [20, 23, 28, 32],
        Codec::Hevc => [22, 26, 30, 34],
        Codec::Vp9 => [31, 35, 40, 45],
    };
    table[level as usize]
}

fn audio_bitrate(container: Container, level: Level) -> &'static str {
    let table = if container == Container::Webm { ["128k", "112k", "96k", "64k"] } else { ["192k", "160k", "128k", "96k"] };
    table[level as usize]
}

pub fn check_ffmpeg() -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map_err(|e| format!("ffmpeg is not installed or not on PATH ({e}); it is needed for video compression"))?;
    let list = String::from_utf8_lossy(&out.stdout);
    let missing: Vec<_> = REQUIRED_ENCODERS.iter().filter(|e| !list.contains(&format!(" {e} "))).collect();
    if !missing.is_empty() {
        return Err(format!("ffmpeg is missing encoders: {missing:?}"));
    }
    Command::new("ffprobe").arg("-version").output().map_err(|e| format!("ffprobe is not installed or not on PATH ({e})"))?;
    Ok(())
}

/// Finds accelerators that can actually encode: each one with a device present gets a
/// one-second test encode using exactly the arguments real jobs use.
pub fn detect_hw() -> Vec<HwAccel> {
    let nvidia = Path::new("/dev/nvidia0").exists();
    let render = render_node().is_some();
    [(HwAccel::Nvenc, nvidia), (HwAccel::Qsv, render), (HwAccel::Vaapi, render)]
        .into_iter()
        .filter(|(_, device)| *device)
        .map(|(hw, _)| hw)
        .filter(|hw| {
            let args = HwArgs::new(*hw, Codec::H264, Level::Medium, false);
            let ok = Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error"])
                .args(&args.before_input)
                .args(["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=10", "-t", "1"])
                .args(args.filter.iter().flat_map(|f| ["-vf", f.as_str()]))
                .args(&args.encoder)
                .args(["-f", "null", "-"])
                .output()
                .is_ok_and(|o| o.status.success());
            tracing::info!(accelerator = ?hw, works = ok, "hardware encoder test");
            ok
        })
        .collect()
}

fn render_node() -> Option<PathBuf> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("renderD")))
        .collect();
    nodes.sort();
    nodes.into_iter().next()
}

/// Encoder arguments for one hardware accelerator (CPU decode, GPU encode).
struct HwArgs {
    before_input: Vec<String>,
    filter: Option<String>,
    encoder: Vec<String>,
    label: String,
}

impl HwArgs {
    fn new(hw: HwAccel, codec: Codec, level: Level, ten_bit: bool) -> Self {
        let q = quality(codec, level).to_string();
        let ten = ten_bit && codec == Codec::Hevc;
        let name = |h264: &str, hevc: &str| if codec == Codec::Hevc { hevc.to_string() } else { h264.to_string() };
        let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let main10 = if ten { s(&["-profile:v", "main10"]) } else { vec![] };
        match hw {
            HwAccel::Nvenc => {
                let enc = name("h264_nvenc", "hevc_nvenc");
                let mut encoder = s(&["-c:v", &enc, "-preset", "p5", "-rc", "vbr", "-cq", &q, "-b:v", "0", "-pix_fmt"]);
                encoder.push(if ten { "p010le" } else { "yuv420p" }.into());
                encoder.extend(main10);
                Self { before_input: vec![], filter: None, encoder, label: format!("{enc} (NVIDIA), CQ {q}") }
            }
            HwAccel::Qsv => {
                let enc = name("h264_qsv", "hevc_qsv");
                let mut encoder = s(&["-c:v", &enc, "-global_quality", &q]);
                encoder.extend(main10);
                Self {
                    before_input: s(&["-init_hw_device", "qsv=hw", "-filter_hw_device", "hw"]),
                    filter: Some(format!("format={},hwupload=extra_hw_frames=64", if ten { "p010le" } else { "nv12" })),
                    encoder,
                    label: format!("{enc} (Intel Quick Sync), quality {q}"),
                }
            }
            HwAccel::Vaapi => {
                let enc = name("h264_vaapi", "hevc_vaapi");
                let device = render_node().map_or_else(|| "/dev/dri/renderD128".into(), |p| p.display().to_string());
                let mut encoder = s(&["-c:v", &enc, "-rc_mode", "CQP", "-qp", &q]);
                encoder.extend(main10);
                Self {
                    before_input: s(&["-vaapi_device", &device]),
                    filter: Some(format!("format={},hwupload", if ten { "p010" } else { "nv12" })),
                    encoder,
                    label: format!("{enc} (VA-API), QP {q}"),
                }
            }
            HwAccel::Off => unreachable!("Off has no hardware arguments"),
        }
    }
}

fn cpu_args(codec: Codec, level: Level, ten_bit: bool, container: Container) -> (Vec<String>, String) {
    let q = quality(codec, level).to_string();
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    match codec {
        Codec::H264 => (s(&["-c:v", "libx264", "-preset", "medium", "-crf", &q, "-pix_fmt", "yuv420p"]), format!("libx264, CRF {q}")),
        Codec::Hevc => {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let mut a = s(&["-c:v", "libx265", "-preset", "medium", "-crf", &q, "-pix_fmt", pix, "-x265-params", "log-level=error"]);
            if container != Container::Mkv {
                a.extend(s(&["-tag:v", "hvc1"])); // plays on Apple devices
            }
            (a, format!("libx265, CRF {q}{}", if ten_bit { ", 10-bit" } else { "" }))
        }
        Codec::Vp9 => (
            s(&["-c:v", "libvpx-vp9", "-crf", &q, "-b:v", "0", "-row-mt", "1", "-deadline", "good", "-cpu-used", "2"]),
            format!("libvpx-vp9, CRF {q}"),
        ),
    }
}

struct Stream {
    codec: String,
    language: Option<String>,
}

struct Probe {
    duration: Option<f64>,
    video_codec: String,
    ten_bit: bool,
    hdr: bool,
    /// Audio and subtitle streams in output order (attached pictures and data streams excluded).
    audio: Vec<Stream>,
    subtitles: Vec<Stream>,
    video_languages: Vec<Option<String>>,
}

fn probe(input: &Path) -> Result<Probe, AppError> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(input)
        .output()
        .map_err(|e| engine_error("ffprobe", &e))?;
    if !out.status.success() {
        tracing::warn!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        return Err(super::image::corrupt("video"));
    }
    let json: Value = serde_json::from_slice(&out.stdout).map_err(|e| engine_error("ffprobe", &e))?;
    let streams: Vec<&Value> = json["streams"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let stream = |s: &Value| Stream {
        codec: s["codec_name"].as_str().unwrap_or_default().to_string(),
        language: s["tags"]["language"].as_str().map(str::to_string),
    };
    let videos: Vec<&&Value> =
        streams.iter().filter(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1).collect();
    let Some(main) = videos.first() else {
        return Err(AppError::new(ErrorCode::UnsupportedType, "This file has no video track, so it can't be compressed as a video."));
    };
    let pix = main["pix_fmt"].as_str().unwrap_or_default();
    let transfer = main["color_transfer"].as_str().unwrap_or_default();
    Ok(Probe {
        duration: json["format"]["duration"].as_str().and_then(|d| d.parse().ok()),
        video_codec: main["codec_name"].as_str().unwrap_or_default().to_string(),
        ten_bit: pix.contains("10") || pix.contains("12"),
        hdr: matches!(transfer, "smpte2084" | "arib-std-b67"),
        audio: streams.iter().filter(|s| s["codec_type"] == "audio").map(|s| stream(s)).collect(),
        subtitles: streams.iter().filter(|s| s["codec_type"] == "subtitle").map(|s| stream(s)).collect(),
        video_languages: videos.iter().map(|s| s["tags"]["language"].as_str().map(str::to_string)).collect(),
    })
}

pub fn compress(
    input: &Path,
    output: &Path,
    opts: Options,
    container: Container,
    hw: Hw,
    progress: &AtomicU8,
) -> Result<Encoded, AppError> {
    if let Hw::Missing(missing) = hw {
        return Err(AppError::new(
            ErrorCode::HwUnavailable,
            format!(
                "{} is selected for video but wasn't found when Minifold started. Choose another option on the admin page.",
                missing.label()
            ),
        ));
    }
    let p = probe(input)?;
    let codec = choose_codec(container, &p.video_codec, p.hdr);
    let ten_bit = codec == Codec::Hevc && (p.ten_bit || p.hdr);
    // VP9 hardware encoding is rare, so WebM always encodes on the CPU.
    let accel = match hw {
        Hw::Use(a) if codec != Codec::Vp9 => Some(a),
        _ => None,
    };

    let mut filters = Vec::new();
    if opts.level == Level::Extra {
        // Longer side at most 1280 px (720p); rotation is applied before filters.
        filters.push("scale='if(gte(iw,ih),min(1280,iw),-2)':'if(gte(iw,ih),-2,min(1280,ih))'".to_string());
    }
    let (before_input, video, label) = match accel {
        Some(a) => {
            let h = HwArgs::new(a, codec, opts.level, ten_bit);
            filters.extend(h.filter);
            (h.before_input, h.encoder, h.label)
        }
        None => {
            let (args, label) = cpu_args(codec, opts.level, ten_bit, container);
            (vec![], args, format!("{label} (CPU)"))
        }
    };
    let audio = audio_args(&p, container, opts.level);

    let tmp = output.with_extension("ffmpeg");
    let mut args: Vec<String> = before_input;
    args.extend(["-i".into(), input.display().to_string()]);
    args.extend(maps(container));
    if !filters.is_empty() {
        args.extend(["-vf".into(), filters.join(",")]);
    }
    args.extend(video);
    args.extend(audio);
    args.extend(metadata_args(&p, opts.strip_metadata));
    args.extend(container.muxer().iter().map(|a| a.to_string()));
    args.push(tmp.display().to_string());

    if let Err(e) = run_ffmpeg(&args, p.duration, progress) {
        remove(&tmp)?;
        return Err(match (e.code, accel) {
            (ErrorCode::EngineFailed, Some(a)) => AppError::new(
                ErrorCode::HwEncodeFailed,
                format!("Hardware encoding ({}) failed for this video. Turn hardware acceleration off on the admin page and try again.", a.label()),
            ),
            _ => e,
        });
    }
    if let Err(e) = check_duration(&tmp, p.duration) {
        remove(&tmp)?;
        return Err(e);
    }
    let meta = metadata_note(opts.strip_metadata);
    let scaled = if opts.level == Level::Extra { ", up to 720p" } else { "" };
    let note = format!("{label}{scaled}{meta}");

    let input_size = size(input)?;
    if size(&tmp)? < input_size || !opts.strip_metadata {
        std::fs::rename(&tmp, output).map_err(|e| AppError::io(&e))?;
        return Ok(Encoded::Written { note });
    }
    // Re-encoding didn't help, but the original still has its metadata (e.g. GPS):
    // copy the streams into a new file without it instead of returning the original.
    remove(&tmp)?;
    let mut remux: Vec<String> = vec!["-i".into(), input.display().to_string()];
    remux.extend(maps(container));
    remux.extend(["-c".into(), "copy".into()]);
    remux.extend(metadata_args(&p, true));
    remux.extend(container.muxer().iter().map(|a| a.to_string()));
    remux.push(output.display().to_string());
    if let Err(e) = run_ffmpeg(&remux, p.duration, progress) {
        remove(output)?;
        return Err(e);
    }
    Ok(Encoded::Stripped { note: "Metadata removed; video unchanged (re-encoding wouldn't make it smaller)".into() })
}

/// All real video streams (not cover art), all audio and subtitles; data streams (e.g.
/// GoPro GPS telemetry) are dropped. MKV attachments (subtitle fonts) are kept.
fn maps(container: Container) -> Vec<String> {
    let mut m: Vec<String> = ["-map", "0:V", "-map", "0:a?", "-map", "0:s?", "-c:s", "copy"].map(String::from).to_vec();
    if container == Container::Mkv {
        m.extend(["-map", "0:t?", "-c:t", "copy"].map(String::from));
    }
    m
}

/// Efficient audio is copied as is; anything else (PCM, ALAC, AC-3 …) is encoded.
fn audio_args(p: &Probe, container: Container, level: Level) -> Vec<String> {
    let webm = container == Container::Webm;
    let mut a = Vec::new();
    for (i, s) in p.audio.iter().enumerate() {
        let efficient = if webm { matches!(s.codec.as_str(), "opus" | "vorbis") } else { s.codec == "aac" };
        if efficient {
            a.extend([format!("-c:a:{i}"), "copy".into()]);
        } else {
            a.extend([format!("-c:a:{i}"), if webm { "libopus" } else { "aac" }.into()]);
            a.extend([format!("-b:a:{i}"), audio_bitrate(container, level).into()]);
        }
    }
    a
}

/// Stripping drops all metadata (location, dates, device, titles) but puts back each
/// track's language, which players need to pick audio and subtitles.
fn metadata_args(p: &Probe, strip: bool) -> Vec<String> {
    if !strip {
        return vec![];
    }
    let mut a: Vec<String> = ["-map_metadata", "-1"].map(String::from).to_vec();
    let langs = p
        .video_languages
        .iter()
        .enumerate()
        .map(|(i, l)| ("v", i, l))
        .chain(p.audio.iter().enumerate().map(|(i, s)| ("a", i, &s.language)))
        .chain(p.subtitles.iter().enumerate().map(|(i, s)| ("s", i, &s.language)));
    for (kind, i, lang) in langs {
        if let Some(lang) = lang {
            a.extend([format!("-metadata:s:{kind}:{i}"), format!("language={lang}")]);
        }
    }
    a
}

/// Runs ffmpeg under a time limit, reporting progress (0–100) from `-progress`.
fn run_ffmpeg(args: &[String], duration: Option<f64>, progress: &AtomicU8) -> Result<(), AppError> {
    let mut child = Command::new("timeout")
        .args(["--kill-after=30", VIDEO_TIMEOUT_SECS, "ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(["-nostats", "-progress", "pipe:1", "-y"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| engine_error("FFmpeg", &e))?;
    // Drain stderr on its own thread so a chatty decoder can't block the pipe.
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    for line in BufReader::new(child.stdout.take().expect("piped")).lines().map_while(Result::ok) {
        if let (Some(us), Some(total)) = (line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()), duration)
            && total > 0.0
        {
            progress.store((us / 1e6 / total * 100.0).clamp(0.0, 99.0) as u8, Ordering::Relaxed);
        }
    }
    let status = child.wait().map_err(|e| engine_error("FFmpeg", &e))?;
    let stderr = errors.join().unwrap_or_default();
    match status.code() {
        Some(0) => {
            progress.store(100, Ordering::Relaxed);
            Ok(())
        }
        Some(124 | 137) => Err(AppError::new(ErrorCode::Timeout, "Compressing this video took longer than 6 hours, so it was stopped.")),
        _ => {
            tracing::warn!("FFmpeg failed: {}", stderr.trim());
            Err(AppError::new(ErrorCode::EngineFailed, "FFmpeg couldn't compress this video; it may be damaged."))
        }
    }
}

/// FFmpeg keeps going past broken frames, so a damaged file can silently come out shorter.
fn check_duration(output: &Path, expected: Option<f64>) -> Result<(), AppError> {
    let Some(expected) = expected else { return Ok(()) };
    let got = probe(output)?.duration.unwrap_or(0.0);
    if got < expected * 0.98 - 0.5 {
        tracing::warn!(expected, got, "video came out shorter than the original");
        return Err(AppError::new(
            ErrorCode::CorruptInput,
            format!("This video is damaged: only {got:.0} of {expected:.0} seconds could be read."),
        ));
    }
    Ok(())
}

fn size(path: &Path) -> Result<u64, AppError> {
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| AppError::io(&e))
}

fn remove(path: &Path) -> Result<(), AppError> {
    remove_if_exists(path).map_err(|e| AppError::io(&e))
}

/// Matroska and WebM share a container; the EBML header's DocType element (0x4282) tells them apart.
pub fn is_webm_doc(head: &[u8]) -> bool {
    let window = &head[..head.len().min(64)];
    window.windows(2).position(|w| w == [0x42, 0x82]).is_some_and(|i| {
        // One-byte EBML size (0x80 | length), then the DocType string.
        let len = window.get(i + 2).map_or(0, |b| (b & 0x7F) as usize);
        window.get(i + 3..i + 3 + len) == Some(b"webm")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_webm_from_matroska() {
        assert!(is_webm_doc(b"\x1aE\xdf\xa3\xa3B\x86\x81\x01B\x82\x84webmB\x87"));
        assert!(!is_webm_doc(b"\x1aE\xdf\xa3\xa3B\x86\x81\x01B\x82\x88matroskaB"));
    }

    #[test]
    fn picks_codec_by_container_and_source() {
        assert_eq!(choose_codec(Container::Mp4, "h264", false), Codec::H264);
        assert_eq!(choose_codec(Container::Mov, "prores", false), Codec::H264);
        assert_eq!(choose_codec(Container::Mov, "hevc", false), Codec::Hevc);
        assert_eq!(choose_codec(Container::Mkv, "av1", true), Codec::Hevc, "HDR stays HDR");
        assert_eq!(choose_codec(Container::Webm, "vp8", false), Codec::Vp9);
    }

    #[test]
    fn maps_mimes_to_containers() {
        assert_eq!(Container::from_mime("video/quicktime"), Some(Container::Mov));
        assert_eq!(Container::from_mime("video/x-msvideo"), None);
    }
}
