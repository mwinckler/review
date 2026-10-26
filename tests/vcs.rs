mod common;

use std::fs;

use common::{fixture, jj};
use review::vcs::{Backend, Repository};

#[test]
fn discovers_jj_first_and_allows_git_override_from_a_subdirectory() {
    let dir = fixture();
    let repo = Repository::discover(&dir.path().join("src"), None).unwrap();
    assert_eq!(repo.backend, Backend::Jj);
    assert_eq!(repo.root, dir.path().canonicalize().unwrap());
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    assert_eq!(repo.backend, Backend::Git);
}

#[test]
fn both_backends_list_changes_and_read_pinned_contents() {
    let dir = fixture();
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot("main", "feature").unwrap();
        let changes = repo.changes(&comparison).unwrap();
        assert!(
            changes
                .iter()
                .any(|change| change.path == "space and\nλ.txt" && change.status == "A")
        );
        assert!(
            changes
                .iter()
                .any(|change| change.path == "removed.txt" && change.status == "D")
        );
        let changed = changes
            .iter()
            .find(|change| change.path == "src/main.rs")
            .unwrap();
        fs::write(dir.path().join("src/main.rs"), "uncommitted contents\n").unwrap();
        assert_eq!(
            repo.content(&comparison, changed, true).unwrap(),
            b"fn main() { println!(\"old\"); }\n"
        );
        assert_eq!(
            repo.content(&comparison, changed, false).unwrap(),
            b"fn main() { println!(\"new\"); }\n"
        );
        let added = changes.iter().find(|change| change.status == "A").unwrap();
        assert!(repo.content(&comparison, added, true).unwrap().is_empty());
        let removed = changes.iter().find(|change| change.status == "D").unwrap();
        assert!(
            repo.content(&comparison, removed, false)
                .unwrap()
                .is_empty()
        );
        let binary = changes
            .iter()
            .find(|change| change.path == "image.bin")
            .unwrap();
        assert_eq!(
            repo.content(&comparison, binary, false).unwrap(),
            [0, 4, 5, 6]
        );
    }
}

#[test]
fn git_detects_renames_and_jj_accepts_revision_expressions() {
    let dir = fixture();
    let git = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let changes = git
        .changes(&git.snapshot("main", "feature").unwrap())
        .unwrap();
    let renamed = changes
        .iter()
        .find(|change| change.path == "renamed.txt")
        .unwrap();
    assert_eq!(renamed.status, "R");
    assert_eq!(renamed.old_path, "rename.txt");
    let jj = Repository::discover(dir.path(), Some(Backend::Jj)).unwrap();
    assert!(jj.snapshot("main", "heads(main::feature)").is_ok());
    assert!(jj.snapshot("main", "main | feature").is_err());
}

#[test]
fn fingerprints_include_both_sides_paths_and_modes() {
    let dir = fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let mut changes = repo
        .changes(&repo.snapshot("main", "feature").unwrap())
        .unwrap();
    let change = changes.pop().unwrap();
    let original = change.fingerprint();
    let mut modified = change.clone();
    modified.old.object_id.push('a');
    assert_ne!(original, modified.fingerprint());
    modified = change.clone();
    modified.new.executable = !modified.new.executable;
    assert_ne!(original, modified.fingerprint());
    modified = change.clone();
    modified.old_path.push('a');
    assert_ne!(original, modified.fingerprint());
    assert_eq!(original, change.fingerprint());
}

#[test]
fn reports_missing_repositories_and_invalid_revisions() {
    let empty = tempfile::tempdir().unwrap();
    assert!(Repository::discover(empty.path(), None).is_err());
    let dir = fixture();
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        assert!(repo.snapshot("missing", "feature").is_err());
    }
}

#[test]
fn comparison_uses_merge_base_instead_of_the_current_base_tip() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::write(dir.path().join("unrelated.txt"), "new on main\n").unwrap();
    jj(dir.path(), &["commit", "-m", "advance main"]);
    jj(dir.path(), &["bookmark", "set", "main", "-r", "@-"]);
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let changes = repo
            .changes(&repo.snapshot("main", "feature").unwrap())
            .unwrap();
        assert!(!changes.iter().any(|change| change.path == "unrelated.txt"));
    }
}

#[cfg(unix)]
#[test]
fn handles_symlinks_and_executable_only_changes() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    symlink("src/main.rs", dir.path().join("link")).unwrap();
    fs::set_permissions(
        dir.path().join("src/main.rs"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    jj(dir.path(), &["bookmark", "create", "modes", "-r", "@"]);
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot("main", "modes").unwrap();
        let changes = repo.changes(&comparison).unwrap();
        let executable = changes
            .iter()
            .find(|change| change.path == "src/main.rs")
            .unwrap();
        assert_eq!(executable.old.object_id, executable.new.object_id);
        assert_ne!(executable.old.executable, executable.new.executable);
        let link = changes.iter().find(|change| change.path == "link").unwrap();
        assert_eq!(link.new.kind, "symlink");
        assert_eq!(
            repo.content(&comparison, link, false).unwrap(),
            b"src/main.rs"
        );
    }
}

#[test]
fn jj_conflicts_have_content_sensitive_fingerprints() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::write(dir.path().join("src/main.rs"), "left\n").unwrap();
    jj(dir.path(), &["bookmark", "create", "left", "-r", "@"]);
    jj(dir.path(), &["new", "main"]);
    fs::write(dir.path().join("src/main.rs"), "right\n").unwrap();
    jj(dir.path(), &["bookmark", "create", "right", "-r", "@"]);
    jj(dir.path(), &["new", "left", "right"]);
    let repo = Repository::discover(dir.path(), Some(Backend::Jj)).unwrap();
    let comparison = repo.snapshot("main", "@").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let conflict = changes
        .iter()
        .find(|change| change.path == "src/main.rs")
        .unwrap();
    assert_eq!(conflict.new.kind, "conflict");
    assert!(!conflict.new.object_id.is_empty());
    let content = repo.content(&comparison, conflict, false).unwrap();
    assert!(String::from_utf8_lossy(&content).contains("left"));
}
