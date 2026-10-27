mod common;

use std::fs;

use common::{fixture, jj};
use review::vcs::{Backend, ComparisonMode, LineCounts, Repository};

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
fn both_backends_count_added_and_removed_lines_from_pinned_snapshots() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::write(dir.path().join("src/main.rs"), "first\nsecond\n").unwrap();
    fs::remove_file(dir.path().join("removed.txt")).unwrap();
    fs::write(dir.path().join("added\tand\nλ.txt"), "new\nlast").unwrap();
    fs::write(dir.path().join("image.bin"), [0, 7, 8, 9]).unwrap();
    jj(dir.path(), &["bookmark", "create", "stats", "-r", "@"]);
    jj(dir.path(), &["new"]);
    let expected = LineCounts {
        added: 4,
        removed: 2,
    };

    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot("main", "stats").unwrap();
        assert_eq!(comparison.line_counts, Some(expected));
        let files = comparison.file_line_counts.as_ref().unwrap();
        assert_eq!(
            files["src/main.rs"],
            LineCounts {
                added: 2,
                removed: 1
            }
        );
        assert_eq!(
            files["removed.txt"],
            LineCounts {
                added: 0,
                removed: 1
            }
        );
        assert_eq!(
            files["added\tand\nλ.txt"],
            LineCounts {
                added: 2,
                removed: 0
            }
        );
        assert_eq!(files["image.bin"], LineCounts::default());
        fs::write(dir.path().join("src/main.rs"), "uncommitted contents\n").unwrap();
        assert_eq!(repo.line_counts(&comparison).unwrap(), expected);
        assert_eq!(repo.file_line_counts(&comparison).unwrap(), *files);
    }
}

#[test]
fn file_line_counts_cover_renames_and_binary_changes_without_changing_fingerprints() {
    let dir = fixture();
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let mut comparison = repo.snapshot("main", "feature").unwrap();
        let changes = repo.changes(&comparison).unwrap();
        let mut review = review::state::Review::new(comparison.clone(), changes.clone());
        review.toggle_viewed(0);
        let viewed = review.viewed.clone();
        let files = comparison.file_line_counts.as_ref().unwrap();
        assert_eq!(
            files["space and\nλ.txt"],
            LineCounts {
                added: 1,
                removed: 0
            }
        );
        assert_eq!(files["image.bin"], LineCounts::default());
        for change in &changes {
            assert!(
                files.contains_key(&change.path),
                "{backend:?}: {}",
                change.path
            );
        }
        if backend == Backend::Git {
            assert_eq!(files["renamed.txt"], LineCounts::default());
            assert!(!files.contains_key("rename.txt"));
        }
        comparison.file_line_counts = None;
        repo.fill_line_counts(&mut comparison).unwrap();
        assert_eq!(review.reconcile(comparison, changes), 0);
        assert_eq!(review.viewed, viewed);
    }
}

#[test]
fn file_line_counts_handle_multiple_hunks_empty_files_and_literal_patch_like_paths() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    let before = (0..20)
        .map(|index| format!("line {index}\n"))
        .collect::<String>();
    fs::write(dir.path().join("src/hunks.txt"), &before).unwrap();
    jj(dir.path(), &["bookmark", "create", "before", "-r", "@"]);
    jj(dir.path(), &["new"]);
    let after = before
        .replace("line 1\n", "changed first\nextra\n")
        .replace("line 18\n", "changed last\n");
    fs::write(dir.path().join("src/hunks.txt"), after).unwrap();
    fs::write(dir.path().join("empty.txt"), "").unwrap();
    let tricky = "quotes\"\tand\n@@ -10 +99 @@\nλ.txt";
    fs::write(dir.path().join(tricky), "one\ntwo").unwrap();
    jj(dir.path(), &["bookmark", "create", "after", "-r", "@"]);
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot_expression("before..after").unwrap();
        let files = comparison.file_line_counts.as_ref().unwrap();
        assert_eq!(
            files["src/hunks.txt"],
            LineCounts {
                added: 3,
                removed: 2
            }
        );
        assert_eq!(files["empty.txt"], LineCounts::default());
        assert_eq!(
            files[tricky],
            LineCounts {
                added: 2,
                removed: 0
            }
        );
        assert_eq!(
            comparison.line_counts,
            Some(LineCounts {
                added: 5,
                removed: 2
            })
        );
    }
}

