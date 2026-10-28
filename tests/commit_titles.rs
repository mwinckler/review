mod common;

use ratatui::{Terminal, backend::TestBackend, text::Text};
use review::{
    app::App,
    state::Review,
    ui,
    vcs::{Backend, Comparison, Repository},
};

#[test]
fn titles_follow_pinned_commits_and_refresh_and_legacy_metadata_uses_the_original_pins() {
    for backend in [Backend::Git, Backend::Jj] {
        let root = common::fixture();
        let title = format!("{} end", "界λ".repeat(30));
        common::jj(
            root.path(),
            &[
                "commit",
                "-m",
                &format!("{title}\nsecond subject line\n\ncommit body"),
            ],
        );
        let repo = Repository::discover(root.path(), Some(backend)).unwrap();
        let comparison = repo
            .snapshot_expression(if backend == Backend::Git {
                "HEAD~1..HEAD"
            } else {
                "@--..@-"
            })
            .unwrap();
        assert_eq!(comparison.base_title.as_deref(), Some("feature"));
        assert_eq!(comparison.head_title.as_deref(), Some(title.as_str()));
        let mut legacy = serde_json::to_value(&comparison).unwrap();
        legacy.as_object_mut().unwrap().remove("base_title");
        legacy.as_object_mut().unwrap().remove("head_title");
        let mut legacy: Comparison = serde_json::from_value(legacy).unwrap();
        assert!(legacy.base_title.is_none());
        assert!(legacy.head_title.is_none());
        common::jj(
            root.path(),
            &[
                "commit",
                "-m",
                "Replacement title\nadditional subject line\n\nbody",
            ],
        );
        assert!(repo.fill_commit_titles(&mut legacy).unwrap());
        assert_eq!(legacy.base_id, comparison.base_id);
        assert_eq!(legacy.head_id, comparison.head_id);
        assert_eq!(legacy.base_title, comparison.base_title);
        assert_eq!(legacy.head_title, comparison.head_title);
        assert!(!repo.fill_commit_titles(&mut legacy).unwrap());
        for refreshed in [
            repo.refresh(&comparison).unwrap(),
            repo.refresh_if_changed(&comparison).unwrap().unwrap(),
        ] {
            assert_eq!(refreshed.base_title.as_deref(), Some(title.as_str()));
            assert_eq!(refreshed.head_title.as_deref(), Some("Replacement title"));
        }
        if backend == Backend::Jj {
            let comparison = repo.snapshot_expression("root()..@-").unwrap();
            assert_eq!(comparison.base_title.as_deref(), Some(""));
        }
    }
}

#[test]
fn header_titles_use_at_most_fifty_unicode_characters_and_fit_around_ids_and_counts() {
    let root = common::fixture();
    let repo = Repository::discover(root.path(), Some(Backend::Git)).unwrap();
    let mut comparison = repo.snapshot_expression("HEAD~1..HEAD").unwrap();
    comparison.base_ref = "master".into();
    comparison.head_ref = "feature".into();
    comparison.base_id = "abcdef1234567890".into();
    comparison.head_id = "1234567890abcdef".into();
    comparison.base_change_id = Some("ukrwmnxyzqst".into());
    comparison.head_change_id = Some("vqmrstuvwxyz".into());
    comparison.base_title = Some(format!("{}\nbase body", "x".repeat(65)));
    comparison.head_title = Some(format!("{}\nhead body", "界".repeat(65)));
    let mut app = App::new(Review::new(comparison, Vec::new()));
    app.session_label = "r1 · Named review".into();
    for width in [400, 180, 140, 60, 24, 1, 0] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &app, &Text::default()))
            .unwrap();
        let top: String = (0..width)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(!top.contains("base body"));
        assert!(!top.contains("head body"));
        if width >= 140 {
            assert!(top.contains("master (ukrwmnxy abcdef12)"), "{top}");
            assert!(top.contains("feature (vqmrstuv 12345678)"), "{top}");
            assert!(top.contains("Viewed 0/0"), "{top}");
            assert!(top.ends_with("total · +0 -0 tests"), "{top}");
        }
        if width == 400 {
            assert!(top.contains(&"x".repeat(50)), "{top}");
            assert!(!top.contains(&"x".repeat(51)), "{top}");
            assert_eq!(top.matches('界').count(), 50);
        }
    }
    app.review.comparison.base_title = Some("safe\u{1b}[31m\tline\nignored body".into());
    app.review.comparison.head_title = Some(String::new());
    let mut terminal = Terminal::new(TestBackend::new(400, 24)).unwrap();
    terminal
        .draw(|frame| ui::draw(frame, &app, &Text::default()))
        .unwrap();
    let top: String = (0..400)
        .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
        .collect();
    assert!(top.contains("safe"));
    assert!(top.contains("line"));
    assert!(!top.contains('\u{1b}'));
    assert!(!top.contains("ignored body"));
}
