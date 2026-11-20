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
    pub(crate) fn new(
        review: &Review,
        backend: Backend,
        lines: Option<LineTarget>,
    ) -> Result<Self> {
        ensure!(review.gitlab.is_none(), "Restore requires a local review.");
        ensure!(
            review.comparison.working_tree.is_none(),
            "Use Git to restore files in a working-tree review."
        );
        ensure!(
            backend != Backend::Jj || review.comparison.head_ref == "@",
            "Jujutsu restore requires a review with @ as the head."
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
        let restore = Self {
            comparison: Comparison {
                base_id: review.comparison.base_id.clone(),
                head_id: review.comparison.head_id.clone(),
                ..Comparison::default()
            },
            change: change.clone(),
            lines,
        };
        for path in restore.paths() {
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
                "Restore paths that switch between files and directories outside Review."
            );
        }
        Ok(restore)
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
        validate_commit_id(&self.comparison.head_id)?;
        validate_commit_id(&self.comparison.base_id)?;
        match repo.backend {
            Backend::Git => crate::git_restore::apply(repo, &self),
            Backend::Jj => self.apply_jj(repo),
        }
    }

    pub(crate) fn comparison(&self) -> &Comparison {
        &self.comparison
    }

    pub(crate) fn change(&self) -> &Change {
        &self.change
    }

    pub(crate) fn paths(&self) -> impl Iterator<Item = &str> {
        let old_path = (self.lines.is_none() && self.change.status == "R")
            .then_some(self.change.old_path.as_str());
        std::iter::once(self.change.path.as_str()).chain(old_path)
    }

    pub(crate) fn check_missing_paths(&self, repo: &Repository) -> Result<()> {
        let destination_exists = !self.change.new.kind.is_empty();
        for path in self.paths().skip(usize::from(destination_exists)) {
            ensure_path_absent(repo, path)?;
        }
        Ok(())
    }

    pub(crate) fn partial(&self, repo: &Repository) -> Result<Option<PartialRestore>> {
        let Some(lines) = &self.lines else {
            return Ok(None);
        };
        let change = &self.change;
        let old = repo.content(&self.comparison, change, true)?;
        let new = repo.content(&self.comparison, change, false)?;
        let restored = restore_lines(change, lines, &old, &new)?;
        let content = (!restored.is_empty() || !change.old.kind.is_empty()).then_some(restored);
        let executable = if change.new.kind.is_empty() {
            change.old.executable
        } else {
            change.new.executable
        };
        Ok(Some(PartialRestore {
            content,
            executable,
        }))
    }

    fn apply_jj(self, repo: &Repository) -> Result<()> {
        let comparison = &self.comparison;
        let change = &self.change;
        ensure!(
            repo.resolve("@", true)? == comparison.head_id,
            "Working copy changed. Refresh and select the changes again."
        );
        self.check_missing_paths(repo)?;
        let mut command = repo.command(true);
        let content;
        if let Some(partial) = self.partial(repo)? {
            command::require("sh", &repo.root, None)?;
            if let Some(restored) = partial.content {
                for program in ["mkdir", "cat", "chmod"] {
                    command::require(program, &repo.root, None)?;
                }
                let mut file = tempfile::NamedTempFile::new()?;
                file.write_all(&restored)?;
                command.env("REVIEW_RESTORE_CONTENT", file.path());
                content = Some(file);
            } else {
                command::require("rm", &repo.root, None)?;
                command.env_remove("REVIEW_RESTORE_CONTENT");
                content = None;
            }
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
                    if partial.executable { "a+x" } else { "a-x" },
                )
                .args(["restore", "--interactive"]);
        } else {
            command.arg("restore");
            content = None;
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
        ]);
        command.args(
            self.paths()
                .map(exact_fileset)
                .collect::<Result<Vec<_>>>()?,
        );
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

pub(crate) struct PartialRestore {
    pub content: Option<Vec<u8>>,
    pub executable: bool,
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
