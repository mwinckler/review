use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, WatcherKind,
    event::{AccessKind, AccessMode, CreateKind, ModifyKind},
};

use crate::{
    git_index::{excluded_state, git_path, global_ignore},
    vcs::Repository,
};

const DEBOUNCE: Duration = Duration::from_millis(200);
const MAX_DELAY: Duration = Duration::from_secs(1);
const SAFETY_SCAN: Duration = Duration::from_secs(60);
const FALLBACK_SCAN: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
    Snapshot,
    Rebuild,
    WatchLost,
}

pub(crate) enum Scan {
    ReuseIndex,
    RebuildIndex,
}

struct Pending {
    first: Instant,
    last: Instant,
    change: Change,
}

impl Pending {
    fn deadline(&self) -> Instant {
        if self.change == Change::WatchLost {
            return self.first;
        }
        (self.first + MAX_DELAY).min(self.last + DEBOUNCE)
    }
}

pub(crate) struct WorktreeWatch {
    watcher: Option<RecommendedWatcher>,
    pending: Arc<Mutex<Option<Pending>>>,
    next_scan: Instant,
}

impl WorktreeWatch {
    pub(crate) fn new(repo: &Repository, objects: &Path, wake: impl Fn() + Send + 'static) -> Self {
        let pending = Arc::new(Mutex::new(None::<Pending>));
        let queued = Arc::clone(&pending);
        let watcher = watch(repo, objects, move |change| {
            let now = Instant::now();
            let mut pending = queued.lock().unwrap();
            if let Some(pending) = pending.as_mut() {
                pending.last = now;
                pending.change = pending.change.max(change);
            } else {
                *pending = Some(Pending {
                    first: now,
                    last: now,
                    change,
                });
                wake();
            }
        })
        .ok();
        let mut state = Self {
            watcher,
            pending,
            next_scan: Instant::now(),
        };
        state.schedule_scan();
        state
    }

    fn schedule_scan(&mut self) {
        self.next_scan = Instant::now()
            + if self.watcher.is_some() {
                SAFETY_SCAN
            } else {
                FALLBACK_SCAN
            };
    }

    pub(crate) fn wait(&self) -> Duration {
        let deadline = self
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .map_or(self.next_scan, |pending| {
                pending.deadline().min(self.next_scan)
            });
        deadline.saturating_duration_since(Instant::now())
    }

    pub(crate) fn take_due(&mut self, force: bool) -> Option<Scan> {
        let now = Instant::now();
        let rescan = force || now >= self.next_scan;
        let change = {
            let mut pending = self.pending.lock().unwrap();
            if rescan
                || pending
                    .as_ref()
                    .is_some_and(|event| now >= event.deadline())
            {
                pending.take().map(|event| event.change)
            } else {
                None
            }
        };
        if change == Some(Change::WatchLost) {
            // An overflow can also miss the creation of a directory to watch.
            self.watcher = None;
        }
        if rescan || self.watcher.is_none() && change.is_some() {
            self.schedule_scan();
        }
        if rescan || change >= Some(Change::Rebuild) {
            Some(Scan::RebuildIndex)
        } else {
            change.map(|_| Scan::ReuseIndex)
        }
    }
}

struct Paths {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    index: PathBuf,
    global_ignore: Option<PathBuf>,
    state: Option<PathBuf>,
}

impl Paths {
    fn change(&self, path: &Path, event: &Event) -> Option<Change> {
        if self
            .state
            .as_ref()
            .is_some_and(|state| path.starts_with(state))
        {
            return None;
        }
        if path == self.index
            || self.global_ignore.as_deref() == Some(path)
            || path.strip_prefix(&self.root).ok() == Some(Path::new(".git"))
        {
            return Some(Change::Rebuild);
        }
        for dir in [&self.git_dir, &self.common_dir] {
            if let Ok(path) = path.strip_prefix(dir) {
                if path.starts_with("refs") && path.extension().is_none_or(|ext| ext != "lock") {
                    return Some(Change::Snapshot);
                }
                if [
                    "",
                    "HEAD",
                    "config",
                    "config.worktree",
                    "info",
                    "info/exclude",
                    "info/attributes",
                    "info/sparse-checkout",
                ]
                .iter()
                .any(|name| path == Path::new(name))
                {
                    return Some(Change::Rebuild);
                }
                return (path == Path::new("packed-refs")
                    || path == Path::new("reftable/tables.list"))
                .then_some(Change::Snapshot);
            }
        }
        if !path.starts_with(&self.root) {
            return None;
        }
        if path
            .file_name()
            .is_some_and(|name| name == ".gitignore" || name == ".gitattributes")
            || matches!(event.kind, EventKind::Create(CreateKind::Folder))
            || matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) && path.is_dir()
        {
            return Some(Change::Rebuild);
        }
        Some(Change::Snapshot)
    }
}

fn watch(
    repo: &Repository,
    objects: &Path,
    changed: impl Fn(Change) + Send + 'static,
) -> anyhow::Result<RecommendedWatcher> {
    anyhow::ensure!(
        !matches!(
            RecommendedWatcher::kind(),
            WatcherKind::NullWatcher | WatcherKind::PollWatcher
        ),
        "Native filesystem notifications are unavailable."
    );
    let paths = Paths {
        root: repo.root.clone(),
        state: excluded_state(repo, objects).map(|path| repo.root.join(path)),
        git_dir: git_path(repo, &["--absolute-git-dir"])?,
        common_dir: git_path(repo, &["--git-common-dir"])?,
        index: git_path(repo, &["--git-path", "index"])?,
        global_ignore: global_ignore(repo),
    };
    let paths = Arc::new(paths);
    let events = Arc::clone(&paths);
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<Event>| {
            let change = match result {
                Err(_) => Some(Change::WatchLost),
                Ok(event) if event.need_rescan() => Some(Change::WatchLost),
                Ok(event) if matches!(event.kind, EventKind::Access(kind) if kind != AccessKind::Close(AccessMode::Write)) => {
                    None
                }
                Ok(event) => event
                    .paths
                    .iter()
                    .filter_map(|path| events.change(path, &event))
                    .max(),
            };
            if let Some(change) = change {
                changed(change);
            }
        },
        Config::default().with_follow_symlinks(false),
    )?;
    watcher.watch(&paths.root, RecursiveMode::Recursive)?;
    if !paths.common_dir.starts_with(&paths.root) {
        watcher.watch(&paths.common_dir, RecursiveMode::Recursive)?;
    }
    if !paths.git_dir.starts_with(&paths.root) && !paths.git_dir.starts_with(&paths.common_dir) {
        watcher.watch(&paths.git_dir, RecursiveMode::Recursive)?;
    }
    let external_parent = |path: &Path| {
        let parent = path.parent()?.to_owned();
        (![&paths.root, &paths.git_dir, &paths.common_dir]
            .iter()
            .any(|dir| parent.starts_with(dir)))
        .then_some(parent)
    };
    let index_parent = external_parent(&paths.index);
    if let Some(parent) = &index_parent {
        watcher.watch(parent, RecursiveMode::NonRecursive)?;
    }
    if let Some(parent) = paths.global_ignore.as_deref().and_then(external_parent)
        && Some(&parent) != index_parent.as_ref()
    {
        // The global exclusions file and its parent may not exist yet.
        let _ = watcher.watch(&parent, RecursiveMode::NonRecursive);
    }
    Ok(watcher)
}
