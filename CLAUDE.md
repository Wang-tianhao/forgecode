# CLAUDE.md — forgecode project notes

## Installing a locally built forge

Use `scripts/install-local.sh` whenever you rebuild and want the new binary
picked up by the shell plugin (and to avoid macOS "killed: 9" from a stale
ad-hoc signature).

```sh
scripts/install-local.sh            # cargo build --release → ~/.local/bin/forge
scripts/install-local.sh debug      # cargo build (debug)   → ~/.local/bin/forge
SKIP_BUILD=1 scripts/install-local.sh   # skip cargo, just re-install target/release/forge
DEST_DIR=/usr/local/bin scripts/install-local.sh
```

What it guarantees (do **not** replace it with a plain `cp`):

1. `install -m 755 <src> <dest>` — fresh inode at the destination so macOS
   does not reuse a stale code-signing cache entry.
2. `codesign -f -s - <dest>` — ad-hoc re-sign; without this, replacing a
   running/previously-signed binary can SIGKILL on launch under macOS's
   code-signing enforcement.
3. `<dest> --version` sanity check so a broken signature surfaces here,
   not on the user's next invocation.

## Shell plugin is embedded, not installed separately

`crates/forge_main/src/zsh/plugin.rs` embeds `shell-plugin/**` via
`include_dir!` / `include_str!`. `~/.zshrc` invokes
`eval "$(forge zsh plugin)"` at every shell startup, so updating the
plugin source requires rebuilding the binary. After rebuild + install,
reload with `exec zsh` (or `source ~/.zshrc`) to pick up the new plugin in
an already-open shell.

## Custom Release Builds via GitHub Releases

This is a **fork** (`Wang-tianhao/forgecode`) of the upstream repo. The
custom code lives on **`wang/main`**; `main` stays synced with upstream.

### Branch layout

| Branch | Purpose |
|---|---|
| `main` | Mirror of upstream `tailcallhq/forgecode` main — never commit here |
| `wang/main` | All custom changes — also the repo's **default branch** |

### Release workflow

`.github/workflows/release.yml` has been customized for this fork:

- **Triggers**: `release: published` (auto) + `workflow_dispatch` (manual, for forks where the release event may not fire).
- **Matrix**: macOS arm64 only — `aarch64-apple-darwin`.
- **No npm/homebrew jobs** (removed — they require upstream secrets).

### How to publish a new release

Fork versions use `UPSTREAM-wang.MAJOR.MINOR.PATCH`, for example
`2.13.21-wang.1.0.0` (Git tag: `v2.13.21-wang.1.0.0`). The root
`FORK_VERSION` file owns the fork version, starting at `1.0.0`. Increment
patch for fixes, minor for backward-compatible features, and major for
breaking fork changes. Keep the fork version when updating upstream; do not
reset it. Cargo crate versions remain unchanged.

Both install scripts combine the latest reachable stable upstream tag with
`FORK_VERSION`. Fork tags are excluded from upstream discovery. Override
`FORK_VERSION` for a one-off build, or pass an exact combined `APP_VERSION`
to `install-local.sh` to build/download an older release without relabeling
it. `FORK_LABEL=` disables the suffix. For direct Cargo builds, pass the
complete tag as `APP_VERSION` and leave `FORK_LABEL` unset.

The suffix uses SemVer prerelease syntax so fork increments sort numerically
(including `1.0.9` → `1.0.10`); `+wang.1.0.0` would be build metadata and
would not affect update ordering. Publish fork tags as regular GitHub releases,
not prereleases, so the updater's latest-release endpoint finds them. The first
migration from a bare upstream version needs a local install or manual download:
SemVer considers a bare `2.13.21` newer than `2.13.21-wang.1.0.0`.

```bash
# 1. Update FORK_VERSION, commit changes on wang/main, then push
git push origin wang/main

# 2. Create a GitHub release (may auto-trigger the workflow)
TAG=$(bash scripts/fork-version.sh v2.13.21)
gh release create "$TAG" \
  --title "$TAG" \
  --notes "What changed" \
  --target wang/main

# 3. If the workflow didn't auto-trigger, run it manually:
gh workflow run release.yml --ref wang/main -f tag="$TAG"
```

### How to download on a workstation

```bash
# macOS Apple Silicon (M-series)
APP_VERSION=v2.13.21-wang.1.0.0 scripts/install-local.sh --download
```

### Syncing `main` with upstream

```bash
git checkout main
git fetch origin main
git reset --hard origin/main
git push origin main --force-with-lease
git checkout wang/main
```
