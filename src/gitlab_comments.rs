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
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discussion {
    pub id: String,
    pub notes: Vec<DiscussionNote>,
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

    pub(crate) fn position(&self) -> Option<&Value> {
        self.comments().find_map(|note| note.position.as_ref())
    }

    pub(crate) fn path(&self) -> Option<&str> {
        let position = self.position()?;
        let side = if position["new_line"].is_null() && !position["old_line"].is_null() {
            "old_path"
        } else {
            "new_path"
        };
        position[side]
            .as_str()
            .filter(|path| !path.is_empty())
            .or_else(|| {
                position["old_path"]
                    .as_str()
                    .filter(|path| !path.is_empty())
            })
    }

    pub(crate) fn is_outdated(&self, review: &Review) -> bool {
        let Some(position) = self.position() else {
            return false;
        };
        position["base_sha"].as_str() != Some(review.comparison.base_id.as_str())
            || position["head_sha"].as_str() != Some(review.comparison.head_id.as_str())
            || review
                .gitlab
                .as_ref()
                .and_then(|gitlab| gitlab.merge_request.diff_refs.as_ref())
                .is_some_and(|refs| position["start_sha"].as_str() != Some(refs.start_sha.as_str()))
    }

    pub(crate) fn location(&self) -> Option<LineTarget> {
        let position = self.position()?;
        if position["position_type"] != "text" {
            return None;
        }
        for (side, coordinate, path) in [
            (DiffSide::New, "new_line", "new_path"),
            (DiffSide::Old, "old_line", "old_path"),
        ] {
            let range = if let Some(range) =
                position.get("line_range").filter(|range| !range.is_null())
            {
                range_line(&range["start"], coordinate).zip(range_line(&range["end"], coordinate))
            } else {
                line(&position[coordinate]).map(|line| (line, line))
            };
            if let Some((start, end)) = range.filter(|(start, end)| end >= start) {
                return Some(LineTarget {
                    path: position[path].as_str()?.to_owned(),
                    side,
                    start_line: start,
                    end_line: end,
                });
            }
        }
        // A range spanning removed and added lines can still jump to its final line.
        let (side, coordinate, path) = if line(&position["new_line"]).is_some() {
            (DiffSide::New, "new_line", "new_path")
        } else {
            (DiffSide::Old, "old_line", "old_path")
        };
        let number = line(&position[coordinate])?;
        Some(LineTarget {
            path: position[path].as_str()?.to_owned(),
            side,
            start_line: number,
            end_line: number,
        })
    }
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
                if discussion.is_outdated(review) {
                    return None;
                }
                let mut target = discussion.location()?;
                let change = review.changes.iter().find(|change| match target.side {
                    DiffSide::Old => change.old_path == target.path && !change.old.kind.is_empty(),
                    DiffSide::New => change.path == target.path && !change.new.kind.is_empty(),
                })?;
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
