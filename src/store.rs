use std::{
    cmp::Ordering,
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{
    state::Review,
    vcs::{Backend, Repository},
};

const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedSession {
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub repository: PathBuf,
    pub backend: Backend,
    pub updated_at: u64,
    #[serde(default)]
    pub opened_at: Option<u64>,
    pub review: Review,
}

impl SavedSession {
    fn read(path: &Path) -> Result<Self> {
        let session: Self = serde_json::from_reader(
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
            path.file_stem()
                .is_some_and(|stem| stem == session.id.as_str()),
            "session ID differs from its filename"
        );
        validate_id(&session.id)?;
        Ok(session)
    }

    pub fn label(&self) -> String {
        match &self.name {
            Some(name) => format!("{} · {name}", self.id),
            None => self.id.clone(),
        }
    }

    pub fn last_opened_at(&self) -> u64 {
        self.opened_at.unwrap_or(self.updated_at)
    }

    fn cmp_saved(&self, other: &Self) -> Ordering {
        self.updated_at
            .cmp(&other.updated_at)
            .then_with(|| session_number(&self.id).cmp(&session_number(&other.id)))
            .then_with(|| self.id.cmp(&other.id))
            .then_with(|| self.repository.cmp(&other.repository))
            .then_with(|| (self.backend == Backend::Jj).cmp(&(other.backend == Backend::Jj)))
    }

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
    file: File,
    target: PathBuf,
}

impl SessionStore {
    pub fn new(directory: &Path, repo: &Repository) -> Result<Self> {
        let directory = repository_directory(directory, &repo.root, repo.backend)?;
        fs::create_dir_all(&directory)
            .with_context(|| format!("creating state directory {}", directory.display()))?;
        Ok(Self {
            directory,
            repository: repo.clone(),
        })
    }

    pub fn git_objects(&self) -> Result<PathBuf> {
        Ok(self.directory.canonicalize()?.join("git-objects"))
    }

    pub fn last_opened(
        directory: &Path,
        repository: Option<&Path>,
        backend: Option<Backend>,
    ) -> Result<Option<SavedSession>> {
        let directories = if let Some(root) = repository {
            [Backend::Git, Backend::Jj]
                .into_iter()
                .filter(|candidate| backend.is_none_or(|backend| backend == *candidate))
                .map(|backend| repository_directory(directory, root, backend))
                .collect::<Result<Vec<_>>>()?
        } else {
            let entries = match fs::read_dir(directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("reading state directory {}", directory.display())
                    });
                }
            };
            let mut directories = Vec::new();
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir()
                    && entry.file_name().to_str().is_some_and(|name| {
                        name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                {
                    directories.push(entry.path());
                }
            }
            directories
        };
        let mut last: Option<SavedSession> = None;
        for folder in directories {
            let entries = match fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("reading sessions in {}", folder.display()));
                }
            };
            for entry in entries {
                let path = entry?.path();
                if path.extension().is_none_or(|extension| extension != "json") {
                    continue;
                }
                let saved = match SavedSession::read(&path) {
                    Ok(saved) => saved,
                    Err(error) if is_io_error(&error, ErrorKind::NotFound) => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                ensure!(
                    repository_directory(directory, &saved.repository, saved.backend)? == folder,
                    "session repository identity differs from its state directory: {}",
                    path.display()
                );
                if backend.is_some_and(|backend| backend != saved.backend) {
                    continue;
                }
                if last.as_ref().is_none_or(|previous| {
                    saved
                        .last_opened_at()
                        .cmp(&previous.last_opened_at())
                        .then_with(|| saved.cmp_saved(previous))
                        .is_gt()
                }) {
                    last = Some(saved);
                }
            }
        }
        Ok(last)
    }

    pub fn create(&self, review: Review) -> Result<(SavedSession, SessionLock)> {
        let reusable_before = SystemTime::now() - WEEK;
        let mut reusable = BTreeSet::new();
        let mut number = 0;
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json" || extension == "lock")
                && let Some(id) = path.file_stem().and_then(|stem| stem.to_str())
                && let Some(existing) = session_number(id)
            {
                number = number.max(existing);
                if path
                    .extension()
                    .is_some_and(|extension| extension == "lock")
                    && entry.metadata()?.modified()? <= reusable_before
                    && !path.with_extension("json").try_exists()?
                {
                    reusable.insert(existing);
                }
            }
        }
        let (id, lock) = loop {
            let recycled = reusable.pop_first();
            let candidate = if let Some(number) = recycled {
                number
            } else {
                number = number.checked_add(1).context("session numbers exhausted")?;
                number
            };
            let id = format!("r{candidate}");
            // Reserve new numbers atomically; recycled numbers retain their lock files.
            if recycled.is_none() {
                match OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(self.directory.join(format!("{id}.lock")))
                {
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                        continue;
                    }
                    Err(error) => return Err(error).context("reserving a review session number"),
                }
            }
            let lock = match self.lock(&id) {
                Ok(lock) => lock,
                Err(error) if is_io_error(&error, ErrorKind::WouldBlock) => {
                    continue;
                }
                Err(error) => return Err(error),
            };
            // A concurrent deletion may have restarted the cooldown since the directory scan.
            if lock.target.try_exists()?
                || (recycled.is_some() && lock.file.metadata()?.modified()? > reusable_before)
            {
                continue;
            }
            lock.file.set_modified(SystemTime::now())?;
            lock.file.sync_all()?;
            break (id, lock);
        };
        let mut session = SavedSession {
            version: 1,
            id,
            name: None,
            repository: self.repository.root.clone(),
            backend: self.repository.backend,
            updated_at: 0,
            opened_at: Some(now_millis()?),
            review,
        };
        lock.save(&mut session)?;
        Ok((session, lock))
    }

    pub fn open(&self, id: &str) -> Result<(SavedSession, SessionLock)> {
        let id = self.resolve(id)?;
        let lock = self.lock(&id)?;
        let mut session = self.load(&lock.target)?;
        session.opened_at = Some(now_millis()?);
        lock.save(&mut session)?;
        Ok((session, lock))
    }

    pub fn is_open(&self, id: &str) -> Result<bool> {
        let id = self.resolve(id)?;
        match self.lock(&id) {
            Ok(_) => Ok(false),
            Err(error) if is_io_error(&error, ErrorKind::WouldBlock) => Ok(true),
            Err(error) => Err(error),
        }
    }

    pub fn rename(&self, prefix: &str, name: &str) -> Result<SavedSession> {
        let id = self.resolve(prefix)?;
        let lock = self.lock(&id)?;
        let mut saved = self.load(&lock.target)?;
        lock.rename(&mut saved, name)?;
        Ok(saved)
    }

    pub fn forget(&self, prefixes: &[String]) -> Result<usize> {
        ensure!(
            !prefixes.is_empty(),
            "Select at least one session to forget"
        );
        let ids = prefixes
            .iter()
            .map(|prefix| self.resolve(prefix))
            .collect::<Result<BTreeSet<_>>>()?;
        let locks = ids
            .iter()
            .map(|id| self.lock(id))
            .collect::<Result<Vec<_>>>()?;
        for lock in &locks {
            ensure!(
                lock.target.try_exists()?,
                "Session disappeared before deletion: {}",
                lock.target.display()
            );
        }
        self.forget_locked(&locks)
    }

    pub fn cleanup_candidates(&self) -> Result<Vec<String>> {
        let now = now_millis()?;
        let mut ids = Vec::new();
        for saved in self.list()? {
            if is_stale(saved.last_opened_at(), now) && !self.is_open(&saved.id)? {
                ids.push(saved.id);
            }
        }
        Ok(ids)
    }

    pub fn cleanup(&self, ids: &[String]) -> Result<usize> {
        let now = now_millis()?;
        let mut locks = Vec::new();
        for id in ids.iter().collect::<BTreeSet<_>>() {
            let lock = match self.lock(id) {
                Ok(lock) => lock,
                Err(error) if is_io_error(&error, ErrorKind::WouldBlock) => {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let saved = match self.load(&lock.target) {
                Ok(saved) => saved,
                Err(error) if is_io_error(&error, ErrorKind::NotFound) => {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if is_stale(saved.last_opened_at(), now) {
                locks.push(lock);
            }
        }
        self.forget_locked(&locks)
    }

    fn forget_locked(&self, locks: &[SessionLock]) -> Result<usize> {
        for (deleted, lock) in locks.iter().enumerate() {
            lock.file
                .set_modified(SystemTime::now())
                .and_then(|()| lock.file.sync_all())
                .and_then(|()| fs::remove_file(&lock.target))
                .with_context(|| {
                    format!(
                        "forgetting session {} after removing {deleted}",
                        lock.target.display()
                    )
                })?;
        }
        #[cfg(unix)]
        {
            File::open(&self.directory)?.sync_all()?;
        }
        Ok(locks.len())
    }

    pub fn resolve(&self, prefix: &str) -> Result<String> {
        validate_id(prefix)?;
        self.find(prefix)?.with_context(|| {
            format!(
                "No saved review matches session ID {prefix:?}. Use review --list to see saved sessions"
            )
        })
    }

    pub fn find(&self, prefix: &str) -> Result<Option<String>> {
        if validate_id(prefix).is_err() {
            return Ok(None);
        }
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
                    return Ok(Some(id.to_owned()));
                }
                matches.push(id.to_owned());
            }
        }
        matches.sort();
        match matches.as_slice() {
            [id] => Ok(Some(id.clone())),
            [] => Ok(None),
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
        sessions.sort_by(|a, b| b.cmp_saved(a));
        Ok(sessions)
    }

    fn load(&self, path: &Path) -> Result<SavedSession> {
        let mut session = SavedSession::read(path)?;
        ensure!(
            session.repository == self.repository.root
                && session.backend == self.repository.backend,
            "session repository identity differs from the current repository"
        );
        if let Some(snapshot) = &mut session.review.comparison.working_tree {
            snapshot.objects = self.git_objects()?;
        }
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
            file,
            target: self.directory.join(format!("{id}.json")),
        })
    }
}

