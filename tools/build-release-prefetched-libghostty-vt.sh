#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFETCH_ROOT="${FLOWMUX_GHOSTTY_PREFETCH_DIR:-$ROOT_DIR/vendor/ghostty-prefetch}"

source "$ROOT_DIR/tools/prefetch-libghostty-vt.sh"

if ! prefetched_inputs_ready; then
    main
fi

export GHOSTTY_SOURCE_DIR="$PREFETCH_ROOT/ghostty-src"
export GHOSTTY_ZIG_SYSTEM_DIR="$PREFETCH_ROOT/zig-system"
export LIBGHOSTTY_VT_SYS_OPTIMIZE="${LIBGHOSTTY_VT_SYS_OPTIMIZE:-ReleaseFast}"

cd "$ROOT_DIR"
exec cargo build --release --locked "$@"
