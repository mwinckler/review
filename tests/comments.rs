mod common;

use std::fs;

use review::{
    comments::{Comment, CommentAnchor, DiffSide, write_todo},
    state::Review,
    store::SessionStore,
    vcs::{Backend, Repository},
};

#[test]
fn comments_round_trip_with_stable_ids_revision_anchors_and_legacy_defaults() {
    for backend in [Backend::Git, Backend::Jj] {
        let root = common::fixture();
        let repo = Repository::discover(root.path(), Some(backend)).unwrap();
        let comparison = repo
            .snapshot_expression(if backend == Backend::Git {
                "HEAD~1..HEAD"
            } else {
                "@--..@-"
            })
            .unwrap();
        let changes = repo.changes(&comparison).unwrap();
        let change = changes
            .iter()
            .find(|change| change.path == "src/main.rs")
            .unwrap();
        let anchor =
            CommentAnchor::capture(&repo, &comparison, change, DiffSide::New, 1, 1).unwrap();
        assert_eq!(anchor.path, "src/main.rs");
        assert_eq!(anchor.base_id, comparison.base_id);
        assert_eq!(anchor.head_id, comparison.head_id);
        assert_eq!(anchor.excerpt, "fn main() { println!(\"new\"); }");
        let comment = Comment::new(anchor.clone(), "Check **this**\nSecond line".into()).unwrap();
        assert_ne!(
            comment.id,
            Comment::new(anchor, "Another".into()).unwrap().id
        );
        let mut review = Review::new(comparison, changes);
        review.comments.push(comment.clone());
        let state = tempfile::tempdir().unwrap();
        let store = SessionStore::new(state.path(), &repo).unwrap();
        let (saved, lock) = store.create(review).unwrap();
        drop(lock);
        let (mut saved, lock) = store.open(&saved.id).unwrap();
        assert_eq!(saved.review.comments, std::slice::from_ref(&comment));
        saved.review.comments[0].edit("Edited λ".into()).unwrap();
        assert_eq!(saved.review.comments[0].id, comment.id);
        assert_eq!(saved.review.comments[0].anchor, comment.anchor);
        assert!(saved.review.comments[0].updated_at >= comment.created_at);
        lock.save(&mut saved).unwrap();
        drop(lock);
        let (saved, _lock) = store.open(&saved.id).unwrap();
        assert_eq!(saved.review.comments[0].body, "Edited λ");
        let mut legacy = serde_json::to_value(saved.review).unwrap();
        legacy.as_object_mut().unwrap().remove("comments");
        assert!(
            serde_json::from_value::<Review>(legacy)
                .unwrap()
                .comments
                .is_empty()
        );
    }
}

#[test]
fn anchors_validate_ranges_sides_and_text_and_keep_outdated_comments_through_refresh() {
    let root = common::fixture();
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let change = changes
        .iter()
        .find(|change| change.path == "src/main.rs")
        .unwrap();
    for (start, end) in [(0, 1), (2, 1), (1, 2), (usize::MAX, usize::MAX)] {
        assert!(
            CommentAnchor::capture(&repo, &comparison, change, DiffSide::New, start, end).is_err()
        );
    }
    let anchor = CommentAnchor::capture(&repo, &comparison, change, DiffSide::Old, 1, 1).unwrap();
    for body in ["", " \n\t", "bad\u{1b}[0m"] {
        assert!(Comment::new(anchor.clone(), body.into()).is_err());
    }
    let removed = changes
        .iter()
        .find(|change| change.path == "removed.txt")
        .unwrap();
    assert!(CommentAnchor::capture(&repo, &comparison, removed, DiffSide::New, 1, 1).is_err());
    let binary = changes
        .iter()
        .find(|change| change.path == "image.bin")
        .unwrap();
    assert!(CommentAnchor::capture(&repo, &comparison, binary, DiffSide::New, 1, 1).is_err());
    let comment = Comment::new(anchor, "Original old line".into()).unwrap();
    let mut review = Review::new(comparison.clone(), changes.clone());
    review.comments.push(comment.clone());
    assert!(!comment.anchor.is_outdated(&review.changes));
    let mut rewritten = comparison.clone();
    rewritten.head_id = "rewritten".into();
    review.reconcile(rewritten, changes.clone());
    assert!(!comment.anchor.is_outdated(&review.changes));
    let mut changed = changes;
    changed
        .iter_mut()
        .find(|change| change.path == "src/main.rs")
        .unwrap()
        .old
        .object_id = "different".into();
    review.reconcile(comparison.clone(), changed);
    assert!(comment.anchor.is_outdated(&review.changes));
    review.reconcile(comparison, Vec::new());
    assert_eq!(review.comments, [comment]);
}

