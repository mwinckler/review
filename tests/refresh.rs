mod common;

use std::{fs, sync::mpsc::RecvTimeoutError, time::Duration};

use review::{
    refresh::RefreshWorker,
    state::Review,
    vcs::{Backend, Repository},
};

#[test]
fn resumed_snapshots_stay_pinned_until_a_new_change_or_explicit_refresh() {
    let dir = common::fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let original = repo.snapshot_expression("main..feature").unwrap();
    common::jj(dir.path(), &["new", "feature"]);
    fs::write(dir.path().join("src/main.rs"), "advanced before resume\n").unwrap();
    common::jj(dir.path(), &["commit", "-m", "advance feature"]);
    common::jj(dir.path(), &["bookmark", "set", "feature", "-r", "@-"]);
    let worker = RefreshWorker::new(&repo, &original);
    assert!(matches!(
        worker.results.recv_timeout(Duration::from_millis(1300)),
        Err(RecvTimeoutError::Timeout)
    ));
    worker.request();
    let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(result.manual);
    let (updated, changes) = result.snapshot.unwrap();
    assert_ne!(updated.head_id, original.head_id);
    assert_eq!(updated.base_id, original.base_id);
    assert!(!changes.is_empty());
    assert!(matches!(
        worker.results.recv_timeout(Duration::from_millis(1300)),
        Err(RecvTimeoutError::Timeout)
    ));
}

#[test]
fn working_copy_polls_reload_edits_and_leave_identical_snapshots_alone() {
    let dir = common::fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Jj)).unwrap();
    let original = repo.snapshot_expression("main..@").unwrap();
    let worker = RefreshWorker::new(&repo, &original);
    fs::write(dir.path().join("src/main.rs"), "live working copy\n").unwrap();
    let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!result.manual);
    let (updated, changes) = result.snapshot.unwrap();
    assert_ne!(updated.head_id, original.head_id);
    assert_eq!(updated.head_change_id, original.head_change_id);
    let file = changes
        .iter()
        .find(|change| change.path == "src/main.rs")
        .unwrap();
    assert_eq!(
        repo.content(&updated, file, false).unwrap(),
        b"live working copy\n"
    );
    assert!(matches!(
        worker.results.recv_timeout(Duration::from_millis(1300)),
        Err(RecvTimeoutError::Timeout)
    ));
}

#[test]
fn polling_recovers_after_revision_errors_and_suppresses_duplicate_errors() {
    let dir = common::fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let original = repo.snapshot_expression("main..feature").unwrap();
    let worker = RefreshWorker::new(&repo, &original);
    common::jj(dir.path(), &["bookmark", "forget", "feature"]);
    let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!result.manual);
    assert!(result.snapshot.is_err());
    assert!(matches!(
        worker.results.recv_timeout(Duration::from_millis(1300)),
        Err(RecvTimeoutError::Timeout)
    ));
    common::jj(dir.path(), &["bookmark", "create", "feature", "-r", "@-"]);
    let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
    let (recovered, _) = result.snapshot.unwrap();
    assert_eq!(recovered.head_id, original.head_id);
}

#[test]
fn automatic_rewrites_preserve_viewed_marks_and_unchanged_scroll_positions() {
    let dir = common::fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Jj)).unwrap();
    let original = repo.snapshot_expression("main..feature").unwrap();
    let mut review = Review::new(original.clone(), repo.changes(&original).unwrap());
    for index in 0..review.changes.len() {
        review.toggle_viewed(index);
    }
    review
        .view
        .positions
        .entry("src/".into())
        .or_default()
        .vertical = 7;
    let viewed = review.viewed.clone();
    let worker = RefreshWorker::new(&repo, &original);
    common::jj(
        dir.path(),
        &["describe", "-r", "feature", "-m", "rewritten description"],
    );
    let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
    let (updated, changes) = result.snapshot.unwrap();
    assert_ne!(updated.head_id, original.head_id);
    assert_eq!(updated.head_change_id, original.head_change_id);
    assert_eq!(review.reconcile(updated, changes), 0);
    assert_eq!(review.viewed, viewed);
    assert_eq!(review.view.positions["src/"].vertical, 7);
}
