# syntax=docker/dockerfile:1.7
# Worker: Rolle cpu (Standard) oder Rolle vision (OCR und Embeddings, auf der CPU).
# Build-Kontext ist das Repo-Root.
#
#   Rolle cpu (gala-coordinator):
#     docker build -f docker/worker.Dockerfile -t gala/worker-cpu .
#   Rolle vision (gala-ocr):
#     docker build -f docker/worker.Dockerfile -t gala/worker-vision \
#       --build-arg FEATURES=vision \
#       --build-arg BUILD_IMAGE=rust:1-trixie \
#       --build-arg RUNTIME_IMAGE=debian:trixie-slim .
#
# FEATURES        Cargo-Features des Pakets worker (leer = keine, "vision" = OCR und Embeddings)
# BUILD_IMAGE     Build-Basis. Mit "vision" muss sie glibc >= 2.38 und libstdc++ >= 13
#                 haben (vorgebaute ONNX Runtime, sonst Linkfehler __isoc23_strtol):
#                 rust:1-trixie. Die Laufzeit-Basis darf nicht älter sein als das,
#                 was das Binary verlangt (trixie-slim passt zu trixie, bookworm nicht).
# RUNTIME_IMAGE   Laufzeit-Basis: debian:bookworm-slim (cpu) oder debian:trixie-slim (vision)
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
    # Legt ort ONNX-Runtime-Bibliotheken neben das Binary, kommen sie mit (sonst ist
    # die Runtime statisch gelinkt und es gibt nichts zu kopieren).
    && find target/release -maxdepth 1 -name 'libonnxruntime*.so*' -exec cp {} /out/lib/ \;
RUN curl -fsSL "$PDFIUM_URL" | tar -xz -C /out lib/libpdfium.so

FROM ${RUNTIME_IMAGE}
RUN apt-get update \
    # OpenSSL 3 heißt ab trixie libssl3t64, davor libssl3.
    && (apt-get install -y --no-install-recommends libssl3t64 \
        || apt-get install -y --no-install-recommends libssl3) \
    && apt-get install -y --no-install-recommends ca-certificates libgomp1 \
    && rm -rf /var/lib/apt/lists/* \
    # UID/GID 10001 = Host-Nutzer "gala" (schreibt in den NFS-Datei-Store)
    && groupadd --system --gid 10001 gala \
    && useradd --system --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin gala \
    && mkdir -p /srv/gala/data /var/cache/gala/oar \
    && chown -R gala:gala /srv/gala /var/cache/gala
# Binary und Bibliotheken (PDFium) liegen im selben Ordner; gestartet wird mit
# absolutem Pfad. (Die ONNX Runtime sucht Zusatzbibliotheken neben argv[0].)
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
# Commit des Builds; der Worker meldet ihn im Admin-Panel. Erst hier gesetzt, damit
# ein neuer Wert die Layer davor nicht ungültig macht.
ARG GALA_BUILD_REV=""
ENV GALA_BUILD_REV=${GALA_BUILD_REV}
ENTRYPOINT ["/usr/local/lib/gala/worker"]
CMD ["--role", "cpu"]
