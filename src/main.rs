use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::{Args, CommandFactory, Parser, Subcommand};
use crossterm::{
    clipboard::CopyToClipboard,
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
    gitlab::{GitLab, MrReference, Project},
    gitlab_ui::{self, Pick},
    gitlab_worker::{GitLabJob, GitLabOutcome, GitLabWorker},
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
    subcommand_precedence_over_arg = true,
    group(clap::ArgGroup::new("forget_selection").args(["forget", "forget_all", "cleanup"]))
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Resume a saved review by ID/prefix, or compare revisions.
    ///
    /// Saved IDs and unique prefixes take precedence over revision expressions.
    /// BASE..HEAD compares snapshots directly; REV means REV..@.
    /// Endpoints accept the selected backend's single-revision expressions.
    /// Omitted endpoints and @ select HEAD (Git) or the working copy (Jujutsu).
    /// Git also accepts BASE...HEAD for a merge-base comparison.
    #[arg(value_name = "REVSET|ID", conflicts_with_all = ["base", "head", "resume", "list", "forget_selection"])]
    comparison: Option<String>,
    /// Repository directory (defaults to the current directory)
    #[arg(short = 'C', long, default_value = ".", global = true)]
    repo: PathBuf,
    /// Override automatic repository detection
    #[arg(long, value_enum, global = true)]
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
    #[arg(long, visible_alias = "prune", conflicts_with_all = ["base", "head", "resume", "list"])]
    cleanup: bool,
    /// Confirm forgetting sessions without prompting
    #[arg(short = 'y', long, requires = "forget_selection")]
    yes: bool,
    /// Override the review state directory
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    /// Load a TOML config (defaults to $XDG_CONFIG_HOME/review/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Open the most recently opened review for this repository, or overall outside a repository
    Last,
    /// Select a GitLab merge request and prepare its source branch for review
    Gitlab(GitLabArgs),
}

#[derive(Debug, Args)]
#[command(subcommand_precedence_over_arg = true)]
struct GitLabArgs {
    #[command(subcommand)]
    command: Option<GitLabCommand>,
    /// Merge request number or URL; omit to select from open MRs
    mr: Option<String>,
    /// Repository remote containing the MR's target project
    #[arg(long)]
    remote: Option<String>,
    /// GitLab API hostname, including a port when needed
    #[arg(long, global = true)]
    hostname: Option<String>,
}

