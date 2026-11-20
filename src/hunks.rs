use std::{ops::Range, path::Path};

use ratatui::{
    style::{Color, Modifier},
    text::{Line, Span, Text},
};
use serde::{Deserialize, Serialize};

use crate::{comments::DiffSide, vcs::Change};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HunkFormat {
    #[default]
    Auto,
    Difftastic,
    Unified,
    None,
}

impl HunkFormat {
    pub fn for_command(self, command: &[String]) -> Self {
        if !matches!(self, Self::Auto) {
            return self;
        }
        match command
            .first()
            .and_then(|program| Path::new(program).file_name())
            .and_then(|name| name.to_str())
        {
            Some("difft") => Self::Difftastic,
            Some("diff" | "gdiff" | "git") => Self::Unified,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunkLocation {
    pub path: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub rows: Range<usize>,
    pub location: HunkLocation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLine {
    pub row: usize,
    pub location: HunkLocation,
    /// Display-column boundary between old and new in side-by-side output.
    pub split_column: Option<usize>,
}

impl SourceLine {
    pub fn number(&self, side: DiffSide) -> Option<usize> {
        match side {
            DiffSide::Old => self.location.old_line,
            DiffSide::New => self.location.new_line,
        }
    }
}

pub fn source_lines(
    text: &Text<'_>,
    change: &Change,
    format: HunkFormat,
    hunks: &[Hunk],
    line_counts: (usize, usize),
) -> Vec<SourceLine> {
    let mut result = Vec::new();
    match format {
        HunkFormat::Unified => {
            for hunk in hunks {
                let Some((old, new)) = unified_ranges(&text.lines[hunk.rows.start].to_string())
                else {
                    continue;
                };
                let mut old = old.start + 1;
                let mut new = new.start + 1;
                for row in hunk.rows.start + 1..hunk.rows.end {
                    let line = text.lines[row].to_string();
                    let (old_line, new_line) = match line.as_bytes().first() {
                        Some(b' ') => (Some(old), Some(new)),
                        Some(b'-') => (Some(old), None),
                        Some(b'+') => (None, Some(new)),
                        _ => continue,
                    };
                    old += usize::from(old_line.is_some());
                    new += usize::from(new_line.is_some());
                    result.push(SourceLine {
                        row,
                        location: HunkLocation {
                            path: change.path.clone(),
                            old_line: old_line.filter(|_| !change.old.kind.is_empty()),
                            new_line: new_line.filter(|_| !change.new.kind.is_empty()),
                        },
                        split_column: None,
                    });
                }
            }
        }
        HunkFormat::Difftastic => {
            for hunk in hunks {
                for row in hunk.rows.start + 1..hunk.rows.end {
                    let line = &text.lines[row];
                    let gutters = gutters(line);
                    let mut location = HunkLocation {
                        path: change.path.clone(),
                        old_line: None,
                        new_line: None,
                    };
                    for gutter in &gutters {
                        let Some(number) = gutter.number else {
                            continue;
                        };
                        match gutter.side(change) {
                            DiffSide::Old if !change.old.kind.is_empty() => {
                                location.old_line = Some(number);
                            }
                            DiffSide::New if !change.new.kind.is_empty() => {
                                location.new_line = Some(number);
                            }
                            _ => {}
                        }
                    }
                    if location.old_line.is_some() || location.new_line.is_some() {
                        let split_column = gutters
                            .get(1)
                            .filter(|gutter| gutter.offset > gutters[0].end)
                            .map(|gutter| Span::raw(&line.to_string()[..gutter.offset]).width());
                        result.push(SourceLine {
                            row,
                            location,
                            split_column,
                        });
                    }
                }
            }
        }
        HunkFormat::Auto | HunkFormat::None => {}
    }
    for line in &mut result {
        line.location.old_line = line
            .location
            .old_line
            .filter(|number| *number <= line_counts.0);
        line.location.new_line = line
            .location
            .new_line
            .filter(|number| *number <= line_counts.1);
    }
    result.retain(|line| line.location.old_line.is_some() || line.location.new_line.is_some());
    result
}

pub(crate) fn unified_ranges(line: &str) -> Option<(Range<usize>, Range<usize>)> {
    let mut fields = line.strip_prefix("@@ ")?.split_whitespace();
    let old = range(fields.next()?.strip_prefix('-')?)?;
    let new = range(fields.next()?.strip_prefix('+')?)?;
    if fields.next()? != "@@" {
        return None;
    }
    Some((old, new))
}

fn range(value: &str) -> Option<Range<usize>> {
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start: usize = start.parse().ok()?;
    let count: usize = count.parse().ok()?;
    let start = if count == 0 {
        start
    } else {
        start.checked_sub(1)?
    };
    Some(start..start.checked_add(count)?)
}

pub fn parse(text: &Text<'_>, change: &Change, format: HunkFormat) -> Vec<Hunk> {
    match format {
        HunkFormat::Unified => unified(text, change),
        HunkFormat::Difftastic => difftastic(text, change),
        HunkFormat::Auto | HunkFormat::None => Vec::new(),
    }
}

fn unified(text: &Text<'_>, change: &Change) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    for (row, line) in text.lines.iter().enumerate() {
        let Some((old, new)) = unified_ranges(&line.to_string()) else {
            continue;
        };
        let mut old_cursor = old.start + 1;
        let mut new_cursor = new.start + 1;
        let mut old_line = None;
        let mut new_line = None;
        let mut deletion_line = None;
        let mut end = row + 1;
        let mut old_remaining = old.len();
        let mut new_remaining = new.len();
        for (offset, line) in text.lines[row + 1..].iter().enumerate() {
            let line = line.to_string();
            if line.starts_with("\\ No newline") {
                end = row + offset + 2;
                continue;
            }
            if old_remaining == 0 && new_remaining == 0 {
                break;
            }
            match line.as_bytes().first() {
                Some(b' ') if old_remaining > 0 && new_remaining > 0 => {
                    old_cursor += 1;
                    new_cursor += 1;
                    old_remaining -= 1;
                    new_remaining -= 1;
                }
                Some(b'-') if old_remaining > 0 => {
                    old_line.get_or_insert(old_cursor);
                    deletion_line.get_or_insert(new_cursor);
                    old_cursor += 1;
                    old_remaining -= 1;
                }
                Some(b'+') if new_remaining > 0 => {
                    new_line.get_or_insert(new_cursor);
                    new_cursor += 1;
                    new_remaining -= 1;
                }
                _ => {
                    break;
                }
            }
            end = row + offset + 2;
        }
        if old_remaining != 0 || new_remaining != 0 {
            continue;
        }
        hunks.push(Hunk {
            rows: row..end,
            location: HunkLocation {
                path: change.path.clone(),
                old_line: if change.old.kind.is_empty() {
                    None
                } else {
                    old_line.or(Some((old.start + 1).max(1)))
                },
                new_line: if change.new.kind.is_empty() {
                    None
                } else {
                    new_line.or(deletion_line).or(Some((new.start + 1).max(1)))
                },
            },
        });
    }
    hunks
}

pub(crate) fn difftastic_title(line: &str) -> Option<(&str, &str)> {
    let (prefix, language) = line.rsplit_once(" --- ")?;
    if language.is_empty() {
        return None;
    }
    let path = if let Some((path, part)) = prefix.rsplit_once(" --- ") {
        if part.split_once('/').is_some_and(|(part, count)| {
            part.parse::<usize>().is_ok() && count.parse::<usize>().is_ok()
        }) {
            path
        } else {
            prefix
        }
    } else {
        prefix
    };
    Some((path, language))
}

struct Gutter {
    number: Option<usize>,
    changed: Option<DiffSide>,
    offset: usize,
    end: usize,
}

impl Gutter {
    fn side(&self, change: &Change) -> DiffSide {
        if let Some(side) = self.changed {
            side
        } else if change.old.kind.is_empty() || self.offset > 0 {
            DiffSide::New
        } else {
            DiffSide::Old
        }
    }
}

fn gutters(line: &Line<'_>) -> Vec<Gutter> {
    let mut result: Vec<Gutter> = Vec::new();
    let mut offset = 0;
    let mut previous = "";
    for span in &line.spans {
        let content = span.content.as_ref();
        let end = offset + content.len();
        // Difftastic styles gutters separately from the source. Unchanged
        // numbers and missing-line markers are dim; changed numbers use color.
        let changed = match span.style.fg {
            Some(Color::Red | Color::LightRed) => Some(DiffSide::Old),
            Some(Color::Green | Color::LightGreen) => Some(DiffSide::New),
            _ => None,
        };
        let dim = span.style.add_modifier.contains(Modifier::DIM);
        if let Some(field) = content
            .strip_suffix(' ')
            .map(|field| field.trim_start_matches(' '))
            && (dim || changed.is_some())
            && (field.bytes().all(|byte| byte.is_ascii_digit())
                || (dim && field.bytes().all(|byte| byte == b'.')))
        {
            let gutter = Gutter {
                number: field.parse().ok().filter(|number| *number > 0),
                changed,
                offset,
                end,
            };
            if let Some(first) = result.first() {
                // The old gutter starts at column zero. The new gutter follows
                // either immediately (shared content) or after padded old content.
                if first.offset == 0
                    && first.changed != Some(DiffSide::New)
                    && (dim
                        || (gutter.changed == Some(DiffSide::New)
                            && span.style.add_modifier.contains(Modifier::BOLD)))
                    && (offset == first.end || previous.ends_with("  "))
                {
                    result.push(gutter);
                    break;
                }
            } else if offset == 0 || (previous == "   " && offset == 3) {
                result.push(gutter);
            }
        }
        previous = content;
        offset = end;
    }
    result
}

fn difftastic(text: &Text<'_>, change: &Change) -> Vec<Hunk> {
    let lines: Vec<_> = text.lines.iter().map(ToString::to_string).collect();
    let Some((path, language)) = lines.iter().find_map(|line| difftastic_title(line)) else {
        return Vec::new();
    };
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            (difftastic_title(line) == Some((path, language))).then_some(index)
        })
        .collect();
    let mut hunks = Vec::new();
    for (index, start) in starts.iter().copied().enumerate() {
        let mut end = starts.get(index + 1).copied().unwrap_or(lines.len());
        while end > start + 1 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let mut old_line = None;
        let mut new_line = None;
        let mut first_old = None;
        let mut first_new = None;
        let mut last_new: Option<usize> = None;
        let mut deletion = None;
        for line in &text.lines[start + 1..end] {
            for gutter in gutters(line) {
                let Some(number) = gutter.number else {
                    continue;
                };
                match (gutter.changed, gutter.side(change)) {
                    (Some(_), DiffSide::New) => {
                        new_line.get_or_insert(number);
                    }
                    (Some(_), DiffSide::Old) => {
                        old_line.get_or_insert(number);
                        if deletion.is_none() {
                            deletion = last_new.map(|line| line + 1);
                        }
                    }
                    (None, DiffSide::New) => {
                        first_new.get_or_insert(number);
                        last_new = Some(number);
                    }
                    (None, DiffSide::Old) => {
                        first_old.get_or_insert(number);
                    }
                }
            }
            // Plain output still offers a trustworthy old-side hunk start.
            if first_old.is_none()
                && let Some(number) = line
                    .to_string()
                    .split_whitespace()
                    .next()
                    .and_then(|token| token.parse::<usize>().ok())
            {
                first_old = Some(number);
            }
        }
        let old_line = old_line.or(first_old);
        let new_line = new_line.or(deletion).or(first_new);
        if old_line.is_none() && new_line.is_none() {
            continue;
        }
        hunks.push(Hunk {
            rows: start..end,
            location: HunkLocation {
                path: change.path.clone(),
                old_line: if change.old.kind.is_empty() {
                    None
                } else {
                    old_line
                },
                new_line: if change.new.kind.is_empty() {
                    None
                } else {
                    new_line
                },
            },
        });
    }
    hunks
}
