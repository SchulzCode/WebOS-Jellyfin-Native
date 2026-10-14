#!/usr/bin/env bash
# Build the release binary and package build/out/<id>_<version>_arm.ipk (no TV needed; sysroot/ must exist).
# DEV=1 adds the development hooks (key injection over SSH, GPU timing). RELEASE=1 builds what goes on GitHub.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
[ -f config.env ] && . ./config.env
export PATH="$HOME/.local/share/zig/current:$PATH"
# RELEASE=1: a build for publishing - no suggested server baked in, no development hooks
if [ "${RELEASE:-0}" = 1 ]; then
	[ "${DEV:-0}" = 1 ] && { echo "RELEASE=1 and DEV=1 don't go together"; exit 1; }
	unset JFNATIVE_SERVER
elif [ -n "${JFNATIVE_SERVER:-}" ]; then export JFNATIVE_SERVER; fi
FEAT=(); [ "${DEV:-0}" = 1 ] && FEAT=(--features dev)
# keep this computer's paths (home folder, user name) out of the binary's panic messages; RUSTFLAGS replaces the
# rustflags from .cargo/config.toml, so the link flag is repeated here. The last matching prefix wins: most specific last
export RUSTFLAGS="-C link-arg=-Wl,--allow-shlib-undefined --remap-path-prefix=$HOME=/home --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$ROOT=/src"
cargo zigbuild --release --target armv7-unknown-linux-gnueabi.2.35 "${FEAT[@]}"
ARES="${ARES_PACKAGE:-ares-package}"
rm -rf build/stage build/out; mkdir -p build/stage build/out
cp -r pkg/. build/stage/
cp target/armv7-unknown-linux-gnueabi/release/jfnative build/stage/jfnative
cp -r assets build/stage/assets
"$ARES" build/stage -o build/out --no-minify >/dev/null
IPK=$(ls build/out/*.ipk); echo "package: $IPK  sha256 $(sha256sum "$IPK" | cut -d' ' -f1)"
