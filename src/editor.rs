use std::{
    fs,
    io::Write,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::{
    command,
    hunks::HunkLocation,
    renderer::changed_line_ranges,
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
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(&content)?;
    match temporary.persist_noclobber(&file) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::symlink_metadata(&file)?.is_file() && fs::read(&file)? == content,
                "editor snapshot contents have changed: {}",
                file.display()
            );
        }
        Err(error) => {
            return Err(error.error).context("writing editor snapshot");
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
    Ok(remap_range(change, before, after, line..=line)?.map(|range| *range.start()))
}

pub(crate) fn remap_range(
    change: &Change,
    before: &[u8],
    after: &[u8],
    lines: RangeInclusive<usize>,
) -> Result<Option<RangeInclusive<usize>>> {
    if before.contains(&0) || after.contains(&0) {
        return Ok(None);
    }
    let ranges = changed_line_ranges(change, before, after)?;
    let before = String::from_utf8_lossy(before);
    let after = String::from_utf8_lossy(after);
    let before: Vec<_> = before.lines().collect();
    let after: Vec<_> = after.lines().collect();
    let map = |line: usize| -> Option<usize> {
        let target = line.checked_sub(1)?;
        let source = before.get(target)?;
        let mut mapped = target;
        for (old, new) in &ranges {
            if target < old.start {
                break;
            }
            if old.contains(&target) {
                let mut matches = new.clone().filter(|&line| after.get(line) == Some(source));
                return matches
                    .next()
                    .filter(|_| matches.next().is_none())
                    .map(|line| line + 1);
            }
            mapped = target - old.end + new.end;
        }
        after
            .get(mapped)
            .filter(|line| *line == source)
            .map(|_| mapped + 1)
    };
    let Some(start) = map(*lines.start()) else {
        return Ok(None);
    };
    for (offset, line) in lines.clone().enumerate() {
        if map(line) != Some(start + offset) {
            return Ok(None);
        }
    }
    Ok(Some(start..=start + lines.end() - lines.start()))
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
        let uses_terminal_launcher = config
            .command
            .first()
            .and_then(|program| Path::new(program).file_name())
            .is_some_and(|name| name == "xdg-terminal-exec");
        let launch_error = move |status| {
            if uses_terminal_launcher {
                format!(
                    "Configure a preferred terminal or set `editor.command`. Launcher exited with {status}."
                )
            } else {
                format!("Editor launcher exited with {status}. Check `editor.command`.")
            }
        };
        let thread = thread::spawn(move || {
            let mut children: Vec<Child> = Vec::new();
            loop {
                children.retain_mut(|child| match child.try_wait() {
                    Ok(None) => true,
                    Ok(Some(status)) => {
                        if !status.success() {
                            let _ = output.send(Err(launch_error(status)));
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
                    let program = config
                        .command
                        .first()
                        .context("editor.command must contain an executable")?;
                    let setting = Some("editor.command");
                    if uses_terminal_launcher
                        && let [_, separator, editor, ..] = config.command.as_slice()
                        && separator == "--"
                        && !editor.contains('{')
                    {
                        command::require(program, &job.repo.root, setting)?;
                        command::require(editor, &job.repo.root, setting)?;
                    }
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
                    let arguments: Vec<String> = config.command[1..]
                        .iter()
                        .filter(|argument| {
                            !(argument.as_str() == "{read_only}" && read_only.is_empty())
                        })
                        .map(|argument| command::expand(argument, &replacements))
                        .collect();
                    let mut child = command::launch(
                        Command::new(program)
                            .args(arguments)
                            .current_dir(&job.repo.root)
                            .env(
                                "REVIEW_EDITOR_READ_ONLY",
                                if location.read_only { "1" } else { "0" },
                            )
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null()),
                        setting,
                        Command::spawn,
                    )?;
                    thread::sleep(Duration::from_millis(30));
                    match child.try_wait()? {
                        Some(status) if !status.success() => {
                            bail!(launch_error(status));
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
