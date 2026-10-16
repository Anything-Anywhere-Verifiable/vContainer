#!/usr/bin/env bash
# Builds the guest /init (static musl) inside the pinned builder container.
set -euo pipefail
cd "$(dirname "$0")/.."
docker build -q -t vcontainer/builder:dev -f image/Containerfile image >/dev/null
mkdir -p .cache/cargo
docker run --rm -u "$(id -u):$(id -g)" -v "$PWD:/work" -w /work \
    -e CARGO_HOME=/work/.cache/cargo -e SOURCE_DATE_EPOCH="$(git log -1 --format=%ct 2>/dev/null || echo 1767225600)" \
    -e RUSTFLAGS="--remap-path-prefix=/work=. --remap-path-prefix=/work/.cache/cargo=cargo" \
    vcontainer/builder:dev \
    cargo build --locked --release -p vcontainer-init --target x86_64-unknown-linux-musl
ls -l target/x86_64-unknown-linux-musl/release/vcontainer-init
