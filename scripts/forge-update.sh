#!/usr/bin/env bash
# forge-update.sh — full fork sync, build, install, and release workflow.
#
# This script is invoked by `forge update` and performs the following:
#   1. Fetch latest upstream main (and tags).
#   2. Rebase or merge local commits on top of upstream/main.
#   3. Build the binary with the upstream release version.
#   4. Install the binary locally to ~/.local/bin (with codesign on macOS).
#   5. Push the updated branch to origin.
#   6. Create (or reuse) a GitHub release on the fork and upload the binary.
#
# Environment variables
# ---------------------
#   DEST_DIR        Installation directory (default: $HOME/.local/bin).
#   FORK_REPO       Override the fork repo slug, e.g. Wang-tianhao/forgecode.
#   SKIP_BUILD      Set to 1 to skip cargo build (use existing target/release/forge).
#   SKIP_RELEASE    Set to 1 to skip GitHub release creation/upload.
#   PROFILE         Build profile: release (default) or debug.

set -euo pipefail

PROFILE="${PROFILE:-release}"
DEST_DIR="${DEST_DIR:-$HOME/.local/bin}"
BIN_NAME="forge"

# ------------------------------------------------------------------
# Resolve repo root
# ------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# Validate we are inside the forge repo
if [[ ! -f "$REPO_ROOT/Cargo.toml" ]] || ! grep -q '^\s*name\s*=\s*"forge_main"' "$REPO_ROOT/crates/forge_main/Cargo.toml" 2>/dev/null; then
    echo "error: unable to locate forge repository root" >&2
    exit 1
fi

# ------------------------------------------------------------------
# 1. Fetch upstream main and tags
# ------------------------------------------------------------------
echo "==> Fetching upstream main and tags..."
if ! git remote get-url upstream &>/dev/null; then
    echo "error: 'upstream' remote not found. Add it with:" >&2
    echo "  git remote add upstream https://github.com/tailcallhq/forgecode.git" >&2
    exit 1
fi
git fetch upstream main --tags --quiet || true

# ------------------------------------------------------------------
# 2. Merge upstream/main into the current branch
# ------------------------------------------------------------------
CURRENT_BRANCH="$(git branch --show-current 2>/dev/null || true)"
if [[ -z "$CURRENT_BRANCH" ]]; then
    echo "error: not on any branch (detached HEAD?)" >&2
    exit 1
fi

UPSTREAM_REF="upstream/main"
LOCAL_COMMIT_COUNT="$(git rev-list --count "$UPSTREAM_REF..HEAD" 2>/dev/null || echo 0)"

if [[ "$LOCAL_COMMIT_COUNT" -gt 0 ]]; then
    echo "==> Found $LOCAL_COMMIT_COUNT local commit(s) on $CURRENT_BRANCH. Merging $UPSTREAM_REF..."
    git merge --no-edit "$UPSTREAM_REF"
else
    echo "==> No local commits ahead of upstream. Fast-forwarding $CURRENT_BRANCH..."
    git merge --ff-only "$UPSTREAM_REF"
fi

# ------------------------------------------------------------------
# 3. Resolve version from upstream latest tag
# ------------------------------------------------------------------
UPSTREAM_TAG="$(git describe --tags --abbrev=0 upstream/main 2>/dev/null || true)"
if [[ -z "$UPSTREAM_TAG" ]]; then
    UPSTREAM_TAG="$(git describe --tags --abbrev=0 HEAD 2>/dev/null || true)"
fi
if [[ -n "$UPSTREAM_TAG" ]]; then
    APP_VERSION="$UPSTREAM_TAG"
else
    APP_VERSION="0.1.0-dev"
fi
echo "==> version: $APP_VERSION"

# ------------------------------------------------------------------
# Build binary
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
DEST_BIN="$DEST_DIR/$BIN_NAME"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    echo "==> cargo build ${CARGO_FLAGS[*]} --bin $BIN_NAME"
    APP_VERSION="$APP_VERSION" cargo build "${CARGO_FLAGS[@]}" --bin "$BIN_NAME"
