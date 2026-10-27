mod common;
#[path = "common/session.rs"]
mod session_fixture;

use std::{path::Path, process::Command};

use review::{
    state::Review,
    store::SessionStore,
    vcs::{Backend, Repository},
};

fn cli(repo: &Path, state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_review"));
    command.env("JJ_CONFIG", "/dev/null").args([
        "-C",
        repo.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
        "--backend",
        "git",
    ]);
    command
}

#[test]
fn lists_saved_sessions_without_a_terminal() {
    let repo_dir = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let output = cli(repo_dir.path(), state.path())
        .arg("--list")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo = Repository::discover(repo_dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot("main", "feature").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    let (saved, lock) = store.create(Review::new(comparison, changes)).unwrap();
    drop(lock);
    let output = cli(repo_dir.path(), state.path())
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&saved.id));
    assert!(text.contains("main → feature"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn resume_prefix_errors_are_actionable_and_valid_prefixes_reach_the_terminal_check() {
    let repo_dir = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let repo = Repository::discover(repo_dir.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let store = SessionStore::new(state.path(), &repo).unwrap();
    for id in ["abc123000001", "abc9ff000002"] {
        session_fixture::write(
            &store,
            &repo,
            id,
            Review::new(comparison.clone(), changes.clone()),
        );
    }
    for (prefix, message) in [
        ("abc", "Ambiguous session ID prefix"),
        ("f", "No saved review matches"),
        ("abc1", "interactive terminal"),
    ] {
        let output = cli(repo_dir.path(), state.path())
            .args(["--resume", prefix])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(message), "{error}");
        if prefix == "abc" {
            assert!(error.contains("abc123000001"), "{error}");
            assert!(error.contains("abc9ff000002"), "{error}");
        }
    }
    assert_eq!(std::fs::read_dir(&store.directory).unwrap().count(), 2);
}

#[test]
fn noninteractive_launch_accepts_comparisons_and_keeps_state_empty() {
    let repo = common::fixture();
    let state = tempfile::tempdir().unwrap();
    for args in [
        &[] as &[&str],
        &["--base", "main"],
        &["main..feature"],
        &["main.."],
        &["..feature"],
        &[".."],
        &["main"],
        &["@"],
        &["main...feature"],
        &["HEAD~1..@"],
    ] {
        let output = cli(repo.path(), state.path()).args(args).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("interactive terminal"), "{args:?}: {error}");
    }
    let repo = Repository::discover(repo.path(), Some(Backend::Git)).unwrap();
    assert!(
        SessionStore::new(state.path(), &repo)
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn positional_comparisons_conflict_with_other_comparison_and_session_options() {
    for args in [
        &["main..feature", "--base", "main"] as &[&str],
        &["--base", "main", "main..feature"],
        &["main..feature", "--head", "feature"],
        &["main..feature", "--resume"],
        &["--resume", "latest", "main..feature"],
        &["main..feature", "--list"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_review"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("cannot be used with"), "{args:?}: {error}");
    }
}

#[test]
fn resume_without_sessions_explains_how_to_start() {
    let repo = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let output = cli(repo.path(), state.path())
        .arg("--resume")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("No saved review"));
    assert!(error.contains("Start one with review"));
}
