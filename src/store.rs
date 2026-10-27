use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{
    state::Review,
    vcs::{Backend, Repository},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedSession {
    pub version: u32,
    pub id: String,
    pub repository: PathBuf,
    pub backend: Backend,
    pub updated_at: u64,
    pub review: Review,
}

impl SavedSession {
    pub fn unique_prefix(&self, sessions: &[Self]) -> &str {
        let shared = sessions
            .iter()
            .filter(|other| other.id != self.id)
            .map(|other| {
                self.id
                    .bytes()
                    .zip(other.id.bytes())
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .max()
            .unwrap_or(0);
        &self.id[..(shared + 1).min(self.id.len())]
    }
}

pub struct SessionStore {
    pub directory: PathBuf,
    repository: Repository,
}

pub struct SessionLock {
    _file: File,
    target: PathBuf,
}

impl SessionStore {
    pub fn new(directory: &Path, repo: &Repository) -> Result<Self> {
        let identity = serde_json::to_vec(&(&repo.root, repo.backend))?;
        let directory = directory.join(blake3::hash(&identity).to_hex().as_str());
        fs::create_dir_all(&directory)
            .with_context(|| format!("creating state directory {}", directory.display()))?;
        Ok(Self {
            directory,
            repository: repo.clone(),
        })
    }

    pub fn create(&self, review: Review) -> Result<(SavedSession, SessionLock)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let (id, lock) = loop {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let seed = format!(
                "{nanos:x}-{:x}-{:x}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let id = blake3::hash(seed.as_bytes()).to_hex()[..12].to_owned();
            let lock = self.lock(&id)?;
            if !lock.target.try_exists()? {
                break (id, lock);
            }
        };
        let mut session = SavedSession {
            version: 1,
            id,
            repository: self.repository.root.clone(),
            backend: self.repository.backend,
            updated_at: 0,
            review,
        };
        lock.save(&mut session)?;
        Ok((session, lock))
    }

    pub fn open(&self, id: &str) -> Result<(SavedSession, SessionLock)> {
        let id = self.resolve(id)?;
        let lock = self.lock(&id)?;
        let session = self.load(&lock.target)?;
        Ok((session, lock))
    }

    pub fn resolve(&self, prefix: &str) -> Result<String> {
        validate_id(prefix)?;
        let mut matches = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && let Some(id) = path.file_stem().and_then(|stem| stem.to_str())
                && id.starts_with(prefix)
            {
                if id == prefix {
                    return Ok(id.to_owned());
                }
                matches.push(id.to_owned());
            }
        }
        matches.sort();
        match matches.as_slice() {
            [id] => Ok(id.clone()),
            [] => bail!(
                "No saved review matches session ID {prefix:?}. Use review --list to see saved sessions"
            ),
            _ => bail!(
                "Ambiguous session ID prefix {prefix:?}: {}. Use a longer prefix from review --list",
                matches.join(", ")
            ),
        }
    }

    pub fn list(&self) -> Result<Vec<SavedSession>> {
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                sessions.push(self.load(&path)?);
            }
        }
        sessions.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(sessions)
    }

    fn load(&self, path: &Path) -> Result<SavedSession> {
        let session: SavedSession = serde_json::from_reader(
            File::open(path).with_context(|| format!("opening session {}", path.display()))?,
        )
        .with_context(|| format!("reading session {}", path.display()))?;
        ensure!(
            session.version == 1,
            "unsupported session version {} in {}",
            session.version,
            path.display()
        );
        ensure!(
            session.repository == self.repository.root
                && session.backend == self.repository.backend,
            "session repository identity differs from the current repository"
        );
        ensure!(
            path.file_stem()
                .is_some_and(|stem| stem == session.id.as_str()),
            "session ID differs from its filename"
        );
        validate_id(&session.id)?;
        Ok(session)
    }

    fn lock(&self, id: &str) -> Result<SessionLock> {
        validate_id(id)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join(format!("{id}.lock")))?;
        file.try_lock_exclusive()
            .with_context(|| format!("session {id} is already open or could not be locked"))?;
        Ok(SessionLock {
            _file: file,
            target: self.directory.join(format!("{id}.json")),
        })
    }
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'),
        "invalid session ID"
    );
    Ok(())
}

impl SessionLock {
    pub fn save(&self, session: &mut SavedSession) -> Result<()> {
        let directory = self
            .target
            .parent()
            .expect("session target includes its directory");
        session.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .try_into()?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut temporary, session)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.target)
            .context("atomically saving review session")?;
        #[cfg(unix)]
        {
            File::open(directory)?.sync_all()?;
        }
        Ok(())
    }
}
