use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{Read, Seek, SeekFrom},
    ops::Range,
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
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use serde::{Deserialize, Serialize};

use crate::{
    command,
    hunks::{self, Hunk, HunkFormat, SourceLine},
    ui::{change_heading, display_path},
    vcs::{Change, Comparison, Repository, Revision},
};

const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_CONTEXT: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RendererConfig {
    pub command: Vec<String>,
    pub timeout_ms: u64,
    pub hunk_format: HunkFormat,
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
            hunk_format: HunkFormat::Auto,
        }
    }
}

struct ContextFlag {
    prefix: &'static str,
    lines: u32,
    replaced: Vec<usize>,
}

impl RendererConfig {
    fn context_placeholder(&self) -> bool {
        self.command
            .iter()
            .skip(1)
            .any(|arg| arg.contains("{context}"))
    }

    pub fn default_context(&self) -> Option<u32> {
        if self.context_placeholder() {
            Some(DEFAULT_CONTEXT)
        } else {
            self.context_flag().map(|flag| flag.lines)
        }
    }

    fn context_flag(&self) -> Option<ContextFlag> {
        if self.context_placeholder() {
            return None;
        }
        let program = Path::new(self.command.first()?).file_name()?.to_str()?;
        let difftastic = program == "difft";
        let (short, long, prefix) = match program {
            "difft" => ("-c", "--context", "--context="),
            "diff" | "gdiff" => ("-U", "--unified", "-U"),
            _ => {
                return None;
            }
        };
        let mut lines = None;
        let mut replaced = Vec::new();
        let mut args = self.command.iter().enumerate().skip(1).peekable();
        while let Some((index, arg)) = args.next() {
            if arg == "--" {
                break;
            }
            let count = if arg == short || (difftastic && arg == long) {
                let Some(count) = args.peek().and_then(|(_, value)| value.parse().ok()) else {
                    continue;
                };
                args.next();
                replaced.push(index + 1);
                Some(count)
            } else if !difftastic && (arg == "-u" || arg == long) {
                Some(DEFAULT_CONTEXT)
            } else if let Some(value) = arg.strip_prefix(long).and_then(|arg| arg.strip_prefix('='))
            {
                value.parse().ok()
            } else {
                arg.strip_prefix(short).and_then(|value| {
                    let value = if difftastic {
                        value.strip_prefix('=').unwrap_or(value)
                    } else {
                        value
                    };
                    value.parse().ok()
                })
            };
            if let Some(count) = count {
                // GNU diff uses the largest context when several options specify it.
                lines = Some(lines.map_or(count, |previous: u32| previous.max(count)));
                replaced.push(index);
            }
        }
        Some(ContextFlag {
            prefix,
            lines: match lines {
                Some(lines) => lines,
                None if difftastic => std::env::var("DFT_CONTEXT")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(DEFAULT_CONTEXT),
                None => {
                    return None;
                }
            },
            replaced,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RenderOptions {
    pub width: u16,
    pub inline: bool,
    pub context: Option<u32>,
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
    let context = options.context.unwrap_or(DEFAULT_CONTEXT).to_string();
    let old_path = old_path.to_string_lossy();
    let new_path = new_path.to_string_lossy();
    let replacements = [
        ("{old}", old_path.as_ref()),
        ("{new}", new_path.as_ref()),
        ("{width}", &width),
        ("{display}", display),
        ("{path}", &change.path),
        ("{context}", &context),
    ];
    let context_flag = options.context.and_then(|_| config.context_flag());
    let context_arg = context_flag
        .as_ref()
        .map(|flag| format!("{}{context}", flag.prefix));
    let arguments = config
        .command
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(index, _)| {
            context_flag
                .as_ref()
                .is_none_or(|flag| !flag.replaced.contains(index))
        })
        .map(|(_, argument)| command::expand(argument, &replacements));
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    // Git also provides the internal line-mapping diff.
    let setting = (config.command[0] != "git").then_some("renderer.command");
    let mut child = command::launch(
        Command::new(&config.command[0])
            .args(context_arg.into_iter().chain(arguments))
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?),
        setting,
        Command::spawn,
    )?;
    let started = Instant::now();
    let status = loop {
        let error = if generation.load(Ordering::Relaxed) != expected {
            Some("render cancelled")
        } else if started.elapsed() >= Duration::from_millis(config.timeout_ms) {
            Some("renderer timed out; increase renderer.timeout_ms or choose another renderer")
        } else if stdout.metadata()?.len() + stderr.metadata()?.len() > MAX_OUTPUT_BYTES {
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
    stdout.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_OUTPUT_BYTES as usize,
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

pub(crate) fn changed_line_ranges(
    change: &Change,
    before: &[u8],
    after: &[u8],
) -> Result<Vec<(Range<usize>, Range<usize>)>> {
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
            "--inter-hunk-context=0",
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
            context: None,
        },
        &AtomicU64::new(1),
        1,
    )?;
    Ok(patch
        .lines
        .iter()
        .filter_map(|line| hunks::unified_ranges(&line.to_string()))
        .collect())
}

struct Job {
    id: u64,
    repo: Repository,
    comparison: Comparison,
    path: String,
    changes: Vec<Change>,
    options: RenderOptions,
    selection: RenderSelection,
}

#[derive(Default)]
struct RenderSelection {
    revisions: Option<(Revision, Revision)>,
    contexts: BTreeMap<String, u32>,
}

#[derive(Clone)]
pub struct FileHeading {
    pub row: usize,
    pub path: String,
}

fn text_size(text: &Text<'_>) -> usize {
    text.lines
        .iter()
        .map(|line| {
            std::mem::size_of_val(line)
                + line
                    .spans
                    .iter()
                    .map(|span| std::mem::size_of_val(span) + span.content.len())
                    .sum::<usize>()
        })
        .sum()
}

#[derive(Default)]
pub struct DiffPreview {
    pub text: Text<'static>,
    pub width: usize,
    pub file_headings: Vec<FileHeading>,
    pub hunks: Vec<Hunk>,
    pub source_lines: Vec<SourceLine>,
    pub rendering_rows: Vec<Range<usize>>,
}

impl DiffPreview {
    pub fn from_text(text: Text<'static>) -> Self {
        Self {
            width: text.width(),
            text,
            ..Self::default()
        }
    }

    fn append_line(&mut self, line: Line<'static>) {
        self.width = self.width.max(line.width());
        self.text.lines.push(line);
    }

    fn append_heading(&mut self, change: &Change, file: Option<&FileDiff>, viewed: bool) {
        self.file_headings.push(FileHeading {
            row: self.text.lines.len(),
            path: change.path.clone(),
        });
        let mut line = file
            .and_then(|file| file.heading.clone())
            .unwrap_or_else(|| {
                let (title, metadata) = change_heading(change);
                Line::from(vec![
                    Span::styled(title, heading_style()),
                    heading_metadata(metadata),
                ])
            });
        if viewed {
            for span in &mut line.spans {
                span.style = span
                    .style
                    .remove_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::DIM);
            }
        }
        self.append_line(line);
    }

    fn append_diff(&mut self, file: &FileDiff) {
        // A shared heading already occupies the renderer's first row.
        let first = usize::from(file.heading.is_some());
        let offset = self.text.lines.len() - first;
        self.width = self.width.max(file.width);
        self.text.lines.extend_from_slice(&file.text.lines[first..]);
        self.hunks
            .extend(file.hunks.iter().cloned().map(|mut hunk| {
                hunk.rows.start += offset;
                hunk.rows.end += offset;
                hunk
            }));
        self.source_lines
            .extend(file.source_lines.iter().cloned().map(|mut line| {
                line.row += offset;
                line
            }));
    }

    pub fn single_file(files: &FileDiffs, revision_filtered: bool) -> Self {
        if let Some(file) = files.values().next() {
            Self {
                text: file.text.clone(),
                width: file.width,
                hunks: file.hunks.clone(),
                source_lines: file.source_lines.clone(),
                ..Self::default()
            }
        } else if revision_filtered {
            Self::from_text(Text::raw(
                "No changes to this file in the selected revision range",
            ))
        } else {
            Self::default()
        }
    }
}

pub struct FileDiff {
    text: Text<'static>,
    heading: Option<Line<'static>>,
    width: usize,
    hunks: Vec<Hunk>,
    source_lines: Vec<SourceLine>,
    text_size: usize,
    context: Option<u32>,
}

pub type FileDiffs = BTreeMap<String, Arc<FileDiff>>;

pub struct FolderPreview {
    changes: Vec<Change>,
    files: FileDiffs,
    viewed: BTreeSet<String>,
    contexts: BTreeMap<String, u32>,
    pending: Vec<(String, Option<u32>)>,
    errors: BTreeMap<String, String>,
}

impl FolderPreview {
    pub fn new(changes: Vec<Change>) -> Self {
        Self {
            changes,
            files: BTreeMap::new(),
            viewed: BTreeSet::new(),
            contexts: BTreeMap::new(),
            pending: Vec::new(),
            errors: BTreeMap::new(),
        }
    }

    pub fn update_view(&mut self, review: &crate::state::Review) -> bool {
        let viewed: BTreeSet<_> = self
            .changes
            .iter()
            .filter(|change| review.is_viewed(change))
            .map(|change| change.path.clone())
            .collect();
        let contexts = self
            .changes
            .iter()
            .filter_map(|change| {
                review
                    .view
                    .diff_context_for(&change.path)
                    .map(|context| (change.path.clone(), context))
            })
            .collect::<BTreeMap<_, _>>();
        if self.viewed == viewed && self.contexts == contexts {
            return false;
        }
        self.errors.retain(|path, _| {
            !viewed.contains(path) && self.contexts.get(path) == contexts.get(path)
        });
        self.viewed = viewed;
        self.contexts = contexts;
        true
    }

    pub fn request_missing(&mut self) -> Vec<Change> {
        if !self.pending.is_empty() {
            return Vec::new();
        }
        let missing: Vec<_> =
            self.changes
                .iter()
                .filter(|change| {
                    !self.viewed.contains(&change.path)
                        && self.files.get(&change.path).is_none_or(|file| {
                            file.context != self.contexts.get(&change.path).copied()
                        })
                        && !self.errors.contains_key(&change.path)
                })
                .cloned()
                .collect();
        self.pending.extend(missing.iter().map(|change| {
            (
                change.path.clone(),
                self.contexts.get(&change.path).copied(),
            )
        }));
        missing
    }

    pub fn contexts(&self) -> &BTreeMap<String, u32> {
        &self.contexts
    }

    pub fn discard_superseded_request(&mut self) -> bool {
        if self.pending.is_empty()
            || self
                .pending
                .iter()
                .any(|(path, context)| self.contexts.get(path).copied() == *context)
        {
            return false;
        }
        self.pending.clear();
        true
    }

    pub fn finish(&mut self, result: Result<FileDiffs, String>) {
        match result {
            Ok(mut files) => {
                files.retain(|path, file| file.context == self.contexts.get(path).copied());
                self.store_diffs(files);
            }
            Err(error) => self.errors.extend(
                self.pending
                    .iter()
                    .filter(|(path, context)| {
                        !self.viewed.contains(path) && self.contexts.get(path).copied() == *context
                    })
                    .map(|(path, _)| (path.clone(), error.clone())),
            ),
        }
        self.pending.clear();
    }

    fn store_diffs(&mut self, files: FileDiffs) {
        // Prefer keeping expanded diffs when the active folder cache fills up.
        let limit = MAX_OUTPUT_BYTES as usize;
        let incoming: usize = files
            .iter()
            .map(|(path, file)| {
                file.text_size
                    .saturating_sub(self.files.get(path).map_or(0, |old| old.text_size))
            })
            .sum();
        let mut size: usize = self.files.values().map(|file| file.text_size).sum();
        self.files.retain(|path, file| {
            if size + incoming > limit && self.viewed.contains(path) {
                size -= file.text_size;
                return false;
            }
            true
        });
        for (path, file) in files {
            let next_size =
                size - self.files.get(&path).map_or(0, |old| old.text_size) + file.text_size;
            if next_size <= limit {
                size = next_size;
                self.files.insert(path, file);
            } else if !self.viewed.contains(&path) {
                self.errors
                    .insert(path, "Folder diff output exceeded 16 MiB.".into());
            }
        }
    }

    pub fn assemble(&self) -> DiffPreview {
        let mut preview = DiffPreview::default();
        for change in &self.changes {
            let viewed = self.viewed.contains(&change.path);
            let file = self.files.get(&change.path);
            preview.append_heading(change, file.map(Arc::as_ref), viewed);
            if viewed {
                continue;
            }
            let start = preview.text.lines.len();
            if let Some(error) = self.errors.get(&change.path) {
                preview.append_line(Line::raw(display_path(error)));
            } else {
                if let Some(file) = file {
                    preview.append_diff(file);
                }
                if file.is_none_or(|file| file.context != self.contexts.get(&change.path).copied())
                {
                    if preview.text.lines.len() == start {
                        preview.append_line(Line::default());
                    }
                    preview.rendering_rows.push(start..preview.text.lines.len());
                }
            }
        }
        preview
    }
}

fn heading_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn heading_metadata(metadata: String) -> Span<'static> {
    Span::styled(metadata, Style::default().fg(Color::DarkGray))
}

fn difftastic_heading(text: &Text<'static>, change: &Change) -> Option<Line<'static>> {
    let line = text.lines.first()?;
    let text = line.to_string();
    let (path, _) = hunks::difftastic_title(&text)?;
    let shown = display_path(&change.path);
    if path.is_empty() || (path != shown && shown.rsplit('/').next() != Some(path)) {
        return None;
    }
    let (title, metadata) = change_heading(change);
    let prefix = title.strip_suffix(' ')?.strip_suffix(path)?;
    let mut line = line.clone();
    line.spans
        .insert(0, Span::styled(prefix.to_owned(), heading_style()));
    line.spans.push(heading_metadata(metadata));
    Some(line)
}

fn render_file(
    config: &RendererConfig,
    job: &Job,
    comparison: &Comparison,
    change: &Change,
    options: RenderOptions,
    started: Instant,
    generation: &AtomicU64,
) -> Result<FileDiff> {
    let old = job.repo.content(comparison, change, true)?;
    let new = job.repo.content(comparison, change, false)?;
    let mut remaining = config.clone();
    remaining.timeout_ms = config
        .timeout_ms
        .saturating_sub(started.elapsed().as_millis() as u64);
    ensure!(
        remaining.timeout_ms > 0,
        "renderer timed out; increase renderer.timeout_ms or choose another renderer"
    );
    let text = render_pair(&remaining, change, &old, &new, options, generation, job.id)?;
    let line_counts = (
        old.split_inclusive(|byte| *byte == b'\n').count(),
        new.split_inclusive(|byte| *byte == b'\n').count(),
    );
    let format = config.hunk_format.for_command(&config.command);
    let hunks = hunks::parse(&text, change, format);
    let source_lines = hunks::source_lines(&text, change, format, &hunks, line_counts);
    let heading = if matches!(format, HunkFormat::Difftastic) {
        difftastic_heading(&text, change)
    } else {
        None
    };
    let text_size = text_size(&text);
    let width = text.width();
    Ok(FileDiff {
        text,
        heading,
        width,
        hunks,
        source_lines,
        text_size,
        context: options.context,
    })
}

type FileCache = VecDeque<((String, RenderOptions), Arc<FileDiff>, usize)>;

fn render_job(
    config: &RendererConfig,
    job: &Job,
    generation: &AtomicU64,
    cache: &mut FileCache,
) -> Result<RenderResult> {
    let started = Instant::now();
    let directory = job.path.ends_with('/');
    let revision_preview = if let Some((first, last)) = &job.selection.revisions {
        let comparison = job.repo.revision_comparison(first, last)?;
        let changes: Vec<_> = job
            .repo
            .changes(&comparison)?
            .into_iter()
            .filter(|change| {
                job.changes.iter().any(|original| {
                    [&original.path, &original.old_path]
                        .iter()
                        .any(|path| **path == change.path || **path == change.old_path)
                })
            })
            .collect();
        Some((comparison, changes))
    } else {
        None
    };
    let (comparison, changes) = revision_preview
        .as_ref()
        .map(|(comparison, changes)| (comparison, changes.as_slice()))
        .unwrap_or((&job.comparison, &job.changes));
    let mut size = 0;
    let mut files = BTreeMap::new();
    for change in changes {
        ensure!(
            generation.load(Ordering::Relaxed) == job.id,
            "render cancelled"
        );
        let options = RenderOptions {
            context: job
                .selection
                .contexts
                .get(&change.path)
                .copied()
                .or(job.options.context),
            ..job.options
        };
        let key = (change.fingerprint(), options);
        let file = if let Some(index) = cache.iter().position(|entry| entry.0 == key) {
            let entry = cache.remove(index).expect("cache index exists");
            let file = Arc::clone(&entry.1);
            cache.push_back(entry);
            file
        } else {
            let file = Arc::new(
                render_file(
                    config, job, comparison, change, options, started, generation,
                )
                .with_context(|| format!("rendering {}", display_path(&change.path)))?,
            );
            let size = file.text_size
                + key.0.len()
                + std::mem::size_of::<FileDiff>()
                + file
                    .hunks
                    .iter()
                    .map(|hunk| std::mem::size_of_val(hunk) + hunk.location.path.len())
                    .sum::<usize>()
                + file
                    .source_lines
                    .iter()
                    .map(|line| std::mem::size_of_val(line) + line.location.path.len())
                    .sum::<usize>();
            if size <= MAX_CACHE_BYTES {
                cache.push_back((key, Arc::clone(&file), size));
                let mut bytes: usize = cache.iter().map(|entry| entry.2).sum();
                while bytes > MAX_CACHE_BYTES || cache.len() > 256 {
                    bytes -= cache.pop_front().expect("cache contains bytes").2;
                }
            }
            file
        };
        size += file.text_size;
        ensure!(
            !directory || size <= MAX_OUTPUT_BYTES as usize,
            "renderer output exceeded 16 MiB"
        );
        files.insert(change.path.clone(), file);
        if !directory {
            break;
        }
    }
    Ok(RenderResult {
        id: job.id,
        files: Ok(files),
        revision_preview,
    })
}

pub struct RenderResult {
    pub id: u64,
    pub files: Result<FileDiffs, String>,
    pub revision_preview: Option<(Comparison, Vec<Change>)>,
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
            let mut cache = FileCache::new();
            while let Ok(mut job) = jobs.recv() {
                for newer in jobs.try_iter() {
                    job = newer;
                }
                if current.load(Ordering::Relaxed) != job.id {
                    continue;
                }
                let result = match render_job(&config, &job, &current, &mut cache) {
                    Ok(result) => result,
                    Err(error) => RenderResult {
                        id: job.id,
                        files: Err(format!("{error:#}")),
                        revision_preview: None,
                    },
                };
                if current.load(Ordering::Relaxed) != job.id {
                    continue;
                }
                if output.send(result).is_err() {
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
        path: &str,
        changes: Vec<Change>,
        options: RenderOptions,
    ) -> u64 {
        self.request_revisions(repo, comparison, path, changes, options, None)
    }

    pub fn request_revisions(
        &self,
        repo: &Repository,
        comparison: &Comparison,
        path: &str,
        changes: Vec<Change>,
        options: RenderOptions,
        revisions: Option<(Revision, Revision)>,
    ) -> u64 {
        self.request_selected(
            repo,
            comparison,
            path,
            changes,
            options,
            RenderSelection {
                revisions,
                ..RenderSelection::default()
            },
        )
    }

    pub fn request_folder(
        &self,
        repo: &Repository,
        comparison: &Comparison,
        path: &str,
        changes: Vec<Change>,
        options: RenderOptions,
        contexts: &BTreeMap<String, u32>,
    ) -> u64 {
        let contexts = changes
            .iter()
            .filter_map(|change| {
                contexts
                    .get(&change.path)
                    .map(|&context| (change.path.clone(), context))
            })
            .collect();
        self.request_selected(
            repo,
            comparison,
            path,
            changes,
            options,
            RenderSelection {
                contexts,
                ..RenderSelection::default()
            },
        )
    }

    fn request_selected(
        &self,
        repo: &Repository,
        comparison: &Comparison,
        path: &str,
        changes: Vec<Change>,
        options: RenderOptions,
        selection: RenderSelection,
    ) -> u64 {
        let id = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Job {
            id,
            repo: repo.clone(),
            comparison: comparison.clone(),
            path: path.to_owned(),
            changes,
            options,
            selection,
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
