mod app;
mod diff;
mod git;
mod herdr;
mod highlight;
mod model;
mod ui;
mod worker;

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::event::{self, Event, KeyEventKind};
use serde_json::json;

use app::{Action, App, View};
use git::Mode;
use herdr::Client;
use worker::{AppEvent, Job};

/// Live diff viewer for the git repos your herdr agents are working in.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// herdr socket path (default: $HERDR_SOCKET_PATH or ~/.config/herdr/herdr.sock)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    /// Diff mode to start in
    #[arg(long, short, value_enum, default_value_t = ModeArg::Uncommitted, global = true)]
    mode: ModeArg,

    /// Extra directories to include even without a herdr pane
    #[arg(long = "dir", short = 'd', global = true)]
    dirs: Vec<PathBuf>,

    /// Diff layout: auto = side-by-side when the diff panel is at least 140 columns wide
    #[arg(long, value_enum, default_value_t = ViewArg::Auto)]
    view: ViewArg,

    /// Syntax highlighting theme (see --list-themes)
    #[arg(long, default_value = highlight::DEFAULT_THEME)]
    theme: String,

    /// Disable syntax highlighting
    #[arg(long)]
    no_highlight: bool,

    /// Print available themes and exit
    #[arg(long)]
    list_themes: bool,

    /// Poll interval in seconds (herdr events trigger refreshes in between)
    #[arg(long, default_value_t = 2.0)]
    interval: f64,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print repos, panes, and changed files once, then exit
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Uncommitted,
    Unstaged,
    Staged,
    Branch,
}

#[derive(Clone, Copy, ValueEnum)]
enum ViewArg {
    Auto,
    Unified,
    Split,
}

impl From<ViewArg> for View {
    fn from(v: ViewArg) -> View {
        match v {
            ViewArg::Auto => View::Auto,
            ViewArg::Unified => View::Unified,
            ViewArg::Split => View::Split,
        }
    }
}

