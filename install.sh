#!/usr/bin/env sh
# knack CLI installer for macOS and Linux.
#
# Usage:
#   curl -fsSL https://getknack.ai/install | sh
#   curl -fsSL https://getknack.ai/install | sh -s -- --version 0.2.0
#   curl -fsSL https://getknack.ai/install | sh -s -- --bin-dir /custom/path
#
# Detects OS+arch, downloads the matching binary from R2 (cli.getknack.ai
# by default, overridable via KNACK_R2_BASE), extracts it into ~/.local/bin
# (or $KNACK_BIN_DIR), and reminds you to add that directory to PATH if
# it's not already there. Idempotent: re-running upgrades in place.

set -eu

# R2 base URL. Defaults to the cli.getknack.ai custom domain. Override via
# KNACK_R2_BASE to use the raw r2.dev URL or a different host.
R2_BASE="${KNACK_R2_BASE:-https://cli.getknack.ai}"
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

# We don't ship a native arm64 macOS binary — M-series Macs run the
# x86_64 build via Rosetta. Save 10×-multiplier macOS-runner minutes on
# the release matrix; re-add native arm64 if startup latency matters.
if [ "$target" = "aarch64-apple-darwin" ]; then
    target="x86_64-apple-darwin"
fi

# Resolve version. R2 holds a small text file at /cli/latest/version.txt
# (e.g. "0.1.0\n") that points at the current version.
if [ "$VERSION" = "latest" ]; then
    VERSION="$(curl -fsSL "${R2_BASE}/cli/latest/version.txt" | head -n1 | tr -d '\r\n ')"
    if [ -z "$VERSION" ]; then
        echo "knack-install: couldn't resolve latest version from ${R2_BASE}/cli/latest/version.txt" >&2
        exit 1
    fi
fi

archive="knack-${target}.tar.gz"
url="${R2_BASE}/cli/v${VERSION}/${archive}"

echo "→ knack v${VERSION} for ${target}"
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

# PATH check: print the export line if BIN_DIR isn't on PATH yet, but never
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

# Register knack with the AI agent the user is running in (Claude Code,
# Codex, Cursor, ...). Best-effort: if the detector finds nothing, it writes
# the generic AGENTS.md fallback. Non-fatal — agent integration is a
# courtesy, not a requirement for the CLI to work.
if "$BIN_DIR/knack" install --auto >/dev/null 2>&1; then
    echo "[OK] registered with detected agent (re-run \`knack install\` to refresh)"
else
    echo "  Tip: run \`knack install\` to register the CLI with your AI agent."
fi
