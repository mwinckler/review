use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    restore::Restore,
    vcs::{Backend, Change, Comparison, ComparisonMode, Repository},
};

enum Request {
    Refresh,
    Compare {
        base: String,
        head: String,
        mode: ComparisonMode,
    },
    Restore(Box<Restore>),
    Stop,
}

#[derive(Debug)]
pub struct RefreshResult {
    pub manual: bool,
    pub revised: bool,
    pub snapshot: Result<(Comparison, Vec<Change>), String>,
}

pub struct RefreshWorker {
    sender: Sender<Request>,
    pub results: Receiver<RefreshResult>,
    pub restores: Receiver<Result<(), String>>,
    thread: Option<JoinHandle<()>>,
}

impl RefreshWorker {
    pub fn new(repo: &Repository, comparison: &Comparison) -> Self {
        let (sender, requests) = mpsc::channel();
        let (output, results) = mpsc::channel();
        let (restored, restores) = mpsc::channel();
        let mut last_error = None;
        // Establish the live baseline before showing the review, preserving a
        // resumed session's pins until a subsequent repository change.
        let mut observed = match repo.refresh_if_changed(comparison) {
            Ok(updated) => updated.unwrap_or_else(|| comparison.clone()),
            Err(error) => {
                let error = format!("{error:#}");
                last_error = Some(error.clone());
                let _ = output.send(RefreshResult {
                    manual: false,
                    revised: false,
                    snapshot: Err(error),
                });
                comparison.clone()
            }
        };
        let repo = repo.clone();
        let thread = thread::spawn(move || {
            'watch: loop {
                let first = match requests.recv_timeout(Duration::from_secs(1)) {
                    Ok(request) => Some(request),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => {
                        break;
                    }
                };
                let mut manual = false;
                let mut restore = None;
                let mut revisions = None;
                for request in first.into_iter().chain(requests.try_iter()) {
                    match request {
                        Request::Refresh => {
                            manual = true;
                        }
                        Request::Compare { base, head, mode } => {
                            revisions = Some((base, head, mode));
                        }
                        Request::Restore(action) => {
                            restore = Some(action);
                        }
                        Request::Stop => {
                            break 'watch;
                        }
                    }
                }
                let revised = revisions.is_some();
                manual |= revised;
                let restore =
                    restore.map(|action| action.apply(&repo).map_err(|error| format!("{error:#}")));
                let polled = (|| -> anyhow::Result<_> {
                    let comparison = if let Some((base, head, mode)) = revisions {
                        Some(repo.snapshot_revisions(&base, &head, mode)?)
                    } else if manual
                        || (repo.backend == Backend::Jj && matches!(restore, Some(Ok(()))))
                    {
                        Some(repo.refresh(&observed)?)
                    } else {
                        repo.refresh_if_changed(&observed)?
                    };
                    let Some(comparison) =
                        comparison.or_else(|| last_error.as_ref().map(|_| observed.clone()))
                    else {
                        return Ok(None);
                    };
                    let changes = repo.changes(&comparison)?;
                    Ok(Some((comparison, changes)))
                })();
                let snapshot = match polled {
                    Ok(Some((comparison, changes))) => {
                        observed = comparison.clone();
                        last_error = None;
                        Some(Ok((comparison, changes)))
                    }
                    Ok(None) => None,
                    Err(error) => {
                        let error = format!("{error:#}");
                        if revised {
                            Some(Err(error))
                        } else if !manual && last_error.as_ref() == Some(&error) {
                            None
                        } else {
                            last_error = Some(error.clone());
                            Some(Err(error))
                        }
                    }
                };
                if let Some(snapshot) = snapshot
                    && output
                        .send(RefreshResult {
                            manual,
                            revised,
                            snapshot,
                        })
                        .is_err()
                {
                    break;
                }
                if let Some(result) = restore
                    && restored.send(result).is_err()
                {
                    break;
                }
            }
        });
        Self {
            sender,
            results,
            restores,
            thread: Some(thread),
        }
    }

    pub fn request(&self) {
        let _ = self.sender.send(Request::Refresh);
    }

    pub fn compare(&self, base: String, head: String, mode: ComparisonMode) -> anyhow::Result<()> {
        self.sender
            .send(Request::Compare { base, head, mode })
            .map_err(|_| anyhow::anyhow!("The repository worker has stopped."))
    }

    pub fn has_stopped(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn restore(&self, action: Restore) -> anyhow::Result<()> {
        self.sender
            .send(Request::Restore(Box::new(action)))
            .map_err(|_| anyhow::anyhow!("The repository worker has stopped."))
    }
}

impl Drop for RefreshWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(Request::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
