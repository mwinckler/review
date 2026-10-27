use std::fs;

use review::{
    state::Review,
    store::{SavedSession, SessionStore},
    vcs::Repository,
};

pub fn write(store: &SessionStore, repo: &Repository, id: &str, review: Review) -> SavedSession {
    let saved = SavedSession {
        version: 1,
        id: id.into(),
        repository: repo.root.clone(),
        backend: repo.backend,
        updated_at: 0,
        review,
    };
    fs::write(
        store.directory.join(format!("{id}.json")),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    saved
}
