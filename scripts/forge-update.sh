#!/usr/bin/env bash
# forge-update.sh — full fork sync, build, install, and release workflow.
#
# Fast path
# -------
# If the fork already has a GitHub release matching the upstream latest tag,
# the script downloads the pre-built binary for the current platform and
# installs it, skipping all git/build/release steps.
#
# Full path (when fork release does not yet exist)
# ------------------------------------------------
#   1. Fetch upstream main and tags.
#   2. Merge upstream/main into the current branch.
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
# Helper: resolve fork repo slug
# ------------------------------------------------------------------
resolve_fork_repo() {
    if [[ -n "${FORK_REPO:-}" ]]; then
        echo "$FORK_REPO"
        return
    fi
    local origin_url
    origin_url="$(git remote get-url origin 2>/dev/null || true)"
    if [[ -n "$origin_url" ]]; then
        # Handles both SSH (git@github.com:user/repo.git) and HTTPS
        echo "$origin_url" | sed -E 's/.*github\.com[:\/]([^\/]+\/[^\/]+)\.git$/\1/'
    fi
}

# ------------------------------------------------------------------
# Helper: compute asset name for the current platform
# ------------------------------------------------------------------
compute_asset_name() {
    local arch os
    arch="$(uname -m)"
    os="$(uname -s)"

    case "$arch" in
        x86_64)          target_arch="x86_64" ;;
        arm64|aarch64)   target_arch="aarch64" ;;
        *)               target_arch="$arch" ;;
    esac

    case "$os" in
        Darwin)
            echo "forge-${target_arch}-apple-darwin"
            ;;
        Linux)
            echo "forge-${target_arch}-unknown-linux-gnu"
            ;;
        *)
            echo "forge-${target_arch}-${os}" ;;
    esac
}

# ------------------------------------------------------------------
# Helper: install a binary file to DEST_DIR
# ------------------------------------------------------------------
install_binary() {
    local src="$1"
    local dest="$DEST_DIR/$BIN_NAME"

    mkdir -p "$DEST_DIR"
    echo "==> install -m 755 $src -> $dest"
    install -m 755 "$src" "$dest"

    if command -v codesign >/dev/null 2>&1; then
        echo "==> codesign -f -s - $dest"
        codesign -f -s - "$dest"
    else
        echo "note: codesign not found, skipping re-sign (non-macOS?)" >&2
    fi

    if "$dest" --version >/dev/null 2>&1; then
        echo "==> installed: $("$dest" --version)"
    else
        echo "warning: $dest --version returned non-zero — check signing/quarantine" >&2
        return 1
    fi
}

# ------------------------------------------------------------------
# 1. Fetch upstream tags (lightweight — needed for version resolution)
# ------------------------------------------------------------------
echo "==> Fetching upstream tags..."
if ! git remote get-url upstream &>/dev/null; then
    echo "error: 'upstream' remote not found. Add it with:" >&2
    echo "  git remote add upstream https://github.com/tailcallhq/forgecode.git" >&2
    exit 1
fi
git fetch upstream --tags --quiet 2>/dev/null || true

# ------------------------------------------------------------------
# 2. Resolve upstream latest tag
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
echo "==> upstream version: $APP_VERSION"

# ------------------------------------------------------------------
# 3. FAST PATH: if fork already has this release, download & install
# ------------------------------------------------------------------
FORK_REPO="$(resolve_fork_repo)"
ASSET_NAME="$(compute_asset_name)"

if [[ -n "$FORK_REPO" ]] && command -v gh >/dev/null 2>&1; then
    if gh release view "$APP_VERSION" --repo "$FORK_REPO" >/dev/null 2>&1; then
        echo "==> Fork already has release $APP_VERSION. Checking for asset $ASSET_NAME..."

        TMP_BIN="$(mktemp)"
        if gh release download "$APP_VERSION" \
            --repo "$FORK_REPO" \
            --pattern "$ASSET_NAME" \
            -O "$TMP_BIN" 2>/dev/null; then

            install_binary "$TMP_BIN"
            rm -f "$TMP_BIN"
            echo "==> Done. Installed from existing fork release $APP_VERSION."
            exit 0
        else
            echo "==> Asset $ASSET_NAME not found in fork release $APP_VERSION. Falling back to build."
            rm -f "$TMP_BIN"
        fi
    fi
fi

# ------------------------------------------------------------------
# 4. FULL PATH: fetch upstream main, merge, build, install, push, release
# ------------------------------------------------------------------
echo "==> Full build path: syncing, building, and publishing..."

# Fetch upstream main
git fetch upstream main --quiet || true

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

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    echo "==> cargo build ${CARGO_FLAGS[*]} --bin $BIN_NAME"
    APP_VERSION="$APP_VERSION" cargo build "${CARGO_FLAGS[@]}" --bin "$BIN_NAME"
fi

if [[ ! -x "$SRC_BIN" ]]; then
    echo "error: built binary not found at $SRC_BIN" >&2
    exit 1
fi

install_binary "$SRC_BIN"

# ------------------------------------------------------------------
# Push updated branch to origin
# ------------------------------------------------------------------
echo "==> Pushing $CURRENT_BRANCH to origin..."
git push origin "$CURRENT_BRANCH" --force-with-lease || {
    echo "warning: push to origin failed" >&2
}

# ------------------------------------------------------------------
# Publish GitHub release on fork
# ------------------------------------------------------------------
if [[ "${SKIP_RELEASE:-0}" == "1" ]]; then
    echo "==> SKIP_RELEASE=1; skipping GitHub release publish"
    exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
    echo "warning: gh CLI not found; install it to auto-publish releases" >&2
    exit 0
fi

if [[ -z "$FORK_REPO" ]]; then
    echo "warning: could not detect fork repo; set FORK_REPO=user/repo to publish releases" >&2
    exit 0
fi

echo "==> Publishing release $APP_VERSION to $FORK_REPO..."

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
