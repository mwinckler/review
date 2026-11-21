//! Saved GitLab discussions and their mapping onto the reviewed snapshots.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    comment_dialog::LineTarget,
    comments::{Comment, DiffSide, comment_id, validate_body},
    gitlab::User,
    state::Review,
    store::now_millis,
    vcs::{Change, Comparison},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discussion {
    pub id: String,
    pub notes: Vec<DiscussionNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pinned_position: Option<PinnedPosition>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PinnedPosition {
    commit: String,
    #[serde(flatten)]
    target: LineTarget,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscussionNote {
    pub id: u64,
    pub body: String,
    pub author: User,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub resolvable: bool,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub position: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscussionReply {
    pub id: String,
    pub discussion_id: String,
    pub body: String,
    pub created_at: u64,
    pub updated_at: u64,
}

impl DiscussionReply {
    pub(crate) fn new(discussion_id: String, body: String) -> Result<Self> {
        validate_body(&body)?;
        let now = now_millis()?;
        Ok(Self {
            id: comment_id()?,
            discussion_id,
            body,
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadId {
    Local(String),
    GitLab(String),
}

impl ThreadId {
    pub(crate) fn entry<'a>(&self, review: &'a Review) -> Option<CommentEntry<'a>> {
        match self {
            Self::Local(id) => {
                let comment = review.comments.iter().find(|comment| &comment.id == id)?;
                Some(
                    review
                        .published_discussion(comment)
                        .map_or(CommentEntry::Local(comment), CommentEntry::GitLab),
                )
            }
            Self::GitLab(id) => review
                .gitlab
                .as_ref()?
                .discussions
                .iter()
                .find(|discussion| &discussion.id == id && discussion.comments().next().is_some())
                .map(CommentEntry::GitLab),
        }
    }
}

impl Discussion {
    pub(crate) fn comments(&self) -> impl Iterator<Item = &DiscussionNote> {
        self.notes.iter().filter(|note| !note.system)
    }

    fn position(&self) -> Option<&Value> {
        self.comments().find_map(|note| note.position.as_ref())
    }

    fn review_position(&self, comparison: &Comparison) -> Option<LineTarget> {
        let position = self.position()?;
        if same_snapshots(position, comparison) {
            return text_location(position, |_| true);
        }
        self.pinned_position
            .as_ref()
            .filter(|position| position.commit == snapshot(comparison, position.target.side))
            .map(|position| position.target.clone())
            .or_else(|| text_location(position, |side| same_snapshot(position, comparison, side)))
    }

    pub(crate) fn retain_position(&mut self, previous: Option<&Self>, comparison: &Comparison) {
        self.pinned_position = None;
        if let Some(position) = self.position()
            && position["position_type"] == "text"
            && !same_snapshots(position, comparison)
            && let Some(target) = previous.and_then(|previous| previous.review_position(comparison))
        {
            self.pin(comparison, target);
        }
    }

    pub(crate) fn unmapped_position(&self, comparison: &Comparison) -> Option<(&str, LineTarget)> {
        if self.review_position(comparison).is_some() {
            return None;
        }
        let position = self.position()?;
        let target = text_location(position, |_| true)?;
        let commit = position[commit_field(target.side)].as_str()?;
        Some((commit, target))
    }

    pub(crate) fn pin(&mut self, comparison: &Comparison, target: LineTarget) {
        self.pinned_position = Some(PinnedPosition {
            commit: snapshot(comparison, target.side).into(),
            target,
        });
    }

    pub(crate) fn path(&self) -> Option<&str> {
        let position = self.position()?;
        let side = if position["new_line"].is_null() && !position["old_line"].is_null() {
            DiffSide::Old
        } else {
            DiffSide::New
        };
        position[path_field(side)]
            .as_str()
            .filter(|path| !path.is_empty())
            .or_else(|| {
                position["old_path"]
                    .as_str()
                    .filter(|path| !path.is_empty())
            })
    }

    pub(crate) fn is_outdated(&self, review: &Review) -> bool {
        self.position().is_some_and(|position| {
            !same_snapshots(position, &review.comparison)
                && self.review_position(&review.comparison).is_none()
        })
    }

    pub(crate) fn location(&self, review: &Review) -> Option<LineTarget> {
        self.review_position(&review.comparison)
            .or_else(|| text_location(self.position()?, |_| true))
    }
}

fn same_snapshots(position: &Value, comparison: &Comparison) -> bool {
    same_snapshot(position, comparison, DiffSide::Old)
        && same_snapshot(position, comparison, DiffSide::New)
}

fn same_snapshot(position: &Value, comparison: &Comparison, side: DiffSide) -> bool {
    position[commit_field(side)].as_str() == Some(snapshot(comparison, side))
}

fn snapshot(comparison: &Comparison, side: DiffSide) -> &str {
    match side {
        DiffSide::Old => &comparison.base_id,
        DiffSide::New => &comparison.head_id,
    }
}

fn commit_field(side: DiffSide) -> &'static str {
    match side {
        DiffSide::Old => "base_sha",
        DiffSide::New => "head_sha",
    }
}

fn path_field(side: DiffSide) -> &'static str {
    match side {
        DiffSide::Old => "old_path",
        DiffSide::New => "new_path",
    }
}

pub(crate) fn matching_change<'a>(
    changes: &'a [Change],
    target: &LineTarget,
) -> Option<&'a Change> {
    changes.iter().find(|change| match target.side {
        DiffSide::Old => change.old_path == target.path && !change.old.kind.is_empty(),
        DiffSide::New => change.path == target.path && !change.new.kind.is_empty(),
    })
}

fn text_location(position: &Value, accept: impl Fn(DiffSide) -> bool) -> Option<LineTarget> {
    if position["position_type"] != "text" {
        return None;
    }
    let sides = [(DiffSide::New, "new_line"), (DiffSide::Old, "old_line")];
    let target = |side, start_line, end_line| {
        let path = position[path_field(side)].as_str()?;
        Some(LineTarget {
            path: path.to_owned(),
            side,
            start_line,
            end_line,
        })
    };
    if let Some(range) = position.get("line_range").filter(|range| !range.is_null()) {
        for (side, coordinate) in sides {
            if accept(side)
                && let Some((start, end)) = range_line(&range["start"], coordinate)
                    .zip(range_line(&range["end"], coordinate))
                && end >= start
                && let Some(target) = target(side, start, end)
            {
                return Some(target);
            }
        }
    }
    // A range spanning removed and added lines can still jump to its final line.
    sides.into_iter().find_map(|(side, coordinate)| {
        let number = line(&position[coordinate]).filter(|_| accept(side))?;
        target(side, number, number)
    })
}

fn line(value: &Value) -> Option<usize> {
    usize::try_from(value.as_u64()?)
        .ok()
        .filter(|line| *line > 0)
}

fn range_line(endpoint: &Value, coordinate: &str) -> Option<usize> {
    line(&endpoint[coordinate]).or_else(|| {
        let side = if coordinate == "new_line" {
            "new"
        } else {
            "old"
        };
        if endpoint["type"] != side {
            return None;
        }
        let (_, numbers) = endpoint["line_code"].as_str()?.split_once('_')?;
        let (old, new) = numbers.split_once('_')?;
        let number = if side == "new" { new } else { old };
        number.parse::<usize>().ok().filter(|line| *line > 0)
    })
}

#[derive(Clone, Copy)]
pub(crate) enum CommentEntry<'a> {
    Local(&'a Comment),
    GitLab(&'a Discussion),
}

impl<'a> CommentEntry<'a> {
    pub(crate) fn id(self) -> ThreadId {
        match self {
            Self::Local(comment) => ThreadId::Local(comment.id.clone()),
            Self::GitLab(discussion) => ThreadId::GitLab(discussion.id.clone()),
        }
    }
    pub(crate) fn local(self, review: &'a Review) -> Option<&'a Comment> {
        match self {
            Self::Local(comment) => Some(comment),
            Self::GitLab(discussion) => review.comments.iter().find(|comment| {
                review
                    .published_discussion(comment)
                    .is_some_and(|published| published.id == discussion.id)
            }),
        }
    }

    pub(crate) fn target(self, review: &Review) -> Option<LineTarget> {
        match self {
            Self::Local(comment) => Some(LineTarget {
                path: comment.anchor.change(&review.changes)?.path.clone(),
                side: comment.anchor.side,
                start_line: comment.anchor.start_line,
                end_line: comment.anchor.end_line,
            }),
            Self::GitLab(discussion) => {
                let mut target = discussion.review_position(&review.comparison)?;
                let change = matching_change(&review.changes, &target)?;
                target.path.clone_from(&change.path);
                Some(target)
            }
        }
    }

    pub(crate) fn path(self, review: &'a Review) -> Option<&'a str> {
        match self {
            Self::Local(comment) => Some(
                comment
                    .anchor
                    .change(&review.changes)
                    .map(|change| change.path.as_str())
                    .unwrap_or(&comment.anchor.path),
            ),
            Self::GitLab(discussion) => {
                if let Some(target) = discussion.review_position(&review.comparison)
                    && let Some(change) = matching_change(&review.changes, &target)
                {
                    return Some(&change.path);
                }
                let path = discussion.path()?;
                Some(
                    review
                        .changes
                        .iter()
                        .find(|change| change.path == path || change.old_path == path)
                        .map(|change| change.path.as_str())
                        .unwrap_or(path),
                )
            }
        }
    }
}

impl Review {
    fn published_discussion(&self, comment: &Comment) -> Option<&Discussion> {
        let gitlab = self.gitlab.as_ref()?;
        let id = gitlab.publication.comments.get(&comment.id)?.note_id?;
        gitlab
            .discussions
            .iter()
            .find(|discussion| discussion.comments().any(|note| note.id == id))
    }

    pub(crate) fn comment_entries(&self) -> impl Iterator<Item = CommentEntry<'_>> {
        let discussions = self.gitlab.iter().flat_map(|gitlab| &gitlab.discussions);
        let local = self
            .comments
            .iter()
            .filter(|comment| self.published_discussion(comment).is_none());
        local.map(CommentEntry::Local).chain(
            discussions
                .filter(|discussion| discussion.comments().next().is_some())
                .map(CommentEntry::GitLab),
        )
    }
}