fi

if [[ ! -x "$SRC_BIN" ]]; then
    echo "error: built binary not found at $SRC_BIN" >&2
    exit 1
fi

# ------------------------------------------------------------------
# 4. Install locally with correct permissions / code signature
# ------------------------------------------------------------------
mkdir -p "$DEST_DIR"
echo "==> install -m 755 $SRC_BIN -> $DEST_BIN"
install -m 755 "$SRC_BIN" "$DEST_BIN"

if command -v codesign >/dev/null 2>&1; then
    echo "==> codesign -f -s - $DEST_BIN"
    codesign -f -s - "$DEST_BIN"
else
    echo "note: codesign not found, skipping re-sign (non-macOS?)" >&2
fi

if "$DEST_BIN" --version >/dev/null 2>&1; then
    echo "==> installed: $("$DEST_BIN" --version)"
else
    echo "warning: $DEST_BIN --version returned non-zero — check signing/quarantine" >&2
    exit 1
fi

# ------------------------------------------------------------------
# 5. Push updated branch to origin
# ------------------------------------------------------------------
echo "==> Pushing $CURRENT_BRANCH to origin..."
git push origin "$CURRENT_BRANCH" --force-with-lease || {
    echo "warning: push to origin failed" >&2
}

# ------------------------------------------------------------------
# 6. Publish GitHub release on fork
# ------------------------------------------------------------------
if [[ "${SKIP_RELEASE:-0}" == "1" ]]; then
    echo "==> SKIP_RELEASE=1; skipping GitHub release publish"
    exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
    echo "warning: gh CLI not found; install it to auto-publish releases" >&2
    exit 0
fi

# Resolve fork repo slug from origin remote
if [[ -z "${FORK_REPO:-}" ]]; then
    ORIGIN_URL="$(git remote get-url origin 2>/dev/null || true)"
    if [[ -n "$ORIGIN_URL" ]]; then
        # Handles both SSH (git@github.com:user/repo.git) and HTTPS
        FORK_REPO="$(echo "$ORIGIN_URL" | sed -E 's/.*github\.com[:\/]([^\/]+\/[^\/]+)\.git$/\1/')"
    fi
fi

if [[ -z "${FORK_REPO:-}" ]]; then
    echo "warning: could not detect fork repo; set FORK_REPO=user/repo to publish releases" >&2
    exit 0
fi

echo "==> Publishing release $APP_VERSION to $FORK_REPO..."

# Determine asset name matching upstream release workflow
ARCH="$(uname -m)"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$ARCH" in
    x86_64)  TARGET_ARCH="x86_64" ;;
    arm64|aarch64) TARGET_ARCH="aarch64" ;;
    *)       TARGET_ARCH="$ARCH" ;;
esac
ASSET_NAME="forge-${TARGET_ARCH}-${OS}-darwin"
if [[ "$OS" == "linux" ]]; then
    ASSET_NAME="forge-${TARGET_ARCH}-unknown-linux-gnu"
fi

TMP_ASSET="$(mktemp)"
cp "$SRC_BIN" "$TMP_ASSET"

# Create release if it doesn't exist; ignore errors if it already does
if ! gh release view "$APP_VERSION" --repo "$FORK_REPO" >/dev/null 2>&1; then
    gh release create "$APP_VERSION" \
        --repo "$FORK_REPO" \
        --title "$APP_VERSION" \
        --notes "Fork release $APP_VERSION (synced from upstream)" || true
fi

# Upload / overwrite asset
gh release upload "$APP_VERSION" "$TMP_ASSET" --repo "$FORK_REPO" --clobber || {
    echo "warning: failed to upload release asset" >&2
}
rm -f "$TMP_ASSET"

echo "==> Done. Release $APP_VERSION published to https://github.com/$FORK_REPO/releases/tag/$APP_VERSION"
