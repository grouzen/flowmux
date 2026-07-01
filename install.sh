#!/bin/sh
set -eu

OWNER_REPO="grouzen/flowmux"
BINARY_NAME="flowmux"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
BASE_DOWNLOAD_URL="https://github.com/${OWNER_REPO}/releases/download"
LATEST_API_URL="https://api.github.com/repos/${OWNER_REPO}/releases/latest"

INSTALL_DIR="${FLOWMUX_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
REQUESTED_VERSION="${FLOWMUX_VERSION:-}"
ASSUME_YES="${FLOWMUX_YES:-0}"

usage() {
    cat <<'EOF'
Install Flowmux from GitHub Releases.

Usage:
  install.sh [--version <tag>] [--install-dir <dir>] [--yes]
  curl -fsSL https://raw.githubusercontent.com/grouzen/flowmux/main/install.sh | sh

Options:
  --version <tag>      Install a specific release tag (for example: v0.1.2)
  --install-dir <dir>  Install directory (default: ~/.local/bin)
  --yes                Skip the overwrite prompt when flowmux already exists
  --help               Show this help text

Environment:
  FLOWMUX_VERSION      Same as --version
  FLOWMUX_INSTALL_DIR  Same as --install-dir
  FLOWMUX_YES=1        Same as --yes
EOF
}

log() {
    printf '%s\n' "$*"
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

download_to() {
    url="$1"
    dest="$2"

    if has_cmd curl; then
        curl -fsSL "$url" -o "$dest"
        return
    fi

    if has_cmd wget; then
        wget -qO "$dest" "$url"
        return
    fi

    fail "missing required download tool: curl or wget"
}

download_text() {
    url="$1"

    if has_cmd curl; then
        curl -fsSL "$url"
        return
    fi

    if has_cmd wget; then
        wget -qO- "$url"
        return
    fi

    fail "missing required download tool: curl or wget"
}

resolve_version() {
    if [ -n "$REQUESTED_VERSION" ]; then
        printf '%s\n' "$REQUESTED_VERSION"
        return
    fi

    latest_json="$(download_text "$LATEST_API_URL" | tr -d '\n')"
    version="$(printf '%s' "$latest_json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
    [ -n "$version" ] || fail "failed to resolve latest release tag from GitHub"
    printf '%s\n' "$version"
}

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64)
                    printf '%s\n' "x86_64-unknown-linux-gnu"
                    ;;
                *)
                    fail "unsupported Linux architecture: $arch (supported: x86_64)"
                    ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64|arm64)
                    printf '%s\n' "universal2-apple-darwin"
                    ;;
                *)
                    fail "unsupported macOS architecture: $arch (supported: x86_64, arm64)"
                    ;;
            esac
            ;;
        *)
            fail "unsupported operating system: $os (supported: Linux, macOS)"
            ;;
    esac
}

print_tmux_guidance() {
    os="$(uname -s)"

    if [ "$os" = "Darwin" ]; then
        printf '%s\n' "Install tmux first: brew install tmux"
        return
    fi

    if has_cmd apt-get; then
        printf '%s\n' "Install tmux first: sudo apt-get update && sudo apt-get install -y tmux"
    elif has_cmd dnf; then
        printf '%s\n' "Install tmux first: sudo dnf install tmux"
    elif has_cmd yum; then
        printf '%s\n' "Install tmux first: sudo yum install tmux"
    elif has_cmd pacman; then
        printf '%s\n' "Install tmux first: sudo pacman -S tmux"
    elif has_cmd zypper; then
        printf '%s\n' "Install tmux first: sudo zypper install tmux"
    elif has_cmd apk; then
        printf '%s\n' "Install tmux first: sudo apk add tmux"
    else
        printf '%s\n' "Install tmux with your system package manager, then rerun this installer."
    fi
}

ensure_tmux_installed() {
    if has_cmd tmux; then
        return
    fi

    print_tmux_guidance >&2
    fail "tmux is required to run Flowmux"
}

checksum_for_archive() {
    archive_name="$1"
    checksums_file="$2"
    awk -v file="$archive_name" '$2 == file { print $1; exit }' "$checksums_file"
}

