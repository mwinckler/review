use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Component, Path},
};

use anyhow::{Context, Result, ensure};

use crate::{
    command,
    comment_dialog::LineTarget,
    comments::DiffSide,
    renderer::changed_line_ranges,
    state::Review,
    vcs::{Backend, Change, Comparison, Repository, exact_fileset, validate_commit_id},
};

pub enum RestoreState {
    Confirm(Restore),
    Apply(Restore),
    Applying,
}

impl RestoreState {
    pub fn take_confirmed(state: &mut Option<Self>) -> Option<Restore> {
        match state.take() {
            Some(Self::Apply(action)) => Some(action),
            other => {
                *state = other;
                None
            }
        }
    }
}

pub struct Restore {
    comparison: Comparison,
    change: Change,
    lines: Option<LineTarget>,
}

impl Restore {
    pub(crate) fn new(review: &Review, lines: Option<LineTarget>) -> Result<Self> {
        ensure!(
            review.gitlab.is_none() && review.comparison.head_ref == "@",
            "Restore requires a local Jujutsu review with @ as the head."
        );
        let path = lines
            .as_ref()
            .map_or(review.view.active.as_str(), |lines| &lines.path);
        let change = review
            .changes
            .iter()
            .find(|change| change.path == path)
            .context("Select a changed file or highlight source lines to restore.")?;
        if lines.is_some() {
            ensure!(
                [&change.old, &change.new]
                    .iter()
                    .all(|entry| matches!(entry.kind.as_str(), "file" | "")),
                "Line restore requires a regular text file."
            );
        }
        let whole_rename = lines.is_none() && change.status == "R";
        for path in [Some(path), whole_rename.then_some(change.old_path.as_str())]
            .into_iter()
            .flatten()
        {
            ensure!(
                !path.is_empty()
                    && Path::new(path)
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "Restore requires a repository-relative file path."
            );
            ensure!(
                !review.changes.iter().any(|other| {
                    [other.path.as_str(), other.old_path.as_str()]
                        .into_iter()
                        .any(|other| {
                            other != path
                                && (Path::new(other).starts_with(path)
                                    || Path::new(path).starts_with(other))
                        })
                }),
                "Restore paths that switch between files and directories with jj."
            );
        }
        Ok(Self {
            comparison: Comparison {
                base_id: review.comparison.base_id.clone(),
                head_id: review.comparison.head_id.clone(),
                ..Comparison::default()
            },
            change: change.clone(),
            lines,
        })
    }

    pub fn label(&self) -> String {
        let path = crate::ui::display_path(&self.change.path);
        match &self.lines {
            Some(lines) => {
                let range = if lines.start_line == lines.end_line {
                    lines.start_line.to_string()
                } else {
                    format!("{}–{}", lines.start_line, lines.end_line)
                };
                format!(
                    "Restore {} {range} ({}) in {path}",
                    if lines.start_line == lines.end_line {
                        "line"
                    } else {
                        "lines"
                    },
                    lines.side.label()
                )
            }
            None if self.change.status == "R" => format!(
                "Restore file {path} as {}",
                crate::ui::display_path(&self.change.old_path)
            ),
            None => format!("Restore file {path}"),
        }
    }

