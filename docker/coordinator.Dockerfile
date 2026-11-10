# syntax=docker/dockerfile:1.7
# Coordinator (axum): Desktop-API, Admin-Panel, interne Routen.
# Build-Kontext ist das Repo-Root:
#   docker build -f docker/coordinator.Dockerfile -t gala/coordinator .

FROM rust:1-bookworm AS build
# rust-toolchain.toml verlangt u. a. das wasm32-Target (für die Desktop-App);
# für den Server-Build genügt der mitgelieferte stable-Toolchain.
ENV RUSTUP_TOOLCHAIN=stable
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# Cargo-Features des Coordinators. "embed" aktiviert den Dokumenten-Chatbot
# (BGE-M3 über fastembed/ONNX Runtime für die Frage-Embeddings); ohne das
# Feature antwortet POST /api/v1/chat mit 501.
ARG FEATURES="embed"
COPY . .
# Cache-Mounts: Registry und Build-Verzeichnis überleben zwischen Builds.
# Das Binary wird innerhalb desselben RUN aus dem Cache-Mount herauskopiert.
RUN --mount=type=cache,id=gala-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=gala-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=gala-target-coordinator,target=/src/target \
    cargo build --release --locked -p coordinator --features "$FEATURES" \
    && mkdir -p /out \
    && cp target/release/coordinator /out/coordinator

FROM debian:bookworm-slim
# curl: Healthcheck im compose-File
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl3 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    # UID/GID 10001 = Host-Nutzer "gala" (Dateien in /srv/gala/data, /var/lib/gala/state)
    && groupadd --system --gid 10001 gala \
    && useradd --system --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin gala \
    && mkdir -p /srv/gala/data /var/lib/gala/state \
    && chown gala:gala /srv/gala/data /var/lib/gala/state
COPY --from=build /out/coordinator /usr/local/bin/coordinator
USER gala:gala
WORKDIR /home
ENV GALA_BIND=0.0.0.0:8080 \
    GALA_INTERNAL_BIND=0.0.0.0:8081 \
    GALA_DATA_DIR=/srv/gala/data \
    GALA_STATE_DIR=/var/lib/gala/state \
    GALA_EMBED_CACHE_DIR=/srv/gala/data/models/fastembed
EXPOSE 8080 8081
ENTRYPOINT ["coordinator"]
