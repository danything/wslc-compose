# 開発用イメージ。Windows に Rust や Build Tools を入れずに、wslc のコンテナで
# Windows 向け (x86_64-pc-windows-gnu) にクロスコンパイルする。使い方は README
FROM rust:1.99

RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-pc-windows-gnu \
 && rustup component add clippy rustfmt

ENV CARGO_TARGET_DIR=/target
WORKDIR /src
