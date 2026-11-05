# ---- build ----
FROM rust:1.97-slim-trixie AS build
# nasm: SIMD code for mozjpeg (JPEG)
RUN apt-get update && apt-get install -y --no-install-recommends nasm && rm -rf /var/lib/apt/lists/*
RUN cargo install --locked cargo-about@0.9.3 --features cli
WORKDIR /src
COPY Cargo.toml Cargo.lock about.toml about.hbs ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked \
    && cargo about generate --locked about.hbs -o THIRD_PARTY_LICENSES.md

# ---- runtime ----
FROM debian:trixie-slim
# Image tools, all run as separate programs (not linked into Minifold):
#   pngquant (GPL-3.0)      lossy PNG color reduction
#   gifsicle (GPL-2.0)      GIF optimization
#   libvips-tools (LGPL)    HEIC, AVIF, TIFF, animated WebP
#   libheif plugins         HEVC (x265, GPL-2.0) and AV1 (libaom, BSD-2) encoders/decoders for HEIC/AVIF
#   qpdf (Apache-2.0)       lossless PDF optimization
#   ghostscript (AGPL-3.0)  PDF image downsampling
#   ffmpeg (GPL build)      video (x264, x265, libvpx, Opus; NVENC/QSV/VA-API)
#   VA-API drivers          Intel (intel-media-va-driver) and AMD (mesa-va-drivers) GPUs
RUN apt-get update && apt-get install -y --no-install-recommends \
        pngquant gifsicle libvips-tools qpdf ghostscript ffmpeg mesa-va-drivers \
        $([ "$(dpkg --print-architecture)" = amd64 ] && echo intel-media-va-driver) \
        libheif-plugin-x265 libheif-plugin-libde265 libheif-plugin-aomenc libheif-plugin-dav1d \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin minifold \
    && mkdir /data /config && chown minifold /data /config
COPY --from=build /src/target/release/minifold /usr/local/bin/minifold
COPY LICENSE /usr/share/doc/minifold/LICENSE
COPY web/fonts/LICENSE-Roboto-OFL.txt web/LICENSE-Material-Symbols.txt /usr/share/doc/minifold/
COPY --from=build /src/THIRD_PARTY_LICENSES.md /usr/share/doc/minifold/THIRD_PARTY_LICENSES.md
USER minifold
ENV MINIFOLD_DATA_DIR=/data \
    MINIFOLD_CONFIG_DIR=/config \
    MINIFOLD_BIND=0.0.0.0:8080
VOLUME ["/data", "/config"]
EXPOSE 8080
STOPSIGNAL SIGTERM
CMD ["minifold"]