impl From<ModeArg> for Mode {
    fn from(m: ModeArg) -> Mode {
        match m {
            ModeArg::Uncommitted => Mode::Uncommitted,
            ModeArg::Unstaged => Mode::Unstaged,
            ModeArg::Staged => Mode::Staged,
            ModeArg::Branch => Mode::Branch,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(cli.socket.clone());
    let mode: Mode = cli.mode.into();
    if cli.list_themes {
        for name in highlight::theme_names() {
            println!("{name}");
        }
        return Ok(());
    }
    match cli.command {
        Some(Cmd::List { json }) => list(client, mode, cli.dirs, json),
        None => {
            let highlighter = if cli.no_highlight {
                None
            } else {
                Some(highlight::Highlighter::new(&cli.theme)?)
            };
            let opts = TuiOpts {
                mode,
                view: cli.view.into(),
                dirs: cli.dirs,
                interval: Duration::from_secs_f64(cli.interval.max(0.2)),
                highlighter,
            };
            run_tui(client, opts)
        }
    }
}

fn list(client: Client, mode: Mode, dirs: Vec<PathBuf>, as_json: bool) -> Result<()> {
    let (job_tx, job_rx) = mpsc::channel();
    let (tx, rx) = mpsc::channel();
    worker::spawn_git_worker(client, dirs, None, job_rx, tx);
    job_tx.send(Job::Refresh { mode })?;
    let Ok(AppEvent::Refreshed(r)) = rx.recv() else {
        anyhow::bail!("worker failed")
    };

    if as_json {
        let repos: Vec<_> = r
            .groups
            .iter()
            .map(|g| {
                let stats = g.stats.as_ref().map(|s| {
                    json!({
                        "branch": s.branch,
                        "base": s.base,
                        "files": s.files.iter().map(|f| json!({
                            "path": f.path,
                            "status": f.status.letter().to_string(),
                            "added": f.added,
                            "removed": f.removed,
                        })).collect::<Vec<_>>(),
                    })
                });
                json!({
                    "root": g.root,
                    "panes": g.panes.iter().map(|p| json!({
                        "pane_id": p.pane_id,
                        "workspace": p.workspace,
                        "tab": p.tab,
                        "agent": p.agent,
                        "status": p.status,
                        "title": p.title,
                    })).collect::<Vec<_>>(),
                    "stats": stats.as_ref().ok(),
                    "error": stats.as_ref().err(),
                })
            })
            .collect();
        let out = json!({ "mode": mode.label(), "herdr_error": r.herdr_error, "repos": repos });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if let Some(e) = &r.herdr_error {
        eprintln!("herdr: {e}");
    }
    for g in &r.groups {
        match &g.stats {
            Ok(s) => {
                let (a, d) = s.totals();
                println!(
                    "{}  [{}]  {} files  +{a} -{d}  (vs {})",
                    g.root.display(),
                    s.branch,
                    s.files.len(),
                    s.base
                );
                for p in &g.panes {
                    println!(
                        "  pane {} {}/{} {} {}",
                        p.pane_id,
                        p.workspace,
                        p.tab,
                        p.agent.as_deref().unwrap_or("shell"),
                        p.status.as_deref().unwrap_or("")
                    );
                }
                for f in &s.files {
                    let cnt = match (f.added, f.removed) {
                        _ if f.is_dir() => "nested repo".into(),
                        (Some(a), Some(r)) => format!("+{a} -{r}"),
                        _ => "binary".into(),
                    };
                    println!("    {} {:<60} {cnt}", f.status.letter(), f.path);
                }
            }
            Err(e) => println!("{}  error: {e}", g.root.display()),
        }
    }
    Ok(())
}

struct TuiOpts {
    mode: Mode,
    view: View,
    dirs: Vec<PathBuf>,
    interval: Duration,
    highlighter: Option<highlight::Highlighter>,
}

fn run_tui(client: Client, opts: TuiOpts) -> Result<()> {
    let TuiOpts {
        mode,
        view,
        dirs,
        interval,
        highlighter,
    } = opts;
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (tx, rx) = mpsc::channel::<AppEvent>();
    worker::spawn_git_worker(client.clone(), dirs, highlighter, job_rx, tx.clone());
    worker::spawn_herdr_listener(client.clone(), tx.clone());
    worker::spawn_ticker(interval, tx);

    let mut terminal = ratatui::init();
    let mut app = App::new(mode, view);
    job_tx.send(Job::Refresh { mode })?;

    // herdr can emit bursts of events; coalesce them.
    const DEBOUNCE: Duration = Duration::from_millis(300);
    let mut refresh_due: Option<Instant> = None;
    let mut last_draw_secs = u64::MAX;

    let result = (|| -> Result<()> {
        let mut dirty = true;
        loop {
            if dirty {
                terminal.draw(|f| ui::draw(f, &mut app))?;
                dirty = false;
            }

            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => {
                        dirty = true;
                        match app.on_key(k) {
                            Action::None => {}
                            Action::Quit => return Ok(()),
                            Action::Refresh => job_tx.send(Job::Refresh { mode: app.mode })?,
                            Action::LoadDiff => request_diff(&app, &job_tx)?,
                            Action::FocusPane(id) => match client.focus_pane(&id) {
                                Ok(()) => app.flash(format!("focused {id}")),
                                Err(e) => app.flash(format!("focus failed: {e:#}")),
                            },
                            Action::OpenEditor { path, line } => {
                                ratatui::restore();
                                let res = open_editor(&path, line);
                                terminal = ratatui::init();
                                terminal.clear()?;
                                if let Err(e) = res {
                                    app.flash(format!("editor: {e:#}"));
                                }
                                job_tx.send(Job::Refresh { mode: app.mode })?;
                            }
                        }
                    }
                    Event::Resize(..) => dirty = true,
                    _ => {}
                }
            }

            for ev in rx.try_iter() {
                match ev {
                    AppEvent::Refreshed(r) => {
                        if app.apply_refresh(r) {
                            request_diff(&app, &job_tx)?;
                        }
                        dirty = true;
                    }
                    AppEvent::Diff(d) => {
                        app.apply_diff(d);
                        dirty = true;
                    }
                    AppEvent::HerdrChanged => {
                        refresh_due.get_or_insert(Instant::now() + DEBOUNCE);
                    }
                    AppEvent::Tick => {
                        refresh_due.get_or_insert(Instant::now());
                    }
                }
            }
            if refresh_due.is_some_and(|t| Instant::now() >= t) {
                refresh_due = None;
                job_tx.send(Job::Refresh { mode: app.mode })?;
            }
            // Keep the "updated Ns ago" counter moving.
            let secs = app.last_refresh.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            if secs != last_draw_secs {
                last_draw_secs = secs;
                dirty = true;
            }
        }
    })();

    ratatui::restore();
    result
}

fn request_diff(app: &App, jobs: &mpsc::Sender<Job>) -> Result<()> {
    if let (Some(g), Some(f)) = (app.repo(), app.file()) {
        jobs.send(Job::Diff {
            root: g.root.clone(),
            mode: app.mode,
            file: f.clone(),
        })?;
    }
    Ok(())
}

fn open_editor(path: &std::path::Path, line: Option<u32>) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("vi");
    let mut cmd = Command::new(bin);
    cmd.args(parts);
    let name = std::path::Path::new(bin)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if let Some(l) = line {
        match name {
            "code" | "cursor" | "zed" | "subl" => {
                if name == "code" || name == "cursor" {
                    cmd.arg("--goto");
                }
                cmd.arg(format!("{}:{l}", path.display()));
            }
            _ => {
                cmd.arg(format!("+{l}")).arg(path);
            }
        }
    } else {
        cmd.arg(path);
    }
    let status = cmd.status()?;
    anyhow::ensure!(status.success(), "{editor} exited with {status}");
    Ok(())
}
