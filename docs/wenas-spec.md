# wenas — project spec

*A fast, minimal terminal diff tool that replaces Meld for two-way comparison.*

| | |
|---|---|
| Status | Draft v1 — ready for milestone M0 |
| Author | Sachin |
| Date | 2026-11-02 |
| Language | Rust |
| Platforms | macOS (primary), Linux |

---

## 1. Summary

`wenas` is a terminal UI for comparing two files or two directories side by side, copying changes between them, and saving the result. It replaces Meld, which is slow on macOS, with a single static binary that starts instantly and runs entirely in the terminal.

It is built for one workflow above all others: `git difftool -d` in a Ghostty + tmux + Neovim setup.

### Why not `nvim -d`?

`nvim -d` already offers a fast side-by-side diff with hunk copying, undo and save. `wenas` exists to give a Meld-style experience with no setup:

- a gutter between the panes that shows which way each hunk can be copied
- intra-line (word-level) highlighting, on by default
- one keystroke per hunk action (`>` / `<`)
- a directory comparison view
- instant startup, with no editor configuration to load

If a feature doesn't serve one of these, it doesn't belong in v1.

## 2. Goals

1. **Fast.** The first screen appears in tens of milliseconds for ordinary files. Scrolling and hunk copies never stall the UI.
2. **Minimal.** One binary, no runtime dependencies, few crates, small surface area.
3. **Safe with your files.** Bytes you didn't touch are never changed. Saves are atomic. Files changed on disk behind your back are detected.
4. **Native to a vim user.** Vim-style keys, and free-form editing handed to `$EDITOR`.
5. **A first-class `git difftool`.** Both one file at a time and `--dir-diff`.

## 3. Non-goals

- **Being an editor.** There's no cursor-level text editing inside the panes; `e` opens `$EDITOR`.
- **Being a pager.** `wenas` doesn't replace `git diff`, `delta` or `less`. With non-terminal output, it exits with a hint (§8.3).
- **A non-interactive diff command.** There's no patch output mode; use `git diff --no-index` for that.
- **Syntax highlighting in v1.** Lines are coloured by change type only (§7.5).
- **Windows.** Not planned.
- **Version control beyond git.** No Mercurial, SVN etc.

## 4. Scope

### v1

| Area | Included |
|---|---|
| File compare | Two-way, side-by-side and unified views, intra-line highlighting |
| Editing | Copy hunks left/right, undo/redo, save, hand off to `$EDITOR` |
| Directory compare | Parallel walk, changed/added/deleted/identical files, open any file into the file view |
| Git | `git difftool` one file at a time and `--dir-diff`; `--git-config` prints the setup snippet |
| Options | Ignore whitespace (`-w`, `-b`), CRLF/LF-agnostic comparison |
| Config | Optional TOML file (§11.2) |
| Distribution | Release binaries via `cargo-dist`, a personal Homebrew tap, `cargo install` |

### Later

| Item | Notes |
|---|---|
| Three-way merge and `git mergetool` | Needs its own alignment of three panes against a common base. Largest future item. |
| Syntax highlighting | `syntect`, as an optional feature, highlighting only the rows on screen |
| Line wrapping | Needs filler logic for wrapped rows to keep panes aligned |
| Copying part of a hunk | Select individual lines within a hunk |
| Key remapping | In the config file |
| Hex view for binary files | |
| UTF-16 decoding | v1 treats UTF-16 with a byte-order mark as binary |
| `--exit-code` | `diff`-style exit codes (0 identical, 1 different) for scripts |
| Directory filters | `.gitignore` support and include/exclude patterns beyond the config list |
| Undo history kept after quitting | |

## 5. Architecture

### 5.1 Standard tools: where they're used and where they're not

The starting principle was to lean on standard system tools. Working through the design, Rust made it cheap to do most things in-process, and several standard tools turned out to fit badly:

| Job | Standard tool considered | Decision | Why |
|---|---|---|---|
| Line diff | `diff`, `git diff --no-index` | In-process with `imara-diff` (histogram) | One engine for the first diff and every re-diff after an edit, so hunks never jump around. Re-diffing in memory takes microseconds rather than a temp file plus a process start. macOS ships a BSD `diff` whose flags differ from GNU's. |
| Intra-line highlighting | `git diff --word-diff` | In-process token diff | `--word-diff` works on whole hunks in its own output format, not as character ranges per line, so mapping it onto two panes is fragile. |
| Directory compare | `diff -rq`, `git diff --no-index --name-status` | Own parallel walk + size/byte comparison | About as fast or faster, no output parsing, lists identical files (Meld-style), and results can stream in. |
| File equality | `cmp`, `md5sum` | In-process chunked byte comparison | Stops at the first differing byte. Hashing must read both files fully and buys nothing for one-to-one comparison. |
| Syntax colours | `bat` | Not in v1 | One process per file plus ANSI parsing, for a feature that is deferred anyway. |
| Free-form editing | `$EDITOR` | **Used** | Avoids building a text editor. |
| Reference output for tests | `git diff --no-index --histogram` | **Used (tests only)** | The trusted reference that wenas's hunks are compared against. |

**Result:** at runtime, `wenas` needs nothing but itself. It runs `$EDITOR` when you press `e`, and `git` only ever appears as the program that launches it.

### 5.2 Components

```
                ┌───────────┐
   argv ───────▶│   cli     │── config (TOML)
                └─────┬─────┘
          files │           │ directories
                ▼           ▼
          ┌─────────┐   ┌──────────┐
          │  input  │   │  dircmp  │── parallel walk, size → byte compare
          └────┬────┘   └────┬─────┘      (streams entries over a channel)
               ▼             │
          ┌─────────┐        │ Enter on a file
          │document │◀───────┘
          └────┬────┘
               ▼
          ┌─────────┐   ┌────────┐
          │  diff   │──▶│ align  │── row model (pairs + filler rows)
          └────┬────┘   └───┬────┘
               │            ▼
               │       ┌────────┐
               │       │ inline │── word-level ranges, visible hunks only, cached
               │       └───┬────┘
               ▼           ▼
          ┌─────────────────────┐        ┌────────┐
          │         ui          │◀──────▶│  ops   │── hunk copy, undo/redo, save
          └─────────────────────┘        └───┬────┘
                     ▲                       │
                     │                  ┌────────┐
                     └──────────────────│ editor │── suspend UI, run $EDITOR, reload
                                        └────────┘
          worker thread: runs diff and dircmp off the UI thread, cancellable
```