#[test]
fn edited_rename_line_counts_follow_the_backend_change_paths() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::write(
        dir.path().join("rename.txt"),
        "keep1\nkeep2\nkeep3\nkeep4\nold\n",
    )
    .unwrap();
    jj(dir.path(), &["bookmark", "create", "before", "-r", "@"]);
    jj(dir.path(), &["new"]);
    fs::create_dir_all(dir.path().join("dest")).unwrap();
    fs::rename(
        dir.path().join("rename.txt"),
        dir.path().join("dest/moved.txt"),
    )
    .unwrap();
    fs::write(
        dir.path().join("dest/moved.txt"),
        "keep1\nkeep2\nkeep3\nkeep4\nnew\nextra\n",
    )
    .unwrap();
    jj(dir.path(), &["bookmark", "create", "after", "-r", "@"]);
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot_expression("before..after").unwrap();
        let changes = repo.changes(&comparison).unwrap();
        let files = comparison.file_line_counts.as_ref().unwrap();
        let renamed = changes
            .iter()
            .find(|change| change.path == "dest/moved.txt")
            .unwrap();
        if renamed.status == "R" {
            assert_eq!(
                files["dest/moved.txt"],
                LineCounts {
                    added: 2,
                    removed: 1
                }
            );
            assert!(!files.contains_key("rename.txt"));
        } else {
            assert_eq!(
                files["dest/moved.txt"],
                LineCounts {
                    added: 6,
                    removed: 0
                }
            );
            assert_eq!(
                files["rename.txt"],
                LineCounts {
                    added: 0,
                    removed: 5
                }
            );
        }
    }
}

#[test]
fn line_counts_handle_additions_only_deletions_only_and_identical_snapshots() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::write(dir.path().join("added.txt"), "first\nlast").unwrap();
    jj(dir.path(), &["bookmark", "create", "additions", "-r", "@"]);
    jj(dir.path(), &["new", "main"]);
    fs::remove_file(dir.path().join("removed.txt")).unwrap();
    jj(dir.path(), &["bookmark", "create", "deletions", "-r", "@"]);

    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        for (head, counts) in [
            (
                "additions",
                LineCounts {
                    added: 2,
                    removed: 0,
                },
            ),
            (
                "deletions",
                LineCounts {
                    added: 0,
                    removed: 1,
                },
            ),
            ("main", LineCounts::default()),
        ] {
            assert_eq!(
                repo.snapshot("main", head).unwrap().line_counts,
                Some(counts)
            );
        }
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

#[test]
fn incremental_refresh_detects_endpoint_changes_and_ignores_unchanged_merge_bases() {
    for backend in [Backend::Git, Backend::Jj] {
        let dir = fixture();
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let merged = repo.snapshot("main", "feature").unwrap();
        let direct = repo.snapshot_expression("main..feature").unwrap();
        assert!(repo.refresh_if_changed(&merged).unwrap().is_none());
        assert!(repo.refresh_if_changed(&direct).unwrap().is_none());
        jj(dir.path(), &["new", "main"]);
        fs::write(dir.path().join("base-only.txt"), "new on base\n").unwrap();
        jj(dir.path(), &["commit", "-m", "advance base"]);
        jj(dir.path(), &["bookmark", "set", "main", "-r", "@-"]);
        assert!(repo.refresh_if_changed(&merged).unwrap().is_none());
        let updated = repo.refresh_if_changed(&direct).unwrap().unwrap();
        assert_ne!(updated.base_id, direct.base_id);
        assert_eq!(updated.head_id, direct.head_id);
        assert_eq!(updated.mode, direct.mode);
        assert_eq!(
            updated.line_counts,
            Some(repo.line_counts(&updated).unwrap())
        );
        assert!(repo.refresh_if_changed(&updated).unwrap().is_none());
    }
}

