#!/usr/bin/env bash
# install-local.sh — install forge locally, either by building from source
# or by downloading a pre-built release asset from GitHub.
#
# Why this exists
# ---------------
# On Apple Silicon (arm64), an unsigned or stale-signed Mach-O binary is
# killed by the kernel with "zsh: killed  forge" (SIGKILL / code-signing
# enforcement) before `main` ever runs. `cargo build` produces an ad-hoc
# signature, but a plain `cp` over an existing file can end up with the
# destination keeping the *old* inode's signature or — worse — being
# copied in a way that invalidates the signature entirely (e.g. when the
# destination was previously signed with a different identity, or when
# copy-on-write metadata gets stale). `install(1)` creates a fresh inode
# at the destination, and a forced `codesign -f -s -` rewrites the
# signature in place so the kernel accepts the new bytes.
#
# Usage
# -----
#   scripts/install-local.sh                    # release build → ~/.local/bin/forge
#   scripts/install-local.sh debug              # debug build   → ~/.local/bin/forge
#   scripts/install-local.sh --download         # fetch from GitHub release → ~/.local/bin/forge
#   DEST_DIR=/usr/local/bin scripts/install-local.sh
#   SKIP_BUILD=1 scripts/install-local.sh       # assume target/release/forge exists
#   APP_VERSION=v2.12.7 scripts/install-local.sh
#   GITHUB_REPO=Wang-tianhao/forgecode scripts/install-local.sh --download
#
# Safe to re-run; each invocation re-signs after copy.

set -euo pipefail

PROFILE="${1:-release}"
DEST_DIR="${DEST_DIR:-$HOME/.local/bin}"
BIN_NAME="forge"
GITHUB_REPO="${GITHUB_REPO:-Wang-tianhao/forgecode}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ------------------------------------------------------------------
# Resolve version: explicit APP_VERSION > latest upstream tag > fallback
# ------------------------------------------------------------------
if [[ -z "${APP_VERSION:-}" ]]; then
    # Fetch upstream tags if the remote exists
    if git -C "$REPO_ROOT" remote get-url upstream &>/dev/null; then
        git -C "$REPO_ROOT" fetch upstream --tags --quiet 2>/dev/null || true
        UPSTREAM_TAG="$(git -C "$REPO_ROOT" describe --tags --abbrev=0 upstream/main 2>/dev/null || true)"
    fi
    # Fall back to the latest tag reachable from HEAD
    if [[ -z "$UPSTREAM_TAG" ]]; then
        UPSTREAM_TAG="$(git -C "$REPO_ROOT" describe --tags --abbrev=0 HEAD 2>/dev/null || true)"
    fi
    if [[ -n "$UPSTREAM_TAG" ]]; then
        APP_VERSION="$UPSTREAM_TAG"
    else
        APP_VERSION="0.1.0-dev"
    fi
fi

echo "==> version: $APP_VERSION"

DEST_BIN="$DEST_DIR/$BIN_NAME"
mkdir -p "$DEST_DIR"

# ------------------------------------------------------------------
# --download mode: fetch pre-built binary from GitHub release
# ------------------------------------------------------------------
if [[ "$PROFILE" == "--download" ]]; then
    ARCH="$(uname -m)"
    OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
    case "$ARCH" in
        x86_64)  TARGET_ARCH="x86_64" ;;
        arm64|aarch64) TARGET_ARCH="aarch64" ;;
        *)       TARGET_ARCH="$ARCH" ;;
    esac

    if [[ "$OS" == "darwin" ]]; then
        ASSET_NAME="forge-${TARGET_ARCH}-apple-darwin"
    else
        ASSET_NAME="forge-${TARGET_ARCH}-unknown-linux-gnu"
    fi

    DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download/${APP_VERSION}/${ASSET_NAME}"
    TMP_BIN="$(mktemp)"

    echo "==> downloading ${ASSET_NAME} from ${GITHUB_REPO} release ${APP_VERSION}..."
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$TMP_BIN" "$DOWNLOAD_URL"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$TMP_BIN" "$DOWNLOAD_URL"
    else
        echo "error: curl or wget is required" >&2
        exit 1
    fi

    echo "==> install -m 755 downloaded binary -> $DEST_BIN"
    install -m 755 "$TMP_BIN" "$DEST_BIN"
    rm -f "$TMP_BIN"

    if command -v codesign >/dev/null 2>&1; then
        echo "==> codesign -f -s - $DEST_BIN"
        codesign -f -s - "$DEST_BIN"
    fi

    if "$DEST_BIN" --version >/dev/null 2>&1; then
        echo "==> ok: $("$DEST_BIN" --version)"
    else
        echo "warning: $DEST_BIN --version returned non-zero — check signing/quarantine" >&2
        exit 1
    fi
    exit 0
fi

# ------------------------------------------------------------------
# Local build mode
# ------------------------------------------------------------------
case "$PROFILE" in
    release) CARGO_FLAGS=(--release) TARGET_SUBDIR="release" ;;
    debug)   CARGO_FLAGS=()          TARGET_SUBDIR="debug"   ;;
    *)
        echo "error: profile must be 'release' or 'debug' (got: $PROFILE)" >&2
        exit 2
        ;;
esac

SRC_BIN="$REPO_ROOT/target/$TARGET_SUBDIR/$BIN_NAME"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    echo "==> cargo build ${CARGO_FLAGS[*]} --bin $BIN_NAME"
    ( cd "$REPO_ROOT" && APP_VERSION="$APP_VERSION" cargo build "${CARGO_FLAGS[@]}" --bin "$BIN_NAME" )
fi

if [[ ! -x "$SRC_BIN" ]]; then
    echo "error: built binary not found at $SRC_BIN" >&2
    exit 1
fi

# `install` creates a fresh file at the destination with mode 0755. Unlike
# `cp`, it does not preserve the source's signed metadata in a way that can
# confuse the code-signing cache on macOS.
echo "==> install -m 755 $SRC_BIN -> $DEST_BIN"
install -m 755 "$SRC_BIN" "$DEST_BIN"

# Re-sign ad-hoc so the kernel's code-signing enforcement accepts the new
# bytes. `-f` forces replacement of any existing signature; `-s -` uses the
# ad-hoc signer (no identity), matching what `cargo build` produces on
# macOS. A missing `codesign` (non-macOS, minimal CI image) is a soft error.
if command -v codesign >/dev/null 2>&1; then
    echo "==> codesign -f -s - $DEST_BIN"
    codesign -f -s - "$DEST_BIN"
else
    echo "note: codesign not found, skipping re-sign (non-macOS?)" >&2
fi

# Quick sanity check: the binary should at minimum respond to --version.
# Runs the *installed* copy so a broken signature surfaces here, not later.
if "$DEST_BIN" --version >/dev/null 2>&1; then
    echo "==> ok: $("$DEST_BIN" --version)"
else
    echo "warning: $DEST_BIN --version returned non-zero — check signing/quarantine" >&2
    exit 1
fi