| Component | Responsibility |
|---|---|
| `cli` | Parse arguments (`clap`), pick file or directory mode, print `--git-config`, map errors to exit codes. |
| `config` | Load the optional TOML file; unknown keys warn, never fail. |
| `input` | Read files, stdin (`-`) and named pipes fully into memory. Detect binary (NUL byte in the first 8 KB), line endings (LF/CRLF/mixed), a missing final newline, UTF-16 byte-order marks, and size thresholds. |
| `document` | Holds the original bytes, an index of line-start offsets, and an edit overlay (a piece table over lines) so hunk copies never duplicate the whole buffer. Tracks the dirty flag, line-ending style, read-only flag, and the on-disk identity (size, mtime, hash) at load time. |
| `diff` | Normalise each line for comparison (strip the line ending; apply `-w`/`-b`), intern lines into tokens, run `imara-diff` histogram, apply the indent heuristic (see S1), and produce a list of hunks. |
| `align` | Turn hunks into rows: side-by-side rows with filler on the shorter side, or unified rows. Maps between file line numbers and screen rows. |
| `inline` | For each changed block: pair old and new lines (one-to-one when counts match, otherwise by similarity), split them into words and punctuation, and diff the tokens with Myers. Skips lines over about 2,000 characters. Calculated lazily for hunks on screen and cached until the next re-diff. |
| `dircmp` | Walk both trees in parallel (`ignore` or `jwalk`), match by relative path, classify (identical, modified, left-only, right-only, binary, type mismatch, symlink). Compare size first, then bytes in chunks, stopping at the first difference. Skip `.git/`. Don't follow symlinks; compare their targets, except that a symlink opposite a regular file is followed (D32). Ignore permission-only changes. |
| `ops` | Copy a hunk left→right or right→left (converting line endings to the target's), keep the undo/redo list, save. |
| `editor` | Leave the alternate screen, run `$EDITOR +<line> <file>`, restore the terminal, reload the file, re-diff. Records one undo step from before/after snapshots. |
| `ui` | `ratatui` + `crossterm`. File view (side-by-side and unified), directory view, status line, prompts, help. Theme detection and colour depth. Draws only visible rows. |
| `worker` | Runs diffs and directory comparison on a background thread with a cancellation flag, so the UI thread never blocks. |

### 5.3 Data flow for a hunk copy

1. `ops` replaces the target hunk's lines in the target `document` overlay with the source lines, converted to the target's line ending, and pushes an undo step.
2. `diff` re-runs on the two documents' token streams (in memory; microseconds for normal files).
3. `align` rebuilds the row model; `inline` drops its cache.
4. `ui` keeps the cursor on the same logical position and redraws.

### 5.4 Candidate crates

| Purpose | Crate |
|---|---|
| CLI parsing | `clap` (derive) |
| Terminal UI | `ratatui`, `crossterm` |
| Line diff | `imara-diff` |
| Directory walk | `ignore` (parallel walker) or `jwalk` |
| Byte scanning | `memchr` |
| Display width | `unicode-width` |
| Word splitting | `unicode-segmentation` |
| Config | `serde`, `toml` |
| Atomic save | `tempfile` |
| Errors | `anyhow` |
| Tests | `proptest`, `insta`, `criterion` |

Every added crate should be justified against binary size and build time.

## 6. User interface

### 6.1 File view, side-by-side

```
 a/src/auth/token.rs  [read-only]       │   │ src/auth/token.rs  [+]
────────────────────────────────────────┼───┼────────────────────────────────────────
 40  fn refresh(&self) -> Result {      │   │ 40  fn refresh(&self) -> Result {
 41      let ttl = self.ttl;            │ ~ │ 41      let ttl = self.ttl.max(60);
 ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │ + │ 42      log::debug!("ttl={ttl}");
 42      self.fetch(ttl)?;              │   │ 43      self.fetch(ttl)?;
 43      self.cache.clear();            │→- │ ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
 44      Ok(())                         │   │ 44      Ok(())
 45  }                                  │   │ 45  }
 hunk 3/7 · ws: off · LF │ LF · F1 help
```

- **Panes** are row-locked. The shorter side of a hunk gets filler rows (`░`), so matching lines always sit on the same screen row.
- **Gutter** (3 columns): `~` changed, `+` added on the right, `-` removed from the left. On the current hunk, arrows show the copy direction: `→` (`>`) and `←` (`<`).
- **Colours** (not visible above): changed lines get a subtle background tint; the changed words within them get a stronger tint.
- **Headers** show the label (or path), `[read-only]`, and `[+]` when there are unsaved changes.
- **Status line** shows the current hunk, the whitespace mode, each side's line endings (flagged when they differ), and the help key.
- **Long lines** are cut off at the pane edge with `›`. `←`/`→` scroll both panes sideways together. No wrapping.

### 6.2 File view, unified

Used when you press `v`, or automatically when the terminal is narrower than 100 columns (configurable).

```
 a/src/auth/token.rs ↔ src/auth/token.rs  [+]
──────────────────────────────────────────────────────────
 40 40      fn refresh(&self) -> Result {
 41    -         let ttl = self.ttl;
    41 +         let ttl = self.ttl.max(60);
    42 +         log::debug!("ttl={ttl}");
 42 43          self.fetch(ttl)?;
 43    -         self.cache.clear();
 hunk 3/7 · ws: off · F1 help
```

All hunk actions work the same way in both views.

### 6.3 Directory view

```
 left: /tmp/git-difftool.Xa12/left   right: ~/code/app   [identical hidden]
──────────────────────────────────────────────────────────────────────────────
 M   src/auth/token.rs
 M   src/auth/session.rs
 R   src/auth/refresh.rs            only on right
 L   docs/old-flow.md               only on left
 B   assets/logo.png                binary · differs at byte 2,048
 T   config                         file vs directory
──────────────────────────────────────────────────────────────────────────────
 6 differ · scanning… 1,204 files · i: show identical · Enter: open
```

- Results appear as the walk finds them.
- `i` toggles identical files (hidden by default).
- `Enter` opens the selected file in the file view; `Backspace` returns to the list, keeping the selection.
- A one-sided file opens against an empty pane on the other side, so its hunk can be copied across.

### 6.4 Binary file screen

```
 assets/logo.png   (binary)
 left:   48,213 bytes
 right:  48,301 bytes
 first difference at byte 2,048
```

### 6.5 Colours and themes

- 24-bit colour by default, falling back to 256 and then 16 colours according to the terminal's reported support.
- Dark or light background detected by asking the terminal with an OSC 11 query, with a short timeout. See S2 for tmux.
- `theme = "auto" | "dark" | "light"` in config overrides detection.
- `NO_COLOR` is respected: changes are shown with gutter markers and reverse video only.

## 7. Interaction

### 7.1 Keybindings, file view

| Key | Action |
|---|---|
| `j` / `k`, `↓` / `↑` | Move the cursor down / up |
| `Ctrl-d` / `Ctrl-u` | Half a page down / up |
| `gg` / `G` | Top / bottom |
| `J` / `K` (also `]c` / `[c`) | Next / previous hunk, centred on screen |
| `←` / `→` (also `zh` / `zl`) | Scroll both panes sideways |
| `Tab` | Switch focus to the other pane |
| `>` | Copy the current hunk left → right |
| `<` | Copy the current hunk right → left |
| `u` / `Ctrl-r` | Undo / redo |
| `e` | Open `$EDITOR` on the focused file at the cursor line |
| `/` / `?` | Search forward / backward |
| `n` / `N` | Next / previous search match |
| `v` | Switch between side-by-side and unified |
| `W` | Cycle whitespace mode: off → ignore amount (`-b`) → ignore all (`-w`) |
| `w` | Save the changed side(s) |
| `q` | Quit; asks first if there are unsaved changes |
| `Q` | Quit without saving |
| `Ctrl-c` | Abort with exit code 1 (stops a `git difftool` run) |
| `Esc` | Cancel a running diff or directory scan; clear search highlight |
| `Backspace` | Back to the directory view (when opened from one) |
| `F1` | Help |

### 7.2 Keybindings, directory view

| Key | Action |
|---|---|
| `j` / `k`, `↓` / `↑` | Move the selection |
| `gg` / `G` | First / last entry |
| `Enter` | Open the selected file |
| `i` | Show / hide identical files |
| `/` / `n` / `N` | Search file names |
| `q` | Quit (asks first if any opened file has unsaved changes) |
| `Ctrl-c` | Abort with exit code 1 |
| `F1` | Help |

### 7.3 Behaviour rules

- **Current hunk:** the hunk the cursor is inside; if the cursor is in an unchanged area, the next hunk below it. The gutter highlights it, so `>` and `<` always show what they'll act on.
- **Read-only sides:** `>` or `<` into a read-only side is refused with a status-line message.
- **Undo:** one list for the session. Each hunk copy is one step; each trip out to `$EDITOR` is one step (before/after snapshots). Undo still works after saving. History is lost on quit.
- **`$EDITOR` handoff:** runs `$EDITOR +<line> <file>` (falls back to `$VISUAL`, then `vi`). The `+<line>` form works for vi, Vim, Neovim, nano and Emacs. The file must not have unsaved changes in wenas; if it does, wenas asks to save first.

### 7.4 Saving

1. Resolve symlinks, so the real file is written. (This is what makes edits during `git difftool -d` land in your working tree; see §8.2.)
2. Check whether the file changed on disk since loading (size, mtime, then hash). If it did, ask: reload (dropping wenas's changes), overwrite, or cancel.
3. Write to a temp file in the same directory, `fsync`, copy the original's permissions, then rename it over the original.
4. Clear the dirty flag. The undo list is kept.

**Known trade-off:** renaming over the original breaks hard links to it. Accepted, because writing in place risks a half-written file if wenas crashes.

### 7.5 Syntax highlighting

None in v1. Lines are coloured by change type; intra-line highlighting sits on top. Planned later as an optional `syntect` feature that colours only visible rows.

## 8. Command-line interface

### 8.1 Usage

```
wenas [OPTIONS] <LEFT> <RIGHT>
wenas --git-config
wenas --help | --version
```

`LEFT` and `RIGHT` must both be files or both be directories. `-` means stdin (only one side may use it). Named pipes work, so process substitution does too:

```sh
wenas <(git show HEAD~3:src/main.rs) src/main.rs
```

| Option | Meaning |
|---|---|
| `--label-left <TEXT>` / `--label-right <TEXT>` | Pane titles instead of the paths |
| `--readonly-left` / `--readonly-right` | Force a side read-only |
| `-w`, `--ignore-all-space` | Ignore all whitespace when comparing |
| `-b`, `--ignore-space-change` | Ignore changes in the amount of whitespace |
| `--unified` | Start in unified view |
| `--show-identical` | Directory view: show identical files from the start |
| `--config <PATH>` | Use this config file instead of the default |
| `--git-config` | Print the git configuration snippet and exit |

Sides read from stdin or a pipe are always read-only.

### 8.2 Git integration

`wenas --git-config` prints the snippet; wenas never edits `~/.gitconfig` itself.

```ini
[diff]
    tool = wenas
[difftool]
    prompt = false
    trustExitCode = true
[difftool "wenas"]
    cmd = wenas "$LOCAL" "$REMOTE"
[alias]
    dd = difftool --dir-diff
```

- **Recommended use:** `git dd`. Git passes two directories, and wenas opens one session with the file list.
- **Read-only detection:** sides that git created as temporary copies of old versions (single-file mode: `git-blob-*` paths under the temp directory; dir-diff mode: files under `git-difftool.*/left/`) are marked read-only automatically. See S3.
- **Working-tree edits in dir-diff mode:** git gives wenas symlinks to your working-tree files. Because saving follows symlinks (§7.4), edits land in the real files.

### 8.3 Exit codes and start-up behaviour

| Code | When |
|---|---|
| `0` | Normal quit, whether or not the sides differed; or the inputs were identical |
| `1` | Aborted with `Ctrl-c` (with `difftool.trustExitCode = true`, git stops the rest of a per-file run; in `--dir-diff` mode git makes a single call and ignores the exit code, so `Ctrl-c` simply ends the session) |
| `2` | Error: unreadable file, mismatched argument types, output not a terminal, file over the size limit |

- **Identical inputs (files):** print `identical` and exit `0` without opening the UI. Directories always open the list (D33).
- **Output not a terminal:** exit `2` with a hint to use `git diff --no-index`.

## 9. Performance targets

These are **goals to measure, not gates.** Get the tool working first; optimise where the numbers say it matters. Benchmarks run in CI and report results but never fail the build.

| Measure | Target (M4 Pro) |
|---|---|
| First screen, files under 10k lines | ≤ 30 ms |
| First screen, files around 100k lines | ≤ 200 ms |
| Re-diff after a hunk copy, under 10k lines | ≤ 16 ms (one frame) |
| Any frame while scrolling | ≤ 16 ms |
| Memory | ≤ about 3× the combined size of both files |
| Directory compare, ~50k files | First results ≤ 100 ms, the rest streamed |
| File size | Ask before loading over 100 MB; refuse over 1 GB |

**Built in from M1, regardless of the numbers** (these are cheap now and painful to retrofit):

- drawing only the visible rows
- diff and directory comparison on a worker thread, with cancellation
- one buffer per file with a line-start index and an edit overlay, rather than a vector of line strings

## 10. Edge cases

| Case | Handling |
|---|---|
| Binary file | NUL byte in the first 8 KB (git's rule). Binary screen (§6.4); no hunk actions. |
| Invalid UTF-8 | Shown as `\xNN`; the original bytes are written back unchanged. |
| UTF-16 with a byte-order mark | Treated as binary in v1, with a note on the binary screen. |
| Wide characters and emoji | Widths from `unicode-width`; a wide character cut by the pane edge is replaced with a space. |
| Tabs | Shown as 4 columns (configurable). |
| CRLF vs LF | Ignored when comparing; the header flags a mismatch. Copied hunks take the target file's line ending. |
| Mixed line endings in one file | Each line keeps its own ending; the header shows `mixed`. Copied lines take the target's most common ending. |
| No newline at end of file | `⏎` marker on the last line. Copying a hunk that includes it carries the state across. |
| Whitespace-only differences under `-w`/`-b` | The hunk disappears from view and so can't be copied. Expected; documented in help. |
| Very long lines | No intra-line highlighting over about 2,000 characters; column positions measured only for the visible part. |
| Large or pathological diffs | Run in the background; progress shown after about 300 ms; `Esc` cancels. |
| Empty file vs non-empty | Normal diff; the empty side is all filler. |
| Missing file (one side) | Error, exit `2`. In directory view, a one-sided file opens against an empty pane. |
| File changed on disk while open | Detected at save time (§7.4). |
| Stdin or named pipe | Read fully into memory; read-only; no size check before reading. |
| Symlinks in directory compare | Not followed; compared by target path. |
| Permission-only changes | Ignored in v1. |
| Terminal resize | Re-layout; switch to or from unified view at the threshold. |
| Unreadable file in a directory walk | Shown with an `!` marker and the error; the walk continues. |

## 11. Distribution and configuration

### 11.1 Distribution

- `cargo-dist` builds release binaries for:
  - macOS: Apple Silicon and Intel
  - Linux: x86_64 and arm64, statically linked with musl
- It publishes a formula to a personal Homebrew tap (`brew install <user>/tap/wenas`) and generates the GitHub Actions release workflow.
- `cargo install wenas` also works.
- Homebrew's main repository can come later, once the project qualifies.

### 11.2 Config file

Optional, at `~/.config/wenas/config.toml` on both macOS and Linux (deliberately not `~/Library/Application Support`). A missing file means the defaults; an unknown key prints a warning and is otherwise ignored.

```toml
theme = "auto"              # "auto" | "dark" | "light"
tab_width = 4
whitespace = "off"          # "off" | "change" | "all"
unified_below_columns = 100

[colors]
# Override any colour by name, e.g.
# changed_bg = "#2a2f3a"
# changed_word_bg = "#45506a"

[directory]
exclude = [".git", "node_modules", "target"]
show_identical = false
```

Colour names accepted under `[colors]`, each as `#rrggbb`: `panel_bg`, `dim`, `muted`, `bright`, `amber`, `on_amber`, `cursor_bg`, `changed_bg`, `changed_word_bg`, `added_bg`, `added_word_bg`, `removed_bg`, `removed_word_bg`, `filler`, `added_fg`, `removed_fg`, `changed_fg`, `match_bg`, `match_fg`. Unknown names and malformed values warn and are ignored; overrides are ignored under `NO_COLOR` and follow the terminal's colour depth.

Key remapping is planned for a later version.

## 12. Testing

| Layer | What | Tools |
|---|---|---|
| Unit | Line normalisation, alignment and filler rows, intra-line pairing and token diff, line-ending conversion, binary detection | `cargo test` |
| Comparison with git | For a corpus of file pairs (real repo history plus crafted cases), wenas's hunks must match `git diff --no-index --histogram --indent-heuristic`. Differences are reviewed, then accepted or fixed. This is also how S1 is checked. | `git` in CI |
| Property | Copying every hunk left→right yields exactly the right file, byte for byte. Undoing every step restores the original. Line-ending conversion round-trips. Saving never alters untouched bytes. | `proptest` |
| Screen snapshots | Rendered side-by-side, unified, narrow, CRLF, binary, directory and help screens | `ratatui` test backend, `insta` |
| Integration | Run the binary against fixture files: exit codes, identical inputs, non-terminal output, `--git-config`, size limits | `assert_cmd` |
| Benchmarks | First-screen time, re-diff time, directory compare; reported in CI, never failing the build | `criterion` |
| Manual | Real `git difftool` and `git dd` sessions in Ghostty + tmux on macOS, and on Linux | Checklist per release |

CI runs on macOS and Linux.

## 13. Milestones

| Milestone | Content | Done when |
|---|---|---|
| **M0 — Checks** | Spikes S1–S3. Cargo workspace, CI on macOS + Linux. | Each spike has a written answer and a decision. |
| **M1 — Engine** | `input`, `document`, `diff` (with whitespace modes and CRLF handling), `align`, worker thread. Comparison-with-git and property tests. | The corpus matches git (or every difference is understood); property tests pass. |
| **M2 — Viewer** | Read-only file view: side-by-side and unified, gutter, scrolling, hunk navigation, sideways scroll, search, intra-line highlighting, colours and theme detection, narrow-terminal switch. Exit codes and start-up rules. | Usable daily as a read-only `git difftool`. |
| **M3 — Editing** | Hunk copy, undo/redo, atomic save with on-disk change detection, read-only rules and git temp detection, `$EDITOR` handoff. | Can resolve real differences with `git difftool` and save safely. |
| **M4 — Directories** | `dircmp`, directory view, opening files from it, `git dd` integration. | `git dd` works end to end, including saving edits into the working tree. |
| **M5 — Edge cases and config** | Binary screen, encodings, mixed line endings, large-file prompts, cancellation, config file, help screen, `--git-config`. | The edge-case table (§10) is covered by tests. |
| **M6 — Release** | `cargo-dist`, Homebrew tap, README with screenshots, benchmark reporting in CI. | `brew install` works on a clean Mac. |

## 14. Open questions and spikes

| ID | Question | Plan |
|---|---|---|
| **S1** | Does `imara-diff` support git's indent heuristic (or an equivalent post-processing hook)? | Check its API. If missing, port the heuristic from git's `xdiff` (a few hundred lines). If porting is impractical, fall back to running `git diff` for the first diff only. |
| | **S1 answer (2026-11-02)** | Yes: `imara-diff` 0.2 provides `Diff::postprocess_lines` (indent heuristic). On 400 random line-pair inputs, 390 produced hunks identical to `git diff --no-index --histogram --indent-heuristic`; the rest are equally valid diffs placed differently on degenerate ties. All crafted realistic cases match exactly (`tests/engine.rs`). **Decision:** use `imara-diff` as is; no port, no `git` fallback. |
| **S2** | Does the OSC 11 background query work inside tmux in Ghostty? | Test with your setup; if unreliable, default to `theme = "dark"` when `$TMUX` is set. |
| | **S2 answer (2026-11-03)** | Not "default to dark under tmux": that penalises light-theme users in the main workflow. wenas sends the OSC 11 query followed by a DA1 query (`ESC [ c`). Terminals reply in order, so the DA1 reply marks the end of the exchange: if no background reply preceded it (tmux with nothing to forward, or a terminal that ignores OSC 11) the answer is "unknown" immediately and the theme falls back to dark, with no timeout and no late bytes left to be read as key presses. A silent terminal costs the 150 ms timeout. Verified with a pty that plays each terminal behaviour, with and without `$TMUX`. **Still to confirm by hand:** whether Ghostty + tmux forwards the real colour (RELEASING.md checklist). |
| **S3** | How reliably can git's temporary sides be detected on macOS (`$TMPDIR` paths, `git-blob-*`, `git-difftool.*/left/`)? Does `--no-symlinks` mode still copy edits back? | Run both difftool modes and record the paths git passes; adjust detection rules and docs. |
| | **S3 answer (2026-11-02, Linux, git 2.43)** | Single-file mode: `$LOCAL` is `<tmp>/git-blob-XXXXXX/<name>` (a *directory* named `git-blob-*`), `$REMOTE` is the working file as a relative path. `--dir-diff`: `<tmp>/git-difftool.XXXXXX/left/` and `.../right/`; the right side is symlinks into the working tree, or with `--no-symlinks` copies that git copies back after the tool exits. Edits land in the working tree in both modes. Rules in `src/gitenv.rs`: any `git-blob-*` path component, or `git-difftool.*` followed by `left`. **Correction to §8.2:** git ignores `trustExitCode` inside `[difftool "wenas"]`; only `difftool.trustExitCode` (or `--trust-exit-code`) makes a non-zero exit stop the run, so the snippet now sets it under `[difftool]`. macOS paths still to be checked (only `$TMPDIR` should differ). |
| O1 | Is `wenas` free on Homebrew? | Free on crates.io (checked 2026-11-02); Homebrew not yet checked. |
| O2 | Default tab width 4 or 8? | 4 for now (most code); revisit after daily use. |
| O3 | Should a side with unsaved changes block `e`, or should wenas save automatically before handing off? | Ask in v1 (§7.3); revisit after use. |

## Appendix A — Decision log

| # | Decision |
|---|---|
| D1 | Positioning: Meld-style experience with no setup (gutter, intra-line highlighting, one key per hunk, directory compare, instant start). |
| D2 | v1: two-way file diff, `git difftool` (single file and `--dir-diff`), directory compare. |
| D3 | Later: three-way merge and `git mergetool`. |
| D4 | Editing: hunk copy, undo, save; free-form edits via `$EDITOR`. |
| D5 | macOS first, Linux supported and tested in CI. |
| D6 | Rust, `ratatui` + `crossterm`. |
| D7 | `imara-diff` histogram for every file diff; `git` is the test reference only. |
| D8 | Intra-line highlighting in-process; skipped above ~2,000 characters; visible hunks only. |
| D9 | Directory compare: own parallel walk, size then byte comparison; identical files available; `.git/` skipped; symlinks not followed. |
| D10 | Row-locked side-by-side with filler rows and a 3-column gutter; `v` toggles unified. |
| D11 | Unified view below 100 columns; no wrapping; synchronized sideways scrolling. |
| D12 | No syntax highlighting in v1. |
| D13 | 24-bit colour with fallbacks; OSC 11 theme detection; `NO_COLOR`. |
| D14 | Vim-style keymap (§7.1). |
| D15 | Current hunk = hunk under the cursor, or the next one below. |
| D16 | Session-wide undo; hunk copies and editor trips are single steps. |
| D17 | Atomic save following symlinks; on-disk change detection; read-only sides. |
| D18 | CLI `wenas [OPTIONS] LEFT RIGHT`; labels; `-` and pipes as read-only inputs. |
| D19 | `--git-config` prints a snippet; docs recommend `difftool -d`. |
| D20 | Exit codes 0 / 1 (abort) / 2 (error). |
| D21 | Identical inputs exit immediately; non-terminal output exits 2. |
| D22 | Performance numbers are tracked goals, not gates; core performance design built in from M1. |
| D23 | Background diffing with progress and cancellation; 100 MB prompt, 1 GB limit. |
| D24 | Raw bytes in, raw bytes out; NUL-byte binary detection; UTF-16 treated as binary in v1. |
| D25 | CRLF/LF ignored when comparing; copied hunks take the target's line ending; `-w`/`-b` with `W` to cycle. |
| D26 | `cargo-dist` releases, personal Homebrew tap, `cargo install`. |
| D27 | Optional `~/.config/wenas/config.toml`; key remapping later. |
| D28 | Testing: unit, comparison with git, property, snapshot, integration, benchmarks. |
| D29 | Name: `wenas`. |
| D30 | `Esc` cancels a running diff or scan; `Ctrl-c` always aborts with exit 1. (Resolves a clash between D20 and D23.) |
| D31 | Directory compare: one thread walks both trees in name order, a pool of up to 8 threads compares file contents, and a collector puts results back in order, so the list streams in sorted while the slow part (reading files) uses every core. A full scan of two 50k-file trees takes about 0.37 s on 4 cores (1.7 s single-threaded); the first result arrives in about 5 ms. |
| D32 | A symlink opposite a regular file is compared through the link, because `git difftool --dir-diff` presents the working tree as symlinks. Two symlinks are still compared by target; a symlink to a directory is never followed. |
| D33 | Directory mode always opens the list (§6.3), including when nothing differs (identical files are one `i` away); without a terminal it exits 2 immediately (§8.3). The `identical` early exit applies to files only. |
| D34 | Files opened from the directory list keep their session (edits, undo) when you return to the list. Quit asks once about all of them; "save both" resolves disk conflicts file by file. |
| D35 | Inputs over 50,000 lines (both sides together) are diffed on a worker thread. After 300 ms the "Comparing…" screen appears; `Esc` leaves (exit 0) and the process exit ends the worker, because `imara-diff` has no cancel hook. Re-diffs after edits and `W` stay synchronous, and a file opened from the directory list is diffed on the UI thread. |
| D36 | Files over 100 MB are confirmed first: on `[y/N]` at the terminal in file mode, with a panel in the directory view. Declining exits 0 with "skipped". With no terminal to ask on, the file is loaded. Files over 1 GB are refused (exit 2) without asking. |
| D37 | The help screen uses three columns from 96 columns wide so that, with the editing keys, the whitespace note still fits a 24-row terminal. |
| D38 | `⏎` marks an unterminated last line on either side. |
| D39 | Release builds use the `dist` profile (fat LTO, stripped, `panic = "abort"` inherited from release); the static musl binary is about 1.5 MB. The minimum supported Rust is 1.88 (verified by building with it, and checked in CI). |
| D40 | Licensed MIT OR Apache-2.0. |
| D41 | Benchmarks live in `benches/engine.rs` (criterion) and CI reports them on a macOS runner against the §9 targets via `scripts/bench-summary.py`; they never fail the build. Measured on a 4-core Linux VM (the directory compare was 1.7 s before D31's pool, 0.37 s after), everything meets its target except the first screen for 100k-line files (about 380 ms against 200 ms), which is the cost of the histogram diff itself. |
| D42 | Module layout follows §5.2: `document` owns loading and read-only state (`Access`, with a `ReadOnlyReason` so a read-only side always has a reason to show), `ops` owns undo/redo and saving, `editor` owns the `$EDITOR` handoff, `cli` maps outcomes to exit codes, and `run` starts the right screen so `main` only parses arguments. |
