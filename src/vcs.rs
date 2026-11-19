use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Seek},
    iter::Sum,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Git,
    Jj,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub object_id: String,
    pub kind: String,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub old_path: String,
    pub status: String,
    pub old: Entry,
    pub new: Entry,
}

impl Change {
    pub fn fingerprint(&self) -> String {
        blake3::hash(&serde_json::to_vec(self).expect("change is serializable"))
            .to_hex()
            .to_string()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Comparison {
    pub base_ref: String,
    pub head_ref: String,
    pub base_id: String,
    pub head_id: String,
    #[serde(default)]
    pub base_title: Option<String>,
    #[serde(default)]
    pub head_title: Option<String>,
    #[serde(default)]
    pub base_change_id: Option<String>,
    #[serde(default)]
    pub head_change_id: Option<String>,
    #[serde(default)]
    pub mode: ComparisonMode,
    #[serde(default)]
    pub line_counts: Option<LineCounts>,
    #[serde(default)]
    pub file_line_counts: Option<BTreeMap<String, LineCounts>>,
    #[serde(default)]
    pub file_commit_ids: Option<BTreeMap<String, Vec<String>>>,
    #[serde(default)]
    pub revisions: Option<BTreeMap<String, Revision>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub commit_id: String,
    pub parents: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonMode {
    #[default]
    MergeBase,
    Direct,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineCounts {
    pub added: u64,
    pub removed: u64,
}

impl Sum for LineCounts {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |mut total, counts| {
            total.added += counts.added;
            total.removed += counts.removed;
            total
        })
    }
}

#[derive(Clone, Debug)]
pub struct Repository {
    pub root: PathBuf,
    pub backend: Backend,
}

#[derive(Deserialize)]
struct CommitFiles {
    id: String,
    #[serde(flatten)]
    revision: Revision,
    paths: Vec<String>,
}

impl Repository {
    pub fn remotes(&self) -> Result<Vec<(String, String)>> {
        let git = match self.backend {
            Backend::Git => self.clone(),
            Backend::Jj => {
                let git_dir = String::from_utf8(self.run(&["git", "root"], false)?)?;
                Self {
                    root: PathBuf::from(git_dir.trim_end_matches('\n')),
                    backend: Backend::Git,
                }
            }
        };
        // Read configured URLs before insteadOf expansion so API host/project detection is stable.
        let names = String::from_utf8(git.run(&["remote"], false)?)?;
        names
            .lines()
            .map(|name| {
                validate_remote(name)?;
                let url = String::from_utf8(
                    git.run(&["config", "--get", &format!("remote.{name}.url")], false)?,
                )?;
                Ok((name.to_owned(), url.trim_end_matches('\n').to_owned()))
            })
            .collect()
    }

    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        validate_remote(name)?;
        let args = match self.backend {
            Backend::Git => vec!["remote", "add", name, url],
            Backend::Jj => vec!["git", "remote", "add", name, url],
        };
        self.run(&args, false)?;
        Ok(())
    }

    pub fn fetch(&self, remote: &str, branches: &[&str]) -> Result<()> {
        validate_remote(remote)?;
        let mut command = self.command(false);
        match self.backend {
            Backend::Git => {
                command.args(["fetch", "--no-tags", remote]);
                for branch in branches {
                    validate_branch(branch)?;
                    command.arg(format!(
                        "+refs/heads/{branch}:refs/remotes/{remote}/{branch}"
                    ));
                }
            }
            Backend::Jj => {
                command.args([
                    "--config",
                    "git.abandon-unreachable-commits=false",
                    "git",
                    "fetch",
                    "--remote",
                    &format!("exact:{remote}"),
                ]);
                for branch in branches {
                    validate_branch(branch)?;
                    command.args(["--branch", &format!("exact:{branch}")]);
                }
            }
        }
        let output = crate::command::output(&mut command, None)?;
        ensure!(
            output.status.success(),
            "fetching {remote}: {}. Check repository SSH/HTTPS credentials and branch access",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    pub fn remote_head(&self, remote: &str, branch: &str) -> Result<String> {
        validate_remote(remote)?;
        validate_branch(branch)?;
        let revision = match self.backend {
            Backend::Git => format!("refs/remotes/{remote}/{branch}"),
            Backend::Jj => format!(
                "{}@{}",
                serde_json::to_string(branch)?,
                serde_json::to_string(remote)?
            ),
        };
        self.resolve(&revision, false)
    }

    pub fn ensure_clean(&self) -> Result<()> {
        if self.backend == Backend::Jj && self.root.join(".git").exists() {
            let git = Self {
                root: self.root.clone(),
                backend: Backend::Git,
            };
            let output = git
                .command(false)
                .args(["diff", "--cached", "--quiet", "--no-ext-diff", "--"])
                .output()?;
            ensure!(
                output.status.success(),
                "Git index contains staged changes or could not be checked. Save the staged work before preparing an MR with jj"
            );
        }
        let clean = match self.backend {
            Backend::Git => self
                .run(
                    &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
                    false,
                )?
                .is_empty(),
            Backend::Jj => {
                self.run(&["log", "--no-graph", "-r", "@", "-T", "empty"], true)? == b"true"
            }
        };
        ensure!(
            clean,
            "Working copy contains local changes. Save your work and prepare a clean working copy before switching to an MR"
        );
        Ok(())
    }

    pub fn checkout_review(
        &self,
        branch: &str,
        head: &str,
        remote: &str,
        source: &str,
    ) -> Result<()> {
        validate_branch(branch)?;
        validate_commit_id(head)?;
        self.ensure_clean()?;
        match self.backend {
            Backend::Git => {
                self.run(&["check-ref-format", "--branch", branch], false)?;
                let existing = self
                    .command(false)
                    .args([
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{branch}"),
                    ])
                    .output()?
                    .status;
                if existing.success() {
                    let local = self.resolve(&format!("refs/heads/{branch}"), false)?;
                    ensure!(
                        self.merge_base(&local, head).ok().as_deref() == Some(&local),
                        "Local branch {branch:?} has commits absent from the MR. Preserve or rename it before reviewing this MR"
                    );
                    let upstream = String::from_utf8(self.run(
                        &[
                            "for-each-ref",
                            "--format=%(upstream)",
                            &format!("refs/heads/{branch}"),
                        ],
                        false,
                    )?)?;
                    ensure!(
                        upstream.trim().is_empty()
                            || upstream.trim() == format!("refs/remotes/{remote}/{source}"),
                        "Local branch {branch:?} tracks another source. Rename it before reviewing this MR"
                    );
                    self.run(&["switch", "--no-overwrite-ignore", branch], false)?;
                    self.run(
                        &[
                            "merge",
                            "--ff-only",
                            "--no-edit",
                            "--no-overwrite-ignore",
                            head,
                        ],
                        false,
                    )?;
                } else {
                    ensure!(
                        existing.code() == Some(1),
                        "checking local branch {branch:?} failed"
                    );
                    self.run(
                        &["switch", "--no-overwrite-ignore", "--create", branch, head],
                        false,
                    )?;
                    self.run(
                        &[
                            "branch",
                            "--set-upstream-to",
                            &format!("{remote}/{source}"),
                            branch,
                        ],
                        false,
                    )?;
                }
                ensure!(
                    self.resolve("HEAD", false)? == head,
                    "Working copy no longer matches the MR head; retry"
                );
            }
            Backend::Jj => {
                self.check_checkout_paths(head)?;
                // Keep the reviewed commit immutable; edits belong to a fresh child change.
                self.run(&["new", head], true)?;
                ensure!(
                    self.resolve("@-", false)? == head,
                    "Working copy no longer follows the MR head; retry"
                );
            }
        }
        Ok(())
    }

    fn check_checkout_paths(&self, head: &str) -> Result<()> {
        let comparison = self.resolve_snapshot("@", head, ComparisonMode::Direct, true)?;
        let changes = self.changes(&comparison)?;
        let tracked: BTreeSet<_> = changes
            .iter()
            .filter(|change| !change.old.kind.is_empty())
            .map(|change| self.root.join(&change.old_path))
            .collect();
        for change in changes.iter().filter(|change| !change.new.kind.is_empty()) {
            let mut path = self.root.clone();
            for part in Path::new(&change.path).components() {
                path.push(part);
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(error) => return Err(error).context("checking MR checkout paths"),
                };
                if metadata.is_dir() {
                    if path == self.root.join(&change.path) {
                        ensure_tracked_contents(&path, &tracked)?;
                    }
                } else {
                    ensure!(
                        tracked.contains(&path),
                        "Untracked or ignored path {} would be overwritten by the MR. Move it aside before switching",
                        path.display()
                    );
                    // A tracked file/symlink becoming a directory has no existing children to inspect.
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn discover(path: &Path, backend: Option<Backend>) -> Result<Self> {
        Self::find(path, backend)?.context("no Git or Jujutsu repository found")
    }

    pub fn find(path: &Path, backend: Option<Backend>) -> Result<Option<Self>> {
        let path = path
            .canonicalize()
            .context("opening repository directory")?;
        let detected = path.ancestors().find_map(|parent| {
            if parent.join(".jj").is_dir() {
                Some(Backend::Jj)
            } else if parent.join(".git").exists() {
                Some(Backend::Git)
            } else {
                None
            }
        });
        let Some(detected) = detected else {
            return Ok(None);
        };
        let mut repo = Self {
            root: path,
            backend: backend.unwrap_or(detected),
        };
        let args = match repo.backend {
            Backend::Git => vec!["rev-parse", "--show-toplevel"],
            Backend::Jj => vec!["root"],
        };
        let root = String::from_utf8(repo.run(&args, false)?)?;
        repo.root = PathBuf::from(root.strip_suffix('\n').unwrap_or(&root)).canonicalize()?;
        Ok(Some(repo))
    }

    pub fn snapshot(&self, base: &str, head: &str) -> Result<Comparison> {
        self.snapshot_with_mode(base, head, ComparisonMode::MergeBase, true)
    }

    pub(crate) fn snapshot_commits(&self, base_id: &str, head_id: &str) -> Result<Comparison> {
        self.snapshot_with_mode(base_id, head_id, ComparisonMode::Direct, false)
    }

    pub fn snapshot_expression(&self, expression: &str) -> Result<Comparison> {
        let expression = expression.trim();
        ensure!(
            !expression.is_empty(),
            "comparison expression cannot be empty"
        );
        let (base, head, mode) = match range_separator(expression, self.backend)? {
            Some((index, length)) => {
                ensure!(
                    length == 2 || self.backend == Backend::Git,
                    "use BASE..HEAD for a Jujutsu comparison"
                );
                (
                    expression[..index].trim(),
                    expression[index + length..].trim(),
                    if length == 3 {
                        ComparisonMode::MergeBase
                    } else {
                        ComparisonMode::Direct
                    },
                )
            }
            None => (expression, "", ComparisonMode::Direct),
        };
        let current = match self.backend {
            Backend::Git => "HEAD",
            Backend::Jj => "@",
        };
        self.snapshot_with_mode(
            if base.is_empty() { current } else { base },
            if head.is_empty() { current } else { head },
            mode,
            true,
        )
    }

    pub fn refresh(&self, comparison: &Comparison) -> Result<Comparison> {
        self.snapshot_with_mode(
            &comparison.base_ref,
            &comparison.head_ref,
            comparison.mode,
            true,
        )
    }

    pub fn refresh_if_changed(&self, comparison: &Comparison) -> Result<Option<Comparison>> {
        let mut updated = self.resolve_snapshot(
            &comparison.base_ref,
            &comparison.head_ref,
            comparison.mode,
            true,
        )?;
        if updated.base_id == comparison.base_id && updated.head_id == comparison.head_id {
            return Ok(None);
        }
        self.fill_change_ids(&mut updated)?;
        self.fill_commit_titles(&mut updated)?;
        self.fill_line_counts(&mut updated)?;
        self.fill_file_commit_ids(&mut updated)?;
        Ok(Some(updated))
    }

    fn snapshot_with_mode(
        &self,
        base: &str,
        head: &str,
        mode: ComparisonMode,
        snapshot_working_copy: bool,
    ) -> Result<Comparison> {
        let mut comparison = self.resolve_snapshot(base, head, mode, snapshot_working_copy)?;
        self.fill_change_ids(&mut comparison)?;
        self.fill_commit_titles(&mut comparison)?;
        self.fill_line_counts(&mut comparison)?;
        self.fill_file_commit_ids(&mut comparison)?;
        Ok(comparison)
    }

    fn resolve_snapshot(
        &self,
        base: &str,
        head: &str,
        mode: ComparisonMode,
        snapshot_working_copy: bool,
    ) -> Result<Comparison> {
        let head_id = self.resolve(head, snapshot_working_copy)?;
        let base_tip = self.resolve(base, false)?;
        let base_id = match mode {
            ComparisonMode::Direct => base_tip,
            ComparisonMode::MergeBase => self.merge_base(&base_tip, &head_id)?,
        };
        Ok(Comparison {
            base_ref: base.to_owned(),
            head_ref: head.to_owned(),
            base_id,
            head_id,
            base_title: None,
            head_title: None,
            base_change_id: None,
            head_change_id: None,
            mode,
            line_counts: None,
            file_line_counts: None,
            file_commit_ids: None,
            revisions: None,
        })
    }

    pub fn fill_change_ids(&self, comparison: &mut Comparison) -> Result<bool> {
        if self.backend != Backend::Jj {
            return Ok(false);
        }
        let mut updated = false;
        for (commit_id, change_id) in [
            (&comparison.base_id, &mut comparison.base_change_id),
            (&comparison.head_id, &mut comparison.head_change_id),
        ] {
            if change_id.is_none() {
                *change_id = Some(String::from_utf8(self.run(
                    &["log", "--no-graph", "-r", commit_id, "-T", "change_id"],
                    false,
                )?)?);
                updated = true;
            }
        }
        Ok(updated)
    }

    pub fn fill_commit_titles(&self, comparison: &mut Comparison) -> Result<bool> {
        let mut updated = false;
        for (commit_id, title) in [
            (&comparison.base_id, &mut comparison.base_title),
            (&comparison.head_id, &mut comparison.head_title),
        ] {
            if title.is_none() {
                *title = Some(self.commit_title(commit_id)?);
                updated = true;
            }
        }
        Ok(updated)
    }

    fn commit_title(&self, commit_id: &str) -> Result<String> {
        let message = match self.backend {
            Backend::Git => self.run(
                &[
                    "show",
                    "--no-patch",
                    "--no-show-signature",
                    "--format=%B",
                    "--encoding=UTF-8",
                    commit_id,
                    "--",
                ],
                false,
            )?,
            Backend::Jj => self.run(
                &["log", "--no-graph", "-r", commit_id, "-T", "description"],
                false,
            )?,
        };
        Ok(String::from_utf8_lossy(&message)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned())
    }

    pub fn fill_file_commit_ids(&self, comparison: &mut Comparison) -> Result<bool> {
        if comparison.file_commit_ids.is_some()
            && let Some(revisions) = &mut comparison.revisions
        {
            let mut updated = false;
            for revision in revisions.values_mut() {
                if revision.title.is_none() {
                    revision.title = Some(self.commit_title(&revision.commit_id)?);
                    updated = true;
                }
            }
            return Ok(updated);
        }
        let (base, head) = (&comparison.base_id, &comparison.head_id);
        let jj = Self {
            root: self.root.clone(),
            backend: Backend::Jj,
        };
        let commits = match jj.run(
            &[
                "log",
                "--no-graph",
                "--reversed",
                "-r",
                &format!("({base}..{head}) | ({head}..{base})"),
                "-T",
                r#"json({"id" => stringify(change_id.shortest(3)), "commit_id" => commit_id, "parents" => parents.map(|parent| parent.commit_id()), "title" => description.first_line(), "paths" => diff.files().map(|file| file.path())}) ++ "\n""#,
            ],
            false,
        ) {
            Ok(output) => serde_json::Deserializer::from_slice(&output)
                .into_iter::<CommitFiles>()
                .collect::<std::result::Result<Vec<_>, _>>()?,
            Err(error) if self.backend == Backend::Jj => return Err(error),
            Err(_) => parse_git_file_commits(&self.run(
                &[
                    "log",
                    "--reverse",
                    "--topo-order",
                    "--no-color",
                    "--no-show-signature",
                    "--no-notes",
                    "--format=%h %H %P%x00%B%x00",
                    "--encoding=UTF-8",
                    "--abbrev=8",
                    "--raw",
                    "-z",
                    "--no-renames",
                    "--diff-merges=combined",
                    "--root",
                    "--no-ext-diff",
                    "--no-textconv",
                    &format!("{base}...{head}"),
                    "--",
                ],
                false,
            )?)?,
        };
        let mut files = BTreeMap::new();
        for change in self.changes(comparison)? {
            let mut ids = Vec::new();
            for commit in &commits {
                if commit
                    .paths
                    .iter()
                    .any(|path| path == &change.path || path == &change.old_path)
                    && !ids.contains(&commit.id)
                {
                    ids.push(commit.id.clone());
                }
            }
            files.insert(change.path, ids);
        }
        comparison.file_commit_ids = Some(files);
        comparison.revisions = Some(
            commits
                .into_iter()
                .map(|commit| (commit.id, commit.revision))
                .collect(),
        );
        Ok(true)
    }

    pub fn revision_comparison(&self, first: &Revision, last: &Revision) -> Result<Comparison> {
        let base_id = match first.parents.first() {
            Some(parent) => parent.clone(),
            None => match self.backend {
                Backend::Jj => self.resolve("root()", false)?,
                Backend::Git => {
                    String::from_utf8(self.run(&["hash-object", "-t", "tree", "--stdin"], false)?)?
                        .trim()
                        .to_owned()
                }
            },
        };
        Ok(Comparison {
            base_ref: base_id.clone(),
            head_ref: last.commit_id.clone(),
            base_id,
            head_id: last.commit_id.clone(),
            mode: ComparisonMode::Direct,
            ..Comparison::default()
        })
    }

    fn merge_base(&self, base: &str, head: &str) -> Result<String> {
        let ancestors = match self.backend {
            Backend::Git => {
                String::from_utf8(self.run(&["merge-base", "--all", base, head], false)?)?
            }
            Backend::Jj => {
                let expression = format!("heads(::{base} & ::{head})");
                String::from_utf8(self.run(
                    &[
                        "log",
                        "--no-graph",
                        "-r",
                        &expression,
                        "-T",
                        "commit_id ++ \"\\n\"",
                    ],
                    false,
                )?)?
            }
        };
        let bases: Vec<_> = ancestors.lines().collect();
        ensure!(
            bases.len() == 1,
            "comparison requires exactly one merge base, found {}",
            bases.len()
        );
        Ok(bases[0].to_owned())
    }

    pub fn changes(&self, comparison: &Comparison) -> Result<Vec<Change>> {
        let mut changes = match self.backend {
            Backend::Git => {
                let output = self.run(
                    &[
                        "diff",
                        "--raw",
                        "--no-abbrev",
                        "-z",
                        "--find-renames",
                        "--no-ext-diff",
                        "--no-textconv",
                        &comparison.base_id,
                        &comparison.head_id,
                        "--",
                    ],
                    false,
                )?;
                parse_git_changes(&output)?
            }
            Backend::Jj => {
                let template = r#"json({"path" => path, "old_path" => source.path(), "status" => status_char, "old" => {"object_id" => stringify(source.normal_value()), "kind" => source.file_type(), "executable" => source.executable()}, "new" => {"object_id" => stringify(target.normal_value()), "kind" => target.file_type(), "executable" => target.executable()}}) ++ "\n""#;
                let output = self.run(
                    &[
                        "diff",
                        "--from",
                        &comparison.base_id,
                        "--to",
                        &comparison.head_id,
                        "-T",
                        template,
                    ],
                    false,
                )?;
                serde_json::Deserializer::from_slice(&output)
                    .into_iter::<Change>()
                    .collect::<std::result::Result<Vec<_>, _>>()?
            }
        };
        for change in &mut changes {
            for old in [true, false] {
                let entry = if old { &change.old } else { &change.new };
                if entry.kind == "conflict" {
                    let hash = blake3::hash(&self.content(comparison, change, old)?)
                        .to_hex()
                        .to_string();
                    let entry = if old {
                        &mut change.old
                    } else {
                        &mut change.new
                    };
                    entry.object_id = hash;
                }
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(changes)
    }

    pub fn line_counts(&self, comparison: &Comparison) -> Result<LineCounts> {
        let args: &[&str] = match self.backend {
            Backend::Git => &[
                "diff",
                "--shortstat",
                "--color=never",
                "--find-renames",
                "--no-ext-diff",
                "--no-textconv",
                &comparison.base_id,
                &comparison.head_id,
                "--",
            ],
            Backend::Jj => &[
                "diff",
                "--from",
                &comparison.base_id,
                "--to",
                &comparison.head_id,
                "--stat",
            ],
        };
        let output = String::from_utf8(self.run(args, false)?)?;
        let mut counts = LineCounts::default();
        for field in output.lines().last().unwrap_or_default().split(',') {
            let count = if field.ends_with("(+)") {
                &mut counts.added
            } else if field.ends_with("(-)") {
                &mut counts.removed
            } else {
                continue;
            };
            *count = field
                .split_whitespace()
                .next()
                .context("missing changed line count")?
                .parse()
                .context("invalid changed line count")?;
        }
        Ok(counts)
    }

    pub fn file_line_counts(
        &self,
        comparison: &Comparison,
    ) -> Result<BTreeMap<String, LineCounts>> {
        let output = match self.backend {
            Backend::Git => self.run(
                &[
                    "diff",
                    "--numstat",
                    "-z",
                    "--find-renames",
                    "--no-ext-diff",
                    "--no-textconv",
                    &comparison.base_id,
                    &comparison.head_id,
                    "--",
                ],
                false,
            )?,
            Backend::Jj => {
                let changes = self.changes(comparison)?;
                let mut errors = tempfile::tempfile()?;
                let mut child = self
                    .command(false)
                    .args([
                        "diff",
                        "--from",
                        &comparison.base_id,
                        "--to",
                        &comparison.head_id,
                        "--git",
                        "--context",
                        "0",
                    ])
                    .stdout(Stdio::piped())
                    .stderr(errors.try_clone()?)
                    .spawn()
                    .context("running jj diff for line counts")?;
                let counts = parse_jj_line_counts(
                    BufReader::new(child.stdout.take().expect("diff stdout is piped")),
                    &changes,
                );
                if counts.is_err() {
                    let _ = child.kill();
                }
                let status = child.wait()?;
                let counts = counts?;
                errors.rewind()?;
                let mut stderr = String::new();
                errors.take(64 * 1024).read_to_string(&mut stderr)?;
                ensure!(status.success(), "jj diff: {}", stderr.trim());
                return Ok(counts);
            }
        };
        parse_numstat(&output)
    }

    pub fn fill_line_counts(&self, comparison: &mut Comparison) -> Result<bool> {
        let mut updated = false;
        if comparison.file_line_counts.is_none() {
            comparison.file_line_counts = Some(self.file_line_counts(comparison)?);
            updated = true;
        }
        if comparison.line_counts.is_none() {
            comparison.line_counts = comparison
                .file_line_counts
                .as_ref()
                .map(|files| files.values().copied().sum());
            updated = true;
        }
        Ok(updated)
    }

    pub fn content(&self, comparison: &Comparison, change: &Change, old: bool) -> Result<Vec<u8>> {
        let entry = if old { &change.old } else { &change.new };
        if entry.kind.is_empty() {
            return Ok(Vec::new());
        }
        if entry.kind == "git-submodule" {
            return Ok(format!("Submodule commit {}\n", entry.object_id).into_bytes());
        }
        match self.backend {
            Backend::Git => self.run(&["cat-file", "blob", &entry.object_id], false),
            Backend::Jj => {
                let revision = if old {
                    &comparison.base_id
                } else {
                    &comparison.head_id
                };
                let path = if old { &change.old_path } else { &change.path };
                let fileset = exact_fileset(path)?;
                // `jj file show` skips symlinks. An addition from the empty root
                // exposes the stored target without following a working-tree link.
                if entry.kind == "symlink" {
                    let patch = self.run(
                        &[
                            "diff",
                            "--from",
                            "root()",
                            "--to",
                            revision,
                            "--git",
                            "--context",
                            "0",
                            &fileset,
                        ],
                        false,
                    )?;
                    let mut content = Vec::new();
                    let mut in_hunk = false;
                    for line in patch.split_inclusive(|byte| *byte == b'\n') {
                        if line.starts_with(b"@@ ") {
                            in_hunk = true;
                        } else if in_hunk && line.starts_with(b"+") {
                            content.extend_from_slice(&line[1..]);
                        } else if in_hunk && line.starts_with(b"\\ No newline") {
                            content.pop();
                        }
                    }
                    return Ok(content);
                }
                self.run(&["file", "show", "-r", revision, &fileset], false)
            }
        }
    }

    pub(crate) fn resolve(&self, revision: &str, snapshot: bool) -> Result<String> {
        let output = match self.backend {
            Backend::Git => self.run(
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &if revision.starts_with(":/") {
                        revision.to_owned()
                    } else {
                        format!("{revision}^{{commit}}")
                    },
                ],
                false,
            )?,
            Backend::Jj => self.run(
                &[
                    "log",
                    "--no-graph",
                    "-r",
                    revision,
                    "-T",
                    "commit_id ++ \"\\n\"",
                ],
                snapshot,
            )?,
        };
        let text = String::from_utf8(output)?;
        let revisions: Vec<_> = text.lines().collect();
        ensure!(
            revisions.len() == 1,
            "{revision:?} must resolve to exactly one revision, found {}",
            revisions.len()
        );
        Ok(revisions[0].to_owned())
    }

    pub(crate) fn command(&self, snapshot: bool) -> Command {
        let program = match self.backend {
            Backend::Git => "git",
            Backend::Jj => "jj",
        };
        let mut command = Command::new(program);
        command.current_dir(&self.root).arg("--no-pager");
        match self.backend {
            Backend::Git => {
                command
                    .env("LC_ALL", "C")
                    .env("GIT_OPTIONAL_LOCKS", "0")
                    .env("GIT_TERMINAL_PROMPT", "0");
            }
            Backend::Jj => {
                command.args(["--color", "never"]);
                if !snapshot {
                    command.arg("--ignore-working-copy");
                }
            }
        }
        command
    }

    fn run(&self, args: &[&str], snapshot: bool) -> Result<Vec<u8>> {
        let mut command = self.command(snapshot);
        let program = command.get_program().to_string_lossy().into_owned();
        let output = command
            .args(args)
            .output()
            .with_context(|| format!("running {program}; is it installed?"))?;
        ensure!(
            output.status.success(),
            "{program} {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output.stdout)
    }
}

pub(crate) fn exact_fileset(path: &str) -> Result<String> {
    Ok(format!("root-file:{}", serde_json::to_string(path)?))
}

fn ensure_tracked_contents(path: &Path, tracked: &BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            ensure_tracked_contents(&entry.path(), tracked)?;
        } else {
            ensure!(
                tracked.contains(&entry.path()),
                "Untracked or ignored path {} would be overwritten by the MR. Move it aside before switching",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn validate_remote(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.starts_with('-') && !name.chars().any(char::is_whitespace),
        "invalid remote name {name:?}"
    );
    Ok(())
}

fn validate_branch(branch: &str) -> Result<()> {
    ensure!(
        !branch.is_empty()
            && !branch.starts_with('-')
            && !branch.chars().any(|ch| ch.is_control()
                || matches!(ch, ' ' | ':' | '*' | '?' | '[' | '\\' | '~' | '^'))
            && !branch.contains("..")
            && !branch.contains("@{"),
        "invalid branch name {branch:?}"
    );
    Ok(())
}

pub(crate) fn validate_commit_id(id: &str) -> Result<()> {
    ensure!(
        matches!(id.len(), 40 | 64) && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "GitLab returned an invalid commit SHA"
    );
    Ok(())
}

fn range_separator(expression: &str, backend: Backend) -> Result<Option<(usize, usize)>> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut separator = None;
    let mut characters = expression.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' && delimiter == '"' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' if backend == Backend::Jj => {
                quote = Some(character);
            }
            '(' | '[' if backend == Backend::Jj => {
                depth += 1;
            }
            '{' if backend == Backend::Jj
                || depth > 0
                || (index > 0 && matches!(expression.as_bytes()[index - 1], b'@' | b'^')) =>
            {
                depth += 1;
            }
            ')' | ']' | '}' if backend == Backend::Jj || character == '}' => {
                depth = depth.saturating_sub(1);
            }
            ':' if backend == Backend::Git
                && depth == 0
                && expression[index..].starts_with(":/") =>
            {
                // A Git message-search revision consumes the remaining text as a regex.
                break;
            }
            '.' if depth == 0 && characters.next_if(|(_, next)| *next == '.').is_some() => {
                ensure!(
                    separator.is_none(),
                    "comparison requires exactly two endpoints"
                );
                let length = if characters.next_if(|(_, next)| *next == '.').is_some() {
                    3
                } else {
                    2
                };
                separator = Some((index, length));
            }
            _ => {}
        }
    }
    Ok(separator)
}

fn parse_git_changes(output: &[u8]) -> Result<Vec<Change>> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut changes = Vec::new();
    while let Some(header) = fields.next() {
        let header = std::str::from_utf8(header)?;
        let parts: Vec<_> = header.trim_start_matches(':').split_whitespace().collect();
        ensure!(parts.len() == 5, "invalid Git raw diff header");
        let status = parts[4].get(..1).context("missing change status")?;
        let old_path =
            std::str::from_utf8(fields.next().context("missing Git diff path")?)?.to_owned();
        let path = if status == "R" || status == "C" {
            std::str::from_utf8(fields.next().context("missing rename destination")?)?.to_owned()
        } else {
            old_path.clone()
        };
        changes.push(Change {
            path,
            old_path,
            status: status.to_owned(),
            old: git_entry(parts[0], parts[2])?,
            new: git_entry(parts[1], parts[3])?,
        });
    }
    Ok(changes)
}

fn parse_git_file_commits(output: &[u8]) -> Result<Vec<CommitFiles>> {
    let mut fields = output.split(|byte| *byte == 0);
    let mut commits = Vec::<CommitFiles>::new();
    while let Some(field) = fields.find(|field| !field.is_empty()) {
        let header = std::str::from_utf8(field)?.trim_start_matches('\n');
        if header.starts_with(':') {
            let path = std::str::from_utf8(fields.next().context("missing Git history path")?)?;
            commits
                .last_mut()
                .context("missing Git history commit")?
                .paths
                .push(path.to_owned());
        } else {
            let mut ids = header.split_whitespace();
            let message = fields.next().context("missing Git history message")?;
            commits.push(CommitFiles {
                id: ids.next().context("missing Git history ID")?.to_owned(),
                revision: Revision {
                    commit_id: ids.next().context("missing Git history commit")?.to_owned(),
                    parents: ids.map(str::to_owned).collect(),
                    title: Some(
                        String::from_utf8_lossy(message)
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_owned(),
                    ),
                },
                paths: Vec::new(),
            });
        }
    }
    Ok(commits)
}

fn parse_numstat(output: &[u8]) -> Result<BTreeMap<String, LineCounts>> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut files = BTreeMap::<String, LineCounts>::new();
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, |byte| *byte == b'\t');
        let added = parts.next().context("missing added line count")?;
        let removed = parts.next().context("missing removed line count")?;
        let mut path = parts.next().context("missing diff statistics path")?;
        if path.is_empty() {
            fields.next().context("missing rename source path")?;
            path = fields.next().context("missing rename target path")?;
        }
        let counts = files
            .entry(std::str::from_utf8(path)?.to_owned())
            .or_default();
        if added != b"-" && removed != b"-" {
            counts.added += std::str::from_utf8(added)?
                .parse::<u64>()
                .context("invalid added line count")?;
            counts.removed += std::str::from_utf8(removed)?
                .parse::<u64>()
                .context("invalid removed line count")?;
        }
    }
    Ok(files)
}

fn parse_jj_line_counts(
    mut reader: impl BufRead,
    changes: &[Change],
) -> Result<BTreeMap<String, LineCounts>> {
    let headers: BTreeMap<_, _> = changes
        .iter()
        .map(|change| {
            (
                format!("diff --git a/{} b/{}\n", change.old_path, change.path).into_bytes(),
                change,
            )
        })
        .collect();
    let mut files: BTreeMap<_, _> = changes
        .iter()
        .map(|change| (change.path.clone(), LineCounts::default()))
        .collect();
    let mut line = Vec::new();
    let mut current: Option<&Change> = None;
    let mut metadata = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.starts_with(b"diff --git ") {
            // jj writes paths literally, including embedded newlines. Match the
            // complete header against the structured diff's paths.
            let change = loop {
                if let Some(change) = headers.get(&line) {
                    break *change;
                }
                ensure!(
                    reader.read_until(b'\n', &mut line)? > 0,
                    "missing Jujutsu diff header"
                );
            };
            current = Some(change);
            metadata = vec![
                format!("--- a/{}\n", change.old_path),
                format!("+++ b/{}\n", change.path),
                format!("rename from {}\n", change.old_path),
                format!("rename to {}\n", change.path),
                format!("copy from {}\n", change.old_path),
                format!("copy to {}\n", change.path),
                format!(
                    "Binary files a/{} and b/{} differ\n",
                    change.old_path, change.path
                ),
            ];
            continue;
        }
        if let Some(header) = metadata
            .iter()
            .find(|header| header.as_bytes().starts_with(&line))
        {
            let start = line.len();
            line.resize(header.len(), 0);
            reader.read_exact(&mut line[start..])?;
            ensure!(line == header.as_bytes(), "invalid Jujutsu diff metadata");
            continue;
        }
        if line.starts_with(b"@@ ") {
            let change = current.context("missing Jujutsu diff path")?;
            let counts = files.get_mut(&change.path).unwrap();
            let mut ranges = line.split(|byte| *byte == b' ').skip(1);
            let removed = hunk_line_count(ranges.next().context("missing removed hunk range")?)?;
            let added = hunk_line_count(ranges.next().context("missing added hunk range")?)?;
            counts.removed += removed;
            counts.added += added;
            // Zero-context ranges give exact counts. Stream past the contents,
            // including no-final-newline notices, without buffering large lines.
            let mut remaining = added + removed;
            while remaining > 0 {
                let prefix = *reader
                    .fill_buf()?
                    .first()
                    .context("incomplete Jujutsu diff hunk")?;
                reader.skip_until(b'\n')?;
                if prefix != b'\\' {
                    remaining -= 1;
                }
            }
        }
    }
    Ok(files)
}

fn hunk_line_count(range: &[u8]) -> Result<u64> {
    if let Some(index) = range.iter().position(|byte| *byte == b',') {
        Ok(std::str::from_utf8(&range[index + 1..])?.parse()?)
    } else {
        Ok(1)
    }
}

fn git_entry(mode: &str, object_id: &str) -> Result<Entry> {
    let kind = match mode {
        "000000" => "",
        "100644" | "100755" => "file",
        "120000" => "symlink",
        "160000" => "git-submodule",
        _ => bail!("unsupported Git file mode {mode}"),
    };
    Ok(Entry {
        object_id: if kind.is_empty() {
            String::new()
        } else {
            object_id.to_owned()
        },
        kind: kind.to_owned(),
        executable: mode == "100755",
    })
}
