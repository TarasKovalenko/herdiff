//! Rendering with ratatui.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, Focus};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::diff::{DiffLine, LineKind, Row};
use crate::git::Mode;
use crate::highlight::{Fg, Highlights, HlSpan};

const ADD_FG: Color = Color::Green;
const DEL_FG: Color = Color::Red;
const ADD_BG: Color = Color::Rgb(18, 46, 28);
const DEL_BG: Color = Color::Rgb(58, 22, 26);
const FILLER_BG: Color = Color::Rgb(28, 28, 32);
const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    let wide = main.width >= 120;
    let (left, diff_area) = if wide {
        let [l, d] = Layout::horizontal([Constraint::Length(46), Constraint::Min(20)]).areas(main);
        (l, d)
    } else {
        let [l, d] = Layout::vertical([Constraint::Percentage(40), Constraint::Min(5)]).areas(main);
        (l, d)
    };
    let [repos_area, files_area] = if wide {
        // Size the repo list to its content (a single scoped repo needs a few rows),
        // capped so the file list keeps most of the column.
        let content: usize = app.groups.iter().map(|g| 1 + g.panes.len()).sum();
        let cap = (left.height as usize * 45 / 100).max(4);
        // No repos: the panel holds a wrapped message (often a herdr error), give it room.
        let height = if app.groups.is_empty() {
            cap
        } else {
            (content + 2).clamp(4, cap)
        } as u16;
        Layout::vertical([Constraint::Length(height), Constraint::Min(4)]).areas(left)
    } else {
        Layout::horizontal([Constraint::Percentage(45), Constraint::Min(10)]).areas(left)
    };

    app.hit.repos = repos_area;
    app.hit.files = files_area;
    app.hit.diff = diff_area;
    app.hit.repos_offset = draw_repos(f, app, repos_area);
    app.hit.files_offset = draw_files(f, app, files_area);
    draw_diff(f, app, diff_area);
    draw_status(f, app, status);
    if app.show_help {
        draw_help(f);
    }
    if app.commit.is_some() {
        draw_commit(f, app);
    }
    if let Some(notice) = &app.notice {
        draw_notice(f, notice);
    }
}

fn block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(DIM)
    };
    Block::bordered()
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(style)
        .title(title)
}

fn status_style(status: &str) -> (char, Style) {
    match status {
        "working" => ('●', Style::new().fg(Color::Yellow)),
        "blocked" => ('▲', Style::new().fg(Color::Magenta).bold()),
        "done" => ('✓', Style::new().fg(Color::Green)),
        "idle" => ('○', Style::new().fg(Color::Blue)),
        _ => ('·', Style::new().fg(DIM)),
    }
}

fn counts(added: Option<u32>, removed: Option<u32>) -> Vec<Span<'static>> {
    match (added, removed) {
        (Some(a), Some(r)) => vec![
            Span::styled(format!("+{a}"), Style::new().fg(ADD_FG)),
            Span::raw(" "),
            Span::styled(format!("-{r}"), Style::new().fg(DEL_FG)),
        ],
        _ => vec![Span::styled("bin", Style::new().fg(DIM))],
    }
}