#[test]
fn jj_change_ids_identify_the_pinned_endpoints_for_direct_and_merge_base_comparisons() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::write(dir.path().join("base-only.txt"), "new on main\n").unwrap();
    jj(dir.path(), &["commit", "-m", "advance main"]);
    jj(dir.path(), &["bookmark", "set", "main", "-r", "@-"]);
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        for comparison in [
            repo.snapshot("main", "feature").unwrap(),
            repo.snapshot_expression("main..feature").unwrap(),
        ] {
            for (change_id, commit) in [
                (&comparison.base_change_id, &comparison.base_id),
                (&comparison.head_change_id, &comparison.head_id),
            ] {
                let expected = match backend {
                    Backend::Git => None,
                    Backend::Jj => Some(jj(
                        dir.path(),
                        &["log", "--no-graph", "-r", commit, "-T", "change_id"],
                    )),
                };
                assert_eq!(change_id, &expected, "{backend:?} {commit}");
            }
        }
    }
}

#[test]
fn positional_ranges_compare_tips_directly_even_when_branches_diverge() {
    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::write(dir.path().join("base-only.txt"), "new on main\n").unwrap();
    jj(dir.path(), &["commit", "-m", "advance main"]);
    jj(dir.path(), &["bookmark", "set", "main", "-r", "@-"]);
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot_expression("main..feature").unwrap();
        assert_eq!(comparison.mode, ComparisonMode::Direct);
        assert_eq!(
            comparison.base_id,
            repo.snapshot("main", "main").unwrap().head_id
        );
        assert_eq!(
            comparison.head_id,
            repo.snapshot("feature", "feature").unwrap().head_id
        );
        let changes = repo.changes(&comparison).unwrap();
        let deleted = changes
            .iter()
            .find(|change| change.path == "base-only.txt")
            .unwrap();
        assert_eq!(deleted.status, "D");
        assert_eq!(
            repo.content(&comparison, deleted, true).unwrap(),
            b"new on main\n"
        );
        assert!(
            repo.content(&comparison, deleted, false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            comparison.line_counts,
            Some(repo.line_counts(&comparison).unwrap())
        );

        let merged = repo.snapshot("main", "feature").unwrap();
        assert_eq!(merged.mode, ComparisonMode::MergeBase);
        assert_ne!(merged.base_id, comparison.base_id);
        if backend == Backend::Git {
            let triple = repo.snapshot_expression("main...feature").unwrap();
            assert_eq!(triple.mode, ComparisonMode::MergeBase);
            assert_eq!(triple.base_id, merged.base_id);
            assert_eq!(triple.head_id, merged.head_id);
        }
    }
}

#[test]
fn positional_revisions_and_omitted_endpoints_default_to_the_current_revision() {
    let dir = fixture();
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let current = match backend {
            Backend::Git => "HEAD",
            Backend::Jj => "@",
        };
        let main_id = repo.snapshot("main", "main").unwrap().head_id;
        let feature_id = repo.snapshot("feature", "feature").unwrap().head_id;
        let current_id = repo.snapshot(current, current).unwrap().head_id;
        for (expression, base, head, base_id, head_id) in [
            ("main..feature", "main", "feature", &main_id, &feature_id),
            ("main..@", "main", "@", &main_id, &current_id),
            ("@..main", "@", "main", &current_id, &main_id),
            ("main..", "main", current, &main_id, &current_id),
            ("..feature", current, "feature", &current_id, &feature_id),
            ("..", current, current, &current_id, &current_id),
            ("main", "main", current, &main_id, &current_id),
            ("@", "@", current, &current_id, &current_id),
            (
                " main .. feature ",
                "main",
                "feature",
                &main_id,
                &feature_id,
            ),
        ] {
            let comparison = repo.snapshot_expression(expression).unwrap();
            assert_eq!(comparison.base_ref, base, "{expression}");
            assert_eq!(comparison.head_ref, head, "{expression}");
            assert_eq!(&comparison.base_id, base_id, "{expression}");
            assert_eq!(&comparison.head_id, head_id, "{expression}");
            assert_eq!(comparison.mode, ComparisonMode::Direct, "{expression}");
        }
    }
}

