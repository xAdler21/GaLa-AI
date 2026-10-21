# M0-Benchmark mit OCR auf der GPU (CUDA 12 + cuDNN 9 für die ONNX-Runtime-CUDA-Builds).

FROM rust:1-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p bench --features cuda \
    && mkdir -p /out/lib \
    && cp target/release/bench /out/ \
    # ort legt die ONNX-Runtime-Bibliotheken (inkl. CUDA-Provider) neben das Binary
    && find target/release -maxdepth 1 -name 'libonnxruntime*.so*' -exec cp {} /out/lib/ \;
# PDFium-Binary (Apache-2.0/BSD) – nicht im Repo, wird beim Build geladen
RUN curl -fsSL https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-linux-x64.tgz \
    | tar -xz -C /out lib/libpdfium.so

FROM nvidia/cuda:12.8.1-cudnn-runtime-ubuntu24.04
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/bench /usr/local/bin/bench
COPY --from=build /out/lib/ /usr/local/lib/bench/
ENV LD_LIBRARY_PATH=/usr/local/lib/bench \
    PDFIUM_DIR=/usr/local/lib/bench \
    OAR_HOME=/root/.oar
WORKDIR /out
ENTRYPOINT ["bench"]
