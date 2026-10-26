use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::vcs::{Change, Comparison, Repository};

enum Request {
    Refresh,
    Stop,
}

#[derive(Debug)]
pub struct RefreshResult {
    pub manual: bool,
    pub snapshot: Result<(Comparison, Vec<Change>), String>,
}

pub struct RefreshWorker {
    sender: Sender<Request>,
    pub results: Receiver<RefreshResult>,
    thread: Option<JoinHandle<()>>,
}

impl RefreshWorker {
    pub fn new(repo: &Repository, comparison: &Comparison) -> Self {
        let (sender, requests) = mpsc::channel();
        let (output, results) = mpsc::channel();
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
                    snapshot: Err(error),
                });
                comparison.clone()
            }
        };
        let repo = repo.clone();
        let thread = thread::spawn(move || {
            'watch: loop {
                let mut manual = match requests.recv_timeout(Duration::from_secs(1)) {
                    Ok(Request::Refresh) => true,
                    Err(RecvTimeoutError::Timeout) => false,
                    Ok(Request::Stop) | Err(RecvTimeoutError::Disconnected) => {
                        break;
                    }
                };
                for request in requests.try_iter() {
                    match request {
                        Request::Refresh => {
                            manual = true;
                        }
                        Request::Stop => {
                            break 'watch;
                        }
                    }
                }
                let polled = (|| -> anyhow::Result<_> {
                    let comparison = if manual {
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
                        Ok((comparison, changes))
                    }
                    Ok(None) => {
                        continue;
                    }
                    Err(error) => {
                        let error = format!("{error:#}");
                        if !manual && last_error.as_ref() == Some(&error) {
                            continue;
                        }
                        last_error = Some(error.clone());
                        Err(error)
                    }
                };
                if output.send(RefreshResult { manual, snapshot }).is_err() {
                    break;
                }
            }
        });
        Self {
            sender,
            results,
            thread: Some(thread),
        }
    }

    pub fn request(&self) {
        let _ = self.sender.send(Request::Refresh);
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
