# syntax=docker/dockerfile:1.7
# Worker: CPU-Rolle (Standard) oder Vision-Rolle mit GPU (OCR, Embeddings).
# Build-Kontext ist das Repo-Root.
#
#   CPU (gala-coordinator):
#     docker build -f docker/worker.Dockerfile -t gala/worker-cpu .
#   Vision (gala-ocr, CUDA 12.x für Maxwell/GTX 950):
#     docker build -f docker/worker.Dockerfile -t gala/worker-vision \
#       --build-arg FEATURES=vision \
#       --build-arg RUNTIME_IMAGE=nvidia/cuda:12.8.1-cudnn-runtime-ubuntu24.04 .
#
# FEATURES        Cargo-Features des Pakets worker (leer = keine, "vision" = GPU-OCR)
# RUNTIME_IMAGE   Laufzeit-Basis: debian:bookworm-slim (CPU) oder nvidia/cuda:*-cudnn-runtime
# PDFIUM_URL      PDFium-Binary (Apache-2.0/BSD), nicht im Repo

ARG RUNTIME_IMAGE=debian:bookworm-slim

FROM rust:1-bookworm AS build
ARG FEATURES=""
ARG PDFIUM_URL=https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-linux-x64.tgz
ENV RUSTUP_TOOLCHAIN=stable
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Cache-Mounts: Registry und Build-Verzeichnis überleben zwischen Builds.
RUN --mount=type=cache,id=gala-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=gala-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=gala-target-worker,target=/src/target \
    cargo build --release --locked -p worker ${FEATURES:+--features $FEATURES} \
    && mkdir -p /out/lib \
    && cp target/release/worker /out/worker \
    # ort legt die ONNX-Runtime-Bibliotheken (inkl. CUDA-Provider) neben das Binary
    && find target/release -maxdepth 1 -name 'libonnxruntime*.so*' -exec cp {} /out/lib/ \;
RUN curl -fsSL "$PDFIUM_URL" | tar -xz -C /out lib/libpdfium.so

FROM ${RUNTIME_IMAGE}
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl3 ca-certificates libgomp1 \
    && rm -rf /var/lib/apt/lists/* \
    # UID/GID 10001 = Host-Nutzer "gala" (schreibt in den NFS-Datei-Store)
    && groupadd --system --gid 10001 gala \
    && useradd --system --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin gala \
    && mkdir -p /srv/gala/data /var/cache/gala/oar \
    && chown -R gala:gala /srv/gala /var/cache/gala
COPY --from=build /out/worker /usr/local/bin/worker
COPY --from=build /out/lib/ /usr/local/lib/gala/
USER gala:gala
WORKDIR /home
ENV LD_LIBRARY_PATH=/usr/local/lib/gala \
    PDFIUM_DIR=/usr/local/lib/gala \
    OAR_HOME=/var/cache/gala/oar \
    GALA_DATA_DIR=/srv/gala/data \
    GALA_ARCHIV_ROOT=/mnt/archiv
ENTRYPOINT ["worker"]
CMD ["--role", "cpu"]
