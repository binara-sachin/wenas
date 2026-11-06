# Releasing wenas

Releases are built by [cargo-dist](https://opensource.axo.dev/cargo-dist/) from `dist-workspace.toml`. Pushing a version tag builds binaries for macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64, static musl), creates a GitHub release with a shell installer, and publishes the Homebrew formula to the tap.

## One-time setup

1. **Create the tap.** An empty public repository named `homebrew-tap` under the same owner (`binara-sachin/homebrew-tap`). It must exist before the first release; `dist` writes `Formula/wenas.rb` into it.
2. **Give the release workflow access to it.** Create a fine-grained personal access token with *Contents: read and write* on `homebrew-tap` only, and save it as the secret `HOMEBREW_TAP_TOKEN` in the `wenas` repository (Settings, Secrets and variables, Actions). The workflow cannot push to the tap without it.
3. **Optional, for `cargo install wenas`:** create a crates.io API token and keep it for the publish step below. The name `wenas` was free on crates.io on 2026-11-02; check again before publishing.
4. **Check the Homebrew name.** Whether `wenas` is free in `homebrew/core` is spec question O1 and is still open. The personal tap does not need it.

## Cutting a release

1. Make sure `main` is green: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
2. Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and commit.
3. Run the **manual checklist** below.
4. Preview what will be built, without building anything:

   ```sh
   dist plan
   ```

5. Tag and push. The tag must match the version:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

6. Watch the **Release** workflow. When it finishes, the GitHub release has the tarballs, `wenas-installer.sh` and the checksums, and the tap has the new formula.
7. Publish to crates.io (optional): `cargo publish` (use `--dry-run` first).

Pull requests run `dist plan` only, so the release workflow is exercised without publishing anything.

## After editing `dist-workspace.toml`

The workflow under `.github/workflows/release.yml` is generated. Regenerate it, never edit it by hand:

```sh
dist generate
```

`dist` itself is installed with `cargo install cargo-dist --locked` (the workflow pins the version in `cargo-dist-version`).

## Manual checklist (spec section 12)

Automated tests cannot cover the terminal itself. Before each release, on macOS and on Linux, in Ghostty + tmux:

- [ ] `git difftool` on a single changed file: the left side is marked read-only, `>` then `w` changes the working file.
- [ ] `git dd` (dir-diff): open a file, copy a hunk, save, go back with `Backspace`, quit. The edit is in the working tree, and nothing is left in the temp directory.
- [ ] `git dd --no-symlinks` behaves the same.
- [ ] `Ctrl-c` in a per-file `git difftool` stops the run (git reports `external diff died`).
- [ ] The theme is right in a dark and a light terminal, and inside tmux. Check `theme = "light"` as well.
- [ ] A large diff (try two 100k-line files): the "Comparing" screen appears, `Esc` cancels it, and a re-diff after `>` can be cancelled too.
- [ ] `e` opens Neovim at the cursor line and the file reloads on return.
- [ ] `NO_COLOR=1 wenas a b` shows changes with markers and reverse video.
- [ ] Resize the window across 100 columns: the view switches between side by side and unified.
- [ ] Trackpad and mouse wheel scroll at a comfortable speed, stop when the gesture stops, and a sideways swipe scrolls the panes. Check tmux with `mouse on` and `mouse off`.
- [ ] Click a row, click `→` in the gutter (then `u`), and double-click a file in the directory view.
- [ ] Shift-drag selects text, and `mouse = false` gives the wheel and selection back to the terminal.
- [ ] Paste `QQ` into the file view: the status line says `paste ignored` and nothing quits. Paste into the `/` prompt: the text appears there. Check inside tmux too.
- [ ] Leave Neovim after `e`: the mouse still works in wenas. After `q`, and after `kill <pid>` from another pane, moving and clicking the mouse prints nothing in the shell.

## After the release: verify the install

On a clean Mac (no Rust toolchain):

```sh
brew install binara-sachin/tap/wenas
wenas --version
wenas --git-config
```

Then run the checklist above against the installed binary. The milestone is only done when this works on a machine that has never built the project.