/// Returns the list scroll offset (first visible repo) for mouse hit-testing.
fn draw_repos(f: &mut Frame, app: &App, area: Rect) -> usize {
    let focused = app.focus == Focus::Repos;
    let title = format!(" Repos ({}) ", app.groups.len());
    let width = area.width.saturating_sub(4) as usize;

    let items: Vec<ListItem> = app
        .groups
        .iter()
        .map(|g| {
            let (icon, icon_style) = status_style(g.status().unwrap_or(""));
            let mut head = vec![
                Span::styled(format!("{icon} "), icon_style),
                Span::styled(g.name.clone(), Style::new().bold()),
            ];
            match &g.stats {
                Ok(s) => {
                    let (a, r) = s.totals();
                    let tail: Vec<Span> = if s.files.is_empty() {
                        vec![Span::styled("clean", Style::new().fg(DIM))]
                    } else {
                        let mut t = vec![Span::styled(
                            format!("{}f ", s.files.len()),
                            Style::new().fg(DIM),
                        )];
                        t.extend(counts(Some(a), Some(r)));
                        t
                    };
                    // The counts matter more than the branch: shorten the branch to fit.
                    let used =
                        2 + g.name.width() + tail.iter().map(|s| s.content.width()).sum::<usize>();
                    let room = width.saturating_sub(used + 2);
                    let branch = if room >= 4 {
                        format!(" {} ", truncate(&s.branch, room))
                    } else {
                        " ".into()
                    };
                    head.push(Span::styled(branch, Style::new().fg(Color::Magenta)));
                    head.extend(tail);
                }
                Err(_) => head.push(Span::styled(" git error", Style::new().fg(DEL_FG))),
            }
            let mut lines = vec![Line::from(head)];
            for p in &g.panes {
                let (icon, style) = status_style(p.status.as_deref().unwrap_or(""));
                let who = p.agent.clone().unwrap_or_else(|| "shell".into());
                let mut detail = format!("{}/{}", p.workspace, p.tab);
                if !p.title.is_empty() {
                    detail.push_str(" · ");
                    detail.push_str(&p.title);
                }
                let used = 4 + who.chars().count();
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{icon} "), style),
                    Span::styled(
                        who,
                        if p.is_agent() {
                            style
                        } else {
                            Style::new().fg(DIM)
                        },
                    ),
                    Span::raw(" "),
                    Span::styled(
                        truncate(&detail, width.saturating_sub(used)),
                        Style::new().fg(DIM),
                    ),
                ]));
            }
            ListItem::new(Text::from(lines))
        })
        .collect();

    if items.is_empty() {
        let scoped = app.target.as_ref().and_then(|t| t.label.clone());
        let msg = match (&app.herdr_error, app.loading, scoped) {
            (Some(e), _, _) => e.clone(),
            (None, true, _) => "loading…".into(),
            (None, false, Some(ws)) => format!("no git repos in workspace {ws} · w to widen"),
            (None, false, None) => "no herdr panes inside git repos".into(),
        };
        f.render_widget(
            Paragraph::new(msg)
                .fg(DIM)
                .wrap(Wrap { trim: true })
                .block(block(title, focused)),
            area,
        );
        return 0;
    }
    let list = List::new(items)
        .block(block(title, focused))
        .highlight_style(highlight(focused))
        .highlight_symbol("▌");
    let mut state = ListState::default().with_selected(Some(app.repo_idx));
    f.render_stateful_widget(list, area, &mut state);
    state.offset()
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(Color::Rgb(40, 44, 60))
    } else {
        Style::new().bg(Color::Rgb(30, 30, 36))
    }
}

