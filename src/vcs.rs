use std::{
    path::{Path, PathBuf},
    process::Command,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    pub base_ref: String,
    pub head_ref: String,
    pub base_id: String,
    pub head_id: String,
}

#[derive(Clone, Debug)]
pub struct Repository {
    pub root: PathBuf,
    pub backend: Backend,
}

impl Repository {
    pub fn discover(path: &Path, backend: Option<Backend>) -> Result<Self> {
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
        let mut repo = Self {
            root: path,
            backend: backend
                .or(detected)
                .context("no Git or Jujutsu repository found")?,
        };
        let args = match repo.backend {
            Backend::Git => vec!["rev-parse", "--show-toplevel"],
            Backend::Jj => vec!["root"],
        };
        let root = String::from_utf8(repo.run(&args, false)?)?;
        repo.root = PathBuf::from(root.strip_suffix('\n').unwrap_or(&root)).canonicalize()?;
        Ok(repo)
    }

    pub fn snapshot(&self, base: &str, head: &str) -> Result<Comparison> {
        let head_id = self.resolve(head, true)?;
        let base_tip = self.resolve(base, false)?;
        let ancestors = match self.backend {
            Backend::Git => {
                String::from_utf8(self.run(&["merge-base", "--all", &base_tip, &head_id], false)?)?
            }
            Backend::Jj => {
                let expression = format!("heads(::{base_tip} & ::{head_id})");
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
        Ok(Comparison {
            base_ref: base.to_owned(),
            head_ref: head.to_owned(),
            base_id: bases[0].to_owned(),
            head_id,
        })
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
                let fileset = format!("root-file:{}", serde_json::to_string(path)?);
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

    fn resolve(&self, revision: &str, snapshot: bool) -> Result<String> {
        let output = match self.backend {
            Backend::Git => self.run(
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{revision}^{{commit}}"),
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

    fn run(&self, args: &[&str], snapshot: bool) -> Result<Vec<u8>> {
        let program = match self.backend {
            Backend::Git => "git",
            Backend::Jj => "jj",
        };
        let mut command = Command::new(program);
        command.current_dir(&self.root).arg("--no-pager");
        match self.backend {
            Backend::Git => {
                command
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
