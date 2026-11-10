# syntax=docker/dockerfile:1.7
# Worker: CPU-Rolle (Standard) oder Vision-Rolle mit GPU (OCR, Embeddings).
# Build-Kontext ist das Repo-Root.
#
#   CPU (gala-coordinator):
#     docker build -f docker/worker.Dockerfile -t gala/worker-cpu .
#   Vision (gala-ocr, CUDA 12.x für Maxwell/GTX 950):
#     docker build -f docker/worker.Dockerfile -t gala/worker-vision \
#       --build-arg FEATURES=vision \
#       --build-arg BUILD_IMAGE=rust:1-trixie \
#       --build-arg RUNTIME_IMAGE=nvidia/cuda:12.8.1-cudnn-runtime-ubuntu24.04 .
#
# FEATURES        Cargo-Features des Pakets worker (leer = keine, "vision" = GPU-OCR)
# BUILD_IMAGE     Build-Basis. Mit "vision" muss sie glibc >= 2.38 und libstdc++ >= 13
#                 haben (vorgebaute ONNX Runtime, sonst Linkfehler __isoc23_strtol):
#                 rust:1-trixie. Die Laufzeit-Basis darf nicht älter sein als das,
#                 was das Binary verlangt (Ubuntu 24.04 passt zu trixie, bookworm nicht).
# RUNTIME_IMAGE   Laufzeit-Basis: debian:bookworm-slim (CPU) oder nvidia/cuda:*-cudnn-runtime
# PDFIUM_URL      PDFium-Binary (Apache-2.0/BSD), nicht im Repo

ARG BUILD_IMAGE=rust:1-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

FROM ${BUILD_IMAGE} AS build
ARG FEATURES=""
ARG PDFIUM_URL=https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-linux-x64.tgz
ENV RUSTUP_TOOLCHAIN=stable
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Cache-Mounts: Registry und Build-Verzeichnis überleben zwischen Builds.
# sharing=locked: Coordinator und Worker teilen sich die Registry; parallele
# Builds (compose) würden sonst gleichzeitig dieselben Crates entpacken.
RUN --mount=type=cache,id=gala-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=gala-cargo-git,target=/usr/local/cargo/git,sharing=locked \
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
# Das Binary liegt im selben Ordner wie die Provider-Bibliotheken der ONNX Runtime
# und wird mit absolutem Pfad gestartet: die (statisch gelinkte) Runtime sucht
# libonnxruntime_providers_*.so neben argv[0]. Mit einem Aufruf über PATH sucht sie
# im Arbeitsverzeichnis, findet nichts und fällt still auf die CPU zurück.
COPY --from=build /out/worker /usr/local/lib/gala/worker
COPY --from=build /out/lib/ /usr/local/lib/gala/
RUN ln -s /usr/local/lib/gala/worker /usr/local/bin/worker
USER gala:gala
WORKDIR /home
ENV LD_LIBRARY_PATH=/usr/local/lib/gala \
    PDFIUM_DIR=/usr/local/lib/gala \
    OAR_HOME=/var/cache/gala/oar \
    GALA_DATA_DIR=/srv/gala/data \
    GALA_ARCHIV_ROOT=/mnt/archiv \
    # BGE-M3 (nur Rolle vision): gleicher Ordner wie der Coordinator, über den
    # Datei-Store geteilt. Ohne die Angabe schreibt fastembed ins Arbeitsverzeichnis,
    # das dem Nutzer gala nicht gehört.
    GALA_EMBED_CACHE_DIR=/srv/gala/data/models/fastembed
ENTRYPOINT ["/usr/local/lib/gala/worker"]
CMD ["--role", "cpu"]