/// Returns the list scroll offset (first visible file) for mouse hit-testing.
fn draw_files(f: &mut Frame, app: &App, area: Rect) -> usize {
    let focused = app.focus == Focus::Files;
    let Some(g) = app.repo() else {
        f.render_widget(block(" Files ", focused), area);
        return 0;
    };
    let stats = match &g.stats {
        Ok(s) => s,
        Err(e) => {
            f.render_widget(
                Paragraph::new(e.clone())
                    .fg(DEL_FG)
                    .wrap(Wrap { trim: true })
                    .block(block(" Files ", focused)),
                area,
            );
            return 0;
        }
    };
    let staged = if stats.staged > 0 {
        format!("· {} staged ", stats.staged)
    } else {
        String::new()
    };
    let title = format!(" Files ({}) vs {} {staged}", stats.files.len(), stats.base);
    if stats.files.is_empty() {
        f.render_widget(
            Paragraph::new(format!("no {} changes", app.mode.label()))
                .fg(DIM)
                .block(block(title, focused)),
            area,
        );
        return 0;
    }
    let width = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = stats
        .files
        .iter()
        .map(|file| {
            // Two columns like `git status -s`: staged (green), then unstaged (red).
            let marks = match (file.index, file.worktree) {
                ('?', _) => vec![Span::styled("??", Style::new().fg(ADD_FG).bold())],
                // Only in branch mode: committed on the branch, nothing local.
                (' ', ' ') => vec![Span::styled(
                    format!("{} ", file.status.letter()),
                    Style::new().fg(DIM),
                )],
                (x, y) => vec![
                    Span::styled(x.to_string(), Style::new().fg(ADD_FG).bold()),
                    Span::styled(y.to_string(), Style::new().fg(DEL_FG).bold()),
                ],
            };
            let cnt = if file.is_dir() {
                vec![Span::styled("repo", Style::new().fg(DIM))]
            } else {
                counts(file.added, file.removed)
            };
            let cnt_len: usize = cnt.iter().map(|s| s.content.chars().count()).sum();
            let path_w = width.saturating_sub(cnt_len + 4);
            let mut spans = marks;
            spans.push(Span::raw(format!(
                " {:<path_w$} ",
                truncate_left(&file.path, path_w)
            )));
            spans.extend(cnt);
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block(title, focused))
        .highlight_style(highlight(focused))
        .highlight_symbol("▌");
    let mut state = ListState::default().with_selected(Some(app.file_idx));
    f.render_stateful_widget(list, area, &mut state);
    state.offset()
}

fn draw_diff(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Diff;
    app.diff_height = area.height.saturating_sub(2) as usize;
    app.diff_width = area.width.saturating_sub(2) as usize;
    let split = app.split_active();
    let Some(view) = &app.diff else {
        let msg = if app.file().is_some() {
            "loading…"
        } else {
            ""
        };
        f.render_widget(
            Paragraph::new(msg).fg(DIM).block(block(" Diff ", focused)),
            area,
        );
        return;
    };
    let lines = match &view.lines {
        Ok(l) => l,
        Err(e) => {
            f.render_widget(
                Paragraph::new(e.clone())
                    .fg(DEL_FG)
                    .wrap(Wrap { trim: true })
                    .block(block(" Diff ", focused)),
                area,
            );
            return;
        }
    };
    let rows = view.rows(split);
    let top = view.top_row(split);
    let pos = if rows.rows.is_empty() { 0 } else { top + 1 };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(view.path.clone(), Style::new().bold()),
        Span::styled(format!(" {pos}/{} ", rows.rows.len()), Style::new().fg(DIM)),
        Span::styled(if split { "split " } else { "" }, Style::new().fg(DIM)),
    ]);
    // Mark the hunk `space` would act on, when it would act on one.
    let hunk_action = match app.mode {
        _ if app.read_only || !focused => None,
        Mode::Unstaged => Some(" space: stage hunk "),
        Mode::Staged => Some(" space: unstage hunk "),
        _ => None,
    };
    let active_hunk = crate::diff::hunk_start(lines, view.scroll).zip(hunk_action);
    let ctx = CodeCtx {
        lines,
        highlights: &view.highlights,
        active_hunk,
        hscroll: view.hscroll,
        num_w: lines
            .iter()
            .filter_map(|l| l.old_no.max(l.new_no))
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(3),
    };
    let inner_w = app.diff_width;
    let half = inner_w.saturating_sub(1) / 2;
    let right_w = inner_w.saturating_sub(1 + half);

    let rendered: Vec<Line> = rows
        .rows
        .iter()
        .skip(top)
        .take(app.diff_height)
        .map(|row| match *row {
            Row::Full(i) => match lines[i].kind {
                LineKind::Meta | LineKind::Hunk | LineKind::NoNewline => ctx.header(i, inner_w),
                _ => Line::from(ctx.unified(i, inner_w)),
            },
            Row::Pair(old, new) => {
                let mut spans = ctx.side(old, Side::Old, half);
                spans.push(Span::styled("│", Style::new().fg(DIM)));
                spans.extend(ctx.side(new, Side::New, right_w));
                Line::from(spans)
            }
        })
        .collect();

    f.render_widget(Paragraph::new(rendered).block(block(title, focused)), area);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

/// Shared state for rendering code lines of one diff.
struct CodeCtx<'a> {
    lines: &'a [DiffLine],
    highlights: &'a Highlights,
    /// Hunk header index and the label to show on it.
    active_hunk: Option<(usize, &'static str)>,
    hscroll: usize,
    num_w: usize,
}

