//! Application state and input handling.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::diff::{DiffLine, LineKind, Rows};
use crate::git::{FileChange, Mode};
use crate::highlight::Highlights;
use crate::model::RepoGroup;
use crate::worker::{DiffResult, Refreshed};

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

/// Side effects the main loop must perform after handling input.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    LoadDiff,
    FocusPane(String),
    OpenEditor { path: PathBuf, line: Option<u32> },
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
    agent_cycle: usize,
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
            agent_cycle: 0,
        }
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
        if r.mode != self.mode {
            return false; // stale result from before a mode switch
        }
        let prev_root = self.repo().map(|g| g.root.clone());
        let prev_file = self.file().cloned();

        self.groups = r.groups;
        self.non_repo_panes = r.non_repo_panes;
        self.herdr_error = r.herdr_error;
        self.last_refresh = Some(Instant::now());
        self.loading = false;

        self.repo_idx = prev_root
            .and_then(|root| self.groups.iter().position(|g| g.root == root))
            .unwrap_or(self.repo_idx)
            .min(self.groups.len().saturating_sub(1));
        self.file_idx = prev_file
            .as_ref()
            .and_then(|f| self.files().iter().position(|x| x.path == f.path))
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
        let (scroll, hscroll) = match keep_scroll {
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
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_diff(self.page() as isize),
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
        Refreshed {
            mode: Mode::Uncommitted,
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
