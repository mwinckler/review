use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{layout::Rect, text::Text};
use review::{
    app::{App, Focus},
    config::Config,
    editor::EditorWorker,
    refresh::RefreshWorker,
    renderer::{RenderOptions, RenderWorker},
    state::Review,
    store::{SavedSession, SessionLock, SessionStore},
    ui,
    vcs::{Backend, Repository},
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Review revision comparisons with a file tree, difft, and Vim keys"
)]
struct Cli {
    /// Compare BASE..HEAD directly; REV means REV..@
    ///
    /// Endpoints accept the selected backend's single-revision expressions.
    /// Omitted endpoints and @ select HEAD (Git) or the working copy (Jujutsu).
    /// Git also accepts BASE...HEAD for a merge-base comparison.
    #[arg(value_name = "REVSET", conflicts_with_all = ["base", "head", "resume", "list"])]
    comparison: Option<String>,
    /// Repository directory (defaults to the current directory)
    #[arg(short = 'C', long, default_value = ".")]
    repo: PathBuf,
    /// Override automatic repository detection
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// Compare from the merge base of this revision (defaults to master)
    #[arg(long)]
    base: Option<String>,
    /// Head revision (defaults to HEAD for Git, @ for Jujutsu)
    #[arg(long, requires = "base")]
    head: Option<String>,
    /// Resume a saved session by ID, or the latest session when omitted
    #[arg(long, num_args = 0..=1, default_missing_value = "latest", conflicts_with = "base")]
    resume: Option<String>,
    /// List this repository's saved review sessions
    #[arg(long, conflicts_with_all = ["base", "resume"])]
    list: bool,
    /// Override the review state directory
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Load a TOML config (defaults to $XDG_CONFIG_HOME/review/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = Repository::discover(&cli.repo, cli.backend)?;
    let state_dir = match cli.state_dir {
        Some(directory) => directory,
        None => {
            let base = directories::BaseDirs::new()
                .context("finding the user state directory; use --state-dir")?;
            base.state_dir()
                .unwrap_or(base.data_local_dir())
                .join("review")
        }
    };
    let store = SessionStore::new(&state_dir, &repo)?;
    if cli.list {
        for saved in store.list()? {
            println!(
                "{}  {} → {}  Viewed {}/{}",
                saved.id,
                ui::display_path(&saved.review.comparison.base_ref),
                ui::display_path(&saved.review.comparison.head_ref),
                saved.review.viewed.len(),
                saved.review.changes.len()
            );
        }
        return Ok(());
    }
    let resume_id = match cli.resume.as_deref() {
        Some("latest") => Some(
            store
                .list()?
                .first()
                .context("No saved review. Start one with review")?
                .id
                .clone(),
        ),
        Some(id) => Some(id.to_owned()),
        None => None,
    };
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "review needs an interactive terminal"
    );
    let config_path = match &cli.config {
        Some(path) => path.clone(),
        None => directories::BaseDirs::new()
            .context("finding the config directory; use --config")?
            .config_dir()
            .join("review/config.toml"),
    };
    let config = Config::load(&config_path, cli.config.is_some())?;
    let (mut saved, lock) = if let Some(id) = resume_id {
        store.open(&id)?
    } else {
        let comparison = if let Some(expression) = cli.comparison {
            repo.snapshot_expression(&expression)?
        } else {
            let head = cli.head.as_deref().unwrap_or(match repo.backend {
                Backend::Git => "HEAD",
                Backend::Jj => "@",
            });
            repo.snapshot(cli.base.as_deref().unwrap_or("master"), head)?
        };
        let changes = repo.changes(&comparison)?;
        store.create(Review::new(comparison, changes))?
    };
    let mut metadata_changed = repo.fill_change_ids(&mut saved.review.comparison)?;
    metadata_changed |= repo.fill_line_counts(&mut saved.review.comparison)?;
    if metadata_changed {
        lock.save(&mut saved)?;
    }
    let mut app = App::new(saved.review.clone());
    app.message = format!("Session {} · ? help", saved.id);
    app.bindings_help = config
        .keybindings
        .iter()
        .map(|(key, action)| format!("{key}: {action}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let worker = RenderWorker::new(config.renderer.clone());
    let editor = EditorWorker::new(config.editor.clone(), store.directory.join("snapshots"));
    let result = ratatui::run(|terminal| {
        run(
            terminal,
            &repo,
            &mut app,
            (&worker, &editor),
            &mut saved,
            &lock,
            &config,
        )
    });
    saved.review = app.review;
    let saved_result = lock.save(&mut saved);
    result.and(saved_result)
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    repo: &Repository,
    app: &mut App,
    workers: (&RenderWorker, &EditorWorker),
    saved: &mut SavedSession,
    lock: &SessionLock,
    config: &Config,
) -> Result<()> {
    let (worker, editor) = workers;
    let mut diff = Text::raw("Select a changed file");
    let mut requested = None;
    let mut request_id = 0;
    let mut redraw = true;
    let mut last_save = Instant::now();
    let refresher = RefreshWorker::new(repo, &app.review.comparison);
    while !app.quit {
        let size = terminal.size()?;
        let panes = ui::panes(Rect::new(0, 0, size.width, size.height), app);
        let width = if panes.diff.width > 0 {
            panes.diff.width
        } else {
            size.width
        }
        .saturating_sub(2)
        .max(1);
        let page = if app.focus == Focus::Tree {
            panes.tree.height
        } else {
            panes.diff.height
        }
        .saturating_sub(2)
        .max(1) as usize;
        if !app.review.view.active.is_empty() {
            let key = (
                app.review.view.active.clone(),
                width,
                app.review.view.inline,
            );
            if requested.as_ref() != Some(&key) {
                request_id = worker.request(
                    repo,
                    &app.review.comparison,
                    &app.review.view.active,
                    app.preview_changes().cloned().collect(),
                    RenderOptions {
                        width,
                        inline: app.review.view.inline,
                    },
                );
                requested = Some(key);
                app.rendering = true;
                app.hunks.clear();
                diff = Text::raw("Rendering…");
                redraw = true;
            }
        } else {
            app.rendering = false;
            app.hunks.clear();
            app.editor_requested = false;
            if requested.take().is_some() {
                request_id = worker.cancel();
                redraw = true;
            }
            diff = Text::raw(if app.review.changes.is_empty() {
                "Comparison complete — the snapshots are identical"
            } else if app.review.view.unviewed_only
                && app.review.viewed.len() == app.review.changes.len()
            {
                "Review complete — every file has been viewed. Press u to show all files."
            } else {
                "0 files match the current filters. Press Esc to clear them."
            });
        }
        for result in worker.results.try_iter() {
            if result.id == request_id {
                app.rendering = false;
                app.hunks = result.hunks;
                diff = match result.text {
                    Ok(text) => text,
                    Err(error) => Text::raw(format!("Diff error\n\n{}", ui::display_path(&error))),
                };
                app.position().vertical = app
                    .position()
                    .vertical
                    .min(diff.lines.len().saturating_sub(page));
                redraw = true;
            }
        }
        redraw |= app.apply_hunk_navigation(page, diff.lines.len());
        for result in editor.results.try_iter() {
            app.message = match result {
                Ok(location) => format!(
                    "Opened {}:{}{}",
                    ui::display_path(&location.file.to_string_lossy()),
                    location.line,
                    if location.read_only {
                        " · read-only snapshot"
                    } else {
                        ""
                    }
                ),
                Err(error) => format!("Editor: {error}"),
            };
            redraw = true;
        }
        if redraw {
            app.update_flash(panes.tree.height.saturating_sub(2) as usize);
            terminal.draw(|frame| ui::draw(frame, app, &diff))?;
            redraw = false;
        }
        let mut force_save = false;
        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    app.message.clear();
                    let keys = if app.searching
                        || app.flash.is_some()
                        || app.help
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        vec![key]
                    } else {
                        config.translate(key)
                    };
                    for key in keys {
                        let search = app.focus == Focus::Diff
                            && !app.searching
                            && !app.help
                            && matches!(key.code, KeyCode::Char('n' | 'N'));
                        let search_enter =
                            app.focus == Focus::Diff && app.searching && key.code == KeyCode::Enter;
                        app.handle_key(key, page, diff.lines.len(), size.width);
                        force_save |= key.code == KeyCode::Char(' ');
                        if search || search_enter {
                            app.search_diff(&diff, key.code != KeyCode::Char('N'));
                        }
                    }
                    redraw = true;
                }
                Event::Resize(_, _) => {
                    redraw = true;
                }
                _ => {}
            }
        }
        if app.refresh {
            app.refresh = false;
            refresher.request();
        }
        if app.editor_requested
            && !app.rendering
            && requested.as_ref().is_some_and(|key| {
                key.0 == app.review.view.active && key.2 == app.review.view.inline
            })
        {
            app.editor_requested = false;
            let result = app
                .editor_location()
                .context("Select a file or mapped hunk to open in the editor")
                .and_then(|source| {
                    let change = app
                        .review
                        .changes
                        .iter()
                        .find(|change| change.path == source.path)
                        .context("Selected file is no longer in the comparison")?;
                    editor.request(repo, &app.review.comparison, change, source)
                });
            app.message = match result {
                Ok(()) => "Opening editor…".into(),
                Err(error) => format!("Editor: {error:#}"),
            };
            redraw = true;
        }
        for result in refresher.results.try_iter() {
            match result.snapshot {
                Ok((comparison, changes)) => {
                    let previous_preview: Vec<_> = app.preview_changes().cloned().collect();
                    let invalidated = app.review.reconcile(comparison, changes);
                    app.rebuild();
                    let action = if result.manual {
                        "Snapshots refreshed"
                    } else {
                        "Changes reloaded"
                    };
                    app.message = format!("{action} · {invalidated} viewed marks reset");
                    let preview_changed = !previous_preview.iter().eq(app.preview_changes());
                    if preview_changed {
                        app.hunk_selection = None;
                        app.editor_requested = false;
                    }
                    if result.manual || preview_changed {
                        request_id = worker.cancel();
                        requested = None;
                    }
                    app.dirty = true;
                    force_save = true;
                }
                Err(error) => {
                    let action = if result.manual {
                        "Refresh failed"
                    } else {
                        "Auto-refresh failed"
                    };
                    app.message = format!("{action}: {error}");
                }
            }
            redraw = true;
        }
        if app.dirty
            && (force_save || app.quit || last_save.elapsed() >= Duration::from_millis(250))
        {
            saved.review = app.review.clone();
            match lock.save(saved) {
                Ok(()) => {
                    app.dirty = false;
                }
                Err(error) => {
                    if app.quit {
                        return Err(error);
                    }
                    app.message = format!("Save failed; retrying: {error:#}");
                    redraw = true;
                }
            }
            last_save = Instant::now();
        }
    }
    Ok(())
}
