use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::vcs::{Change, Comparison};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Position {
    pub vertical: usize,
    pub horizontal: u16,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    pub selected: String,
    pub active: String,
    pub diff_focused: bool,
    pub collapsed: BTreeSet<String>,
    pub positions: BTreeMap<String, Position>,
    pub filter: String,
    pub unviewed_only: bool,
    pub inline: bool,
    pub fullscreen: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    pub comparison: Comparison,
    pub changes: Vec<Change>,
    pub viewed: BTreeMap<String, String>,
    pub view: ViewState,
}

impl Review {
    pub fn reconcile(&mut self, comparison: Comparison, changes: Vec<Change>) -> usize {
        let before: BTreeMap<_, _> = self
            .changes
            .iter()
            .map(|change| (&change.path, change.fingerprint()))
            .collect();
        let after: BTreeMap<_, _> = changes
            .iter()
            .map(|change| (&change.path, change.fingerprint()))
            .collect();
        let count = self.viewed.len();
        self.viewed
            .retain(|path, hash| after.get(path) == Some(hash));
        self.view
            .positions
            .retain(|path, _| after.contains_key(path) && before.get(path) == after.get(path));
        self.changes = changes;
        self.comparison = comparison;
        count - self.viewed.len()
    }

    pub fn new(comparison: Comparison, changes: Vec<Change>) -> Self {
        Self {
            comparison,
            changes,
            viewed: BTreeMap::new(),
            view: ViewState::default(),
        }
    }

    pub fn is_viewed(&self, change: &Change) -> bool {
        self.viewed
            .get(&change.path)
            .is_some_and(|hash| *hash == change.fingerprint())
    }

    pub fn toggle_viewed(&mut self, index: usize) {
        let change = &self.changes[index];
        if self.is_viewed(change) {
            self.viewed.remove(&change.path);
        } else {
            self.viewed
                .insert(change.path.clone(), change.fingerprint());
        }
    }
}