    pub(crate) fn apply(self, repo: &Repository) -> Result<()> {
        ensure!(repo.backend == Backend::Jj, "Restore requires Jujutsu.");
        let Self {
            comparison,
            change,
            lines,
        } = self;
        validate_commit_id(&comparison.head_id)?;
        validate_commit_id(&comparison.base_id)?;
        ensure!(
            repo.resolve("@", true)? == comparison.head_id,
            "Working copy changed. Refresh and select the changes again."
        );
        if change.new.kind.is_empty() {
            ensure_path_absent(repo, &change.path)?;
        }
        let fileset = exact_fileset(&change.path)?;
        let mut command = repo.command(true);
        let whole_rename = lines.is_none() && change.status == "R";
        let content;
        if let Some(lines) = lines {
            let old = repo.content(&comparison, &change, true)?;
            let new = repo.content(&comparison, &change, false)?;
            let restored = restore_lines(&change, &lines, &old, &new)?;
            if !restored.is_empty() || !change.old.kind.is_empty() {
                let mut file = tempfile::NamedTempFile::new()?;
                file.write_all(&restored)?;
                command.env("REVIEW_RESTORE_CONTENT", file.path());
                content = Some(file);
            } else {
                command.env_remove("REVIEW_RESTORE_CONTENT");
                content = None;
            }
            let executable = if change.new.kind.is_empty() {
                change.old.executable
            } else {
                change.new.executable
            };
            // The editor writes only into jj's temporary right-hand tree. jj then
            // records the partial restore and updates the working copy itself.
            let script = r#"
target=$1/$REVIEW_RESTORE_PATH
test ! -L "$target"
if [ -n "${REVIEW_RESTORE_CONTENT:-}" ]; then
    mkdir -p -- "${target%/*}"
    cat -- "$REVIEW_RESTORE_CONTENT" > "$target"
    chmod -- "$REVIEW_RESTORE_MODE" "$target"
else
    rm -f -- "$target"
fi
"#;
            let editor = serde_json::to_string(&["sh", "-eu", "-c", script, "review", "$right"])?;
            command
                .args(["--config", &format!("ui.diff-editor={editor}")])
                .args(["--config", "ui.diff-instructions=false"])
                .env("REVIEW_RESTORE_PATH", &change.path)
                .env(
                    "REVIEW_RESTORE_MODE",
                    if executable { "a+x" } else { "a-x" },
                )
                .args(["restore", "--interactive"]);
        } else {
            command.arg("restore");
            content = None;
            if whole_rename {
                ensure_path_absent(repo, &change.old_path)?;
            }
        }
        // The intersection is checked by jj *after* it snapshots the working
        // copy, so a new snapshot cannot become the restore destination.
        command.args([
            "--from",
            &comparison.base_id,
            "--into",
            &format!("@ & {}", comparison.head_id),
            "--restore-descendants",
            "--",
            &fileset,
        ]);
        if whole_rename {
            command.arg(exact_fileset(&change.old_path)?);
        }
        let output = command::output(&mut command, None)?;
        drop(content);
        if !output.status.success() {
            ensure!(
                repo.resolve("@", false)
                    .is_ok_and(|id| id == comparison.head_id),
                "Working copy changed. Refresh and select the changes again."
            );
        }
        ensure!(
            output.status.success(),
            "jj restore: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

fn ensure_path_absent(repo: &Repository, path: &str) -> Result<()> {
    let mut current = repo.root.clone();
    let mut parts = Path::new(path).components().peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if parts.peek().is_some() && metadata.is_dir() => {}
            Ok(_) => anyhow::bail!(
                "Move the existing path {} aside before restoring.",
                current.display()
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("Checking the working copy path."),
        }
    }
    Ok(())
}

fn restore_lines(change: &Change, target: &LineTarget, old: &[u8], new: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !old.contains(&0) && !new.contains(&0),
        "Line restore requires a text file."
    );
    let ranges = changed_line_ranges(change, old, new)?;
    let old_lines: Vec<_> = old.split_inclusive(|byte| *byte == b'\n').collect();
    let new_lines: Vec<_> = new.split_inclusive(|byte| *byte == b'\n').collect();
    let selected = |index| (target.start_line..=target.end_line).contains(&(index + 1));
    let mut result = Vec::new();
    let mut cursor = 0;
    for (before, after) in ranges {
        result.extend_from_slice(&new_lines[cursor..after.start]);
        cursor = after.end;
        if before.len() == after.len() {
            // A balanced replacement pairs lines in order. For other blocks,
            // the selected side controls which lines are reinserted or removed.
            for (old, new) in before.zip(after) {
                let index = if target.side == DiffSide::Old {
                    old
                } else {
                    new
                };
                result.push(if selected(index) {
                    old_lines[old]
                } else {
                    new_lines[new]
                });
            }
        } else if target.side == DiffSide::Old {
            result.extend(
                before
                    .filter(|index| selected(*index))
                    .map(|index| old_lines[index]),
            );
            result.extend_from_slice(&new_lines[after]);
        } else {
            result.extend(
                after
                    .filter(|index| !selected(*index))
                    .map(|index| new_lines[index]),
            );
        }
    }
    result.extend_from_slice(&new_lines[cursor..]);
    ensure!(
        result != new_lines,
        "The selection contains no changed lines to restore."
    );
    ensure!(
        result
            .iter()
            .rev()
            .skip(1)
            .all(|line| line.ends_with(b"\n")),
        "Restoring this range would join lines at a missing final newline."
    );
    Ok(result.concat())
}