#[test]
fn positional_comparisons_accept_native_revision_expressions_and_short_ids() {
    let dir = fixture();
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let main_id = repo.snapshot("main", "main").unwrap().head_id;
        let feature_id = repo.snapshot("feature", "feature").unwrap().head_id;
        let expressions = match backend {
            Backend::Git => vec![
                "main~0..feature^0".to_owned(),
                "feature^..feature".to_owned(),
                "main..HEAD^{commit}".to_owned(),
                format!("{}..{}", &main_id[..12], &feature_id[..12]),
            ],
            Backend::Jj => vec![
                "heads(main..feature)-..heads(main::feature)".to_owned(),
                "(main | main)..latest(main..feature)".to_owned(),
                "(description(\"missing..text\") | main)..feature".to_owned(),
                format!(
                    "{}..{}",
                    jj(
                        dir.path(),
                        &[
                            "log",
                            "--no-graph",
                            "-r",
                            "main",
                            "-T",
                            "change_id.short(8)"
                        ]
                    ),
                    &feature_id[..12]
                ),
            ],
        };
        for expression in expressions {
            let comparison = repo.snapshot_expression(&expression).unwrap();
            assert_eq!(comparison.base_id, main_id, "{expression}");
            assert_eq!(comparison.head_id, feature_id, "{expression}");
        }
        if backend == Backend::Jj {
            for expression in [
                "latest(main..feature)",
                "description(\"missing..text\") | feature",
                "description('missing..text') | feature",
                "description(\"missing \\\"..\\\" text\") | feature",
            ] {
                let comparison = repo.snapshot_expression(expression).unwrap();
                assert_eq!(comparison.base_ref, expression);
                assert_eq!(comparison.base_id, feature_id);
            }
        }
    }
}

#[test]
fn git_ranges_preserve_literal_ref_names_and_message_searches() {
    let dir = fixture();
    let names = ["topic(test", "topic{test", "topic'λ"];
    for name in names {
        jj(
            dir.path(),
            &[
                "bookmark",
                "create",
                &serde_json::to_string(name).unwrap(),
                "-r",
                "main",
            ],
        );
    }
    let repo = Repository::discover(dir.path(), Some(Backend::Git)).unwrap();
    let main_id = repo.snapshot("main", "main").unwrap().head_id;
    let feature_id = repo.snapshot("feature", "feature").unwrap().head_id;
    for expression in names
        .into_iter()
        .map(|name| format!("{name}..feature"))
        .chain([
            "HEAD^{/b..e}..feature".to_owned(),
            "main..:/fea..re".to_owned(),
            ":/b..e".to_owned(),
        ])
    {
        let comparison = repo
            .snapshot_expression(&expression)
            .unwrap_or_else(|error| panic!("{expression}: {error:#}"));
        assert_eq!(comparison.base_id, main_id, "{expression}");
        assert_eq!(comparison.head_id, feature_id, "{expression}");
    }
}

