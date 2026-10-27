use std::{ops::Range, path::Path};

use ratatui::{
    style::Color,
    text::{Line, Text},
};
use serde::{Deserialize, Serialize};

use crate::vcs::Change;

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

fn title(line: &str) -> Option<(&str, &str)> {
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

fn cells(line: &Line<'_>) -> Vec<(Option<usize>, Option<Color>, usize)> {
    let mut result = Vec::new();
    let mut offset = 0;
    for span in &line.spans {
        if span.content.ends_with(' ')
            && span.content.split_whitespace().all(|token| {
                token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
            })
        {
            for token in span.content.split_whitespace() {
                result.push((token.parse().ok(), span.style.fg, offset));
            }
        }
        offset += span.content.len();
    }
    result.truncate(2);
    result
}

fn difftastic(text: &Text<'_>, change: &Change) -> Vec<Hunk> {
    let lines: Vec<_> = text.lines.iter().map(ToString::to_string).collect();
    let Some((path, language)) = lines.iter().find_map(|line| title(line)) else {
        return Vec::new();
    };
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (title(line) == Some((path, language))).then_some(index))
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
            let cells = cells(line);
            for (index, &(number, color, offset)) in cells.iter().enumerate() {
                let Some(number) = number.filter(|number| *number > 0) else {
                    continue;
                };
                match color {
                    Some(Color::Green | Color::LightGreen) => {
                        new_line.get_or_insert(number);
                    }
                    Some(Color::Red | Color::LightRed) => {
                        old_line.get_or_insert(number);
                        if deletion.is_none() {
                            deletion = last_new.map(|line| line + 1);
                        }
                    }
                    _ => {
                        if (cells.len() > 1 && index == 1)
                            || (cells.len() == 1 && offset > 0)
                            || change.old.kind.is_empty()
                        {
                            first_new.get_or_insert(number);
                            last_new = Some(number);
                        } else {
                            first_old.get_or_insert(number);
                        }
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
