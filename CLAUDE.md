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

```bash
# 1. Make changes on wang/main and push
git push origin wang/main

# 2. Create a GitHub release (may auto-trigger the workflow)
gh release create v0.1.0-custom.N \
  --title "v0.1.0-custom.N" \
  --notes "What changed" \
  --target wang/main

# 3. If the workflow didn't auto-trigger, run it manually:
gh workflow run release.yml --ref wang/main -f tag=v0.1.0-custom.N
```

### How to download on a workstation

```bash
# macOS Apple Silicon (M-series)
curl -fLo ~/.local/bin/forge \
  https://github.com/Wang-tianhao/forgecode/releases/download/v0.1.0-custom.N/forge-aarch64-apple-darwin
chmod +x ~/.local/bin/forge
```

### Syncing `main` with upstream

```bash
git checkout main
git fetch origin main
git reset --hard origin/main
git push origin main --force-with-lease
git checkout wang/main
```
