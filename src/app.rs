//! Application state and input handling.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::diff::{self, DiffLine, LineKind, Rows};
use crate::git::{FileChange, GitOp, Mode};
use crate::highlight::Highlights;
use crate::model::RepoGroup;
use crate::scope::{Scope, Target};
use crate::worker::{DiffResult, GitDone, Refreshed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Repos,
    Files,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Split when the diff panel is wide enough.
    Auto,
    Unified,
    Split,
}

/// Diff panel width (columns) at which `View::Auto` switches to side-by-side.
pub const AUTO_SPLIT_WIDTH: usize = 140;

pub struct DiffView {
    pub root: PathBuf,
    pub path: String,
    pub mode: Mode,
    pub lines: Result<Vec<DiffLine>, String>,
    pub highlights: Highlights,
    pub unified: Rows,
    pub split: Rows,
    /// Index of the line anchored at the top of the viewport. Stored as a line (not a row)
    /// so the position survives switching between unified and split layouts.
    pub scroll: usize,
    pub hscroll: usize,
}

impl DiffView {
    pub fn rows(&self, split: bool) -> &Rows {
        if split { &self.split } else { &self.unified }
    }

    pub fn top_row(&self, split: bool) -> usize {
        self.rows(split)
            .line_row
            .get(self.scroll)
            .copied()
            .unwrap_or(0)
    }

    fn scroll_rows(&mut self, split: bool, delta: isize) {
        let rows = self.rows(split);
        if rows.rows.is_empty() {
            return;
        }
        let max = rows.rows.len() as isize - 1;
        let target = (self.top_row(split) as isize)
            .saturating_add(delta)
            .clamp(0, max) as usize;
        self.scroll = rows.rows[target].anchor();
    }
}

/// Selection remembered per scope target, restored when you come back to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Saved {
    root: PathBuf,
    path: Option<String>,
    scroll: usize,
    hscroll: usize,
}

/// Panel rectangles and list offsets from the last frame, for mouse hit-testing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HitMap {
    pub repos: Rect,
    pub files: Rect,
    pub diff: Rect,
    pub repos_offset: usize,
    pub files_offset: usize,
}

/// Rows moved per mouse wheel notch in the diff.
const WHEEL_ROWS: isize = 3;
/// Columns moved per horizontal wheel notch.
const WHEEL_COLS: isize = 8;

/// Side effects the main loop must perform after handling input.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    LoadDiff,
    FocusPane(String),
    OpenEditor {
        path: PathBuf,
        line: Option<u32>,
    },
    /// Run a git write in the worker.
    Git {
        root: PathBuf,
        op: GitOp,
    },
    /// Suspend the TUI and run an interactive `git commit` (editor, signing prompts).
    ExternalCommit {
        root: PathBuf,
    },
}

/// The inline commit message editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBox {
    pub root: PathBuf,
    pub message: String,
    /// Waiting for `git commit` (hooks can take a while).
    pub busy: bool,
}

/// A modal message, for errors and hook output that don't fit the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

pub struct App {
    pub mode: Mode,
    pub groups: Vec<RepoGroup>,
    pub non_repo_panes: usize,
    pub herdr_error: Option<String>,
    pub repo_idx: usize,
    pub file_idx: usize,
    pub focus: Focus,
    pub diff: Option<DiffView>,
    pub last_refresh: Option<Instant>,
    pub loading: bool,
    pub show_help: bool,
    pub message: Option<(String, Instant)>,
    /// Height of the diff viewport, set by the renderer (for paging).
    pub diff_height: usize,
    /// Inner width of the diff panel, set by the renderer (for `View::Auto`).
    pub diff_width: usize,
    pub view: View,
    pub scope: Scope,
    /// Resolved scope of the data on screen; `None` until the first refresh.
    pub target: Option<Target>,
    /// Whether `Scope::Here` makes sense (herdiff runs inside a herdr pane).
    pub here_available: bool,
    agent_cycle: usize,
    saved: HashMap<String, Saved>,
    /// Scroll to restore once the diff of a remembered file arrives.
    pending_scroll: Option<Saved>,
    /// Set by the renderer every frame.
    pub hit: HitMap,
    /// Started with `--read-only`: no staging or committing.
    pub read_only: bool,
    pub commit: Option<CommitBox>,
    pub notice: Option<Notice>,
}

impl App {
    pub fn new(mode: Mode, view: View) -> Self {
        Self {
            mode,
            groups: Vec::new(),
            non_repo_panes: 0,
            herdr_error: None,
            repo_idx: 0,
            file_idx: 0,
            focus: Focus::Repos,
            diff: None,
            last_refresh: None,
            loading: true,
            show_help: false,
            message: None,
            diff_height: 20,
            diff_width: 80,
            view,
            scope: Scope::All,
            target: None,
            here_available: false,
            agent_cycle: 0,
            saved: HashMap::new(),
            pending_scroll: None,
            hit: HitMap::default(),
            read_only: false,
            commit: None,
            notice: None,
        }
    }

    pub fn with_scope(mut self, scope: Scope, here_available: bool) -> Self {
        self.scope = scope;
        self.here_available = here_available;
        self
    }

    pub fn repo(&self) -> Option<&RepoGroup> {
        self.groups.get(self.repo_idx)
    }

