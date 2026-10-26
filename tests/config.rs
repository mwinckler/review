use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use review::config::Config;

#[test]
fn config_loads_renderer_arguments_and_key_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
[renderer]
command = ["diff", "-u", "{old}", "{new}"]
timeout_ms = 1234
[keybindings]
J = "next_file"
"Ctrl-j" = "next_unviewed"
"Space" = "toggle_viewed"
"#,
    )
    .unwrap();
    let config = Config::load(&path, true).unwrap();
    assert_eq!(config.renderer.timeout_ms, 1234);
    assert_eq!(config.renderer.command[0], "diff");
    let alias = config.translate(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
    assert_eq!(
        alias.iter().map(|key| key.code).collect::<Vec<_>>(),
        [KeyCode::Char(']'), KeyCode::Char('f')]
    );
    let alias = config.translate(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    assert_eq!(
        alias.iter().map(|key| key.code).collect::<Vec<_>>(),
        [KeyCode::Char(']'), KeyCode::Char('u')]
    );
    let normal = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(config.translate(normal), [normal]);
}

#[test]
fn config_reports_typos_invalid_actions_and_required_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    assert!(Config::load(&path, false).is_ok());
    assert!(Config::load(&path, true).is_err());
    for invalid in [
        "[rendrer]",
        "[renderer]\ncommand = []",
        "[renderer]\ntimeout_ms = 0",
        "[keybindings]\nj = 'unknown'",
        "[keybindings]\n'not-a-key' = 'quit'",
    ] {
        fs::write(&path, invalid).unwrap();
        assert!(Config::load(&path, true).is_err(), "{invalid}");
    }
}

#[test]
fn pane_focus_aliases_use_direct_control_shortcuts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[keybindings]\nH = 'focus_tree'\nL = 'focus_diff'\n").unwrap();
    let config = Config::load(&path, true).unwrap();
    for ch in ['h', 'l'] {
        assert_eq!(
            config.translate(KeyEvent::new(
                KeyCode::Char(ch.to_ascii_uppercase()),
                KeyModifiers::SHIFT
            )),
            [KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)]
        );
    }
}

#[test]
fn flash_can_be_aliased() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[keybindings]\nF = 'flash'\n").unwrap();
    let config = Config::load(&path, true).unwrap();
    assert_eq!(
        config.translate(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)),
        [KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)]
    );
}
