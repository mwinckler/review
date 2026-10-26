# Review

A keyboard-driven branch review TUI built with Rust and Ratatui. Browse a file
tree, read syntax-aware diffs from [Difftastic](https://difftastic.wilfred.me.uk/),
mark files viewed, and pick up where you left off.

## Install

Requires Rust 1.88+, Git, and `difft` on `PATH`. Jujutsu reviews use `jj` 0.46+
(the structured diff template interface).

```sh
cargo install --path .
```

## Start a review

```sh
# Review the current directory's repository against master
review

# Git: review the current branch against its merge base with main
review --base main

# Explicit branch endpoints
review --base origin/main --head feature/my-change

# Jujutsu: review the working-copy revision against trunk
review --base 'trunk()' --head @

# Select a repository and backend explicitly
review -C ../project --backend git --base main
```

The base defaults to `master`; use `--base` to choose another branch or revision.
The nearest repository is detected automatically from the current directory,
with Jujutsu preferred in a colocated workspace. Each endpoint must resolve to
one revision; jj revision expressions such as `heads(main::feature)` work. The
comparison starts at the unique merge base, so unrelated changes on the base
branch stay outside the review. The header displays the resolved snapshot IDs.

For Jujutsu, starting or refreshing a review snapshots the working copy once
through `jj`. File contents are then read at fixed revisions.

## Stop and resume

```sh
review --resume          # Resume the most recently saved session
review --list            # List saved sessions for this repository/backend
review --resume <id>     # Resume a particular session
review --base main       # Start a fresh review
```

Press `q` or `Ctrl-c` to save and quit. Viewed changes save immediately;
navigation state autosaves within 250 ms. Resume restores the comparison,
viewed files, selection, pane focus, directory expansion, filters, layout,
and per-file scroll positions.

Sessions belong to the canonical workspace path and selected backend. Use the
same backend when resuming a session created with `--backend git` in a colocated
workspace.

**Resume keeps the original snapshots.** Press `r` to resolve the original
branch/revision expressions again. A viewed marker survives when both sides of
the file change—including paths, types, and executable bits—are identical.
Changes to either side reset its marker; equivalent history rewrites preserve it.
Keep reviewed commits reachable while a session is active: file rendering reads
their stored objects.

State lives in `$XDG_STATE_HOME/review/` (normally `~/.local/state/review/`). Use
`--state-dir <directory>` for a different location. Saves are atomic, and each
open session holds an exclusive lock.

## Keyboard controls

| Keys | Action |
|---|---|
| `j` / `k`, arrows | Move in the tree or scroll the diff |
| `h` / `l` | Collapse/expand directories or scroll horizontally |
| `Enter` | Toggle a directory or focus the selected file's diff |
| `Tab`, `Ctrl-w h/l` | Switch panes |
| `gg` / `G`, `Home` / `End` | First/last row or page |
| `Ctrl-d/u`, `Ctrl-f/b`, `PageDown/Up` | Half/full-page scrolling |
| `Space` | Toggle a file's viewed marker; mark all files in a folder viewed |
| `]f` / `[f` | Next/previous file, expanding its parents |
| `]u` / `[u` | Next/previous unviewed file |
| `/` | Filter filenames in the tree; search text in the diff |
| `n` / `N` | Next/previous matching file or diff search result |
| `u` | Toggle the unviewed-only filter |
| `i` | Toggle inline and side-by-side rendering |
| `z` | Toggle a full-width diff |
| `Esc` | Clear file filters; cancel a search prompt |
| `r` | Refresh the comparison |
| `?` | Show help and custom key aliases |
| `q`, `Ctrl-c` | Save and quit |

Search and filtering are case-insensitive. Directory counts show progress across
their changed files. Pressing `Space` on a folder marks all its changed files
viewed, including nested files and files hidden by filters or collapsed folders.
Narrow terminals display the focused pane at full width.

## Configuration

Create `$XDG_CONFIG_HOME/review/config.toml` (normally
`~/.config/review/config.toml`), or pass `--config <file>`. See
[`config.example.toml`](config.example.toml).

```toml
[renderer]
command = ["diff", "-u", "{old}", "{new}"]
timeout_ms = 30000

[keybindings]
J = "next_file"
K = "previous_file"
v = "toggle_viewed"
"Ctrl-j" = "next_unviewed"
```

Renderers receive temporary files containing the reviewed snapshots. Arguments
are passed directly to the executable; use a wrapper script for pipelines.
Supported placeholders are `{old}`, `{new}`, `{path}`, `{width}`, and `{display}`
(`inline` or `side-by-side`). Filenames/extensions are preserved for language
detection. ANSI colors are rendered inside the TUI. Exit codes 0 and 1 support
conventional diff tools.

Diff jobs run in the background. Superseded jobs are cancelled, completed views
are cached, and resizing re-renders for the current pane width. Renderer output
is limited to 16 MiB per job, with the configured timeout applied to each process.
Binary changes, renames, symlinks, executable-bit changes, submodule pointers,
and Jujutsu conflict contents remain visible through file metadata and diffs.

Key aliases supplement the defaults in navigation mode; search prompts accept
literal text. Keys may be a character, a named key (`Space`, `Enter`, `Tab`,
`BackTab`, `Esc`, `Backspace`, arrows, `PageUp`, `PageDown`, `Home`, `End`), or a
`Ctrl-`/`Alt-` combination. Use uppercase characters for shifted letters.
`Ctrl-c` always saves and exits.

Actions: `up`, `down`, `left`, `right`, `focus_tree`, `focus_diff`, `first`, `last`,
`page_down`, `page_up`, `half_page_down`, `half_page_up`, `next_file`,
`previous_file`, `next_unviewed`, `previous_unviewed`, `toggle_viewed`,
`toggle_unviewed`, `toggle_inline`, `toggle_fullscreen`, `search`, `next_match`,
`previous_match`, `refresh`, `clear_filters`, `help`, `quit`.

## Development

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Integration tests use temporary repositories created through Jujutsu and require
Git, `jj`, `difft`, and standard `diff`/`sleep` tools. Unix PTY tests exercise the
real application, autosave, key aliases, pinned resume, and terminal restoration.
