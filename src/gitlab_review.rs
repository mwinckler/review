//! Native GitLab draft reviews, with durable write intents and remote receipts.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::{
    comments::{Comment, DiffSide},
    gitlab::{DiffRefs, GitLab, GitLabReview, User},
    state::Review,
    store::{SavedSession, SessionLock, now_millis},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Publication {
    pub user_id: Option<u64>,
    pub comments: BTreeMap<String, RemoteComment>,
    pub attempt: Option<ReviewAttempt>,
}

impl Publication {
    pub(crate) fn uncertain(&self) -> bool {
        self.comments.values().any(|receipt| receipt.uncertain)
            || self
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.publishing || attempt.summary_uncertain)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteComment {
    pub marker: String,
    pub note: String,
    pub position: Value,
    pub draft_id: Option<u64>,
    pub note_id: Option<u64>,
    pub discussion_id: Option<String>,
    pub uncertain: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewAttempt {
    pub id: String,
    pub summary: String,
    pub comment_ids: Vec<String>,
    pub publishing: bool,
    #[serde(default)]
    pub summary_draft_id: Option<u64>,
    #[serde(default)]
    pub summary_uncertain: bool,
}

impl ReviewAttempt {
    fn marker(&self) -> String {
        format!("<!-- review-batch:{} -->", self.id)
    }

    fn note(&self) -> String {
        format!("{}\n\n{}", self.summary, self.marker())
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PreparedNote {
    id: String,
    marker: String,
    note: String,
    position: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubmissionPreview {
    pub summary: String,
    pub comment_ids: Vec<String>,
    pub external_drafts: usize,
    user_id: u64,
    refs: DiffRefs,
    notes: Vec<PreparedNote>,
    foreign: Vec<DraftNote>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct DraftNote {
    id: u64,
    note: String,
    #[serde(default)]
    position: Option<Value>,
}

#[derive(Deserialize)]
struct Discussion {
    id: String,
    notes: Vec<RemoteNote>,
}

#[derive(Deserialize)]
struct RemoteNote {
    id: u64,
    body: String,
    author: User,
}

#[derive(Deserialize)]
struct DiffVersion {
    id: u64,
    base_commit_sha: String,
    head_commit_sha: String,
    start_commit_sha: String,
}

#[derive(Deserialize)]
struct VersionDiffs {
    diffs: Vec<GitLabDiff>,
}

#[derive(Deserialize)]
struct GitLabDiff {
    old_path: String,
    new_path: String,
    #[serde(default)]
    diff: String,
}

pub fn pending_comments(review: &Review) -> impl Iterator<Item = &Comment> {
    review.comments.iter().filter(|comment| {
        review
            .gitlab
            .as_ref()
            .and_then(|gitlab| gitlab.publication.comments.get(&comment.id))
            .is_none_or(|receipt| receipt.note_id.is_none())
    })
}

/// Acknowledgment is explicit: an uncertain write may already be visible on GitLab.
pub fn acknowledge_retry(review: &mut Review) {
    if let Some(gitlab) = &mut review.gitlab {
        for receipt in gitlab.publication.comments.values_mut() {
            if receipt.uncertain {
                receipt.uncertain = false;
                receipt.draft_id = None;
            }
        }
        if let Some(attempt) = &mut gitlab.publication.attempt {
            attempt.publishing = false;
            if attempt.summary_uncertain {
                attempt.summary_uncertain = false;
                attempt.summary_draft_id = None;
            }
        }
    }
}

impl GitLab {
    pub fn preview_submission(
        &self,
        saved: &mut SavedSession,
        lock: &SessionLock,
        summary: &str,
    ) -> Result<SubmissionPreview> {
        let context = saved
            .review
            .gitlab
            .as_ref()
            .context("Open a GitLab MR to submit a review")?
            .clone();
        let user: User = self.get("user")?;
        ensure!(
            context.publication.user_id.is_none_or(|id| id == user.id),
            "This submission belongs to another GitLab account; authenticate as that account or start a new review session"
        );
        let endpoint = mr_endpoint(&context);
        let drafts: Vec<DraftNote> = self.list(&format!("{endpoint}/draft_notes?per_page=100"))?;
        let discussions: Vec<Discussion> =
            self.list(&format!("{endpoint}/discussions?per_page=100"))?;
        let publication = &mut saved
            .review
            .gitlab
            .as_mut()
            .expect("MR session")
            .publication;
        publication.user_id = Some(user.id);
        let reconciled = reconcile(publication, &drafts, &discussions, user.id);
        lock.save(saved)?;
        ensure!(
            !reconciled?,
            "The previous review was published successfully; its receipts are saved"
        );
        let context = saved.review.gitlab.as_ref().expect("MR session");
        ensure!(
            !context
                .publication
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.publishing),
            "Previous publication has an uncertain outcome. Recheck with Ctrl-s; after verifying the MR on GitLab, use X to acknowledge a retry"
        );
        ensure!(
            !context
                .publication
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.summary_uncertain),
            "The summary write has an uncertain outcome. Recheck with Ctrl-s; after verifying the MR on GitLab, use X to acknowledge a retry"
        );
        let current = self.merge_request(context.merge_request.iid)?;
        let refs = current.refs()?.clone();
        ensure!(
            current.same_version(&context.merge_request)?
                && saved.review.comparison.base_id == refs.base_sha
                && saved.review.comparison.head_id == refs.head_sha,
            "MR changed since this review. Close this dialog and refresh with r before submitting"
        );
        let versions: Vec<DiffVersion> = self.list(&format!("{endpoint}/versions?per_page=100"))?;
        let version = versions
            .iter()
            .find(|version| {
                version.base_commit_sha == refs.base_sha
                    && version.head_commit_sha == refs.head_sha
                    && version.start_commit_sha == refs.start_sha
            })
            .context("GitLab's reviewed diff version is unavailable; refresh the MR")?;
        let diff: VersionDiffs = self.get(&format!("{endpoint}/versions/{}", version.id))?;
        let mut notes = Vec::new();
        for comment in pending_comments(&saved.review) {
            ensure!(
                !comment.anchor.is_outdated(&saved.review.changes),
                "{}:{} is outdated; update the local comment before submitting",
                comment.anchor.path,
                comment.anchor.start_line
            );
            let position = position(comment, &refs, &diff.diffs).with_context(|| {
                format!(
                    "Comment at {}:{}",
                    comment.anchor.path, comment.anchor.start_line
                )
            })?;
            let hash = blake3::hash(&serde_json::to_vec(&(&comment.body, &position))?).to_hex();
            let marker = format!("<!-- review:{}:{} -->", comment.id, &hash[..16]);
            let note = format!("{}\n\n{marker}", comment.body);
            if let Some(receipt) = context.publication.comments.get(&comment.id) {
                ensure!(
                    receipt.note == note && receipt.position == position,
                    "A staged comment changed locally; restore its original text or reconcile it on GitLab"
                );
                ensure!(
                    !receipt.uncertain,
                    "A comment write has an uncertain outcome. Recheck with Ctrl-s; after verifying the MR on GitLab, use X to acknowledge a retry"
                );
            }
            notes.push(PreparedNote {
                id: comment.id.clone(),
                marker,
                note,
                position,
            });
        }
        ensure!(
            !notes.is_empty() || !summary.trim().is_empty(),
            "Every local comment has been published. Write a summary to submit another review"
        );
        let summary = if summary.trim().is_empty() {
            "Review completed.".to_owned()
        } else {
            summary.trim().to_owned()
        };
        ensure!(
            context
                .publication
                .attempt
                .as_ref()
                .is_none_or(
                    |attempt| attempt.summary_draft_id.is_none() || attempt.summary == summary
                ),
            "The summary is already staged on GitLab; restore its original text before continuing"
        );
        let mut foreign: Vec<_> = drafts
            .into_iter()
            .filter(|draft| {
                !context
                    .publication
                    .comments
                    .values()
                    .any(|receipt| receipt.draft_id == Some(draft.id))
                    && context
                        .publication
                        .attempt
                        .as_ref()
                        .is_none_or(|attempt| attempt.summary_draft_id != Some(draft.id))
            })
            .collect();
        foreign.sort_by_key(|draft| draft.id);
        Ok(SubmissionPreview {
            summary,
            comment_ids: notes.iter().map(|note| note.id.clone()).collect(),
            external_drafts: foreign.len(),
            user_id: user.id,
            refs,
            notes,
            foreign,
        })
    }

    pub fn submit_review(
        &self,
        saved: &mut SavedSession,
        lock: &SessionLock,
        approved: &SubmissionPreview,
    ) -> Result<String> {
        let current = self.preview_submission(saved, lock, &approved.summary)?;
        ensure!(
            &current == approved,
            "Pending comments or GitLab drafts changed; preview the review again before publishing"
        );
        let context = saved.review.gitlab.as_ref().expect("MR session");
        let endpoint = mr_endpoint(context);
        let mut attempt = context
            .publication
            .attempt
            .clone()
            .unwrap_or(ReviewAttempt {
                id: blake3::hash(&serde_json::to_vec(&(
                    &saved.repository,
                    &saved.id,
                    now_millis()?,
                    &approved.comment_ids,
                ))?)
                .to_hex()[..32]
                    .to_owned(),
                summary: approved.summary.clone(),
                comment_ids: approved.comment_ids.clone(),
                publishing: false,
                summary_draft_id: None,
                summary_uncertain: false,
            });
        attempt.summary.clone_from(&approved.summary);
        attempt.comment_ids.clone_from(&approved.comment_ids);
        saved
            .review
            .gitlab
            .as_mut()
            .expect("MR session")
            .publication
            .attempt = Some(attempt.clone());
        lock.save(saved)?;
        for note in &approved.notes {
            let publication = &mut saved
                .review
                .gitlab
                .as_mut()
                .expect("MR session")
                .publication;
            if publication
                .comments
                .get(&note.id)
                .is_some_and(|receipt| receipt.draft_id.is_some())
            {
                continue;
            }
            publication.comments.insert(
                note.id.clone(),
                RemoteComment {
                    marker: note.marker.clone(),
                    note: note.note.clone(),
                    position: note.position.clone(),
                    draft_id: None,
                    note_id: None,
                    discussion_id: None,
                    uncertain: true,
                },
            );
            // Persist the intent before sending a mutation; failed responses require reconciliation.
            lock.save(saved)?;
            let body = json!({"note":note.note,"position":note.position});
            let output = self.api(
                "POST",
                &format!("{endpoint}/draft_notes"),
                Some(&body),
                false,
            )?;
            let draft: DraftNote =
                serde_json::from_slice(&output).context("decoding the created GitLab draft")?;
            ensure!(
                draft.note == note.note
                    && draft
                        .position
                        .as_ref()
                        .is_some_and(|position| contains(position, &note.position)),
                "GitLab returned a different draft; recheck before continuing"
            );
            let receipt = saved
                .review
                .gitlab
                .as_mut()
                .expect("MR session")
                .publication
                .comments
                .get_mut(&note.id)
                .expect("write intent is saved");
            receipt.draft_id = Some(draft.id);
            receipt.uncertain = false;
            lock.save(saved)?;
        }
        if attempt.summary_draft_id.is_none() {
            // A regular draft summary works with both older and current bulk-publish APIs.
            attempt.summary_uncertain = true;
            saved
                .review
                .gitlab
                .as_mut()
                .expect("MR session")
                .publication
                .attempt = Some(attempt.clone());
            lock.save(saved)?;
            let body = json!({"note":attempt.note()});
            let output = self.api(
                "POST",
                &format!("{endpoint}/draft_notes"),
                Some(&body),
                false,
            )?;
            let draft: DraftNote =
                serde_json::from_slice(&output).context("decoding the GitLab summary draft")?;
            ensure!(
                draft.note == attempt.note(),
                "GitLab returned a different summary draft; recheck before continuing"
            );
            attempt.summary_draft_id = Some(draft.id);
            attempt.summary_uncertain = false;
            saved
                .review
                .gitlab
                .as_mut()
                .expect("MR session")
                .publication
                .attempt = Some(attempt.clone());
            lock.save(saved)?;
        }
        let drafts: Vec<DraftNote> = self.list(&format!("{endpoint}/draft_notes?per_page=100"))?;
        let publication = &saved
            .review
            .gitlab
            .as_ref()
            .expect("MR session")
            .publication;
        let mut foreign = Vec::new();
        for draft in &drafts {
            if let Some(receipt) = publication
                .comments
                .values()
                .find(|receipt| receipt.draft_id == Some(draft.id))
            {
                ensure!(
                    draft.note == receipt.note
                        && draft
                            .position
                            .as_ref()
                            .is_some_and(|position| contains(position, &receipt.position)),
                    "A staged GitLab comment changed; preview again before publishing"
                );
            } else if attempt.summary_draft_id == Some(draft.id) {
                ensure!(
                    draft.note == attempt.note(),
                    "The GitLab summary draft changed; preview again before publishing"
                );
            } else {
                foreign.push(draft.clone());
            }
        }
        foreign.sort_by_key(|draft| draft.id);
        ensure!(
            foreign == approved.foreign
                && drafts
                    .iter()
                    .any(|draft| Some(draft.id) == attempt.summary_draft_id)
                && approved.notes.iter().all(|note| publication
                    .comments
                    .get(&note.id)
                    .is_some_and(|receipt| drafts
                        .iter()
                        .any(|draft| Some(draft.id) == receipt.draft_id))),
            "GitLab drafts changed while preparing the review; preview again to confirm all pending drafts"
        );
        let reviewed = &saved
            .review
            .gitlab
            .as_ref()
            .expect("MR session")
            .merge_request;
        let latest = self.merge_request(reviewed.iid)?;
        ensure!(
            latest.same_version(reviewed)?,
            "MR changed while preparing the review. Drafts are saved on GitLab; finish the review there, then reopen the MR to review its latest version"
        );
        attempt.publishing = true;
        saved
            .review
            .gitlab
            .as_mut()
            .expect("MR session")
            .publication
            .attempt = Some(attempt.clone());
        lock.save(saved)?;
        let body = json!({"reviewer_state":"reviewed"});
        self.api(
            "POST",
            &format!("{endpoint}/draft_notes/bulk_publish"),
            Some(&body),
            false,
        )?;
        let discussions: Vec<Discussion> =
            self.list(&format!("{endpoint}/discussions?per_page=100"))?;
        let result = reconcile(
            &mut saved
                .review
                .gitlab
                .as_mut()
                .expect("MR session")
                .publication,
            &[],
            &discussions,
            approved.user_id,
        );
        lock.save(saved)?;
        ensure!(
            result?,
            "GitLab accepted the review; its published notes are not visible yet. Recheck with Ctrl-s before retrying"
        );
        Ok(format!(
            "Review published · {} comments",
            approved.notes.len()
        ))
    }
}

fn mr_endpoint(context: &GitLabReview) -> String {
    format!(
        "projects/{}/merge_requests/{}",
        context.merge_request.target_project_id, context.merge_request.iid
    )
}

fn reconcile(
    publication: &mut Publication,
    drafts: &[DraftNote],
    discussions: &[Discussion],
    user_id: u64,
) -> Result<bool> {
    for receipt in publication.comments.values_mut() {
        if receipt.note_id.is_some() {
            continue;
        }
        if let Some((discussion, note)) = discussions.iter().find_map(|discussion| {
            discussion
                .notes
                .iter()
                .find(|note| note.author.id == user_id && note.body.contains(&receipt.marker))
                .map(|note| (discussion, note))
        }) {
            receipt.note_id = Some(note.id);
            receipt.discussion_id = Some(discussion.id.clone());
            receipt.draft_id = None;
            receipt.uncertain = false;
        } else {
            let matches: Vec<_> = drafts
                .iter()
                .filter(|draft| draft.note.contains(&receipt.marker))
                .collect();
            ensure!(
                matches.len() <= 1,
                "Multiple matching GitLab drafts exist; inspect the MR before publishing"
            );
            if let Some(draft) = matches.first() {
                ensure!(
                    draft.note == receipt.note
                        && draft
                            .position
                            .as_ref()
                            .is_some_and(|position| contains(position, &receipt.position)),
                    "A GitLab draft was edited outside Review; inspect it before publishing"
                );
                receipt.draft_id = Some(draft.id);
                receipt.uncertain = false;
            } else if receipt.draft_id.is_some() {
                receipt.uncertain = true;
            }
        }
    }
    if let Some(attempt) = &publication.attempt
        && discussions
            .iter()
            .flat_map(|discussion| &discussion.notes)
            .any(|note| note.author.id == user_id && note.body.contains(&attempt.marker()))
    {
        ensure!(
            attempt.comment_ids.iter().all(|id| publication
                .comments
                .get(id)
                .is_some_and(|receipt| receipt.note_id.is_some())),
            "The review summary was published but some comment receipts are missing; inspect the MR before retrying"
        );
        publication.attempt = None;
        return Ok(true);
    }
    if let Some(attempt) = &mut publication.attempt {
        let matches: Vec<_> = drafts
            .iter()
            .filter(|draft| draft.note.contains(&attempt.marker()))
            .collect();
        ensure!(
            matches.len() <= 1,
            "Multiple matching summary drafts exist; inspect the MR before publishing"
        );
        if let Some(draft) = matches.first() {
            ensure!(
                draft.note == attempt.note(),
                "The GitLab summary draft was edited outside Review; inspect it before publishing"
            );
            attempt.summary_draft_id = Some(draft.id);
            attempt.summary_uncertain = false;
        } else if attempt.summary_draft_id.is_some() {
            attempt.summary_uncertain = true;
        }
    }
    Ok(false)
}

fn contains(actual: &Value, expected: &Value) -> bool {
    if let Some(expected) = expected.as_object() {
        expected.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|actual| contains(actual, value))
        })
    } else {
        actual == expected
    }
}

struct DiffLine<'a> {
    old_pos: usize,
    new_pos: usize,
    old_line: Option<usize>,
    new_line: Option<usize>,
    added: bool,
    text: &'a str,
}

