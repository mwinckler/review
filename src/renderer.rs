use std::{
    collections::VecDeque,
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText;
use anyhow::{Context, Result, bail, ensure};
use ratatui::text::Text;
use serde::{Deserialize, Serialize};

use crate::vcs::{Change, Comparison, Repository};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RendererConfig {
    pub command: Vec<String>,
    pub timeout_ms: u64,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            command: [
                "difft",
                "--color=always",
                "--width",
                "{width}",
                "--display",
                "{display}",
                "{path}",
                "{old}",
                "0",
                "100644",
                "{new}",
                "0",
                "100644",
            ]
            .map(str::to_owned)
            .to_vec(),
            timeout_ms: 30_000,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    pub width: u16,
    pub inline: bool,
}

pub fn render_pair(
    config: &RendererConfig,
    change: &Change,
    old: &[u8],
    new: &[u8],
    options: RenderOptions,
    generation: &AtomicU64,
    expected: u64,
) -> Result<Text<'static>> {
    ensure!(
        generation.load(Ordering::Relaxed) == expected,
        "render cancelled"
    );
    ensure!(
        !config.command.is_empty(),
        "renderer.command must include an executable"
    );
    let dir = tempfile::Builder::new().prefix("review-").tempdir()?;
    fs::create_dir(dir.path().join("old"))?;
    fs::create_dir(dir.path().join("new"))?;
    let old_name = Path::new(&change.old_path)
        .file_name()
        .context("invalid old filename")?;
    let new_name = Path::new(&change.path)
        .file_name()
        .context("invalid new filename")?;
    let old_path = dir.path().join("old").join(old_name);
    let new_path = dir.path().join("new").join(new_name);
    fs::write(&old_path, old)?;
    fs::write(&new_path, new)?;
    let display = if options.inline {
        "inline"
    } else {
        "side-by-side"
    };
    let width = options.width.max(1).to_string();
    let old_path = old_path.to_string_lossy();
    let new_path = new_path.to_string_lossy();
    let replacements = [
        ("{old}", old_path.as_ref()),
        ("{new}", new_path.as_ref()),
        ("{width}", &width),
        ("{display}", display),
        ("{path}", &change.path),
    ];
    let arguments: Vec<String> = config.command[1..]
        .iter()
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
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Command::new(&config.command[0])
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .with_context(|| {
            format!(
                "starting {:?}; install it or configure renderer.command",
                config.command[0]
            )
        })?;
    let started = Instant::now();
    let status = loop {
        let error = if generation.load(Ordering::Relaxed) != expected {
            Some("render cancelled")
        } else if started.elapsed() >= Duration::from_millis(config.timeout_ms) {
            Some("renderer timed out; increase renderer.timeout_ms or choose another renderer")
        } else if stdout.metadata()?.len() + stderr.metadata()?.len() > 16 * 1024 * 1024 {
            Some("renderer output exceeded 16 MiB")
        } else {
            None
        };
        if let Some(error) = error {
            let _ = child.kill();
            let _ = child.wait();
            bail!(error);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(10));
    };
    stderr.seek(SeekFrom::Start(0))?;
    let mut errors = String::new();
    stderr.take(64 * 1024).read_to_string(&mut errors)?;
    ensure!(
        status.success() || status.code() == Some(1),
        "renderer exited with {status}: {}",
        errors.trim()
    );
    stdout.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    stdout.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "renderer output exceeded 16 MiB"
    );
    if bytes.is_empty() {
        ensure!(errors.is_empty(), "renderer: {}", errors.trim());
        return Ok(Text::raw("File metadata changed"));
    }
    let mut text = bytes.into_text().context("decoding renderer ANSI output")?;
    for line in &mut text.lines {
        for span in &mut line.spans {
            span.content = span
                .content
                .chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>()
                .into();
        }
    }
    Ok(text)
}

struct Job {
    id: u64,
    repo: Repository,
    comparison: Comparison,
    change: Change,
    options: RenderOptions,
}

pub struct RenderResult {
    pub id: u64,
    pub text: std::result::Result<Text<'static>, String>,
}

pub struct RenderWorker {
    sender: Option<Sender<Job>>,
    pub results: Receiver<RenderResult>,
    generation: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl RenderWorker {
    pub fn cancel(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn new(config: RendererConfig) -> Self {
        let (sender, jobs) = mpsc::channel::<Job>();
        let (output, results) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&generation);
        let thread = thread::spawn(move || {
            let mut cache = VecDeque::<(String, Text<'static>, usize)>::new();
            while let Ok(mut job) = jobs.recv() {
                for newer in jobs.try_iter() {
                    job = newer;
                }
                if current.load(Ordering::Relaxed) != job.id {
                    continue;
                }
                let key = format!(
                    "{}:{}:{}",
                    job.change.fingerprint(),
                    job.options.width,
                    job.options.inline
                );
                let result = if let Some(index) = cache.iter().position(|entry| entry.0 == key) {
                    let entry = cache.remove(index).expect("cache index exists");
                    let text = entry.1.clone();
                    cache.push_back(entry);
                    Ok(text)
                } else {
                    (|| {
                        let old = job.repo.content(&job.comparison, &job.change, true)?;
                        let new = job.repo.content(&job.comparison, &job.change, false)?;
                        render_pair(
                            &config,
                            &job.change,
                            &old,
                            &new,
                            job.options,
                            &current,
                            job.id,
                        )
                    })()
                    .map_err(|error: anyhow::Error| format!("{error:#}"))
                };
                if current.load(Ordering::Relaxed) != job.id {
                    continue;
                }
                if let Ok(text) = &result {
                    let size = text.to_string().len();
                    if size <= 8 * 1024 * 1024 && !cache.iter().any(|entry| entry.0 == key) {
                        cache.push_back((key, text.clone(), size));
                        while cache.len() > 16
                            || cache.iter().map(|entry| entry.2).sum::<usize>() > 8 * 1024 * 1024
                        {
                            cache.pop_front();
                        }
                    }
                }
                if output
                    .send(RenderResult {
                        id: job.id,
                        text: result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            sender: Some(sender),
            results,
            generation,
            thread: Some(thread),
        }
    }

    pub fn request(
        &self,
        repo: &Repository,
        comparison: &Comparison,
        change: &Change,
        options: RenderOptions,
    ) -> u64 {
        let id = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Job {
            id,
            repo: repo.clone(),
            comparison: comparison.clone(),
            change: change.clone(),
            options,
        };
        let _ = self.sender.as_ref().expect("worker is running").send(job);
        id
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
