#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFETCH_ROOT="${FLOWMUX_GHOSTTY_PREFETCH_DIR:-$ROOT_DIR/vendor/ghostty-prefetch}"
TARGET_AARCH64="aarch64-apple-darwin"
TARGET_X86_64="x86_64-apple-darwin"
UNIVERSAL_TARGET_DIR="$ROOT_DIR/target/universal2-apple-darwin/release"

source "$ROOT_DIR/tools/prefetch-libghostty-vt.sh"

if ! prefetched_inputs_ready; then
    main
fi

export GHOSTTY_SOURCE_DIR="$PREFETCH_ROOT/ghostty-src"
export GHOSTTY_ZIG_SYSTEM_DIR="$PREFETCH_ROOT/zig-system"
export LIBGHOSTTY_VT_SYS_OPTIMIZE="${LIBGHOSTTY_VT_SYS_OPTIMIZE:-ReleaseFast}"

cd "$ROOT_DIR"

cargo build --release --locked --target "$TARGET_AARCH64" "$@"
cargo build --release --locked --target "$TARGET_X86_64" "$@"

mkdir -p "$UNIVERSAL_TARGET_DIR"
lipo -create \
    -output "$UNIVERSAL_TARGET_DIR/flowmux" \
    "target/$TARGET_AARCH64/release/flowmux" \
    "target/$TARGET_X86_64/release/flowmux"
