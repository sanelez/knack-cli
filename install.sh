#!/usr/bin/env sh
# knack CLI installer — macOS + Linux.
#
# Usage:
#   curl -fsSL https://getknack.ai/install | sh
#   curl -fsSL https://getknack.ai/install | sh -s -- --version 0.2.0
#   curl -fsSL https://getknack.ai/install | sh -s -- --bin-dir /custom/path
#
# Detects OS+arch, downloads the matching binary from GitHub Releases,
# extracts it into ~/.local/bin (or $KNACK_BIN_DIR), and reminds you to add
# that directory to PATH if it's not already there. Idempotent — re-running
# upgrades the binary in place.

set -eu

REPO="${KNACK_REPO:-jordan-gibbs/knack}"
VERSION="${KNACK_VERSION:-latest}"
BIN_DIR="${KNACK_BIN_DIR:-$HOME/.local/bin}"
TMPDIR_KEEP=""

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --keep-tmp) TMPDIR_KEEP=1; shift ;;
        -h|--help)
            cat <<EOF
knack installer

Options:
  --version <ver>   install a specific version (default: latest)
  --bin-dir <dir>   install into <dir> (default: \$HOME/.local/bin)
  --keep-tmp        leave the temp download dir behind for debugging
EOF
            exit 0
            ;;
        *) echo "unknown flag: $1" >&2; exit 64 ;;
    esac
done

# Need: curl, tar (or unzip), uname, mktemp.
need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "knack-install: need '$1' on PATH" >&2
        exit 1
    fi
}
need curl
need uname
need mktemp
need tar

# Detect OS + arch → target triple.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux)  os_part="unknown-linux-musl" ;;
    *)
        echo "knack-install: unsupported OS '$os'" >&2
        echo "  → use the PowerShell installer on Windows: https://getknack.ai/install.ps1" >&2
        exit 1
        ;;
esac
case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *)
        echo "knack-install: unsupported arch '$arch'" >&2
        exit 1
        ;;
esac
target="${arch_part}-${os_part}"

# Resolve version → tag.
if [ "$VERSION" = "latest" ]; then
    tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
    if [ -z "$tag" ]; then
        echo "knack-install: couldn't resolve latest tag from ${REPO}" >&2
        exit 1
    fi
else
    tag="cli-v${VERSION}"
fi

archive="knack-${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/${tag}/${archive}"

echo "→ knack ${tag} for ${target}"
echo "→ ${url}"

tmp="$(mktemp -d -t knack-install.XXXXXX)"
trap 'if [ -z "$TMPDIR_KEEP" ]; then rm -rf "$tmp"; fi' EXIT

curl -fsSL "$url" -o "$tmp/$archive"
tar -xzf "$tmp/$archive" -C "$tmp"

# The archive layout from taiki-e/upload-rust-binary-action is:
#   knack-<target>/knack[.exe]
src="$(find "$tmp" -name knack -type f | head -n1)"
if [ -z "$src" ]; then
    echo "knack-install: 'knack' binary not found in archive" >&2
    exit 1
fi

mkdir -p "$BIN_DIR"
mv "$src" "$BIN_DIR/knack"
chmod +x "$BIN_DIR/knack"

echo "[OK] installed to $BIN_DIR/knack"

# PATH check — print the export line if BIN_DIR isn't on PATH yet, but never
# auto-edit the user's shell profile (too easy to mangle).
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo
        echo "$BIN_DIR is not on your PATH. Add this to your shell profile:"
        echo
        echo "    export PATH=\"$BIN_DIR:\$PATH\""
        ;;
esac

"$BIN_DIR/knack" --version || true
