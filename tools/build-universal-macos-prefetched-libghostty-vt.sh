#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFETCH_ROOT="${FLOWMUX_GHOSTTY_PREFETCH_DIR:-$ROOT_DIR/vendor/ghostty-prefetch}"
TARGET_AARCH64="aarch64-apple-darwin"
TARGET_X86_64="x86_64-apple-darwin"
UNIVERSAL_TARGET_DIR="$ROOT_DIR/target/universal2-apple-darwin/release"
ACTIVE_TOOLCHAIN=""
TOOLCHAIN_RUSTC=""

source "$ROOT_DIR/tools/prefetch-libghostty-vt.sh"

init_rustup_toolchain() {
    ACTIVE_TOOLCHAIN="$(rustup show active-toolchain | awk '{print $1}')"
    [[ -n "$ACTIVE_TOOLCHAIN" ]] || {
        echo "failed to determine active rustup toolchain" >&2
        exit 1
    }

    TOOLCHAIN_RUSTC="$(rustup which --toolchain "$ACTIVE_TOOLCHAIN" rustc)"
    [[ -n "$TOOLCHAIN_RUSTC" && -x "$TOOLCHAIN_RUSTC" ]] || {
        echo "failed to locate rustc for toolchain $ACTIVE_TOOLCHAIN" >&2
        exit 1
    }
}

rustup_toolchain_run() {
    rustup run "$ACTIVE_TOOLCHAIN" "$@"
}

toolchain_cargo_build() {
    RUSTC="$TOOLCHAIN_RUSTC" \
        CARGO_BUILD_RUSTC="$TOOLCHAIN_RUSTC" \
        rustup_toolchain_run cargo build "$@"
}

ensure_rust_target_installed() {
    local target="$1"

    if rustup target list --toolchain "$ACTIVE_TOOLCHAIN" --installed | grep -Fxq "$target"; then
        return
    fi

    echo "installing missing Rust target for toolchain $ACTIVE_TOOLCHAIN: $target"
    rustup target add --toolchain "$ACTIVE_TOOLCHAIN" "$target"
}

ensure_rust_target_usable() {
    local target="$1"

    rustup_toolchain_run rustc --print target-libdir --target "$target" >/dev/null
}

if ! prefetched_inputs_ready; then
    main
fi

export GHOSTTY_SOURCE_DIR="$PREFETCH_ROOT/ghostty-src"
export GHOSTTY_ZIG_SYSTEM_DIR="$PREFETCH_ROOT/zig-system"
export LIBGHOSTTY_VT_SYS_OPTIMIZE="${LIBGHOSTTY_VT_SYS_OPTIMIZE:-ReleaseFast}"

cd "$ROOT_DIR"

need_cmd rustup
need_cmd lipo
need_cmd awk
init_rustup_toolchain
ensure_rust_target_installed "$TARGET_AARCH64"
ensure_rust_target_installed "$TARGET_X86_64"
ensure_rust_target_usable "$TARGET_AARCH64"
ensure_rust_target_usable "$TARGET_X86_64"

toolchain_cargo_build --release --locked --target "$TARGET_AARCH64" "$@"
toolchain_cargo_build --release --locked --target "$TARGET_X86_64" "$@"

mkdir -p "$UNIVERSAL_TARGET_DIR"
lipo -create \
    -output "$UNIVERSAL_TARGET_DIR/flowmux" \
    "target/$TARGET_AARCH64/release/flowmux" \
    "target/$TARGET_X86_64/release/flowmux"
