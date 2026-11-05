# Changelog

## v0.2.0

### Added

- **Material 3 interface.** The app and admin pages are redesigned with Material 3: top app bar, segmented buttons for the compression level, cards, a floating Compress button, M3 switches and text fields. Fonts (Roboto) and icons (Material Symbols) are bundled, so nothing is loaded from outside the server.
- **Light and dark mode.** A button in the top bar switches between them; each visitor's choice is remembered in their browser. The first visit follows the admin's default mode (System by default, which follows the device).
- **Theme section on the admin page.** Pick the default mode and a pastel palette for light and dark mode separately: Peach (default), Sage, Dusty blue, Lavender, Blush or Butter. Changes apply immediately for everyone, without a restart.
- New app icon: the Accordion, in pastel orange (Unraid template and the app's logo and favicon).
- Screenshots in the README.

### Changed

- CI runs only when a version tag is pushed, not on every push or pull request. The `:edge` image is no longer published.
- Existing `settings.toml` files get the new `theme_mode`, `theme_light` and `theme_dark` keys added automatically.

## v0.1.1

### Fixed

- The container no longer fails to start when the access and admin tokens are left empty, which is what Unraid does with blank template fields ("MINIFOLD_AUTH_TOKEN is not set"). Minifold now generates any missing token on first start, prints it once in the container log, and saves it in `/config/tokens.toml` (readable only by the container user). Later starts reuse it, and a token you set yourself always takes priority.

- Unraid: the container failed to start with "docker: bad format for path:" because the optional GPU field in the template is passed to Docker even when empty. The field is removed; for GPU encoding add `--device=/dev/dri --group-add=<GID>` to Extra Parameters (described in the template).

### Changed

- Unraid template and `docker-compose.yml`: the token fields are optional and explain where generated tokens can be found.

## v0.1.0 — first release

Minifold is a lightweight, self-hosted file compressor. Drag and drop files into the web page, pick a level, and download smaller files. Everything is processed on your own server: no telemetry, no external requests, nothing leaves the machine.

### Highlights

- **Keeps the original format.** A JPEG stays a JPEG, a PDF stays a PDF, an MP4 stays an MP4. The only exception is BMP, which becomes PNG (lossless) because BMP can't be compressed meaningfully. Files that aren't images, PDFs or videos are compressed with zstd and downloaded as `.zst`.
- **Four levels:** Low, Medium, High and Extra, from near-lossless to smallest.
- **Privacy first.** Metadata such as GPS location, camera details, PDF author and video location is removed by default, while color profiles, rotation and track languages are kept so files look and play the same. Originals are deleted right after compression, and compressed files after a retention time you choose.
- **Never worse than the original.** If compression wouldn't make a file smaller, the original is returned. With metadata stripping on, it's returned *with the metadata removed*, never with its GPS data.
- **Clear errors.** Damaged, password-protected, unsupported or oversized files get a specific message and error code instead of a broken result.
- **Small:** a single 4.5 MB Rust binary with the web interface built in, using about 2 MB of RAM when idle.

### Supported formats

| Type | Engine | Notes |
|---|---|---|
| JPEG (RGB, grayscale, CMYK) | mozjpeg | |
| PNG (incl. animated) | oxipng, pngquant at High/Extra | Lossless at Low/Medium |
| WebP (incl. animated) | libwebp / libvips | Lossless WebP stays lossless |
| GIF (incl. animated) | gifsicle | Lossless at Low/Medium |
| HEIC | libvips + libheif (HEVC) | |
| AVIF | libvips + libheif (AV1) | |
| TIFF (multi-page, 16-bit) | libvips | Always lossless |
| BMP | → PNG | Lossless conversion |
| PDF | qpdf + Ghostscript | Lossless at Low; images downsampled to 300 / 150 / 72 dpi above that |
| Video: MP4, MOV, MKV, WebM | FFmpeg (x264, x265, VP9) | HDR and 10-bit kept; Extra caps at 720p |
| Anything else | zstd | Downloaded as `.zst` |

PDFs with fillable forms, accessibility tags or password protection only get lossless optimization, so none of those are lost. Digitally signed PDFs are left unchanged.

### Admin page (`/admin`)

- Maximum file size (default 2 GB)
- Number of files compressed at the same time (default 2)
- Retention: delete originals right after compression (default on), how long to keep compressed files (default 60 minutes), and delete after first download (default off)
- Strip metadata (default on)
- Hardware video encoding (default off), see below
- Settings are saved to `/config/settings.toml`. **Restart now** applies them; Minifold exits cleanly and Docker starts it again.

### Hardware video encoding (experimental)

At startup Minifold looks for NVIDIA (NVENC), Intel (Quick Sync) and Intel/AMD (VA-API) GPUs and runs a short test encode with each one. Only encoders that pass are offered on the admin page. If the selected encoder is missing at a later start, the admin page shows a warning and video jobs fail until the setting is changed. A failed hardware encode fails the job instead of silently switching to the CPU.

This is marked experimental because it hasn't been tested on real GPU hardware yet. Feedback is welcome.

### Install

- **Docker:** `ghcr.io/a-mayur/minifold:0.1.0` (also `:latest`), for amd64 and arm64. Set `MINIFOLD_AUTH_TOKEN` and `MINIFOLD_ADMIN_TOKEN` (16+ characters each, different from each other), mount `/config` and `/data`, and use `--restart unless-stopped`. A `docker-compose.yml` is included.
- **Unraid:** use the template in `unraid/minifold.xml`. The README explains GPU passthrough.

See the [README](README.md) for configuration, the API and all error codes.

### Known limitations

- Not supported yet, each with a clear error: animated AVIF, HEIC/AVIF files with several images, ICO, JPEG XL, camera RAW, PSD, and AVI/FLV/WMV/MPEG video.
- Images are limited to 512 MB and 150 megapixels, because they are decoded in memory.
- Time limits per file: one hour for Ghostscript (PDF) and six hours for FFmpeg (video).
- Jobs are kept in memory, so a restart cancels running jobs and deletes their files.
- When metadata is stripped from HEIC, depth maps, HDR gain maps and thumbnails are not carried over.
- Some older Intel GPUs need Debian's non-free `intel-media-va-driver-non-free` for hardware encoding, which isn't included. Without it, the encoder simply isn't offered.

### Licenses

Minifold is MIT-licensed. Every library linked into the binary is permissively licensed, and CI enforces this with `cargo deny`. GPL/AGPL tools (FFmpeg, Ghostscript, pngquant, gifsicle) are only run as separate programs. The image includes `/usr/share/doc/minifold/THIRD_PARTY_LICENSES.md` with all license texts. H.264 and HEVC are patent-encumbered in some countries; VP9, AV1 and Opus are royalty-free.