impl DiffLine<'_> {
    fn numbers(&self, position: &mut Value) {
        if let Some(old) = self.old_line {
            position["old_line"] = json!(old);
        }
        if let Some(new) = self.new_line {
            position["new_line"] = json!(new);
        }
    }

    fn endpoint(&self, path: &str) -> Value {
        let mut value = json!({"line_code":format!("{:x}_{}_{}", Sha1::digest(path.as_bytes()), self.old_pos, self.new_pos),"type":if self.added { "new" } else { "old" }});
        self.numbers(&mut value);
        value
    }
}

fn position(comment: &Comment, refs: &DiffRefs, diffs: &[GitLabDiff]) -> Result<Value> {
    let anchor = &comment.anchor;
    let diff = diffs
        .iter()
        .find(|diff| match anchor.side {
            DiffSide::Old => diff.old_path == anchor.path,
            DiffSide::New => diff.new_path == anchor.path,
        })
        .context("File is absent from GitLab's diff")?;
    let mut old = 0;
    let mut new = 0;
    let mut in_hunk = false;
    let mut selected = BTreeMap::new();
    for line in diff.diff.lines() {
        if let Some(header) = line.strip_prefix("@@ -") {
            let (old_range, rest) = header
                .split_once(" +")
                .context("Invalid GitLab diff hunk")?;
            old = old_range
                .split(',')
                .next()
                .context("Missing old hunk position")?
                .parse::<usize>()?;
            new = rest
                .split_whitespace()
                .next()
                .context("Missing new hunk position")?
                .split(',')
                .next()
                .context("Missing new hunk position")?
                .parse::<usize>()?;
            in_hunk = true;
            continue;
        }
        if !in_hunk || line.starts_with('\\') {
            continue;
        }
        let prefix = line.as_bytes().first().copied();
        ensure!(
            matches!(prefix, Some(b' ' | b'+' | b'-')),
            "Invalid source line in GitLab diff"
        );
        let source = DiffLine {
            old_pos: old,
            new_pos: new,
            old_line: (prefix != Some(b'+')).then_some(old),
            new_line: (prefix != Some(b'-')).then_some(new),
            added: prefix == Some(b'+'),
            text: &line[1..],
        };
        let number = match anchor.side {
            DiffSide::Old => source.old_line,
            DiffSide::New => source.new_line,
        };
        if let Some(number) = number
            && (anchor.start_line..=anchor.end_line).contains(&number)
        {
            selected.insert(number, source);
        }
        if prefix != Some(b'+') {
            old += 1;
        }
        if prefix != Some(b'-') {
            new += 1;
        }
    }
    let lines = (anchor.start_line..=anchor.end_line)
        .map(|number| {
            selected.get(&number).with_context(|| {
                format!("Line {number} is outside GitLab's diff; choose a changed or context line")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        lines
            .iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n")
            == anchor.excerpt,
        "GitLab's source differs from the comment's recorded excerpt"
    );
    let first = lines.first().context("Comment has an empty source range")?;
    let last = lines.last().expect("nonempty source range");
    let mut position = json!({"position_type":"text","base_sha":refs.base_sha,"head_sha":refs.head_sha,"start_sha":refs.start_sha,"old_path":diff.old_path,"new_path":diff.new_path});
    last.numbers(&mut position);
    if lines.len() > 1 {
        let path = if diff.new_path.is_empty() {
            &diff.old_path
        } else {
            &diff.new_path
        };
        position["line_range"] = json!({"start":first.endpoint(path),"end":last.endpoint(path)});
    }
    Ok(position)
}
