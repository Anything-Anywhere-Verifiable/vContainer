#!/usr/bin/env bash
# Build tools/tdx-probe as a static musl binary inside vcontainer/builder:dev,
# after running its unit tests. Prints the binary path, for:
#   scripts/build-image.sh --dev --init "$(tools/tdx-probe/build.sh)"
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
docker build -q -t vcontainer/builder:dev -f "$repo/image/Containerfile" "$repo/image" >/dev/null
docker run --rm --pull=never -u "$(id -u):$(id -g)" -e HOME=/tmp -e CARGO_HOME=/work/.cache/cargo-home \
	-e RUSTFLAGS="--remap-path-prefix=/work=. -C target-feature=+crt-static" \
	-v "$repo:/work" -w /work/tools/tdx-probe vcontainer/builder:dev \
	bash -c 'cargo test --quiet --release && cargo build --quiet --release --target x86_64-unknown-linux-musl' >&2
echo "$repo/tools/tdx-probe/target/x86_64-unknown-linux-musl/release/tdx-probe"
