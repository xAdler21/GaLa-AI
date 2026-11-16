# syntax=docker/dockerfile:1.7
# M0-Benchmark (OCR auf der CPU).
#
# Setzt auf dem Vision-Worker-Image auf: Laufzeit-Basis und PDFium kommen von dort.
# Das Bench-Image selbst ist dadurch nur eine dünne Schicht mit dem Binary.
#
#   # zuerst das Worker-Image bauen (auf gala-ocr: deploy/common/dc.sh ocr build)
#   docker build -f docker/bench.Dockerfile -t gala/bench:local .
#   docker run --rm -v <ordner>:/in:ro gala/bench:local ocr /in
#
# Auf gala-ocr erledigt das deploy/ocr/ocr-test.sh.

ARG WORKER_IMAGE=gala/worker-vision:local

FROM rust:1-trixie AS build
ENV RUSTUP_TOOLCHAIN=stable
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Gleiches Build-Verzeichnis wie der Worker (id=gala-target-worker): die großen
# Abhängigkeiten (ort, oar-ocr) sind dort schon kompiliert und belegen die Platte
# nicht ein zweites Mal. Nicht gleichzeitig mit dem Worker bauen.
RUN --mount=type=cache,id=gala-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=gala-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=gala-target-worker,target=/src/target \
    cargo build --release --locked -p bench \
    && mkdir -p /out \
    && cp target/release/bench /out/bench

FROM ${WORKER_IMAGE}
COPY --from=build /out/bench /usr/local/lib/gala/bench
# Läuft wie der Worker als Nutzer gala; Modelle unter $OAR_HOME (/var/cache/gala/oar).
WORKDIR /tmp
ENTRYPOINT ["/usr/local/lib/gala/bench"]
CMD ["--help"]
