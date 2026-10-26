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
    let (saved, lock) = store
        .create(&repo, Review::new(comparison, changes))
        .unwrap();
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
fn first_run_explains_how_to_start_and_noninteractive_launch_keeps_state_empty() {
    let repo = common::fixture();
    let state = tempfile::tempdir().unwrap();
    let output = cli(repo.path(), state.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--base"));
    let output = cli(repo.path(), state.path())
        .args(["--base", "main"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    let repo = Repository::discover(repo.path(), Some(Backend::Git)).unwrap();
    assert!(
        SessionStore::new(state.path(), &repo)
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}