fn repository_directory(directory: &Path, root: &Path, backend: Backend) -> Result<PathBuf> {
    let identity = serde_json::to_vec(&(root, backend))?;
    Ok(directory.join(blake3::hash(&identity).to_hex().as_str()))
}

fn is_io_error(error: &anyhow::Error, kind: ErrorKind) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == kind)
}

pub(crate) fn now_millis() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

fn is_stale(opened_at: u64, now: u64) -> bool {
    Duration::from_millis(now.saturating_sub(opened_at)) > WEEK
}

fn session_number(id: &str) -> Option<u64> {
    id.strip_prefix('r')?.parse().ok()
}

fn validate_id(id: &str) -> Result<()> {
    let numbered = id
        .strip_prefix('r')
        .is_some_and(|number| number.bytes().all(|byte| byte.is_ascii_digit()));
    ensure!(
        !id.is_empty()
            && (numbered
                || id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')),
        "invalid session ID"
    );
    Ok(())
}

impl SessionLock {
    pub fn save(&self, session: &mut SavedSession) -> Result<()> {
        session.updated_at = now_millis()?;
        self.write(session)
    }

    pub fn rename(&self, session: &mut SavedSession, name: &str) -> Result<()> {
        ensure!(
            name.chars()
                .all(|ch| !ch.is_control() && !matches!(ch, '\u{2028}' | '\u{2029}')),
            "Session names must be a single line without control characters"
        );
        let name = name.trim();
        let previous = std::mem::replace(
            &mut session.name,
            (!name.is_empty()).then(|| name.to_owned()),
        );
        let result = self.write(session);
        if result.is_err() {
            session.name = previous;
        }
        result
    }

    fn write(&self, session: &SavedSession) -> Result<()> {
        let directory = self
            .target
            .parent()
            .expect("session target includes its directory");
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

#[cfg(test)]
mod tests {
    use super::{WEEK, is_stale};

    #[test]
    fn cleanup_age_is_strictly_greater_than_a_week_and_preserves_future_timestamps() {
        let opened_at = 1_000;
        let week = WEEK.as_millis() as u64;
        assert!(!is_stale(opened_at, opened_at + week - 1));
        assert!(!is_stale(opened_at, opened_at + week));
        assert!(is_stale(opened_at, opened_at + week + 1));
        assert!(!is_stale(opened_at, opened_at - 1));
        assert!(!is_stale(u64::MAX, opened_at));
    }
}
