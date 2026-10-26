use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use crate::state::Review;

#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub depth: usize,
    pub file: Option<usize>,
    pub total: usize,
    pub viewed: usize,
}

impl Row {
    pub fn name(&self) -> &str {
        self.path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
    }
}

pub fn visible_range(rows: usize, selection: usize, height: usize) -> Range<usize> {
    let offset = selection
        .saturating_sub(height / 2)
        .min(rows.saturating_sub(height));
    offset..offset.saturating_add(height).min(rows)
}

pub fn rows(review: &Review) -> Vec<Row> {
    let mut entries = BTreeMap::<String, Row>::new();
    let mut visible_directories = BTreeSet::new();
    let filter = review.view.filter.to_lowercase();
    for (index, change) in review.changes.iter().enumerate() {
        let viewed = usize::from(review.is_viewed(change));
        let visible = change.path.to_lowercase().contains(&filter)
            && (!review.view.unviewed_only || viewed == 0);
        for (depth, (offset, _)) in change.path.match_indices('/').enumerate() {
            let path = &change.path[..=offset];
            if visible {
                visible_directories.insert(path);
            }
            let row = entries.entry(path.to_owned()).or_insert_with(|| Row {
                path: path.to_owned(),
                depth,
                file: None,
                total: 0,
                viewed: 0,
            });
            row.total += 1;
            row.viewed += viewed;
        }
        if visible {
            entries.insert(
                change.path.clone(),
                Row {
                    path: change.path.clone(),
                    depth: change.path.matches('/').count(),
                    file: Some(index),
                    total: 1,
                    viewed,
                },
            );
        }
    }
    entries
        .into_values()
        .filter(|row| {
            let matches = row.file.is_some() || visible_directories.contains(row.path.as_str());
            let collapsed = filter.is_empty()
                && row
                    .path
                    .trim_end_matches('/')
                    .match_indices('/')
                    .any(|(offset, _)| review.view.collapsed.contains(&row.path[..=offset]));
            matches && !collapsed
        })
        .collect()
}