impl CodeCtx<'_> {
    fn header(&self, i: usize, width: usize) -> Line<'static> {
        let l = &self.lines[i];
        let style = match l.kind {
            LineKind::Hunk => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            _ => Style::new().fg(DIM),
        };
        match self.active_hunk {
            Some((active, label)) if active == i => {
                let badge = Span::styled(label, Style::new().fg(Color::Black).bg(ACCENT).bold());
                let rest = clip(
                    &l.text,
                    self.hscroll,
                    width.saturating_sub(label.width() + 1),
                    false,
                );
                Line::from(vec![badge, Span::raw(" "), Span::styled(rest, style)])
            }
            _ => Line::from(Span::styled(
                clip(&l.text, self.hscroll, width, false),
                style,
            )),
        }
    }

    fn num(&self, n: Option<u32>) -> String {
        n.map(|n| format!("{n:>w$}", w = self.num_w))
            .unwrap_or_else(|| " ".repeat(self.num_w))
    }

    /// Unified row: `old new ±code`
    fn unified(&self, i: usize, width: usize) -> Vec<Span<'static>> {
        let l = &self.lines[i];
        let gutter = format!("{} {} ", self.num(l.old_no), self.num(l.new_no));
        self.code_line(i, gutter, width)
    }

    /// One half of a split row: `num ±code`, or an empty filler.
    fn side(&self, idx: Option<usize>, side: Side, width: usize) -> Vec<Span<'static>> {
        let Some(i) = idx else {
            return vec![Span::styled(" ".repeat(width), Style::new().bg(FILLER_BG))];
        };
        let l = &self.lines[i];
        let no = if side == Side::Old {
            l.old_no
        } else {
            l.new_no
        };
        self.code_line(i, format!("{} ", self.num(no)), width)
    }

    fn code_line(&self, i: usize, gutter: String, width: usize) -> Vec<Span<'static>> {
        let l = &self.lines[i];
        let (sign, sign_fg, bg) = match l.kind {
            LineKind::Added => ("+", ADD_FG, Some(ADD_BG)),
            LineKind::Removed => ("-", DEL_FG, Some(DEL_BG)),
            _ => (" ", Color::Reset, None),
        };
        let gutter_w = gutter.width();
        let code_w = width.saturating_sub(gutter_w + 1);
        let mut spans = vec![
            Span::styled(gutter, Style::new().fg(DIM)),
            Span::styled(
                sign,
                Style::new()
                    .fg(sign_fg)
                    .bold()
                    .bg(bg.unwrap_or(Color::Reset)),
            ),
        ];
        let base = bg.map(|b| Style::new().bg(b)).unwrap_or_default();
        // Always pad: keeps the split divider aligned and fills change backgrounds.
        match self.highlights.get(i).and_then(|h| h.as_ref()) {
            Some(hl) => spans.extend(highlighted(hl, self.hscroll, code_w, base, true)),
            None => spans.push(Span::styled(
                clip(&l.text, self.hscroll, code_w, true),
                base,
            )),
        }
        spans
    }
}

fn fg_color(fg: Fg) -> Color {
    match fg {
        Fg::Rgb(r, g, b) => Color::Rgb(r, g, b),
        Fg::Indexed(i) => Color::Indexed(i),
        Fg::Default => Color::Reset,
    }
}

/// Cut highlighted spans to the visible window `[skip, skip + width)` in display columns.
fn highlighted(
    hl: &[HlSpan],
    skip: usize,
    width: usize,
    base: Style,
    pad: bool,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut col = 0; // display column in the full line
    let mut used = 0; // columns emitted
    for s in hl {
        let mut text = String::new();
        for ch in s.text.chars() {
            let w = ch.width().unwrap_or(0);
            if col >= skip && used + w <= width {
                text.push(ch);
                used += w;
            }
            col += w;
        }
        if !text.is_empty() {
            let mut style = base.fg(fg_color(s.fg));
            if s.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if s.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            out.push(Span::styled(text, style));
        }
        if used >= width {
            break;
        }
    }
    if pad && used < width {
        out.push(Span::styled(" ".repeat(width - used), base));
    }
    out
}