#[derive(Debug, Subcommand)]
enum GitLabCommand {
    /// Authenticate through glab, sharing its saved credentials
    Login {
        /// Use browser OAuth authentication
        #[arg(long, conflicts_with = "device")]
        web: bool,
        /// Use OAuth device authentication
        #[arg(long)]
        device: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.command.is_some()
        && (cli.comparison.is_some()
            || cli.base.is_some()
            || cli.head.is_some()
            || cli.resume.is_some()
            || cli.list
            || !cli.rename.is_empty()
            || !cli.forget.is_empty()
            || cli.forget_all
            || cli.cleanup
            || cli.yes)
    {
        Cli::command().error(clap::error::ErrorKind::ArgumentConflict, "Startup commands cannot be used with local comparison or session-management options").exit();
    }
    if let Some(Commands::Gitlab(args)) = &cli.command
        && let Some(GitLabCommand::Login { web, device }) = args.command
    {
        if args.mr.is_some() || args.remote.is_some() {
            Cli::command()
                .error(
                    clap::error::ErrorKind::ArgumentConflict,
                    "GitLab login cannot be used with an MR or --remote",
                )
                .exit();
        }
        return GitLab::login(args.hostname.as_deref(), web, device);
    }
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
    let (repo, last_id) = if matches!(cli.command, Some(Commands::Last)) {
        let current = Repository::find(&cli.repo, cli.backend)?;
        let saved = SessionStore::last_opened(
            &state_dir,
            current.as_ref().map(|repo| repo.root.as_path()),
            cli.backend,
        )?
        .context(if current.is_some() {
            "No saved review for this repository. Start one with review"
        } else {
            "No saved reviews. Start one with review in a repository"
        })?;
        let repo =
            Repository::discover(&saved.repository, Some(saved.backend)).with_context(|| {
                format!(
                    "opening last review {} in {}",
                    saved.id,
                    saved.repository.display()
                )
            })?;
        ensure!(
            repo.root == saved.repository,
            "The last review's repository has moved or is no longer a repository: {}",
            saved.repository.display()
        );
        (repo, Some(saved.id))
    } else {
        (Repository::discover(&cli.repo, cli.backend)?, None)
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
    let resume_id = last_id.or(match (cli.resume.as_deref(), cli.comparison.as_deref()) {
        (Some("latest"), _) => Some(
            store
                .list()?
                .first()
                .context("No saved review. Start one with review")?
                .id
                .clone(),
        ),
        (Some(id), _) => Some(store.resolve(id)?),
        (None, Some(id)) => store.find(id)?,
        (None, None) => None,
    });
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
    let resuming = resume_id.is_some();
    let (mut saved, lock) = if let Some(id) = resume_id {
        store.open(&id)?
    } else {
        let review = if let Some(Commands::Gitlab(args)) = &cli.command {
            let Some(review) = start_gitlab(&repo, args)? else {
                return Ok(());
            };
            review
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
            Review::new(comparison, changes)
        };
        let (mut saved, lock) = store.create(review)?;
        if let Some(gitlab) = &saved.review.gitlab {
            let label = gitlab.label();
            lock.rename(&mut saved, &label)?;
        }
        (saved, lock)
    };
    let mut metadata_changed = repo.fill_change_ids(&mut saved.review.comparison)?;
    metadata_changed |= repo.fill_commit_titles(&mut saved.review.comparison)?;
    metadata_changed |= repo.fill_line_counts(&mut saved.review.comparison)?;
    metadata_changed |= repo.fill_file_commit_ids(&mut saved.review.comparison)?;
    if metadata_changed {
        lock.save(&mut saved)?;
    }
    let lock = Arc::new(lock);
    let mut app = App::new(saved.review.clone());
    app.session_label = saved.label();
    app.message = format!("Session {} · ? help", app.session_label);
    if resuming && app.review.gitlab.is_some() {
        app.submission_action = Some(GitLabJob::Discussions);
    }
    app.bindings_help = config
        .keybindings
        .iter()
        .map(|(key, action)| format!("{key}: {action}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let worker = RenderWorker::new(config.renderer.clone());
    let editor = EditorWorker::new(config.editor.clone(), store.directory.join("snapshots"));
    let mut gitlab_worker = GitLabWorker::default();
    let mut result = ratatui::run(|terminal| {
        crossterm::execute!(io::stdout(), event::EnableBracketedPaste)?;
        let _paste = PasteMode;
        run(
            terminal,
            &repo,
            &mut app,
            (&worker, &editor, &mut gitlab_worker),
            (&store, &mut saved, &lock),
            &config,
        )
    });
    // A terminal error can occur mid-request. Join the writer before any final save.
    if let Some(reply) = gitlab_worker.finish() {
        saved = reply.saved;
        app.review = saved.review.clone();
        result = result.and(reply.result.map(|_| ()).map_err(anyhow::Error::msg));
    }
    saved.review = app.review;
    let saved_result = lock.save(&mut saved);
    result.and(saved_result)
}

fn start_gitlab(repo: &Repository, args: &GitLabArgs) -> Result<Option<Review>> {
    let reference = args.mr.as_deref().map(MrReference::parse).transpose()?;
    let mut remotes = Vec::new();
    for (name, url) in repo.remotes()? {
        let Ok(mut project) = Project::from_remote(&url) else {
            continue;
        };
        if let Some(hostname) = &args.hostname {
            project.host.clone_from(hostname);
        }
        if args.remote.as_ref().is_none_or(|remote| remote == &name)
            && reference
                .as_ref()
                .and_then(|reference| reference.project.as_ref())
                .is_none_or(|target| target == &project)
        {
            remotes.push((name, project));
        }
    }
    ensure!(
        !remotes.is_empty(),
        "No matching GitLab target-project remote. Add the MR's target repository as a remote, or select it with --remote and --hostname"
    );
    let index = if remotes.len() == 1 {
        0
    } else {
        loop {
            let labels: Vec<_> = remotes
                .iter()
                .map(|(name, project)| format!("{name} · {}/{}", project.host, project.path))
                .collect();
            match gitlab_ui::pick("GitLab repository", &labels)? {
                Pick::Selected(index) => break index,
                Pick::Reload => continue,
                Pick::Cancel => return Ok(None),
            }
        }
    };
    let (remote, project) = &remotes[index];
    let gitlab = GitLab::new(repo, project);
    let iid = if let Some(reference) = reference {
        reference.iid
    } else {
        loop {
            eprintln!(
                "Loading open merge requests from {}/{}…",
                project.host, project.path
            );
            let requests = gitlab.open_merge_requests()?;
            let labels: Vec<_> = requests.iter().map(|mr| mr.label()).collect();
            match gitlab_ui::pick("Open merge requests", &labels)? {
                Pick::Selected(index) => break requests[index].iid,
                Pick::Reload => continue,
                Pick::Cancel => return Ok(None),
            }
        }
    };
    eprintln!("Fetching and preparing MR !{iid}…");
    let prepared = gitlab.prepare(repo, remote, iid)?;
    let mut review = Review::new(prepared.comparison, prepared.changes);
    review.gitlab = Some(prepared.gitlab);
    Ok(Some(review))
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    repo: &Repository,
    app: &mut App,
    workers: (&RenderWorker, &EditorWorker, &mut GitLabWorker),
    session: (&SessionStore, &mut SavedSession, &Arc<SessionLock>),
    config: &Config,
) -> Result<()> {
    let (worker, editor, gitlab_worker) = workers;
    let (store, saved, lock) = session;
    let mut diff = Text::raw("Select a changed file");
    let mut requested = None;
    let mut request_id = 0;
    let mut redraw = true;
    let mut last_save = Instant::now();
    let refresher = app
        .review
        .gitlab
        .is_none()
        .then(|| RefreshWorker::new(repo, &app.review.comparison));
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
        let diff_page = panes
            .diff
            .height
            .max(panes.tree.height)
            .saturating_sub(2 + u16::from(app.revisions.is_some()))
            .max(1) as usize;
        let page = if app.focus == Focus::Tree {
            panes.tree.height.saturating_sub(2).max(1) as usize
        } else {
            diff_page
        };
        if !app.review.view.active.is_empty() {
            let key = (
                app.review.view.active.clone(),
                width,
                app.review.view.inline,
                app.revision_range(),
            );
            if requested.as_ref() != Some(&key) {
                request_id = worker.request_revisions(
                    repo,
                    &app.review.comparison,
                    &app.review.view.active,
                    app.preview_changes().cloned().collect(),
                    RenderOptions {
                        width,
                        inline: app.review.view.inline,
                    },
                    key.3.clone(),
                );
                requested = Some(key);
                app.rendering = true;
                app.hunks.clear();
                app.source_lines.clear();
                app.revision_preview = None;
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
                app.revision_preview = result.revision_preview;
                diff = match result.text {
                    Ok(text) => text,
                    Err(error) => Text::raw(format!("Diff error\n\n{}", ui::display_path(&error))),
                };
                app.position().vertical = app
                    .position()
                    .vertical
                    .min(diff.lines.len().saturating_sub(diff_page));
                redraw = true;
            }
        }
        redraw |= app.apply_hunk_navigation(diff_page, diff.lines.len());
        if app.thread.is_some() {
            app.keep_thread_visible(diff_page);
        } else {
            app.keep_line_selection_visible(diff_page, diff.lines.len());
        }
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
                Event::Key(_) | Event::Paste(_) if app.network_busy => {
                    app.message = "Waiting for the current GitLab operation to finish…".into();
                    redraw = true;
                }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    app.message.clear();
                    let keys = if app.searching
                        || app.flash.is_some()
                        || app.session_dialog.is_some()
                        || app.comment_dialog.is_some()
                        || app.submission_dialog.is_some()
                        || app.thread.is_some()
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
                            && app.submission_dialog.is_none()
                            && app.thread.is_none()
                            && !app.searching
                            && !app.help
                            && !key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('n' | 'N'));
                        let search_enter = app.focus == Focus::Diff
                            && app.searching
                            && app.session_dialog.is_none()
                            && app.comment_dialog.is_none()
                            && app.submission_dialog.is_none()
                            && app.thread.is_none()
                            && key.code == KeyCode::Enter;
                        let input_page = if app.session_dialog.is_some()
                            || app.comment_dialog.is_some()
                            || app.submission_dialog.is_some()
                        {
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
                    } else if let Some(CommentDialog::Reply(draft)) = &mut app.comment_dialog {
                        draft.insert(&text);
                        redraw = true;
                    } else if let Some(dialog) = &mut app.submission_dialog {
                        dialog.insert(&text);
                        redraw = true;
                    }
                }
                _ => {}
            }
        }
        if std::mem::take(&mut app.copy_path_requested) {
            if app.review.view.active.is_empty() {
                app.message = "Select a file or directory to copy".into();
            } else {
                let path = repo.root.join(app.review.view.active.trim_end_matches('/'));
                let path = path.to_string_lossy();
                app.message = match crossterm::execute!(
                    io::stdout(),
                    CopyToClipboard::to_clipboard_from(path.as_bytes())
                ) {
                    Ok(()) => format!("Copied {}", ui::display_path(&path)),
                    Err(error) => format!("Clipboard: {error}"),
                };
            }
            redraw = true;
        }
        if let Some(action) = app.comment_action.take() {
            match action {
                CommentAction::Compose(target) => {
                    match CommentDraft::from_preview(
                        repo,
                        app.diff_comparison(),
                        app.diff_changes(),
                        &target,
                    ) {
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
        let mut gitlab_job = app.submission_action.take();
        if app.refresh {
            app.refresh = false;
            if app.review.gitlab.is_some() {
                gitlab_job = Some(GitLabJob::Refresh);
            } else if let Some(refresher) = &refresher {
                refresher.request();
            }
        }
        if let Some(job) = gitlab_job {
            let started = (|| -> Result<()> {
                saved.review = app.review.clone();
                lock.save(saved)?;
                gitlab_worker.start(repo, saved.clone(), Arc::clone(lock), job)
            })();
            match started {
                Ok(()) => {
                    app.network_busy = true;
                    app.message = "Contacting GitLab…".into();
                }
                Err(error) => {
                    app.message = format!("GitLab: {error:#}");
                    if let Some(dialog) = &mut app.submission_dialog {
                        dialog.message = app.message.clone();
                    }
                }
            }
            redraw = true;
        }
        if app.editor_requested
            && !app.rendering
            && requested.as_ref().is_some_and(|key| {
                key.0 == app.review.view.active
                    && key.2 == app.review.view.inline
                    && key.3 == app.revision_range()
            })
        {
            app.editor_requested = false;
            let result = app
                .editor_location()
                .context("Select a file or mapped hunk to open in the editor")
                .and_then(|source| {
                    let change = app
                        .diff_changes()
                        .iter()
                        .find(|change| change.path == source.path)
                        .context("Selected file is no longer in the comparison")?;
                    editor.request(repo, app.diff_comparison(), change, source)
                });
            app.message = match result {
                Ok(()) => "Opening editor…".into(),
                Err(error) => format!("Editor: {error:#}"),
            };
            redraw = true;
        }
        if let Some(reply) = gitlab_worker.take_finished() {
            app.network_busy = false;
            let previous_preview: Vec<_> = app.preview_changes().cloned().collect();
            *saved = reply.saved;
            app.review = saved.review.clone();
            app.rebuild();
            let preview_changed = !previous_preview.iter().eq(app.preview_changes());
            if preview_changed {
                app.hunk_selection = None;
                app.line_selection = None;
                app.editor_requested = false;
                request_id = worker.cancel();
                requested = None;
            }
            match reply.result {
                Ok(GitLabOutcome::DiscussionsLoaded) => {
                    app.message = "GitLab comments reloaded · C browse comments".into();
                    if let Some(CommentDialog::Browse(list)) = &mut app.comment_dialog {
                        list.selection = 0;
                        list.scroll = 0;
                    }
                }
                Ok(GitLabOutcome::Refreshed(invalidated)) => {
                    app.clear_revision_filter();
                    app.message =
                        format!("MR snapshots refreshed · {invalidated} viewed marks reset");
                    request_id = worker.cancel();
                    requested = None;
                }
                Ok(GitLabOutcome::Preview(preview)) => {
                    if let Some(dialog) = &mut app.submission_dialog {
                        dialog.preview = Some(preview);
                        dialog.message.clear();
                    }
                }
                Ok(GitLabOutcome::Published(message)) => {
                    app.submission_dialog = None;
                    app.message = message;
                }
                Err(error) => {
                    app.message = format!("GitLab: {error}");
                    if let Some(dialog) = &mut app.submission_dialog {
                        dialog.preview = None;
                        dialog.message = app.message.clone();
                    }
                }
            }
            app.dirty = true;
            force_save = true;
            redraw = true;
        }
        let refreshed: Vec<_> = refresher
            .as_ref()
            .map(|worker| worker.results.try_iter().collect())
            .unwrap_or_default();
        for result in refreshed {
            match result.snapshot {
                Ok((comparison, changes)) => {
                    app.clear_revision_filter();
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
            && !app.network_busy
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
