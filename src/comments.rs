//! Provider-independent review comments. Anchors use pinned revisions, a diff side,
//! and inclusive, one-based source lines so remote review adapters can translate
//! them into a host's line-comment API without depending on terminal coordinates.

use std::{
    fs,
    io::Write,
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    editor::remap_range,
    state::Review,
    store::now_millis,
    vcs::{Change, Comparison, Entry, Repository},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffSide {
    Old,
    New,
}

impl DiffSide {
    pub fn label(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentAnchor {
    /// Repository-relative path on the selected side, including pre-rename paths.
    pub path: String,
    pub side: DiffSide,
    pub start_line: usize,
    pub end_line: usize,
    pub base_id: String,
    pub head_id: String,
    pub entry: Entry,
    /// Original selected source, retained when the anchor becomes outdated.
    pub excerpt: String,
}

impl CommentAnchor {
    pub fn capture(
        repo: &Repository,
        comparison: &Comparison,
        change: &Change,
        side: DiffSide,
        start_line: usize,
        end_line: usize,
    ) -> Result<Self> {
        let content = repo.content(comparison, change, side == DiffSide::Old)?;
        Self::from_content(comparison, change, side, start_line, end_line, &content)
    }

    pub(crate) fn from_content(
        comparison: &Comparison,
        change: &Change,
        side: DiffSide,
        start_line: usize,
        end_line: usize,
        content: &[u8],
    ) -> Result<Self> {
        let (path, entry) = match side {
            DiffSide::Old => (&change.old_path, &change.old),
            DiffSide::New => (&change.path, &change.new),
        };
        ensure!(
            matches!(entry.kind.as_str(), "file" | "conflict"),
            "Select a text-file side to comment on"
        );
        ensure!(!content.contains(&0), "Line comments require a text file");
        let content = std::str::from_utf8(content).context("Line comments require UTF-8 source")?;
        Ok(Self {
            path: path.clone(),
            side,
            start_line,
            end_line,
            base_id: comparison.base_id.clone(),
            head_id: comparison.head_id.clone(),
            entry: entry.clone(),
            excerpt: excerpt(content, start_line, end_line)?,
        })
    }

    pub fn change<'a>(&self, changes: &'a [Change]) -> Option<&'a Change> {
        changes.iter().find(|change| {
            let (path, entry) = match self.side {
                DiffSide::Old => (&change.old_path, &change.old),
                DiffSide::New => (&change.path, &change.new),
            };
            path == &self.path && entry == &self.entry
        })
    }

    pub fn is_outdated(&self, changes: &[Change]) -> bool {
        self.change(changes).is_none()
    }
}

pub(crate) fn excerpt(content: &str, start: usize, end: usize) -> Result<String> {
    ensure!(
        start > 0 && end >= start,
        "Use an inclusive line range with 1 ≤ start ≤ end"
    );
    let count = end - start + 1;
    let lines: Vec<_> = content.lines().skip(start - 1).take(count).collect();
    ensure!(
        lines.len() == count,
        "Line range exceeds the file's {} lines",
        content.lines().count()
    );
    Ok(lines.join("\n"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTodo {
    pub path: String,
    pub line: usize,
    pub body: String,
    pub written_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub anchor: CommentAnchor,
    pub body: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub source_todo: Option<SourceTodo>,
}

impl Comment {
    pub fn new(anchor: CommentAnchor, body: String) -> Result<Self> {
        validate_body(&body)?;
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let identity = (
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            &anchor,
        );
        let id = blake3::hash(&serde_json::to_vec(&identity)?).to_hex()[..32].to_owned();
        let now = now_millis()?;
        Ok(Self {
            id,
            anchor,
            body,
            created_at: now,
            updated_at: now,
            source_todo: None,
        })
    }

    pub fn edit(&mut self, body: String) -> Result<()> {
        validate_body(&body)?;
        self.updated_at = now_millis()?;
        self.body = body;
        Ok(())
    }
}

fn validate_body(body: &str) -> Result<()> {
    ensure!(!body.trim().is_empty(), "Write a comment before saving");
    ensure!(
        body.chars()
            .all(|ch| !ch.is_control() || matches!(ch, '\n' | '\t')),
        "Comments may contain text, tabs, and newlines"
    );
    Ok(())
}

/// Insert a confirmed comment above its complete, unchanged working-copy range.
/// The caller persists the returned receipt alongside the original local comment.
pub fn write_todo(repo: &Repository, review: &Review, comment: &Comment) -> Result<SourceTodo> {
    let anchor = &comment.anchor;
    ensure!(
        comment.source_todo.is_none(),
        "This comment already has a source TODO"
    );
    ensure!(
        anchor.side == DiffSide::New,
        "Source TODOs require a new-side comment"
    );
    validate_body(&comment.body)?;
    ensure!(
        comment
            .body
            .split(['\n', '\u{2028}', '\u{2029}'])
            .all(|line| !line.trim_end().ends_with('\\')),
        "A trailing backslash can splice source lines; edit the comment before writing a TODO"
    );
    let path = Path::new(&anchor.path);
    let extension = path.extension().and_then(|extension| extension.to_str());
    ensure!(
        extension != Some("java") || !comment.body.contains("\\u"),
        "Java Unicode escapes can terminate source comments; edit the text before writing a TODO"
    );
    ensure!(
        extension != Some("php") || !comment.body.contains("?>"),
        "PHP closing tags can terminate source comments; edit the text before writing a TODO"
    );
    ensure!(
        !matches!(
            extension,
            Some("c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "hxx")
        ) || comment
            .body
            .split(['\n', '\u{2028}', '\u{2029}'])
            .all(|line| !line.trim_end().ends_with("??/")),
        "C trigraphs can splice source lines; edit the text before writing a TODO"
    );
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(name) if name != ".git" && name != ".jj")),
        "Source TODO path must stay inside repository source files"
    );
    ensure!(
        matches!(
            extension,
            Some(
                "rs" | "c"
                    | "h"
                    | "cc"
                    | "cpp"
                    | "hpp"
                    | "cxx"
                    | "hxx"
                    | "cs"
                    | "go"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "mjs"
                    | "cjs"
                    | "mts"
                    | "cts"
                    | "java"
                    | "kt"
                    | "kts"
                    | "swift"
                    | "dart"
                    | "scala"
                    | "php"
            )
        ),
        "Source TODOs require a language with // comments"
    );
    let change = anchor
        .change(&review.changes)
        .context("Comment is outdated; its original source has changed")?;
    ensure!(
        change.new.kind == "file",
        "Source TODOs require a regular source file"
    );
    let mut file = repo.root.clone();
    for part in path.components() {
        file.push(part);
        ensure!(
            !fs::symlink_metadata(&file)?.file_type().is_symlink(),
            "Source TODOs never follow symlinks"
        );
    }
    let metadata = fs::metadata(&file)?;
    ensure!(
        metadata.is_file() && !metadata.permissions().readonly(),
        "Source TODO target must be a writable regular file"
    );
    let current = fs::read(&file)?;
    ensure!(!current.contains(&0), "Source TODOs require a text file");
    let content = repo.content(&review.comparison, change, false)?;
    ensure!(
        excerpt(
            std::str::from_utf8(&content)?,
            anchor.start_line,
            anchor.end_line
        )? == anchor.excerpt,
        "Comment source differs from its recorded excerpt"
    );
    let mapped = remap_range(
        change,
        &content,
        &current,
        anchor.start_line..=anchor.end_line,
    )?
    .context("Selected source lines changed on disk; keeping the comment local")?;
    let text = std::str::from_utf8(&current).context("Source TODOs require UTF-8 source")?;
    let mut offset: usize = text
        .split_inclusive('\n')
        .take(mapped.start() - 1)
        .map(str::len)
        .sum();
    if offset == 0 && text.starts_with('\u{feff}') {
        offset = '\u{feff}'.len_utf8();
    }
    let indentation: String = text[offset..]
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .collect();
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut todo = String::new();
    for (index, line) in comment
        .body
        .split(['\n', '\u{2028}', '\u{2029}'])
        .enumerate()
    {
        todo.push_str(&indentation);
        todo.push_str(if index == 0 { "// TODO: " } else { "// " });
        todo.push_str(line);
        todo.push_str(newline);
    }
    // Retry safely after a source write whose session save failed.
    let receipt = SourceTodo {
        path: anchor.path.clone(),
        line: *mapped.start(),
        body: comment.body.clone(),
        written_at: now_millis()?,
    };
    if text[..offset].ends_with(&todo) {
        return Ok(receipt);
    }
    let directory = file.parent().expect("Repository source file has a parent");
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&current[..offset])?;
    temporary.write_all(todo.as_bytes())?;
    temporary.write_all(&current[offset..])?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())?;
    temporary.as_file().sync_all()?;
    ensure!(
        !fs::symlink_metadata(&file)?.file_type().is_symlink() && fs::read(&file)? == current,
        "Source changed while preparing TODO; retry after reviewing the file"
    );
    temporary
        .persist(&file)
        .context("Atomically writing source TODO")?;
    #[cfg(unix)]
    {
        fs::File::open(directory)?.sync_all()?;
    }
    Ok(receipt)
}
