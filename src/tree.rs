use std::collections::BTreeMap;

use crate::state::Review;

#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub depth: usize,
    pub file: Option<usize>,
    pub total: usize,
    pub viewed: usize,
}

pub fn rows(review: &Review) -> Vec<Row> {
    let mut entries = BTreeMap::<String, Row>::new();
    let filter = review.view.filter.to_lowercase();
    for (index, change) in review.changes.iter().enumerate() {
        let viewed = usize::from(review.is_viewed(change));
        let visible = change.path.to_lowercase().contains(&filter)
            && (!review.view.unviewed_only || viewed == 0);
        for (offset, _) in change.path.match_indices('/') {
            let path = &change.path[..=offset];
            let row = entries.entry(path.to_owned()).or_insert_with(|| Row {
                path: path.to_owned(),
                depth: path.matches('/').count() - 1,
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
    let visible_paths: Vec<_> = entries
        .values()
        .filter(|row| row.file.is_some())
        .map(|row| row.path.clone())
        .collect();
    entries
        .into_values()
        .filter(|row| {
            let matches =
                row.file.is_some() || visible_paths.iter().any(|path| path.starts_with(&row.path));
            let collapsed = filter.is_empty()
                && review
                    .view
                    .collapsed
                    .iter()
                    .any(|parent| row.path != *parent && row.path.starts_with(parent));
            matches && !collapsed
        })
        .collect()
}
