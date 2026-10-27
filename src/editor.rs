use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::AtomicU64,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::{
    hunks::{HunkFormat, HunkLocation, unified_ranges},
    renderer::{RenderOptions, RendererConfig, render_pair},
    vcs::{Change, Comparison, Repository},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    pub command: Vec<String>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            command: [
                "xdg-terminal-exec",
                "--",
                "nvim",
                "+{line}",
                "{read_only}",
                "--",
                "{file}",
            ]
            .map(str::to_owned)
            .to_vec(),
        }
    }
}

#[derive(Debug)]
pub struct EditorLocation {
    pub file: PathBuf,
    pub line: usize,
    pub read_only: bool,
}

pub fn location(
    repo: &Repository,
    comparison: &Comparison,
    change: &Change,
    source: &HunkLocation,
    snapshots: &Path,
) -> Result<EditorLocation> {
    ensure!(
        source.path == change.path,
        "editor location belongs to a different file"
    );
    let old = source.new_line.is_none() || change.new.kind.is_empty();
    let content = repo.content(comparison, change, old)?;
    let line = if old {
        source.old_line
    } else {
        source.new_line
    }
    .unwrap_or(1)
    .max(1)
    .min(
        content
            .split_inclusive(|byte| *byte == b'\n')
            .count()
            .max(1),
    );
    if !old && change.new.kind == "file" {
        let file = repo.root.join(&change.path);
        if fs::symlink_metadata(&file).is_ok_and(|metadata| metadata.is_file())
            && let Ok(file) = file.canonicalize()
            && file.starts_with(&repo.root)
            && let Ok(current) = fs::read(&file)
        {
            let mapped = if current == content {
                Some(line)
            } else {
                remap(change, &content, &current, line).unwrap_or(None)
            };
            if let Some(line) = mapped {
                return Ok(EditorLocation {
                    file,
                    line,
                    read_only: false,
                });
            }
        }
    }
    let path = if old { &change.old_path } else { &change.path };
    let name = Path::new(path)
        .file_name()
        .context("invalid snapshot filename")?;
    let mut hash = blake3::Hasher::new();
    hash.update(path.as_bytes());
    hash.update(&[0]);
    hash.update(&content);
    let directory = snapshots.join(hash.finalize().to_hex().as_str());
    fs::create_dir_all(&directory).context("creating editor snapshot directory")?;
    let file = directory.join(name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&file) {
        Ok(mut output) => {
            output.write_all(&content)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::read(&file)? == content,
                "editor snapshot contents have changed: {}",
                file.display()
            );
        }
        Err(error) => {
            return Err(error).context("writing editor snapshot");
        }
    }
    let mut permissions = fs::metadata(&file)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions)?;
    Ok(EditorLocation {
        file,
        line,
        read_only: true,
    })
}

fn remap(change: &Change, before: &[u8], after: &[u8], line: usize) -> Result<Option<usize>> {
    if before.contains(&0) || after.contains(&0) {
        return Ok(None);
    }
    let config = RendererConfig {
        command: [
            "git",
            "--no-pager",
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--color=never",
            "--unified=0",
            "--",
            "{old}",
            "{new}",
        ]
        .map(str::to_owned)
        .to_vec(),
        timeout_ms: 2_000,
        hunk_format: HunkFormat::None,
    };
    let patch = render_pair(
        &config,
        change,
        before,
        after,
        RenderOptions {
            width: 80,
            inline: true,
        },
        &AtomicU64::new(1),
        1,
    )?;
    let before = String::from_utf8_lossy(before);
    let after = String::from_utf8_lossy(after);
    let before: Vec<_> = before.lines().collect();
    let after: Vec<_> = after.lines().collect();
    let target = line - 1;
    let Some(source) = before.get(target) else {
        return Ok(None);
    };
    let mut mapped = target;
    for line in &patch.lines {
        let Some((old, new)) = unified_ranges(&line.to_string()) else {
            continue;
        };
        if target < old.start {
            break;
        }
        if old.contains(&target) {
            let mut matches = new.filter(|&line| after.get(line) == Some(source));
            return Ok(matches
                .next()
                .filter(|_| matches.next().is_none())
                .map(|line| line + 1));
        }
        mapped = target - old.end + new.end;
    }
    Ok(after
        .get(mapped)
        .filter(|line| *line == source)
        .map(|_| mapped + 1))
}