#[test]
fn invalid_or_ambiguous_positional_comparisons_report_an_error() {
    let dir = fixture();
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        for expression in [
            "",
            " ",
            "missing..feature",
            "main..missing",
            "main..feature..@",
            "main....feature",
        ] {
            assert!(
                repo.snapshot_expression(expression).is_err(),
                "{expression:?}"
            );
        }
        if backend == Backend::Jj {
            for expression in [
                "main | feature",
                "none()..feature",
                "main..all()",
                "main...feature",
            ] {
                assert!(
                    repo.snapshot_expression(expression).is_err(),
                    "{expression:?}"
                );
            }
        }
    }
}

#[test]
fn jj_positional_comparisons_snapshot_and_pin_the_working_copy_on_either_side() {
    let dir = fixture();
    let repo = Repository::discover(dir.path(), Some(Backend::Jj)).unwrap();
    fs::write(dir.path().join("working.txt"), "snapshotted\n").unwrap();
    let comparison = repo.snapshot_expression("@..main").unwrap();
    let changes = repo.changes(&comparison).unwrap();
    let working = changes
        .iter()
        .find(|change| change.path == "working.txt")
        .unwrap();
    assert_eq!(working.status, "D");
    fs::write(dir.path().join("working.txt"), "updated\n").unwrap();
    assert_eq!(
        repo.content(&comparison, working, true).unwrap(),
        b"snapshotted\n"
    );
    let refreshed = repo.refresh(&comparison).unwrap();
    assert_ne!(refreshed.base_id, comparison.base_id);
    let identical = repo.snapshot_expression("@..@").unwrap();
    assert_eq!(identical.base_id, identical.head_id);
    assert!(repo.changes(&identical).unwrap().is_empty());
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
        let files = comparison.file_line_counts.as_ref().unwrap();
        assert_eq!(files["src/main.rs"], LineCounts::default());
        assert_eq!(
            files["link"],
            LineCounts {
                added: 1,
                removed: 0
            }
        );
        assert_eq!(
            repo.content(&comparison, link, false).unwrap(),
            b"src/main.rs"
        );
    }
}

#[cfg(unix)]
#[test]
fn file_to_symlink_counts_include_both_halves_of_the_type_change() {
    use std::os::unix::fs::symlink;

    let dir = fixture();
    jj(dir.path(), &["new", "main"]);
    fs::remove_file(dir.path().join("src/main.rs")).unwrap();
    symlink("../rename.txt", dir.path().join("src/main.rs")).unwrap();
    jj(
        dir.path(),
        &["bookmark", "create", "type-change", "-r", "@"],
    );
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(dir.path(), Some(backend)).unwrap();
        let comparison = repo.snapshot_expression("main..type-change").unwrap();
        assert_eq!(
            comparison.file_line_counts.as_ref().unwrap()["src/main.rs"],
            LineCounts {
                added: 1,
                removed: 1
            }
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

#[test]
fn discovers_standalone_git_and_non_colocated_jj_repositories() {
    let dir = fixture();
    fs::rename(dir.path().join(".jj"), dir.path().join("saved-jj")).unwrap();
    let repo = Repository::discover(dir.path(), None).unwrap();
    assert_eq!(repo.backend, Backend::Git);
    assert!(
        !repo
            .changes(&repo.snapshot("main", "feature").unwrap())
            .unwrap()
            .is_empty()
    );
    fs::rename(dir.path().join("saved-jj"), dir.path().join(".jj")).unwrap();
    jj(dir.path(), &["git", "colocation", "disable"]);
    let repo = Repository::discover(dir.path(), None).unwrap();
    assert_eq!(repo.backend, Backend::Jj);
    assert!(
        !repo
            .changes(&repo.snapshot("main", "feature").unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn repository_roots_preserve_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(" repository ");
    fs::create_dir(&root).unwrap();
    jj(&root, &["git", "init", "--colocate", "."]);
    for backend in [Backend::Git, Backend::Jj] {
        let repo = Repository::discover(&root, Some(backend)).unwrap();
        assert_eq!(repo.root, root.canonicalize().unwrap());
    }
}
