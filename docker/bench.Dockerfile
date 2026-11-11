# syntax=docker/dockerfile:1.7
# M0-Benchmark mit OCR auf der GPU (CUDA 12 + cuDNN 9).
#
# Setzt auf dem Vision-Worker-Image auf: Laufzeit-Basis, PDFium und der
# CUDA-12-Provider der ONNX Runtime kommen von dort (Gründe in worker.Dockerfile).
# Das Bench-Image selbst ist dadurch nur eine dünne Schicht mit dem Binary.
#
#   # zuerst das Worker-Image bauen (auf gala-ocr: deploy/common/dc.sh ocr build)
#   docker build -f docker/bench.Dockerfile -t gala/bench:local .
#   docker run --rm --gpus all -v <ordner>:/in:ro gala/bench:local ocr /in --device cuda
#
# Auf gala-ocr erledigt das deploy/ocr/gpu-test.sh.

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
    cargo build --release --locked -p bench --features cuda \
    && mkdir -p /out \
    && cp target/release/bench /out/bench

FROM ${WORKER_IMAGE}
# Neben die Provider-Bibliotheken legen und mit absolutem Pfad starten: die
# ONNX Runtime sucht libonnxruntime_providers_*.so neben argv[0].
COPY --from=build /out/bench /usr/local/lib/gala/bench
# Läuft wie der Worker als Nutzer gala; Modelle unter $OAR_HOME (/var/cache/gala/oar).
WORKDIR /tmp
ENTRYPOINT ["/usr/local/lib/gala/bench"]
CMD ["--help"]
