use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, ensure};

use crate::{
    gitlab::{Checkout, GitLab},
    gitlab_review::{ReviewDecision, SubmissionCheck, SubmissionPreview},
    store::{SavedSession, SessionLock},
    vcs::Repository,
};

pub enum GitLabJob {
    Refresh,
    Discussions,
    Preview {
        summary: String,
        decision: ReviewDecision,
    },
    Publish(Box<SubmissionPreview>),
}

pub enum GitLabOutcome {
    Refreshed(usize),
    DiscussionsLoaded,
    Preview(SubmissionPreview),
    Published(String),
}

pub struct GitLabReply {
    pub saved: SavedSession,
    pub result: Result<GitLabOutcome, String>,
}

#[derive(Default)]
pub struct GitLabWorker {
    thread: Option<JoinHandle<GitLabReply>>,
}

impl GitLabWorker {
    /// The UI pauses editing and saving while this worker owns the session writer.
    pub fn start(
        &mut self,
        repo: &Repository,
        mut saved: SavedSession,
        lock: Arc<SessionLock>,
        job: GitLabJob,
    ) -> Result<()> {
        ensure!(
            self.thread.is_none(),
            "A GitLab operation is already running"
        );
        GitLab::require_cli(&repo.root)?;
        let repo = repo.clone();
        self.thread = Some(thread::Builder::new().name("gitlab".into()).spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| -> Result<GitLabOutcome> {
                let context = saved.review.gitlab.as_ref().context("Open a GitLab MR first")?.clone();
                let client = GitLab::new(&repo, &context.project);
                match job {
                    GitLabJob::Discussions => {
                        client.load_discussions(
                            saved.review.gitlab.as_mut().expect("MR session"),
                            &saved.review.comparison,
                            &saved.review.changes,
                        )
                            .context("Reloading comments (cached discussions retained)")?;
                        lock.save(&mut saved)?;
                        Ok(GitLabOutcome::DiscussionsLoaded)
                    }
                    GitLabJob::Refresh => {
                        ensure!(context.publication.attempt.is_none(), "A pending draft review is saved. Finish it in GitLab or start a fresh MR review; P rechecks its receipts");
                        let checkout = if context.conductor_managed {
                            Checkout::PreserveWorkspace
                        } else {
                            Checkout::Direct
                        };
                        let mut prepared = client.prepare(&repo, &context.remote, context.merge_request.iid, checkout)?;
                        prepared.gitlab.publication = context.publication;
                        prepared.gitlab.replies = context.replies;
                        let invalidated = saved.review.reconcile(prepared.comparison, prepared.changes);
                        saved.review.gitlab = Some(prepared.gitlab);
                        lock.save(&mut saved)?;
                        Ok(GitLabOutcome::Refreshed(invalidated))
                    }
                    GitLabJob::Preview { summary, decision } => {
                        Ok(match client.preview_submission(&mut saved, &lock, &summary, decision)? {
                            SubmissionCheck::Ready(preview) => GitLabOutcome::Preview(preview),
                            SubmissionCheck::Published(message) => GitLabOutcome::Published(message),
                        })
                    }
                    GitLabJob::Publish(preview) => client.submit_review(&mut saved, &lock, &preview).map(GitLabOutcome::Published),
                }
            })).map_err(|_| "GitLab worker failed; recheck the saved submission before retrying".to_owned()).and_then(|result| result.map_err(|error| format!("{error:#}")));
            GitLabReply { saved, result }
        }).context("starting GitLab worker")?);
        Ok(())
    }

    pub fn take_finished(&mut self) -> Option<GitLabReply> {
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            self.finish()
        } else {
            None
        }
    }

    pub fn finish(&mut self) -> Option<GitLabReply> {
        self.thread
            .take()
            .map(|thread| thread.join().expect("GitLab task panics are captured"))
    }
}

impl Drop for GitLabWorker {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