struct Job {
    repo: Repository,
    comparison: Comparison,
    change: Change,
    source: HunkLocation,
}

enum Request {
    Open(Box<Job>),
    Stop,
}

pub struct EditorWorker {
    sender: SyncSender<Request>,
    pub results: Receiver<Result<EditorLocation, String>>,
    thread: Option<JoinHandle<()>>,
}

impl EditorWorker {
    pub fn new(config: EditorConfig, snapshots: PathBuf) -> Self {
        let (sender, requests) = mpsc::sync_channel(1);
        let (output, results) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut children: Vec<Child> = Vec::new();
            loop {
                children.retain_mut(|child| match child.try_wait() {
                    Ok(None) => true,
                    Ok(Some(status)) => {
                        if !status.success() {
                            let _ =
                                output.send(Err(format!("Editor launcher exited with {status}")));
                        }
                        false
                    }
                    Err(error) => {
                        let _ = output.send(Err(format!("Checking editor launcher: {error}")));
                        false
                    }
                });
                let job = match requests.recv_timeout(Duration::from_millis(50)) {
                    Ok(Request::Open(job)) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        continue;
                    }
                    Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                };
                let opened = (|| -> Result<EditorLocation> {
                    let location = location(
                        &job.repo,
                        &job.comparison,
                        &job.change,
                        &job.source,
                        &snapshots,
                    )?;
                    let file = location
                        .file
                        .to_str()
                        .context("editor filename is not UTF-8")?;
                    let line = location.line.to_string();
                    let root = job
                        .repo
                        .root
                        .to_str()
                        .context("repository path is not UTF-8")?;
                    let read_only = if location.read_only { "-R" } else { "" };
                    let replacements = [
                        ("{file}", file),
                        ("{line}", line.as_str()),
                        ("{column}", "1"),
                        ("{repo}", root),
                        ("{read_only}", read_only),
                    ];
                    let program = config
                        .command
                        .first()
                        .context("editor.command must contain an executable")?;
                    let arguments: Vec<String> = config.command[1..]
                        .iter()
                        .filter(|argument| {
                            !(argument.as_str() == "{read_only}" && read_only.is_empty())
                        })
                        .map(|argument| {
                            argument
                                .split_inclusive('}')
                                .map(|part| {
                                    if let Some((token, value)) =
                                        replacements.iter().find(|(token, _)| part.ends_with(token))
                                    {
                                        format!("{}{value}", &part[..part.len() - token.len()])
                                    } else {
                                        part.to_owned()
                                    }
                                })
                                .collect()
                        })
                        .collect();
                    let mut child = Command::new(program)
                        .args(arguments)
                        .current_dir(&job.repo.root)
                        .env(
                            "REVIEW_EDITOR_READ_ONLY",
                            if location.read_only { "1" } else { "0" },
                        )
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .with_context(|| format!("starting editor {program:?}"))?;
                    thread::sleep(Duration::from_millis(30));
                    match child.try_wait()? {
                        Some(status) if !status.success() => {
                            bail!("Editor launcher exited with {status}");
                        }
                        Some(_) => {}
                        None => {
                            children.push(child);
                        }
                    }
                    Ok(location)
                })()
                .map_err(|error| format!("{error:#}"));
                if output.send(opened).is_err() {
                    break;
                }
            }
        });
        Self {
            sender,
            results,
            thread: Some(thread),
        }
    }

    pub fn request(
        &self,
        repo: &Repository,
        comparison: &Comparison,
        change: &Change,
        source: HunkLocation,
    ) -> Result<()> {
        self.sender
            .try_send(Request::Open(Box::new(Job {
                repo: repo.clone(),
                comparison: comparison.clone(),
                change: change.clone(),
                source,
            })))
            .context("editor launch queue is busy or stopped")
    }
}

impl Drop for EditorWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(Request::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