/// Plain text cut to `[skip, skip + width)` display columns, optionally padded to `width`.
fn clip(text: &str, skip: usize, width: usize, pad: bool) -> String {
    let mut out = String::new();
    let mut col = 0;
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if col >= skip && used + w <= width {
            out.push(ch);
            used += w;
        }
        col += w;
        if used >= width {
            break;
        }
    }
    if pad && used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.mode.label()),
            Style::new().fg(Color::Black).bg(ACCENT).bold(),
        ),
        Span::styled(
            format!(" {} ", scope_badge(app)),
            Style::new().fg(Color::Black).bg(Color::Magenta).bold(),
        ),
        Span::raw(" "),
    ];
    if let Some(e) = &app.herdr_error {
        spans.push(Span::styled(
            format!("herdr: {} ", truncate(e, 60)),
            Style::new().fg(DEL_FG),
        ));
    }
    if app.loading {
        spans.push(Span::styled("refreshing… ", Style::new().fg(Color::Yellow)));
    } else if let Some(t) = app.last_refresh {
        spans.push(Span::styled(
            format!("updated {}s ago ", t.elapsed().as_secs()),
            Style::new().fg(DIM),
        ));
    }
    if app.non_repo_panes > 0 {
        spans.push(Span::styled(
            format!("· {} pane(s) outside git ", app.non_repo_panes),
            Style::new().fg(DIM),
        ));
    }
    if let Some((msg, at)) = &app.message
        && at.elapsed().as_secs() < 3
    {
        spans.push(Span::styled(
            format!("· {msg} "),
            Style::new().fg(Color::Yellow),
        ));
    }
    let hint = if app.read_only {
        " read-only  w scope  s split  m mode  a agent  ? help  q quit "
    } else {
        " space stage  c commit  w scope  s split  m mode  ? help  q quit "
    };
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(used + hint.len());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(hint, Style::new().fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_commit(f: &mut Frame, app: &App) {
    let Some(c) = &app.commit else { return };
    let rect = centered(f.area(), 76, 16);
    let repo = app.repo().map_or("", |g| g.name.as_str());
    let branch = app
        .repo()
        .and_then(|g| g.stats.as_ref().ok())
        .map_or(String::new(), |s| format!(" ({})", s.branch));
    let title = format!(" Commit {} staged to {repo}{branch} ", app.staged_count());
    let block = block(title, true);
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);

    let mut rows = Vec::new();
    let working = app.working_agents();
    if !working.is_empty() {
        rows.push(Line::from(Span::styled(
            format!("⚠ {} still working in this repo", working.join(", ")),
            Style::new().fg(Color::Yellow).bold(),
        )));
    }
    let text_h = (inner.height as usize)
        .saturating_sub(rows.len() + 2)
        .max(1);
    let width = inner.width as usize;
    // Soft-wrap the message by display width and keep the end (where typing happens) in view.
    let mut msg_lines: Vec<String> = Vec::new();
    for raw in c.message.split('\n') {
        let mut line = String::new();
        for ch in raw.chars() {
            if line.width() + ch.width().unwrap_or(0) > width.saturating_sub(1) {
                msg_lines.push(std::mem::take(&mut line));
            }
            line.push(ch);
        }
        msg_lines.push(line);
    }
    if !c.busy
        && let Some(last) = msg_lines.last_mut()
    {
        last.push('▏');
    }
    let skip = msg_lines.len().saturating_sub(text_h);
    for (i, l) in msg_lines.iter().enumerate().skip(skip) {
        // Git uses the first line as the subject.
        let style = if i == 0 {
            Style::new().bold()
        } else {
            Style::new()
        };
        rows.push(Line::from(Span::styled(l.clone(), style)));
    }
    if c.message.is_empty() {
        rows.push(Line::from(Span::styled(
            "subject line, blank line, then details",
            Style::new().fg(DIM),
        )));
    }
    f.render_widget(Paragraph::new(rows), inner);

    let footer = if c.busy {
        Span::styled(
            " committing… (hooks run here) ",
            Style::new().fg(Color::Yellow),
        )
    } else {
        Span::styled(
            " ctrl-s commit · enter newline · ctrl-u clear · esc cancel ",
            Style::new().fg(DIM),
        )
    };
    let footer_area = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    f.render_widget(Paragraph::new(Line::from(footer)), footer_area);
}

