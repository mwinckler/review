# Review

A keyboard-driven branch review TUI built with Rust and Ratatui. Browse a file
tree, read syntax-aware diffs from [Difftastic](https://difftastic.wilfred.me.uk/),
mark files viewed, and pick up where you left off.

![Screenshot](./review-screenshot.png)

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

# Compare two change/commit IDs, branches, or bookmarks directly
review ukr..vqm
review master..feature/my-change

# Compare against the current revision (@); these forms are equivalent
review ukr..
review ukr..@
review ukr

# Native revision expressions
review 'trunk()..@'
review --backend git 'HEAD~3..@'

# Git: review the current branch against its merge base with master
review --base master

# Git merge-base shorthand
review --backend git master...feature/my-change

# Explicit merge-base branch comparison
review --base origin/master --head feature/my-change

# Jujutsu: review the working-copy revision against trunk
review --base 'trunk()' --head @

# Select a repository and backend explicitly
review -C ../project --backend git --base master
```

Positional `BASE..HEAD` compares the two snapshots directly. Either omitted
endpoint defaults to `@`, and a lone `REV` means `REV..@`. `@` selects Git's
current `HEAD` or Jujutsu's working-copy revision. Each endpoint accepts the
selected backend's revision syntax and resolves to one revision, including branch
names, commit IDs, jj change IDs, and expressions such as `heads(master::feature)`.
Quote expressions containing shell metacharacters.

Bare `review` defaults to `--base master`. Use `--base` with an optional `--head`
to compare from their unique merge base; Git's positional `BASE...HEAD` uses the
same merge-base behavior. This focuses the review on changes made on the head
branch since the histories diverged.

The nearest repository is detected automatically from the current directory,
with Jujutsu preferred in a colocated workspace. The header displays the resolved
snapshot IDs. Jujutsu reviews show each endpoint's change ID and Git commit hash,
such as `master (ukrwmnxy abcdef12)`.
Added and removed line counts appear at the right edge of the header in green
and red: `+12 -3 · +120 -45 total · +30 -8 tests`. The first pair covers the
highlighted file or folder; folder counts include all changed descendants,
including viewed files and files hidden by filters or collapsed folders. The
`total` pair covers the full comparison. The `tests` pair totals changed files in
directories named `test`, `tests`, `spec`, or `specs`, or with `.test.` or `.spec.`
in the filename. All counts follow the pinned snapshots when resuming or refreshing.

Reviews check the selected revisions in the background about once per second.
Jujutsu `@` follows saved working-copy edits; Git comparisons follow commits as
`HEAD` or branches advance. File trees, file and directory previews, header IDs,
and line totals reload together. Updates save automatically, preserving selection,
pane focus, and scroll positions for unchanged diffs.

Jujutsu checks snapshot the working copy through `jj`, then read file contents at
fixed revisions.

## Stop and resume

```sh
review --resume              # Resume the most recently saved session
review --list                # List saved sessions for this repository/backend
review --resume r3           # Resume a particular session
review --base master         # Start a fresh review
```

New sessions use short IDs: `r1`, `r2`, `r3`, and so on, starting at
`r1` for each repository/backend. IDs stay with their sessions as progress is saved,
and deleted sessions' numbers become reusable after seven days. New reviews reuse
the lowest eligible number before allocating a fresh one. After reuse, the ID
identifies the new review. The `r` prefix keeps these IDs distinct from older hash
IDs. Existing saved IDs and unique prefixes remain valid.
On a terminal, `review --list` highlights each session's shortest unique prefix in
bold magenta. Ambiguous prefixes report the matching IDs so you can choose a longer
prefix. Piped listings use plain IDs; `NO_COLOR=1` also selects plain terminal output.

Each `review --list` entry includes its last-opened datetime in UTC, for example
`Opened 2026-10-28T08:30:00Z`. Creating or resuming a review records this time.
Older sessions use their last saved time until their next resume.

Press `q` or `Ctrl-c` to save and quit. Viewed changes save immediately;
navigation state autosaves within 250 ms. Line comments save immediately when
submitted. Resume restores comments, the comparison,
viewed files, selection, pane focus, directory expansion, filters, layout,
tree width, and per-file and per-directory scroll positions.

Sessions belong to the canonical workspace path and selected backend. Use the
same backend when resuming a session created with `--backend git` in a colocated
workspace.

**Resume restores the original snapshots.** Subsequent changes while the review is
open trigger an automatic refresh. Press `r` to resolve the original branch/revision
expressions immediately using the same comparison mode. A viewed marker survives
when both sides of the file change—including paths, types, and executable bits—are
identical. Changes to either side reset its marker; equivalent history rewrites
preserve it. Changed files reappear in the unviewed-only filter.
Keep reviewed commits reachable while a session is active: file rendering reads
their stored objects.

State lives in `$XDG_STATE_HOME/review/` (normally `~/.local/state/review/`). Use
`--state-dir <directory>` for a different location. Saves are atomic, and each
open session holds an exclusive lock.

### Forget saved sessions

```sh
review --forget r3             # Forget one session by ID or unique prefix
review --forget r3 r7          # Forget several sessions
review --forget-all            # Forget every closed session in this repository/backend
review --forget-all --yes      # Confirm immediately, suitable for scripts
review --cleanup              # Forget closed reviews last opened over a week ago
review --cleanup --yes        # Confirm stale-review cleanup immediately
```

The CLI asks for confirmation; `-y` / `--yes` confirms explicitly for automation.
All requested IDs and session locks are checked before deletion. Open sessions
are protected, and `--forget-all` selects closed sessions.

`--cleanup` selects this repository/backend's closed sessions whose last-opened
time is strictly more than seven days ago. Older sessions use their last saved
time. Cleanup asks for confirmation and rechecks the selected sessions' age and
locks before deletion. Use `--cleanup --yes` for automation. Deleted numbers follow
the same seven-day reuse cooldown as manually forgotten sessions.

Press `S` from either pane to open the session dialog. It shows session IDs,
comparisons, viewed progress, and current/open indicators. Use `j` / `k` or arrows
to move, `Space` to toggle selections, and `a` to select all closed sessions or
clear the selection, including sessions outside the viewport. `gg` / `G` and the
page-scrolling keys also work. Press `d`, `Delete`, or `Enter` to review the
confirmation, then `y` to delete or `n` / `Esc` to cancel. `r` reloads the list;
`Esc` / `q` closes the dialog and returns to the review.

Forgetting permanently removes the session's saved comments, progress, and navigation state.
Repository content and persistent editor snapshots are preserved.

## Keyboard controls

| Keys | Action |
|---|---|
| `j` / `k`, arrows | Move in the tree, scroll the diff, or move a highlighted line |
| `h` / `l` | Collapse/expand directories, scroll horizontally, or choose old/new side |
| `Enter` | Toggle a directory, focus a file's diff, or highlight one line in the diff |
| `Tab`, `Ctrl-h` / `Ctrl-l` | Switch panes; focus the tree / diff |
| `Ctrl-Left` / `Ctrl-Right` | Shrink/grow the tree by two columns |
| `gg` / `G`, `Home` / `End` | First/last row or page |
| `Ctrl-d/u`, `Ctrl-f/b`, `PageDown/Up` | Half/full-page scrolling |
| `Space` | Toggle file viewed (advance when marked); mark all files in a folder viewed |
| `]f` / `[f` | Next/previous file, expanding its parents |
| `]u` / `[u` | Next/previous unviewed file |
| `]h` / `[h` | Select and highlight the next/previous diff hunk |
| `]Nh` / `[Nh` | Select hunk number `N` (1-based) |
| `e` | Open the selected hunk or file in the editor |
| `c` | Compose a line comment; `Ctrl-s` saves it locally |
| `v` | Start a range at the highlighted line; `j` / `k` extend, `v` returns to one line |
| `C` | Browse comments, edit, delete, jump to source, or write a source TODO |
| `/` | Filter filenames in the tree; search text in the diff |
| `s` | Flash jump to a visible tree entry from either pane |
| `S` | Manage and forget saved sessions with multiselect |
| `n` / `N` | Next/previous matching file or diff search result |
| `u` | Toggle the unviewed-only filter |
| `i` | Toggle inline and side-by-side rendering |
| `z` | Toggle a full-width diff |
| `Esc` | Clear file filters; cancel a search or Flash prompt |
| `r` | Refresh the comparison |
| `?` | Show help and custom key aliases |
| `q`, `Ctrl-c` | Save and quit |

Marking a file viewed advances to the next unviewed file matching the current
filter, expanding its parent folders and wrapping at the end. Pane focus and
layout are retained. Unmarking keeps the current file selected.

Search and filtering are case-insensitive. Directory counts show progress across
their changed files. Pressing `Space` on a folder marks all its changed files
viewed, including nested files and files hidden by filters or collapsed folders.
Highlighting a folder shows a combined diff of all its changed descendants,
including collapsed and filtered-out files, with a heading for each file.
Focus the right pane with `Ctrl-l` or `Tab` to scroll or search the combined diff.
`Space` in a directory preview marks the directory's files viewed.
Viewed files and fully viewed folders appear slightly dimmed in the tree.
Narrow terminals display the focused pane at full width.
When the rendered diff is taller than its pane, a right-edge scrollbar shows your
position and how much of the diff is visible.

### Hunk selection and LazyVim

Use `]h` / `[h` to select a hunk in the configured renderer's output. Selection
wraps, focuses the diff pane, and highlights the hunk while preserving syntax
colors. Folder previews select hunks across all their changed descendants.
Insert a 1-based number between the bracket and `h` to select that hunk directly:
`]4h` and `[4h` both select hunk #4. Multi-digit numbers work too, and numbering
covers the entire current file or folder preview.
Navigation centers the selected hunk within the available scroll range.
Selection follows the same source location when resizing or switching layouts.
`Esc` clears the hunk selection.

Press `e` to open the selected hunk at its source line. With the diff pane focused,
`e` also opens the nearest displayed hunk; from the file tree it opens the file.
The default command launches a new terminal window running `nvim`, using your
normal Neovim/LazyVim configuration. It requires `xdg-terminal-exec` and `nvim`.

Locations follow the reviewed snapshots. Lines shifted by working-copy edits are
mapped to their current location on disk. Deleted files, replaced lines, and
old-side-only locations open a read-only snapshot stored under the repository's
review state directory. Snapshot files retain their filename extension and remain
available after review exits.

Hunk adapters recognize Difftastic, `diff`, and Git unified output automatically.
Difftastic's colored line numbers identify changed lines (`--color=always`, as in
the default configuration). Plain Difftastic output provides old-side hunk-start
locations. For wrapper scripts, set `[renderer] hunk_format = "difftastic"` or
`"unified"`. `"none"` uses file-level editor opening.

### Flash navigation

Press `s` from either pane, type a filename fragment, then press a highlighted
label to select that file or folder in the tree. Jump keys are case-sensitive,
using lowercase labels first, then uppercase labels for additional matches.
Press each label exactly as displayed. Keep typing to narrow the case-insensitive
matches; `Backspace` edits the fragment, and `Enter` selects the first match.

The typed fragment highlights within each matching name. Jump labels overlay the
second character after each match, keeping its next character visible. Repeated
matches in the same entry share a jump key. Labels leave other matches and their
continuations visible, and use letters that keep every visible query extension
available for typing.

Flash searches the visible tree entries, respecting filters and collapsed folders.
It reveals the tree from a full-width diff and focuses the tree after a jump.
Press `Esc` to restore the previous pane and layout.

### Line comments

Press `c` on a file, selected hunk, or displayed source line to compose a comment.
To choose a single line, focus the diff with `Tab` or `Ctrl-l`, then press `Enter`.
Use `j` / `k` or arrows to move the highlight and `c` to comment on that line.
For a range, press `v` at the highlighted line, extend it with `j` / `k` or the page
keys, then press `c`. Press `v` again or `Enter` to return to a single-line highlight
at the current endpoint. Starting a highlight uses the currently visible source.
`h` / `l` select the old/new side; `gg` / `G` move to the first/last mapped source
line. A range belongs to one file and one side. `Esc` clears the line highlight or
range.
Difftastic and unified renderers support visual selection in file and folder
previews, including inline and side-by-side layouts.

In the composer, type or paste multiline text. `Enter` adds a newline, arrows move
the text cursor, and `Ctrl-s` submits the comment. `Tab` / `Shift-Tab` cycle between
the body and the inclusive start/end line fields for a new comment; `Ctrl-u` clears
a line field. These fields also support renderers with custom output. `Esc`
discards the draft. Comment bodies support Markdown text for future remote reviews.

Press `C` to browse every comment in the session, including comments on hidden or
viewed files. `j` / `k` select a comment, `Enter` jumps to its source, `e` edits its
body, and `d` asks to delete it. `PageUp` / `PageDown` or `Ctrl-u` / `Ctrl-d` scroll
long comments and source excerpts. Magenta `●` markers identify commented diff
lines and show per-file/folder comment counts in the tree.

Comments are local session data stored alongside progress in the state directory.
Each comment retains a stable ID, timestamps, the original base/head revision IDs,
the selected side and repository-relative path, an inclusive source range, a file
object identity, and the quoted source. Refresh preserves these original anchors.
When the anchored file side changes or disappears, the comment is labeled
`outdated` and remains available with its original source excerpt.

The provider-independent records in `src/comments.rs` are separate from terminal
line mapping and dialogs. A future GitHub, GitLab, or Gitea adapter can translate
the revision/side/range anchor and Markdown body into a remote review comment,
using the local ID to associate a remote record with its saved counterpart.

#### Write a source TODO

In the comment browser, press `t`, then `y` to confirm writing the selected comment
above its source range as `// TODO: ...`. Further body lines become `// ...` lines.
This action edits the working source file. It requires a new-side comment on a
regular UTF-8 source file in a supported `//`-comment language, such as Rust,
C/C++, Go, Java, JavaScript, or TypeScript. The complete selected range must still
match; unrelated line shifts are mapped to the current working file. Outdated
anchors, replaced ranges, and symlinks are rejected with an explanation.

TODO insertion preserves indentation, line endings, and file permissions. The
local comment retains its original anchor and a TODO-write receipt, which protects
against duplicate writes. Source TODOs remain ordinary working-copy edits when
local comments are edited, deleted, or forgotten.

## Configuration

Create `$XDG_CONFIG_HOME/review/config.toml` (normally
`~/.config/review/config.toml`), or pass `--config <file>`. See
[`config.example.toml`](config.example.toml).

```toml
[renderer]
command = ["diff", "-u", "{old}", "{new}"]
timeout_ms = 30000

[editor]
command = ["xdg-terminal-exec", "--", "nvim", "+{line}", "{read_only}", "--", "{file}"]

[keybindings]
J = "next_file"
K = "previous_file"
V = "toggle_viewed"
"Ctrl-j" = "next_unviewed"
```

Renderers receive temporary files containing the reviewed snapshots. Arguments
are passed directly to the executable; use a wrapper script for pipelines.
Supported placeholders are `{old}`, `{new}`, `{path}`, `{width}`, and `{display}`
(`inline` or `side-by-side`). Filenames/extensions are preserved for language
detection. ANSI colors are rendered inside the TUI. Exit codes 0 and 1 support
conventional diff tools.

Editor arguments are also passed directly, with `{file}`, `{line}`, `{column}`,
and `{repo}` placeholders. `{read_only}` expands to `-R` for snapshots; a
standalone `{read_only}` argument is omitted for working files. The launcher also
receives `REVIEW_EDITOR_READ_ONLY=1` for snapshots and `0` for working files.
Use an editor bridge script to customize window selection or editor integration.
Editor preparation and launching run in the background; failures appear in the
footer.

Diff jobs run in the background. Superseded jobs are cancelled, completed views
are cached, and resizing re-renders for the current pane width. Renderer output
is limited to 16 MiB per job, with one configured timeout budget for each file or
directory preview.
Binary changes, renames, symlinks, executable-bit changes, submodule pointers,
and Jujutsu conflict contents remain visible through file metadata and diffs.

Key aliases supplement the defaults in navigation mode; comment dialogs, search and Flash prompts
use typed characters for their own input. Keys may be a character, a named key
(`Space`, `Enter`, `Tab`, `BackTab`, `Esc`, `Backspace`, arrows, `PageUp`,
`PageDown`, `Home`, `End`), or a `Ctrl-`/`Alt-` combination. Use uppercase
characters for shifted letters.
`Ctrl-c` always saves and exits.

Actions: `up`, `down`, `left`, `right`, `focus_tree`, `focus_diff`, `first`, `last`,
`page_down`, `page_up`, `half_page_down`, `half_page_up`, `next_file`,
`previous_file`, `next_unviewed`, `previous_unviewed`, `next_hunk`, `previous_hunk`,
`open_editor`, `add_comment`, `select_line`, `select_lines`, `manage_comments`,
`toggle_viewed`,
`toggle_unviewed`, `toggle_inline`, `toggle_fullscreen`, `search`, `flash`,
`manage_sessions`,
`next_match`, `previous_match`, `refresh`, `clear_filters`, `help`, `quit`.

## Development

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Integration tests use temporary repositories created through Jujutsu and require
Git, `jj`, `difft`, standard `diff`/`sleep`/`cat` tools, and a POSIX shell.
Unix PTY tests cover the application, autosave, resumed file and directory
previews, key aliases, and terminal restoration.
