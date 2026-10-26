use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::ErrorKind,
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

use crate::renderer::RendererConfig;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub renderer: RendererConfig,
    pub keybindings: BTreeMap<String, String>,
    #[serde(skip)]
    bindings: HashMap<(KeyCode, KeyModifiers), Vec<KeyEvent>>,
}

impl Config {
    pub fn load(path: &Path, required: bool) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if !required && error.kind() == ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading config {}", path.display()));
            }
        };
        let mut config: Self =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        ensure!(
            config
                .renderer
                .command
                .first()
                .is_some_and(|program| !program.is_empty()),
            "renderer.command must contain an executable"
        );
        ensure!(
            config.renderer.timeout_ms > 0,
            "renderer.timeout_ms must be positive"
        );
        for (source, action) in &config.keybindings {
            let source =
                parse_key(source).with_context(|| format!("invalid keybinding {source:?}"))?;
            let keys = action_keys(action)
                .with_context(|| format!("unknown keybinding action {action:?}"))?;
            config
                .bindings
                .entry(normalized(source))
                .or_insert_with(|| {
                    keys.iter()
                        .map(|key| parse_key(key).expect("builtin binding is valid"))
                        .collect()
                });
        }
        Ok(config)
    }

    pub fn translate(&self, key: KeyEvent) -> Vec<KeyEvent> {
        self.bindings
            .get(&normalized(key))
            .cloned()
            .unwrap_or_else(|| vec![key])
    }
}

fn normalized(mut key: KeyEvent) -> (KeyCode, KeyModifiers) {
    if matches!(key.code, KeyCode::Char(_)) {
        key.modifiers.remove(KeyModifiers::SHIFT);
    }
    (key.code, key.modifiers)
}

fn parse_key(mut name: &str) -> Result<KeyEvent> {
    let mut modifiers = KeyModifiers::NONE;
    loop {
        if let Some(rest) = name.strip_prefix("Ctrl-") {
            modifiers.insert(KeyModifiers::CONTROL);
            name = rest;
        } else if let Some(rest) = name.strip_prefix("Alt-") {
            modifiers.insert(KeyModifiers::ALT);
            name = rest;
        } else {
            break;
        }
    }
    let code = match name {
        "Space" => KeyCode::Char(' '),
        "Enter" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Esc" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        value => {
            let mut chars = value.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) if !ch.is_control() => KeyCode::Char(ch),
                _ => bail!("expected a character, named key, or Ctrl-/Alt- key"),
            }
        }
    };
    Ok(KeyEvent::new(code, modifiers))
}

fn action_keys(action: &str) -> Option<&'static [&'static str]> {
    Some(match action {
        "up" => &["k"],
        "down" => &["j"],
        "left" => &["h"],
        "right" => &["l"],
        "focus_tree" => &["Ctrl-w", "h"],
        "focus_diff" => &["Ctrl-w", "l"],
        "first" => &["g", "g"],
        "last" => &["G"],
        "page_down" => &["Ctrl-f"],
        "page_up" => &["Ctrl-b"],
        "half_page_down" => &["Ctrl-d"],
        "half_page_up" => &["Ctrl-u"],
        "next_file" => &["]", "f"],
        "previous_file" => &["[", "f"],
        "next_unviewed" => &["]", "u"],
        "previous_unviewed" => &["[", "u"],
        "toggle_viewed" => &["Space"],
        "toggle_unviewed" => &["u"],
        "toggle_inline" => &["i"],
        "toggle_fullscreen" => &["z"],
        "search" => &["/"],
        "next_match" => &["n"],
        "previous_match" => &["N"],
        "refresh" => &["r"],
        "clear_filters" => &["Esc"],
        "help" => &["?"],
        "quit" => &["q"],
        _ => {
            return None;
        }
    })
}