#[test]
fn source_todos_map_complete_ranges_preserve_indentation_crlf_permissions_and_local_comments() {
    for backend in [Backend::Git, Backend::Jj] {
        let root = common::fixture();
        fs::write(
            root.path().join("src/main.rs"),
            "fn main() {\r\n    first();\r\n    second();\r\n}\r\n",
        )
        .unwrap();
        common::jj(root.path(), &["commit", "-m", "range"]);
        let repo = Repository::discover(root.path(), Some(backend)).unwrap();
        let comparison = repo
            .snapshot_expression(if backend == Backend::Git {
                "HEAD~1..HEAD"
            } else {
                "@--..@-"
            })
            .unwrap();
        let changes = repo.changes(&comparison).unwrap();
        let change = changes
            .iter()
            .find(|change| change.path == "src/main.rs")
            .unwrap();
        let anchor =
            CommentAnchor::capture(&repo, &comparison, change, DiffSide::New, 2, 3).unwrap();
        assert_eq!(anchor.excerpt, "    first();\n    second();");
        let mut comment = Comment::new(anchor, "Explain this\nwith λ".into()).unwrap();
        let path = root.path().join("src/main.rs");
        fs::write(
            &path,
            "// unrelated edit\r\nfn main() {\r\n    first();\r\n    second();\r\n}\r\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o754)).unwrap();
        }
        let review = Review::new(comparison, changes);
        comment.source_todo = Some(write_todo(&repo, &review, &comment).unwrap());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "// unrelated edit\r\nfn main() {\r\n    // TODO: Explain this\r\n    // with λ\r\n    first();\r\n    second();\r\n}\r\n"
        );
        assert_eq!(comment.source_todo.as_ref().unwrap().line, 3);
        assert_eq!(comment.body, "Explain this\nwith λ");
        assert!(write_todo(&repo, &review, &comment).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o754
            );
        }
    }
}

#[test]
fn source_todos_refuse_replaced_ranges_deleted_sides_symlinks_and_unsupported_languages() {
    let root = common::fixture();
    fs::write(
        root.path().join("src/main.rs"),
        "first();\nsecond();\nthird();\n",
    )
    .unwrap();
    common::jj(root.path(), &["commit", "-m", "range"]);
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let change = changes
        .iter()
        .find(|change| change.path == "src/main.rs")
        .unwrap();
    let anchor = CommentAnchor::capture(&repo, &comparison, change, DiffSide::New, 1, 3).unwrap();
    let comment = Comment::new(anchor, "Check".into()).unwrap();
    let review = Review::new(comparison, changes);
    let path = root.path().join("src/main.rs");
    for replacement in [
        "first();\nreplaced();\nthird();\n",
        "first();\ninserted();\nsecond();\nthird();\n",
        "",
    ] {
        fs::write(&path, replacement).unwrap();
        assert!(write_todo(&repo, &review, &comment).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), replacement);
    }
    let mut old = comment.clone();
    old.anchor.side = DiffSide::Old;
    assert!(write_todo(&repo, &review, &old).is_err());
    let mut unsupported = comment.clone();
    unsupported.anchor.path = "README.md".into();
    assert!(write_todo(&repo, &review, &unsupported).is_err());
    fs::write(&path, "first();\nsecond();\nthird();\n").unwrap();
    let mut escaped = comment.clone();
    escaped.body = "bad \\\ninjection".into();
    assert!(write_todo(&repo, &review, &escaped).is_err());
    #[cfg(unix)]
    {
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), "first();\nsecond();\nthird();\n").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(outside.path(), &path).unwrap();
        assert!(write_todo(&repo, &review, &comment).is_err());
        assert_eq!(
            fs::read_to_string(outside.path()).unwrap(),
            "first();\nsecond();\nthird();\n"
        );
    }
}