    pub fn files(&self) -> &[FileChange] {
        self.repo()
            .and_then(|g| g.stats.as_ref().ok())
            .map(|s| s.files.as_slice())
            .unwrap_or(&[])
    }

    pub fn file(&self) -> Option<&FileChange> {
        self.files().get(self.file_idx)
    }

    pub fn split_active(&self) -> bool {
        match self.view {
            View::Split => true,
            View::Unified => false,
            View::Auto => self.diff_width >= AUTO_SPLIT_WIDTH,
        }
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    /// Apply refreshed data, preserving selection by repo root and file path.
    /// Returns true when the selected file's diff should be (re)loaded.
    pub fn apply_refresh(&mut self, r: Refreshed) -> bool {
        if r.mode != self.mode || r.target.scope != self.scope {
            return false; // stale result from before a mode or scope switch
        }
        let mut prev_root = self.repo().map(|g| g.root.clone());
        let mut prev_path = self.file().map(|f| f.path.clone());

        let new_key = r.target.key();
        let old_key = self.target.as_ref().map(Target::key);
        if old_key.as_ref() != Some(&new_key) {
            // Scope moved to another workspace: park this selection, restore that one's.
            if let (Some(old), Some(root)) = (old_key, prev_root.clone()) {
                // The loaded diff if it's the selected file; otherwise a restore that was
                // still waiting for its diff when focus moved on again.
                let (scroll, hscroll) = self
                    .diff
                    .as_ref()
                    .filter(|v| v.root == root && Some(&v.path) == prev_path.as_ref())
                    .map(|v| (v.scroll, v.hscroll))
                    .or_else(|| {
                        self.pending_scroll
                            .as_ref()
                            .filter(|p| p.root == root && p.path == prev_path)
                            .map(|p| (p.scroll, p.hscroll))
                    })
                    .unwrap_or_default();
                let saved = Saved {
                    root,
                    path: prev_path.clone(),
                    scroll,
                    hscroll,
                };
                self.saved.insert(old, saved);
            }
            let restore = self.saved.get(&new_key).cloned();
            prev_root = restore.as_ref().map(|s| s.root.clone());
            prev_path = restore.as_ref().and_then(|s| s.path.clone());
            self.pending_scroll = restore;
            self.diff = None;
            self.repo_idx = 0;
            self.file_idx = 0;
            self.agent_cycle = 0;
        }
        self.target = Some(r.target);

        self.groups = r.groups;
        self.non_repo_panes = r.non_repo_panes;
        self.herdr_error = r.herdr_error;
        self.last_refresh = Some(Instant::now());
        self.loading = false;

        self.repo_idx = prev_root
            .and_then(|root| self.groups.iter().position(|g| g.root == root))
            .unwrap_or(self.repo_idx)
            .min(self.groups.len().saturating_sub(1));
        self.file_idx = prev_path
            .as_ref()
            .and_then(|p| self.files().iter().position(|x| &x.path == p))
            .unwrap_or(self.file_idx)
            .min(self.files().len().saturating_sub(1));

        // Always reload the selected file's diff: its content may have changed
        // even when +/- counts did not.
        if self.file().is_none() {
            self.diff = None;
            return false;
        }
        true
    }

    pub fn apply_diff(&mut self, d: DiffResult) {
        let Some(g) = self.repo() else { return };
        let Some(f) = self.file() else { return };
        if g.root != d.root || f.path != d.path || d.mode != self.mode {
            return; // selection moved on
        }
        let (unified, split) = match &d.lines {
            Ok(l) => (Rows::unified(l), Rows::split(l)),
            Err(_) => Default::default(),
        };
        let keep_scroll = self
            .diff
            .as_ref()
            .filter(|v| v.root == d.root && v.path == d.path && v.mode == d.mode)
            .map(|v| (v.scroll, v.hscroll));
        let line_count = d.lines.as_ref().map(|l| l.len()).unwrap_or(0);
        let restored = self
            .pending_scroll
            .take()
            .filter(|s| s.root == d.root && s.path.as_deref() == Some(d.path.as_str()))
            .map(|s| (s.scroll, s.hscroll));
        let (scroll, hscroll) = match keep_scroll.or(restored) {
            Some((s, h)) => (s.min(line_count.saturating_sub(1)), h),
            None => (first_change(&d.lines), 0),
        };
        self.diff = Some(DiffView {
            root: d.root,
            path: d.path,
            mode: d.mode,
            lines: d.lines,
            highlights: d.highlights,
            unified,
            split,
            scroll,
            hscroll,
        });
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if self.notice.take().is_some() {
            return Action::None;
        }
        if self.commit.is_some() {
            return self.commit_key(key);
        }
        if self.show_help {
            self.show_help = false;
            return Action::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => Action::Quit,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc => {
                if self.focus == Focus::Repos {
                    Action::Quit
                } else {
                    self.focus_prev();
                    Action::None
                }
            }
            KeyCode::Char('?') => {
                self.show_help = true;
                Action::None
            }
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                self.focus_next();
                Action::None
            }
            KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Left => {
                self.focus_prev();
                Action::None
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('d') if ctrl => self.scroll_diff(self.page() as isize / 2),
            KeyCode::Char('u') if ctrl => self.scroll_diff(-(self.page() as isize / 2)),
            KeyCode::Char('J') => self.scroll_diff(1),
            KeyCode::Char('K') => self.scroll_diff(-1),
            KeyCode::PageDown | KeyCode::Char('f') => self.scroll_diff(self.page() as isize),
            KeyCode::Char(' ') => self.toggle_stage(),
            KeyCode::Char('A') => self.repo_write(|_| GitOp::StageAll),
            KeyCode::Char('R') => self.repo_write(|_| GitOp::UnstageAll),
            KeyCode::Char('c') => self.open_commit(),
            KeyCode::Char('C') => match self.writable_repo() {
                Some(root) => Action::ExternalCommit { root },
                None => Action::None,
            },
            KeyCode::PageUp | KeyCode::Char('b') => self.scroll_diff(-(self.page() as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.scroll_diff(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.scroll_diff(isize::MAX / 2),
            KeyCode::Char('H') => self.hscroll(-8),
            KeyCode::Char('L') => self.hscroll(8),
            KeyCode::Char('n') => self.jump_hunk(true),
            KeyCode::Char('N') | KeyCode::Char('p') => self.jump_hunk(false),
            KeyCode::Char(']') => self.select_file(self.file_idx as isize + 1),
            KeyCode::Char('[') => self.select_file(self.file_idx as isize - 1),
            KeyCode::Char('m') => {
                self.mode = self.mode.next();
                self.diff = None;
                self.loading = true;
                self.flash(format!("mode: {}", self.mode.label()));
                Action::Refresh
            }
            KeyCode::Char('w') => {
                self.scope = self.scope.next(self.here_available);
                self.loading = true;
                self.flash(format!("scope: {}", self.scope.label()));
                Action::Refresh
            }
            KeyCode::Char('r') => {
                self.loading = true;
                Action::Refresh
            }
            KeyCode::Char('a') => self.next_agent_pane(),
            KeyCode::Char('s') => {
                self.view = if self.split_active() {
                    View::Unified
                } else {
                    View::Split
                };
                self.flash(if self.split_active() {
                    "side-by-side view"
                } else {
                    "unified view"
                });
                Action::None
            }
            KeyCode::Char('e') => self.open_editor(),
            _ => Action::None,
        }
    }

    /// Bracketed paste: only the commit message takes text.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(c) = self.commit.as_mut().filter(|c| !c.busy) {
            c.message
                .push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
        }
    }

    /// A git write finished. The caller refreshes either way.
    pub fn apply_git_done(&mut self, done: GitDone) {
        let is_commit = matches!(done.op, GitOp::Commit { .. });
        match done.result {
            Ok(summary) => {
                if is_commit {
                    self.commit = None;
                }
                self.flash(summary);
            }
            Err(err) => {
                if let Some(c) = self.commit.as_mut().filter(|_| is_commit) {
                    c.busy = false; // keep the message so a failed hook doesn't lose it
                }
                self.notice = Some(Notice {
                    title: format!("{} failed", op_name(&done.op)),
                    body: err,
                });
            }
        }
    }

    /// Agents currently working in the selected repo, for the commit warning.
    pub fn working_agents(&self) -> Vec<String> {
        self.repo()
            .map(|g| {
                g.panes
                    .iter()
                    .filter(|p| p.status.as_deref() == Some("working"))
                    .filter_map(|p| p.agent.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn staged_count(&self) -> usize {
        self.repo()
            .and_then(|g| g.stats.as_ref().ok())
            .map_or(0, |s| s.staged)
    }

    fn writable_repo(&mut self) -> Option<PathBuf> {
        if self.read_only {
            self.flash("read-only: herdiff was started with --read-only");
            return None;
        }
        let root = self.repo().map(|g| g.root.clone());
        if root.is_none() {
            self.flash("no repo selected");
        }
        root
    }

    fn repo_write(&mut self, op: impl FnOnce(&Self) -> GitOp) -> Action {
        match self.writable_repo() {
            Some(root) => Action::Git { root, op: op(self) },
            None => Action::None,
        }
    }

    /// `space`: the selected file in the file list, the hunk at the top of the diff.
    fn toggle_stage(&mut self) -> Action {
        match self.focus {
            Focus::Diff => self.toggle_stage_hunk(),
            Focus::Files => self.toggle_stage_file(),
            Focus::Repos => Action::None,
        }
    }

    fn toggle_stage_file(&mut self) -> Action {
        let Some(f) = self.file().cloned() else {
            return Action::None;
        };
        let paths = vec![f.path.clone()];
        // The mode says what the user is looking at: unstage in staged mode, stage in
        // unstaged mode, otherwise stage whatever is left and unstage when nothing is.
        let op = match self.mode {
            Mode::Staged if f.has_staged() => GitOp::Unstage(paths),
            Mode::Unstaged if f.has_unstaged() => GitOp::Stage(paths),
            _ if f.has_unstaged() => GitOp::Stage(paths),
            _ if f.has_staged() => GitOp::Unstage(paths),
            _ => {
                self.flash(format!("{} has no local changes to stage", f.path));
                return Action::None;
            }
        };
        self.repo_write(|_| op)
    }

    fn toggle_stage_hunk(&mut self) -> Action {
        let Some(f) = self.file().cloned() else {
            return Action::None;
        };
        if f.status == crate::git::Status::Untracked {
            // An untracked file is one hunk anyway.
            return self.repo_write(|_| GitOp::Stage(vec![f.path]));
        }
        let reverse = match self.mode {
            Mode::Unstaged => false,
            Mode::Staged => true,
            _ => {
                self.flash("hunks can be staged in unstaged mode and unstaged in staged mode (m)");
                return Action::None;
            }
        };
        let root = self.repo().map(|g| g.root.clone());
        let Some(view) = self
            .diff
            .as_ref()
            .filter(|v| Some(&v.root) == root.as_ref() && v.path == f.path && v.mode == self.mode)
        else {
            // A refresh can move the selection before the new diff arrives.
            self.flash("diff is still loading");
            return Action::None;
        };
        let target = view.lines.as_ref().ok().and_then(|lines| {
            let start = diff::hunk_start(lines, view.scroll)?;
            Some((diff::hunk_index(lines, start), lines[start].text.clone()))
        });
        match target {
            Some((hunk, header)) => self.repo_write(|_| GitOp::ApplyHunk {
                path: f.path,
                hunk,
                header,
                reverse,
            }),
            None => {
                self.flash("no hunk here");
                Action::None
            }
        }
    }

    fn open_commit(&mut self) -> Action {
        let Some(root) = self.writable_repo() else {
            return Action::None;
        };
        if self.staged_count() == 0 {
            self.flash("nothing staged: press space on a file or hunk, or A for everything");
            return Action::None;
        }
        self.commit = Some(CommitBox {
            root,
            message: String::new(),
            busy: false,
        });
        Action::None
    }

    fn commit_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(c) = self.commit.as_mut() else {
            return Action::None;
        };
        if c.busy {
            return Action::None; // git is running; its result closes or reopens the box
        }
        match key.code {
            KeyCode::Esc => self.commit = None,
            KeyCode::Char('s') if ctrl => {
                if c.message.trim().is_empty() {
                    self.flash("write a commit message first");
                    return Action::None;
                }
                c.busy = true;
                return Action::Git {
                    root: c.root.clone(),
                    op: GitOp::Commit {
                        message: c.message.clone(),
                    },
                };
            }
            KeyCode::Char('u') if ctrl => c.message.clear(),
            KeyCode::Enter => c.message.push('\n'),
            KeyCode::Tab => c.message.push_str("    "),
            KeyCode::Backspace => {
                c.message.pop();
            }
            KeyCode::Char(ch) if !ctrl => c.message.push(ch),
            _ => {}
        }
        Action::None
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> Action {
        if self.commit.is_some() {
            return Action::None;
        }
        if self.notice.is_some() {
            if matches!(ev.kind, MouseEventKind::Down(_)) {
                self.notice = None;
            }
            return Action::None;
        }
        if self.show_help {
            if matches!(ev.kind, MouseEventKind::Down(_)) {
                self.show_help = false;
            }
            return Action::None;
        }
        let pos = Position::new(ev.column, ev.row);
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        let panel = if self.hit.repos.contains(pos) {
            Focus::Repos
        } else if self.hit.files.contains(pos) {
            Focus::Files
        } else if self.hit.diff.contains(pos) {
            Focus::Diff
        } else {
            return Action::None;
        };
        match ev.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let dir = if ev.kind == MouseEventKind::ScrollDown {
                    1
                } else {
                    -1
                };
                match panel {
                    Focus::Repos => self.select_repo(self.repo_idx as isize + dir),
                    Focus::Files => self.select_file(self.file_idx as isize + dir),
                    Focus::Diff if shift => self.hscroll(dir * WHEEL_COLS),
                    Focus::Diff => self.scroll_diff(dir * WHEEL_ROWS),
                }
            }
            MouseEventKind::ScrollRight if panel == Focus::Diff => self.hscroll(WHEEL_COLS),
            MouseEventKind::ScrollLeft if panel == Focus::Diff => self.hscroll(-WHEEL_COLS),
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = panel;
                match panel {
                    Focus::Repos => match self.repo_at(ev.row) {
                        Some(i) => self.select_repo(i as isize),
                        None => Action::None,
                    },
                    Focus::Files => match self.file_at(ev.row) {
                        Some(i) => self.select_file(i as isize),
                        None => Action::None,
                    },
                    Focus::Diff => Action::None,
                }
            }
            _ => Action::None,
        }
    }

    /// Repo under screen row `y`. Each repo takes one row plus one per pane.
    fn repo_at(&self, y: u16) -> Option<usize> {
        let inner = y.checked_sub(self.hit.repos.y + 1)? as usize;
        if y + 1 >= self.hit.repos.bottom() {
            return None; // bottom border
        }
        let mut top = 0;
        for (i, g) in self.groups.iter().enumerate().skip(self.hit.repos_offset) {
            let height = 1 + g.panes.len();
            if inner < top + height {
                return Some(i);
            }
            top += height;
        }
        None
    }

    /// File under screen row `y`.
    fn file_at(&self, y: u16) -> Option<usize> {
        let inner = y.checked_sub(self.hit.files.y + 1)? as usize;
        if y + 1 >= self.hit.files.bottom() {
            return None;
        }
        let i = self.hit.files_offset + inner;
        (i < self.files().len()).then_some(i)
    }

    fn page(&self) -> usize {
        self.diff_height.saturating_sub(2).max(1)
    }

    fn focus_next(&mut self) {
        self.focus = match self.focus {
            Focus::Repos => Focus::Files,
            Focus::Files | Focus::Diff => Focus::Diff,
        };
    }

    fn focus_prev(&mut self) {
        self.focus = match self.focus {
            Focus::Diff => Focus::Files,
            Focus::Files | Focus::Repos => Focus::Repos,
        };
    }

    fn move_sel(&mut self, delta: isize) -> Action {
        match self.focus {
            Focus::Repos => self.select_repo(self.repo_idx as isize + delta),
            Focus::Files => self.select_file(self.file_idx as isize + delta),
            Focus::Diff => self.scroll_diff(delta),
        }
    }

    fn select_repo(&mut self, idx: isize) -> Action {
        let idx = clamp(idx, self.groups.len());
        if idx == self.repo_idx {
            return Action::None;
        }
        self.repo_idx = idx;
        self.file_idx = 0;
        self.agent_cycle = 0;
        self.diff = None;
        if self.file().is_some() {
            Action::LoadDiff
        } else {
            Action::None
        }
    }

    fn select_file(&mut self, idx: isize) -> Action {
        let idx = clamp(idx, self.files().len());
        if idx == self.file_idx && self.diff.is_some() {
            return Action::None;
        }
        self.file_idx = idx;
        if self.file().is_some() {
            Action::LoadDiff
        } else {
            Action::None
        }
    }

    fn scroll_diff(&mut self, delta: isize) -> Action {
        let split = self.split_active();
        if let Some(v) = &mut self.diff {
            v.scroll_rows(split, delta);
        }
        Action::None
    }

    fn hscroll(&mut self, delta: isize) -> Action {
        if let Some(v) = &mut self.diff {
            v.hscroll = (v.hscroll as isize + delta).max(0) as usize;
        }
        Action::None
    }

    fn jump_hunk(&mut self, forward: bool) -> Action {
        let Some(v) = &mut self.diff else {
            return Action::None;
        };
        let Ok(lines) = &v.lines else {
            return Action::None;
        };
        let is_hunk = |i: &usize| lines[*i].kind == LineKind::Hunk;
        let found = if forward {
            (v.scroll + 1..lines.len()).find(is_hunk)
        } else {
            (0..v.scroll).rev().find(is_hunk)
        };
        if let Some(i) = found {
            v.scroll = i;
        }
        Action::None
    }

    fn next_agent_pane(&mut self) -> Action {
        let Some(g) = self.repo() else {
            return Action::None;
        };
        // Prefer agent panes; fall back to any pane in the repo.
        let mut panes: Vec<_> = g.panes.iter().filter(|p| p.is_agent()).collect();
        if panes.is_empty() {
            panes = g.panes.iter().collect();
        }
        if panes.is_empty() {
            self.flash("no herdr pane in this repo");
            return Action::None;
        }
        let pane = panes[self.agent_cycle % panes.len()].pane_id.clone();
        self.agent_cycle += 1;
        Action::FocusPane(pane)
    }

    fn open_editor(&mut self) -> Action {
        let (Some(g), Some(f)) = (self.repo(), self.file()) else {
            return Action::None;
        };
        let path = g.root.join(&f.path);
        // Line of the first change at or below the current scroll position.
        let line = self.diff.as_ref().and_then(|v| {
            let lines = v.lines.as_ref().ok()?;
            lines[v.scroll.min(lines.len().saturating_sub(1))..]
                .iter()
                .find_map(|l| l.new_no.filter(|_| l.kind != LineKind::Meta))
        });
        Action::OpenEditor { path, line }
    }
}

fn op_name(op: &GitOp) -> &'static str {
    match op {
        GitOp::Stage(_) | GitOp::StageAll => "git add",
        GitOp::Unstage(_) | GitOp::UnstageAll => "unstage",
        GitOp::ApplyHunk { .. } => "git apply",
        GitOp::Commit { .. } => "git commit",
    }
}

fn clamp(idx: isize, len: usize) -> usize {
    idx.clamp(0, len.saturating_sub(1) as isize) as usize
}

fn first_change(lines: &Result<Vec<DiffLine>, String>) -> usize {
    lines
        .as_ref()
        .ok()
        .and_then(|l| l.iter().position(|x| x.kind == LineKind::Hunk))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{RepoStats, Status};
    use crate::model::PaneRef;

    fn file(path: &str) -> FileChange {
        FileChange {
            path: path.into(),
            status: Status::Modified,
            added: Some(1),
            removed: Some(0),
            index: ' ',
            worktree: 'M',
        }
    }

    fn group(root: &str, files: &[&str]) -> RepoGroup {
        RepoGroup {
            root: root.into(),
            name: root.into(),
            panes: vec![PaneRef {
                pane_id: format!("{root}:p1"),
                workspace: "w".into(),
                tab: "t".into(),
                agent: Some("claude".into()),
                status: Some("idle".into()),
                title: String::new(),
                focused: false,
            }],
            stats: Ok(RepoStats {
                files: files.iter().map(|f| file(f)).collect(),
                ..Default::default()
            }),
        }
    }

    fn refreshed(groups: Vec<RepoGroup>) -> Refreshed {
        refreshed_in(None, groups)
    }

    fn refreshed_in(ws: Option<&str>, groups: Vec<RepoGroup>) -> Refreshed {
        let scope = if ws.is_some() {
            Scope::Follow
        } else {
            Scope::All
        };
        Refreshed {
            mode: Mode::Uncommitted,
            target: Target {
                scope,
                workspace_id: ws.map(String::from),
                label: ws.map(String::from),
            },
            groups,
            non_repo_panes: 0,
            herdr_error: None,
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn selection_survives_refresh_reordering() {
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.apply_refresh(refreshed(vec![
            group("/a", &["x"]),
            group("/b", &["p", "q", "r"]),
        ]));
        assert_eq!(app.on_key(key('j')), Action::LoadDiff);
        app.focus = Focus::Files;
        app.on_key(key('j'));
        app.on_key(key('j'));
        assert_eq!(app.file().unwrap().path, "r");

        // Repo order flips and a file is inserted before the selected one.
        app.apply_refresh(refreshed(vec![
            group("/b", &["a", "p", "q", "r"]),
            group("/a", &["x"]),
        ]));
        assert_eq!(app.repo().unwrap().root, PathBuf::from("/b"));
        assert_eq!(app.file().unwrap().path, "r");
    }

    #[test]
    fn scroll_position_survives_view_toggle() {
        use crate::diff::parse_unified;
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.apply_refresh(refreshed(vec![group("/a", &["x"])]));
        let text = "@@ -1,4 +1,4 @@\n a\n-b\n-c\n+B\n+C\n d\n e\n";
        app.apply_diff(DiffResult {
            root: "/a".into(),
            mode: Mode::Uncommitted,
            path: "x".into(),
            lines: Ok(parse_unified(text)),
            highlights: Vec::new(),
        });
        app.focus = Focus::Diff;
        app.on_key(key('j'));
        app.on_key(key('j'));
        app.on_key(key('j'));
        app.on_key(key('j')); // unified row 4 = "+B"
        assert_eq!(app.diff.as_ref().unwrap().scroll, 4);

        app.on_key(key('s'));
        assert!(app.split_active());
        let v = app.diff.as_ref().unwrap();
        // "+B" shares split row 2 with "-b"
        assert_eq!(v.top_row(true), 2);
        app.on_key(key('j'));
        assert_eq!(app.diff.as_ref().unwrap().scroll, 3); // row 3 = "-c" | "+C"
        app.on_key(key('j'));
        assert_eq!(app.diff.as_ref().unwrap().scroll, 6); // " d"
        app.on_key(key('s'));
        assert_eq!(app.diff.as_ref().unwrap().top_row(false), 6);
    }

    fn diff_for(root: &str, path: &str) -> DiffResult {
        let text: String = (1..=50).map(|i| format!("+line {i}\n")).collect();
        DiffResult {
            root: root.into(),
            mode: Mode::Uncommitted,
            path: path.into(),
            lines: Ok(crate::diff::parse_unified(&format!(
                "@@ -0,0 +1,50 @@\n{text}"
            ))),
            highlights: Vec::new(),
        }
    }

    #[test]
    fn switching_workspaces_restores_selection_and_scroll() {
        let mut app = App::new(Mode::Uncommitted, View::Unified).with_scope(Scope::Follow, true);
        let ws1 = || vec![group("/one", &["a", "b", "c"])];
        let ws2 = || vec![group("/two", &["x", "y"])];

        assert!(app.apply_refresh(refreshed_in(Some("w1"), ws1())));
        app.focus = Focus::Files;
        assert_eq!(app.on_key(key('j')), Action::LoadDiff);
        app.on_key(key('j')); // "c"
        app.apply_diff(diff_for("/one", "c"));
        app.focus = Focus::Diff;
        for _ in 0..10 {
            app.on_key(key('j'));
        }
        assert_eq!(app.diff.as_ref().unwrap().scroll, 10);

        // Focus moves to workspace 2: fresh selection there.
        assert!(app.apply_refresh(refreshed_in(Some("w2"), ws2())));
        assert_eq!(app.file().unwrap().path, "x");
        assert!(app.diff.is_none());

        // And back: file and scroll come back once the diff loads.
        assert!(app.apply_refresh(refreshed_in(Some("w1"), ws1())));
        assert_eq!(app.file().unwrap().path, "c");
        app.apply_diff(diff_for("/one", "c"));
        assert_eq!(app.diff.as_ref().unwrap().scroll, 10);
    }

    #[test]
    fn quick_back_and_forth_keeps_pending_scroll() {
        let mut app = App::new(Mode::Uncommitted, View::Unified).with_scope(Scope::Follow, true);
        let ws1 = || vec![group("/one", &["a"])];
        let ws2 = || vec![group("/two", &["x"])];
        app.apply_refresh(refreshed_in(Some("w1"), ws1()));
        app.apply_diff(diff_for("/one", "a"));
        app.focus = Focus::Diff;
        for _ in 0..5 {
            app.on_key(key('j'));
        }
        app.apply_refresh(refreshed_in(Some("w2"), ws2()));
        // Back to w1 and away again before w1's diff arrives.
        app.apply_refresh(refreshed_in(Some("w1"), ws1()));
        app.apply_refresh(refreshed_in(Some("w2"), ws2()));
        app.apply_refresh(refreshed_in(Some("w1"), ws1()));
        app.apply_diff(diff_for("/one", "a"));
        assert_eq!(app.diff.as_ref().unwrap().scroll, 5);
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Repos panel rows 0..10, files 10..20, diff to the right.
    fn app_with_hitmap() -> App {
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.apply_refresh(refreshed(vec![
            group("/a", &["a1"]),
            group("/b", &["b1", "b2", "b3"]),
        ]));
        app.hit = HitMap {
            repos: Rect::new(0, 0, 40, 10),
            files: Rect::new(0, 10, 40, 10),
            diff: Rect::new(40, 0, 80, 20),
            repos_offset: 0,
            files_offset: 0,
        };
        app
    }

    #[test]
    fn click_selects_repo_by_multi_row_item() {
        let mut app = app_with_hitmap();
        // Row 1 = repo /a, row 2 = its pane, row 3 = repo /b, row 4 = its pane.
        let down = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(app.on_mouse(mouse(down, 5, 4)), Action::LoadDiff);
        assert_eq!(app.repo().unwrap().root, PathBuf::from("/b"));
        assert_eq!(app.on_mouse(mouse(down, 5, 2)), Action::LoadDiff);
        assert_eq!(app.repo().unwrap().root, PathBuf::from("/a"));
        // Empty space and the bottom border select nothing.
        assert_eq!(app.on_mouse(mouse(down, 5, 7)), Action::None);
        assert_eq!(app.on_mouse(mouse(down, 5, 9)), Action::None);
        assert_eq!(app.repo().unwrap().root, PathBuf::from("/a"));
    }

    #[test]
    fn click_file_and_wheel() {
        let mut app = app_with_hitmap();
        app.select_repo(1);
        let down = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(app.on_mouse(mouse(down, 5, 13)), Action::LoadDiff);
        assert_eq!(app.file().unwrap().path, "b3");
        assert_eq!(app.focus, Focus::Files);
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::ScrollUp, 5, 12)),
            Action::LoadDiff
        );
        assert_eq!(app.file().unwrap().path, "b2");

        app.apply_diff(diff_for("/b", "b2"));
        app.on_mouse(mouse(MouseEventKind::ScrollDown, 60, 5));
        assert_eq!(app.diff.as_ref().unwrap().scroll, 3);
        let mut shifted = mouse(MouseEventKind::ScrollDown, 60, 5);
        shifted.modifiers = KeyModifiers::SHIFT;
        app.on_mouse(shifted);
        assert_eq!(app.diff.as_ref().unwrap().hscroll, 8);
        // Clicking the diff focuses it.
        app.on_mouse(mouse(down, 60, 5));
        assert_eq!(app.focus, Focus::Diff);
    }

    #[test]
    fn click_closes_help() {
        let mut app = app_with_hitmap();
        app.show_help = true;
        let down = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(app.on_mouse(mouse(down, 5, 4)), Action::None);
        assert!(!app.show_help);
        assert_eq!(app.repo().unwrap().root, PathBuf::from("/a"));
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn with_file_state(app: &mut App, index: char, worktree: char, staged: usize) {
        if let Ok(stats) = &mut app.groups[app.repo_idx].stats {
            for f in &mut stats.files {
                f.index = index;
                f.worktree = worktree;
            }
            stats.staged = staged;
        }
    }

    #[test]
    fn space_stages_or_unstages_the_selected_file() {
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.apply_refresh(refreshed(vec![group("/a", &["x"])]));
        app.focus = Focus::Files;

        with_file_state(&mut app, ' ', 'M', 0);
        assert_eq!(
            app.on_key(key(' ')),
            Action::Git {
                root: "/a".into(),
                op: GitOp::Stage(vec!["x".into()])
            }
        );
        with_file_state(&mut app, 'M', ' ', 1);
        assert_eq!(
            app.on_key(key(' ')),
            Action::Git {
                root: "/a".into(),
                op: GitOp::Unstage(vec!["x".into()])
            }
        );
        // Partly staged: staged mode unstages, other modes stage the rest.
        with_file_state(&mut app, 'M', 'M', 1);
        assert!(matches!(
            app.on_key(key(' ')),
            Action::Git {
                op: GitOp::Stage(_),
                ..
            }
        ));
        app.mode = Mode::Staged;
        assert!(matches!(
            app.on_key(key(' ')),
            Action::Git {
                op: GitOp::Unstage(_),
                ..
            }
        ));

        assert_eq!(
            app.on_key(key('A')),
            Action::Git {
                root: "/a".into(),
                op: GitOp::StageAll
            }
        );
        assert_eq!(
            app.on_key(key('R')),
            Action::Git {
                root: "/a".into(),
                op: GitOp::UnstageAll
            }
        );
    }

    #[test]
    fn space_in_diff_stages_the_hunk_only_where_it_applies() {
        let mut app = App::new(Mode::Unstaged, View::Unified);
        app.apply_refresh(Refreshed {
            mode: Mode::Unstaged,
            ..refreshed(vec![group("/a", &["x"])])
        });
        app.apply_diff(DiffResult {
            root: "/a".into(),
            mode: Mode::Unstaged,
            path: "x".into(),
            lines: Ok(crate::diff::parse_unified(
                "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
            )),
            highlights: Vec::new(),
        });
        app.focus = Focus::Diff;
        assert_eq!(
            app.on_key(key(' ')),
            Action::Git {
                root: "/a".into(),
                op: GitOp::ApplyHunk {
                    path: "x".into(),
                    hunk: 0,
                    header: "@@ -1 +1 @@".into(),
                    reverse: false,
                }
            }
        );
        // The loaded diff belongs to another file: refuse rather than stage the wrong hunk.
        app.diff.as_mut().unwrap().path = "other".into();
        assert_eq!(app.on_key(key(' ')), Action::None);
        assert_eq!(app.message.as_ref().unwrap().0, "diff is still loading");
        app.diff.as_mut().unwrap().path = "x".into();
        app.mode = Mode::Uncommitted;
        assert_eq!(app.on_key(key(' ')), Action::None);
        assert!(app.message.as_ref().unwrap().0.contains("unstaged mode"));
    }

    #[test]
    fn commit_box_flow() {
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.apply_refresh(refreshed(vec![group("/a", &["x"])]));

        // Nothing staged: no box.
        assert_eq!(app.on_key(key('c')), Action::None);
        assert!(app.commit.is_none());

        with_file_state(&mut app, 'M', ' ', 1);
        app.on_key(key('c'));
        assert!(app.commit.is_some());
        // Keys type into the message instead of running commands.
        for ch in "Fix q".chars() {
            assert_eq!(app.on_key(key(ch)), Action::None);
        }
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.on_paste("body\r\nmore");
        assert_eq!(app.commit.as_ref().unwrap().message, "Fix \nbody\nmore");

        let action = app.on_key(ctrl('s'));
        assert_eq!(
            action,
            Action::Git {
                root: "/a".into(),
                op: GitOp::Commit {
                    message: "Fix \nbody\nmore".into()
                }
            }
        );
        // While git runs, input is ignored.
        assert_eq!(app.on_key(key('x')), Action::None);
        assert!(app.commit.as_ref().unwrap().busy);

        // A failing hook keeps the message and shows the output.
        app.apply_git_done(GitDone {
            root: "/a".into(),
            op: GitOp::Commit {
                message: String::new(),
            },
            result: Err("pre-commit hook failed".into()),
        });
        let c = app.commit.as_ref().unwrap();
        assert!(!c.busy && c.message.starts_with("Fix"));
        assert_eq!(app.notice.as_ref().unwrap().title, "git commit failed");
        app.on_key(key('z')); // closes the notice, doesn't type
        assert!(app.notice.is_none());
        assert_eq!(app.commit.as_ref().unwrap().message, "Fix \nbody\nmore");

        app.on_key(ctrl('s'));
        app.apply_git_done(GitDone {
            root: "/a".into(),
            op: GitOp::Commit {
                message: String::new(),
            },
            result: Ok("[main abc123] Fix".into()),
        });
        assert!(app.commit.is_none());
        assert_eq!(app.message.as_ref().unwrap().0, "[main abc123] Fix");

        app.on_key(key('c'));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.commit.is_none());
    }

    #[test]
    fn read_only_blocks_writes() {
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.read_only = true;
        app.apply_refresh(refreshed(vec![group("/a", &["x"])]));
        with_file_state(&mut app, ' ', 'M', 1);
        app.focus = Focus::Files;
        for k in [' ', 'A', 'R', 'c', 'C'] {
            assert_eq!(app.on_key(key(k)), Action::None, "{k}");
        }
        assert!(app.commit.is_none());
        assert!(app.message.as_ref().unwrap().0.contains("--read-only"));
    }

    #[test]
    fn same_workspace_refresh_keeps_selection() {
        let mut app = App::new(Mode::Uncommitted, View::Unified).with_scope(Scope::Follow, true);
        app.apply_refresh(refreshed_in(Some("w1"), vec![group("/one", &["a", "b"])]));
        app.focus = Focus::Files;
        app.on_key(key('j'));
        app.apply_refresh(refreshed_in(
            Some("w1"),
            vec![group("/one", &["0", "a", "b"])],
        ));
        assert_eq!(app.file().unwrap().path, "b");
    }

    #[test]
    fn scope_key_cycles_and_drops_stale_results() {
        let mut app = App::new(Mode::Uncommitted, View::Unified).with_scope(Scope::All, false);
        assert_eq!(app.on_key(key('w')), Action::Refresh);
        assert_eq!(app.scope, Scope::Follow);
        // Result computed for the old scope arrives late.
        assert!(!app.apply_refresh(refreshed_in(None, vec![group("/a", &["x"])])));
        assert!(app.groups.is_empty());
        // Here isn't offered outside herdr.
        app.on_key(key('w'));
        assert_eq!(app.scope, Scope::All);
    }

    #[test]
    fn auto_view_follows_width() {
        let mut app = App::new(Mode::Uncommitted, View::Auto);
        app.diff_width = AUTO_SPLIT_WIDTH - 1;
        assert!(!app.split_active());
        app.diff_width = AUTO_SPLIT_WIDTH;
        assert!(app.split_active());
    }

    #[test]
    fn stale_mode_results_are_ignored() {
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.on_key(key('m'));
        assert!(!app.apply_refresh(refreshed(vec![group("/a", &["x"])])));
        assert!(app.groups.is_empty());
    }

    #[test]
    fn agent_jump_cycles_panes() {
        let mut app = App::new(Mode::Uncommitted, View::Unified);
        app.apply_refresh(refreshed(vec![group("/a", &["x"])]));
        assert_eq!(app.on_key(key('a')), Action::FocusPane("/a:p1".into()));
    }
}