fn draw_notice(f: &mut Frame, notice: &crate::app::Notice) {
    let lines = notice.body.lines().count() as u16;
    let rect = centered(f.area(), 90, lines + 5);
    let block = Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(Style::new().fg(DEL_FG))
        .title(format!(" {} ", notice.title))
        .title_bottom(Line::from(" any key to close ").right_aligned());
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(notice.body.clone())
            .wrap(Wrap { trim: false })
            .block(block.padding(ratatui::widgets::Padding::horizontal(1))),
        rect,
    );
}

/// `all`, `follow: MedInsight`, `here: h-diff`. Shows the requested scope while its
/// first result is still loading.
fn scope_badge(app: &App) -> String {
    match &app.target {
        Some(t) if t.scope == app.scope => match &t.label {
            Some(label) => format!("{}: {label}", t.scope.label()),
            None => t.scope.label().into(),
        },
        _ => app.scope.label().into(),
    }
}

fn draw_help(f: &mut Frame) {
    const KEYS: &[(&str, &str)] = &[
        ("tab / l / enter", "focus next panel"),
        ("shift-tab / h / esc", "focus previous panel"),
        ("j k / ↓ ↑", "move selection (scroll in diff)"),
        ("[ ]", "previous / next file"),
        ("J K", "scroll diff by line"),
        ("f b / pgdn pgup", "scroll diff by page"),
        ("ctrl-d ctrl-u", "scroll diff half page"),
        ("g G", "diff top / bottom"),
        ("n N", "next / previous hunk"),
        ("H L", "scroll diff horizontally"),
        ("space", "stage / unstage the file, or the marked hunk"),
        ("A R", "stage all / unstage all"),
        ("c", "commit staged changes (ctrl-s commits)"),
        ("C", "run git commit in the terminal (editor, signing)"),
        ("s", "toggle side-by-side / unified view"),
        ("w", "cycle scope: all → follow focus → here"),
        ("m", "cycle mode: uncommitted → unstaged → staged → branch"),
        ("r", "refresh now"),
        ("a", "jump to the repo's agent pane (repeat to cycle)"),
        ("e", "open file in $EDITOR at change"),
        (
            "mouse",
            "wheel scrolls, click selects, shift+wheel sideways",
        ),
        ("q / ctrl-c", "quit"),
    ];
    let area = f.area();
    // Fit the longest line: 1 pad + 21 key column + description + 2 borders + 1 spare.
    let longest = KEYS.iter().map(|(_, d)| d.width()).max().unwrap_or(0);
    let w = ((longest + 25) as u16).min(area.width);
    let h = (KEYS.len() as u16 + 4).min(area.height);
    let rect = Rect::new((area.width - w) / 2, (area.height - h) / 2, w, h);
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<21}"), Style::new().fg(ACCENT)),
                Span::raw(*d),
            ])
        })
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            block(" herdiff keys (any key to close) ", true)
                .padding(ratatui::widgets::Padding::vertical(1)),
        ),
        rect,
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Keep the end of a path (the file name matters most).
fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - max.saturating_sub(1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;
    use crate::git::{FileChange, RepoStats, Status};
    use crate::model::{PaneRef, RepoGroup};
    use crate::scope::{Scope, Target};
    use crate::worker::{DiffResult, Refreshed};
    use ratatui::{Terminal, backend::TestBackend};

    fn app() -> App {
        app_with(
            Target {
                scope: Scope::All,
                workspace_id: None,
                label: None,
            },
            true,
        )
    }

    fn app_with(target: Target, with_repo: bool) -> App {
        let mut app =
            App::new(Mode::Uncommitted, crate::app::View::Auto).with_scope(target.scope, true);
        let file = FileChange {
            path: "src/lib.rs".into(),
            status: Status::Modified,
            added: Some(1),
            removed: Some(1),
            index: ' ',
            worktree: 'M',
        };
        let groups = vec![RepoGroup {
            root: "/r/proj".into(),
            name: "proj".into(),
            panes: vec![PaneRef {
                pane_id: "w1:p1".into(),
                workspace: "proj".into(),
                tab: "1".into(),
                agent: Some("claude".into()),
                status: Some("working".into()),
                title: "fixing bug".into(),
                focused: false,
            }],
            stats: Ok(RepoStats {
                branch: "main".into(),
                base: "HEAD".into(),
                files: vec![file],
                staged: 0,
            }),
        }];
        app.apply_refresh(Refreshed {
            mode: Mode::Uncommitted,
            target,
            groups: if with_repo { groups } else { Vec::new() },
            non_repo_panes: 0,
            herdr_error: None,
        });
        app.apply_diff(DiffResult {
            root: "/r/proj".into(),
            mode: Mode::Uncommitted,
            path: "src/lib.rs".into(),
            lines: Ok(parse_unified(
                "@@ -1,2 +1,2 @@\n ctx\n-old line\n+new line\n",
            )),
            highlights: Vec::new(),
        });
        app
    }

    fn render(w: u16, h: u16) -> String {
        render_app(app(), w, h)
    }

    fn render_app(mut app: App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn status_bar_names_followed_workspace() {
        let target = Target {
            scope: Scope::Follow,
            workspace_id: Some("w1".into()),
            label: Some("MedInsight".into()),
        };
        let screen = render_app(app_with(target.clone(), true), 160, 20);
        assert!(
            screen
                .lines()
                .last()
                .unwrap()
                .contains("follow: MedInsight"),
            "{screen}"
        );

        let empty = render_app(app_with(target, false), 160, 20);
        assert!(
            empty.contains("no git repos in workspace MedInsight"),
            "{empty}"
        );
    }

    #[test]
    fn herdr_error_is_not_cut_off() {
        let mut app = App::new(Mode::Uncommitted, crate::app::View::Auto);
        app.apply_refresh(Refreshed {
            mode: Mode::Uncommitted,
            target: Target {
                scope: Scope::All,
                workspace_id: None,
                label: None,
            },
            groups: Vec::new(),
            non_repo_panes: 0,
            herdr_error: Some(
                "connect to herdr socket /Users/someone/.config/herdr/herdr.sock: \
                 No such file or directory (os error 2) ENDMARK"
                    .into(),
            ),
        });
        let screen = render_app(app, 160, 40);
        assert!(screen.contains("ENDMARK"), "{screen}");
    }

    #[test]
    fn files_show_staged_and_unstaged_columns() {
        let mut app = app();
        if let Ok(stats) = &mut app.groups[0].stats {
            stats.files[0].index = 'M';
            stats.files[0].worktree = 'M';
            stats.staged = 1;
        }
        let screen = render_app(app, 160, 30);
        assert!(screen.contains("MM src/lib.rs"), "{screen}");
        assert!(screen.contains("· 1 staged"), "{screen}");
    }

    #[test]
    fn commit_box_warns_about_working_agents() {
        let mut app = app();
        app.commit = Some(crate::app::CommitBox {
            root: "/r/proj".into(),
            message: "Fix the bug\n\nBecause reasons".into(),
            busy: false,
        });
        let screen = render_app(app, 160, 40);
        assert!(
            screen.contains("⚠ claude still working in this repo"),
            "{screen}"
        );
        assert!(screen.contains("Fix the bug"), "{screen}");
        assert!(screen.contains("Because reasons▏"), "{screen}");
        assert!(screen.contains("ctrl-s commit"), "{screen}");
    }

    #[test]
    fn active_hunk_is_marked_in_unstaged_mode() {
        let mut app = App::new(Mode::Unstaged, crate::app::View::Unified);
        let file = FileChange {
            path: "src/lib.rs".into(),
            status: Status::Modified,
            added: Some(1),
            removed: Some(1),
            index: ' ',
            worktree: 'M',
        };
        app.apply_refresh(Refreshed {
            mode: Mode::Unstaged,
            target: Target {
                scope: Scope::All,
                workspace_id: None,
                label: None,
            },
            groups: vec![RepoGroup {
                root: "/r".into(),
                name: "r".into(),
                panes: Vec::new(),
                stats: Ok(RepoStats {
                    files: vec![file],
                    ..Default::default()
                }),
            }],
            non_repo_panes: 0,
            herdr_error: None,
        });
        app.apply_diff(DiffResult {
            root: "/r".into(),
            mode: Mode::Unstaged,
            path: "src/lib.rs".into(),
            lines: Ok(parse_unified("@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n")),
            highlights: Vec::new(),
        });
        app.focus = crate::app::Focus::Diff;
        let screen = render_app(app, 160, 20);
        assert!(
            screen.contains(" space: stage hunk  @@ -1,2 +1,2 @@"),
            "{screen}"
        );
    }

    #[test]
    fn notice_shows_git_output() {
        let mut app = app();
        app.notice = Some(crate::app::Notice {
            title: "git commit failed".into(),
            body: "pre-commit hook failed:\nclippy found 2 warnings".into(),
        });
        let screen = render_app(app, 160, 30);
        assert!(screen.contains("git commit failed"), "{screen}");
        assert!(screen.contains("clippy found 2 warnings"), "{screen}");
    }

    #[test]
    fn help_lines_are_not_cut_off() {
        let mut app = app();
        app.show_help = true;
        let screen = render_app(app, 160, 40);
        for text in [
            "cycle scope: all → follow focus → here",
            "wheel scrolls, click selects, shift+wheel sideways",
            "cycle mode: uncommitted → unstaged → staged → branch",
        ] {
            assert!(screen.contains(text), "{text}\n{screen}");
        }
    }

    #[test]
    fn long_branch_is_shortened_before_counts() {
        let mut app = app();
        if let Ok(stats) = &mut app.groups[0].stats {
            stats.branch = "feature/a-very-long-branch-name-that-does-not-fit".into();
        }
        let screen = render_app(app, 160, 30);
        let header = screen.lines().find(|l| l.contains("proj ")).expect(&screen);
        assert!(header.contains("+1 -1"), "{header}");
        assert!(header.contains('…'), "{header}");
    }

    #[test]
    fn single_repo_list_is_compact() {
        let screen = render(160, 40);
        // Repo panel: border + repo + 1 pane + border, then the files panel starts.
        let files_row = screen.lines().position(|l| l.contains("Files (")).unwrap();
        assert_eq!(files_row, 4, "{screen}");
    }

    #[test]
    fn split_view_puts_old_and_new_on_one_row() {
        let screen = render(200, 20);
        let row = screen
            .lines()
            .find(|l| l.contains("old line"))
            .expect(&screen);
        assert!(row.contains("new line"), "{screen}");
        // Divider sits in the same column on context and changed rows.
        let ctx = screen.lines().find(|l| l.contains("ctx")).expect(&screen);
        let col = |l: &str| l.chars().position(|c| c == '│');
        let divider = |l: &str| {
            l.char_indices()
                .filter(|(_, c)| *c == '│')
                .map(|(i, _)| l[..i].chars().count())
                .collect::<Vec<_>>()
        };
        assert_eq!(divider(row), divider(ctx), "{screen}");
        assert!(col(row).is_some());
        assert!(screen.contains("split"), "{screen}");
    }

    #[test]
    fn highlighted_spans_are_clipped_by_display_width() {
        let hl = vec![
            HlSpan {
                fg: Fg::Rgb(1, 2, 3),
                bold: false,
                italic: false,
                text: "let ".into(),
            },
            HlSpan {
                fg: Fg::Indexed(4),
                bold: true,
                italic: false,
                text: "名前 = 1".into(),
            },
        ];
        let spans = highlighted(&hl, 2, 6, Style::new(), true);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // skip "le", then "t " (2) + "名前" (4 columns) = 6
        assert_eq!(text, "t 名前");
        assert_eq!(spans[1].style.fg, Some(Color::Indexed(4)));
        assert_eq!(clip("abc", 1, 4, true), "bc  ");
    }

    #[test]
    fn renders_wide_and_narrow() {
        for (w, h) in [(160, 30), (80, 30), (40, 12)] {
            let screen = render(w, h);
            assert!(screen.contains("proj"), "{w}x{h}\n{screen}");
            if w >= 80 {
                assert!(screen.contains("claude"), "{screen}");
                assert!(screen.contains("+new line"), "{screen}");
                assert!(screen.contains("-old line"), "{screen}");
            }
        }
    }
}
