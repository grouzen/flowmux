#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFETCH_ROOT="${FLOWMUX_GHOSTTY_PREFETCH_DIR:-$ROOT_DIR/vendor/ghostty-prefetch}"
GHOSTTY_REPO="https://github.com/ghostty-org/ghostty.git"
GHOSTTY_COMMIT="bfe633a9487892ff3d27ed727db540267f22ef90"

GHOSTTY_SOURCE_DIR="$PREFETCH_ROOT/ghostty-src"
GHOSTTY_ZIG_SYSTEM_DIR="$PREFETCH_ROOT/zig-system"
ZIG_GLOBAL_CACHE_DIR="$PREFETCH_ROOT/zig-global-cache"
ARTIFACT_DIR="$PREFETCH_ROOT/artifacts"
STAMP_FILE="$GHOSTTY_SOURCE_DIR/.flowmux-ghostty-commit"

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "missing required command: $1" >&2
        exit 1
    }
}

clone_or_update_ghostty() {
    if [[ -d "$GHOSTTY_SOURCE_DIR/.git" ]] \
        && [[ -f "$STAMP_FILE" ]] \
        && [[ "$(cat "$STAMP_FILE")" == "$GHOSTTY_COMMIT" ]]; then
        return
    fi

    rm -rf "$GHOSTTY_SOURCE_DIR"
    mkdir -p "$PREFETCH_ROOT"

    git clone --filter=blob:none --no-checkout "$GHOSTTY_REPO" "$GHOSTTY_SOURCE_DIR"
    git -C "$GHOSTTY_SOURCE_DIR" checkout "$GHOSTTY_COMMIT"
    printf '%s\n' "$GHOSTTY_COMMIT" >"$STAMP_FILE"
}

prefetch_zig_packages() {
    local zon_nix_file="$GHOSTTY_SOURCE_DIR/build.zig.zon.nix"
    [[ -f "$zon_nix_file" ]] || {
        echo "missing $zon_nix_file" >&2
        exit 1
    }

    rm -rf "$GHOSTTY_ZIG_SYSTEM_DIR"
    mkdir -p "$GHOSTTY_ZIG_SYSTEM_DIR" "$ZIG_GLOBAL_CACHE_DIR" "$ARTIFACT_DIR"

    perl -0ne '
        while (/\{\s*name = "([^"]+)";\s*path = fetchZigArtifact \{\s*name = "[^"]+";\s*url = "([^"]+)";/sg) {
            print "$1\t$2\n";
        }
    ' "$zon_nix_file" | while IFS=$'\t' read -r pkg_hash pkg_url; do
        [[ -n "$pkg_hash" && -n "$pkg_url" ]] || continue

        echo "prefetching $pkg_url"
        fetched_hash="$(prefetch_one_package "$pkg_hash" "$pkg_url")"
        if [[ "$fetched_hash" != "$pkg_hash" ]]; then
            echo "hash mismatch for $pkg_url: expected $pkg_hash, got $fetched_hash" >&2
            exit 1
        fi

        ln -sfn "$ZIG_GLOBAL_CACHE_DIR/p/$pkg_hash" "$GHOSTTY_ZIG_SYSTEM_DIR/$pkg_hash"
    done
}

prefetch_one_package() {
    local pkg_hash="$1"
    local pkg_url="$2"

    case "$pkg_url" in
        http://*|https://*)
            local artifact_name artifact_path
            artifact_name="$(basename "${pkg_url%%\?*}")"
            artifact_path="$ARTIFACT_DIR/$pkg_hash-$artifact_name"
            curl -L --fail --silent --show-error -o "$artifact_path" "$pkg_url"
            zig fetch --global-cache-dir "$ZIG_GLOBAL_CACHE_DIR" "$artifact_path"
            ;;
        *)
            zig fetch --global-cache-dir "$ZIG_GLOBAL_CACHE_DIR" "$pkg_url"
            ;;
    esac
}

print_exports() {
    cat <<EOF
Prefetch complete.

export GHOSTTY_SOURCE_DIR="$GHOSTTY_SOURCE_DIR"
export GHOSTTY_ZIG_SYSTEM_DIR="$GHOSTTY_ZIG_SYSTEM_DIR"
EOF
}

main() {
    need_cmd git
    need_cmd zig
    need_cmd perl
    need_cmd curl

    clone_or_update_ghostty
    prefetch_zig_packages
    print_exports
}

main "$@"
