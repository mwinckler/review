use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    style::{Colored, Stylize},
};
use ratatui::{layout::Rect, text::Text};
use review::{
    app::{App, Focus},
    comment_dialog::{CommentAction, CommentDialog, CommentDraft},
    comments::write_todo,
    config::Config,
    editor::EditorWorker,
    refresh::RefreshWorker,
    renderer::{RenderOptions, RenderWorker},
    session_dialog::{SessionAction, SessionDialog},
    state::Review,
    store::{SavedSession, SessionLock, SessionStore},
    ui,
    vcs::{Backend, Repository},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

struct PasteMode;

impl Drop for PasteMode {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), event::DisableBracketedPaste);
    }
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Review revision comparisons with a file tree, difft, and Vim keys",
    group(clap::ArgGroup::new("forget_selection").args(["forget", "forget_all", "cleanup"]))
)]
struct Cli {
    /// Compare BASE..HEAD directly; REV means REV..@
    ///
    /// Endpoints accept the selected backend's single-revision expressions.
    /// Omitted endpoints and @ select HEAD (Git) or the working copy (Jujutsu).
    /// Git also accepts BASE...HEAD for a merge-base comparison.
    #[arg(value_name = "REVSET", conflicts_with_all = ["base", "head", "resume", "list", "forget_selection"])]
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
    /// Resume by short ID (such as r3) or unique prefix; omit to resume the latest
    #[arg(short = 'r', long, value_name = "ID", num_args = 0..=1, default_missing_value = "latest", conflicts_with = "base")]
    resume: Option<String>,
    /// List this repository's saved review sessions
    #[arg(long, conflicts_with_all = ["base", "resume"])]
    list: bool,
    /// Set a session's display name by ID or unique prefix; an empty name clears it
    #[arg(long, num_args = 2, value_names = ["ID", "NAME"], action = clap::ArgAction::Set, conflicts_with_all = ["comparison", "base", "head", "resume", "list", "forget_selection", "yes"])]
    rename: Vec<String>,
    /// Permanently forget saved sessions by full ID or unique prefix
    #[arg(long, value_name = "ID", num_args = 1.., conflicts_with_all = ["base", "head", "resume", "list"])]
    forget: Vec<String>,
    /// Permanently forget all closed sessions for this repository/backend
    #[arg(long, conflicts_with_all = ["base", "head", "resume", "list"])]
    forget_all: bool,
    /// Permanently forget closed reviews last opened more than a week ago
    #[arg(long, conflicts_with_all = ["base", "head", "resume", "list"])]
    cleanup: bool,
    /// Confirm forgetting sessions without prompting
    #[arg(short = 'y', long, requires = "forget_selection")]
    yes: bool,
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
    if let [id, name] = cli.rename.as_slice() {
        let saved = store.rename(id, name)?;
        println!("Renamed session {}.", ui::display_path(&saved.label()));
        return Ok(());
    }
    if cli.cleanup || cli.forget_all || !cli.forget.is_empty() {
        let mut ids = BTreeSet::new();
        if cli.cleanup {
            ids.extend(store.cleanup_candidates()?);
        } else if cli.forget_all {
            for saved in store.list()? {
                if !store.is_open(&saved.id)? {
                    ids.insert(saved.id);
                }
            }
        } else {
            for prefix in &cli.forget {
                ids.insert(store.resolve(prefix)?);
            }
        }
        if ids.is_empty() {
            if cli.cleanup {
                println!("No stale review sessions to clean up.");
            } else {
                println!("No closed review sessions to forget.");
            }
            return Ok(());
        }
        if !cli.yes {
            ensure!(
                io::stdin().is_terminal() && io::stdout().is_terminal(),
                "Forgetting sessions requires confirmation; pass --yes to confirm without a terminal"
            );
            println!("Permanently forget {} saved review sessions:", ids.len());
            for id in &ids {
                println!("  {id}");
            }
            print!("Delete saved review progress and comments? [y/N] ");
            io::stdout().flush()?;
            let mut reply = String::new();
            io::stdin().read_line(&mut reply)?;
            if !reply.trim().eq_ignore_ascii_case("y") && !reply.trim().eq_ignore_ascii_case("yes")
            {
                println!("Cancelled.");
                return Ok(());
            }
        }
        let ids: Vec<_> = ids.into_iter().collect();
        let count = if cli.cleanup {
            store.cleanup(&ids)?
        } else {
            store.forget(&ids)?
        };
        println!(
            "Forgot {count} review session{}.",
            if count == 1 { "" } else { "s" }
        );
        return Ok(());
    }
    if cli.list {
        let sessions = store.list()?;
        let color = io::stdout().is_terminal() && !Colored::ansi_color_disabled();
        for saved in &sessions {
            let id = if color {
                let prefix = saved.unique_prefix(&sessions);
                let suffix = &saved.id[prefix.len()..];
                format!("{}{}", prefix.magenta().bold(), suffix.dark_grey())
            } else {
                saved.id.clone()
            };
            let opened =
                OffsetDateTime::from_unix_timestamp((saved.last_opened_at() / 1_000) as i64)?
                    .format(&Rfc3339)?;
            let name = saved
                .name
                .as_ref()
                .map(|name| format!(" · {}", ui::display_path(name)))
                .unwrap_or_default();
            println!(
                "{}{name}  Opened {}  {} → {}  Viewed {}/{}",
                id,
                opened,
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
        Some(id) => Some(store.resolve(id)?),
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
    app.session_label = saved.label();
    app.message = format!("Session {} · ? help", app.session_label);
    app.bindings_help = config
        .keybindings
        .iter()
        .map(|(key, action)| format!("{key}: {action}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let worker = RenderWorker::new(config.renderer.clone());
    let editor = EditorWorker::new(config.editor.clone(), store.directory.join("snapshots"));
    let result = ratatui::run(|terminal| {
        crossterm::execute!(io::stdout(), event::EnableBracketedPaste)?;
        let _paste = PasteMode;
        run(
            terminal,
            &repo,
            &mut app,
            (&worker, &editor),
            (&store, &mut saved, &lock),
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
    session: (&SessionStore, &mut SavedSession, &SessionLock),
    config: &Config,
) -> Result<()> {
    let (worker, editor) = workers;
    let (store, saved, lock) = session;
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
                app.source_lines.clear();
                diff = Text::raw("Rendering…");
                redraw = true;
            }
        } else {
            app.rendering = false;
            app.hunks.clear();
            app.source_lines.clear();
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
                app.source_lines = result.source_lines;
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
        app.keep_line_selection_visible(page, diff.lines.len());
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
                        || app.session_dialog.is_some()
                        || app.comment_dialog.is_some()
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
                            && app.session_dialog.is_none()
                            && app.comment_dialog.is_none()
                            && !app.searching
                            && !app.help
                            && matches!(key.code, KeyCode::Char('n' | 'N'));
                        let search_enter = app.focus == Focus::Diff
                            && app.searching
                            && app.session_dialog.is_none()
                            && app.comment_dialog.is_none()
                            && key.code == KeyCode::Enter;
                        let input_page =
                            if app.session_dialog.is_some() || app.comment_dialog.is_some() {
                                ui::session_dialog_area(Rect::new(0, 0, size.width, size.height))
                                    .height
                                    .saturating_sub(5)
                                    .max(1) as usize
                            } else {
                                page
                            };
                        app.handle_key(key, input_page, diff.lines.len(), size.width);
                        force_save |= key.code == KeyCode::Char(' ')
                            && app.session_dialog.is_none()
                            && app.comment_dialog.is_none();
                        if search || search_enter {
                            app.search_diff(&diff, key.code != KeyCode::Char('N'));
                        }
                    }
                    redraw = true;
                }
                Event::Resize(_, _) => {
                    redraw = true;
                }
                Event::Paste(text) => {
                    if let Some(dialog) = &mut app.session_dialog {
                        dialog.paste(&text);
                        redraw = true;
                    } else if let Some(CommentDialog::Compose(draft)) = &mut app.comment_dialog {
                        draft.insert(&text);
                        redraw = true;
                    }
                }
                _ => {}
            }
        }
        if let Some(action) = app.comment_action.take() {
            match action {
                CommentAction::Compose(target) => {
                    match CommentDraft::new(repo, &app.review, &target) {
                        Ok(draft) => {
                            app.comment_dialog = Some(CommentDialog::Compose(Box::new(draft)))
                        }
                        Err(error) => app.message = format!("Comment: {error:#}"),
                    }
                }
                CommentAction::WriteTodo(id) => {
                    let result = app
                        .review
                        .comments
                        .iter()
                        .position(|comment| comment.id == id)
                        .context("Comment was removed")
                        .and_then(|index| {
                            let receipt =
                                write_todo(repo, &app.review, &app.review.comments[index])?;
                            app.review.comments[index].updated_at = receipt.written_at;
                            app.review.comments[index].source_todo = Some(receipt);
                            Ok(())
                        });
                    app.message = match result {
                        Ok(()) => {
                            app.dirty = true;
                            app.comments_changed = true;
                            "TODO written to source; local comment retained".into()
                        }
                        Err(error) => format!("TODO: {error:#}"),
                    };
                }
            }
            redraw = true;
        }
        if let Some(action) = app.session_action.take() {
            match action {
                SessionAction::Open => match SessionDialog::load(store, &saved.id) {
                    Ok(dialog) => app.session_dialog = Some(dialog),
                    Err(error) => app.message = format!("Loading sessions failed: {error:#}"),
                },
                SessionAction::Close => app.session_dialog = None,
                SessionAction::Reload => {
                    let dialog = app.session_dialog.as_mut().expect("session dialog is open");
                    dialog.message = match dialog.reload(store) {
                        Ok(()) => "Sessions reloaded.".into(),
                        Err(error) => format!("Reload failed: {error:#}"),
                    };
                }
                SessionAction::Forget(ids) => {
                    app.session_dialog
                        .as_mut()
                        .expect("session dialog is open")
                        .forget(store, &ids);
                }
                SessionAction::Rename { id, name } => {
                    let result = if id == saved.id {
                        saved.review = app.review.clone();
                        lock.rename(saved, &name).map(|()| {
                            app.session_label = saved.label();
                        })
                    } else {
                        store.rename(&id, &name).map(|_| ())
                    };
                    let dialog = app.session_dialog.as_mut().expect("session dialog is open");
                    dialog.message = match result {
                        Ok(()) => {
                            dialog.renaming = None;
                            match dialog.reload(store) {
                                Ok(()) => "Session renamed.".into(),
                                Err(error) => format!("Session renamed · Reload failed: {error:#}"),
                            }
                        }
                        Err(error) => format!("Rename failed: {error:#}"),
                    };
                }
            }
            redraw = true;
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
                    if app.comment_dialog.is_none() {
                        app.message = format!("{action} · {invalidated} viewed marks reset");
                    }
                    let preview_changed = !previous_preview.iter().eq(app.preview_changes());
                    if preview_changed {
                        app.hunk_selection = None;
                        app.line_selection = None;
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
        force_save |= std::mem::take(&mut app.comments_changed);
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