verify_checksum() {
    archive_path="$1"
    checksums_file="$2"
    archive_name="$(basename "$archive_path")"
    expected="$(checksum_for_archive "$archive_name" "$checksums_file")"

    [ -n "$expected" ] || fail "checksum entry not found for $archive_name"

    if has_cmd shasum; then
        actual="$(shasum -a 256 "$archive_path" | awk '{print $1}')"
    elif has_cmd sha256sum; then
        actual="$(sha256sum "$archive_path" | awk '{print $1}')"
    elif has_cmd openssl; then
        actual="$(openssl dgst -sha256 "$archive_path" | awk '{print $NF}')"
    else
        log "warning: no SHA-256 tool found; skipping checksum verification"
        return
    fi

    [ "$actual" = "$expected" ] || fail "checksum mismatch for $archive_name"
}

confirm_overwrite() {
    target="$1"

    if [ ! -e "$target" ] || [ "$ASSUME_YES" = "1" ]; then
        return
    fi

    if [ ! -r /dev/tty ]; then
        fail "$target already exists; rerun with --yes or FLOWMUX_YES=1 to overwrite non-interactively"
    fi

    printf '%s' "$target already exists. Overwrite? [y/N] " >/dev/tty
    read reply </dev/tty || exit 1
    case "$reply" in
        y|Y|yes|YES)
            ;;
        *)
            fail "installation cancelled"
            ;;
    esac
}

extract_binary() {
    archive_path="$1"
    staging_dir="$2"

    tar -xzf "$archive_path" -C "$staging_dir"
    binary_path="$(find "$staging_dir" -type f -name "$BINARY_NAME" | head -n 1)"
    [ -n "$binary_path" ] || fail "failed to locate extracted $BINARY_NAME binary"
    printf '%s\n' "$binary_path"
}

ensure_install_dir() {
    dir="$1"
    mkdir -p "$dir"
    [ -w "$dir" ] || fail "install directory is not writable: $dir"
}

print_path_hint() {
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            ;;
        *)
            log
            log "Add $INSTALL_DIR to your PATH to run $BINARY_NAME directly."
            log "Example:"
            log "  export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version)
                [ "$#" -ge 2 ] || fail "--version requires a value"
                REQUESTED_VERSION="$2"
                shift 2
                ;;
            --install-dir)
                [ "$#" -ge 2 ] || fail "--install-dir requires a value"
                INSTALL_DIR="$2"
                shift 2
                ;;
            --yes)
                ASSUME_YES=1
                shift 1
                ;;
            --help|-h)
                usage
                exit 0
                ;;
            *)
                fail "unknown argument: $1"
                ;;
        esac
    done
}

main() {
    parse_args "$@"

    need_cmd uname
    need_cmd mktemp
    need_cmd tar
    need_cmd sed
    need_cmd tr
    need_cmd awk
    need_cmd find
    need_cmd head
    need_cmd install

    ensure_tmux_installed

    version="$(resolve_version)"
    target="$(detect_target)"
    archive_name="flowmux-${version}-${target}.tar.gz"
    archive_url="${BASE_DOWNLOAD_URL}/${version}/${archive_name}"
    checksums_url="${BASE_DOWNLOAD_URL}/${version}/checksums.txt"

    temp_dir="$(mktemp -d)"
    trap 'rm -rf "$temp_dir"' EXIT INT TERM HUP

    archive_path="${temp_dir}/${archive_name}"
    checksums_path="${temp_dir}/checksums.txt"

    log "Installing Flowmux ${version} for ${target}"
    log "Downloading release archive..."
    download_to "$archive_url" "$archive_path"

    log "Downloading checksums..."
    download_to "$checksums_url" "$checksums_path"

    log "Verifying archive..."
    verify_checksum "$archive_path" "$checksums_path"

    ensure_install_dir "$INSTALL_DIR"
    target_path="${INSTALL_DIR}/${BINARY_NAME}"
    confirm_overwrite "$target_path"

    binary_path="$(extract_binary "$archive_path" "$temp_dir")"
    install -m 755 "$binary_path" "$target_path"

    log
    log "Installed ${BINARY_NAME} to ${target_path}"
    print_path_hint
}

main "$@"
