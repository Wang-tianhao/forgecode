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
