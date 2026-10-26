mod common;

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
        vec!["main..feature", "--base", "main"],
        vec!["--base", "main", "main..feature"],
        vec!["main..feature", "--head", "feature"],
        vec!["main..feature", "--resume"],
        vec!["--resume", "latest", "main..feature"],
        vec!["main..feature", "--list"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_review"))
            .args(&args)
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
