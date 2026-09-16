//! Rendering with ratatui.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, Focus};
use crate::diff::LineKind;
use crate::git::Status;

const ADD_FG: Color = Color::Green;
const DEL_FG: Color = Color::Red;
const ADD_BG: Color = Color::Rgb(18, 46, 28);
const DEL_BG: Color = Color::Rgb(58, 22, 26);
const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    let wide = main.width >= 120;
    let (left, diff_area) = if wide {
        let [l, d] = Layout::horizontal([Constraint::Length(46), Constraint::Min(20)]).areas(main);
        (l, d)
    } else {
        let [l, d] = Layout::vertical([Constraint::Percentage(40), Constraint::Min(5)]).areas(main);
        (l, d)
    };
    let [repos_area, files_area] = if wide {
        Layout::vertical([Constraint::Percentage(45), Constraint::Min(4)]).areas(left)
    } else {
        Layout::horizontal([Constraint::Percentage(45), Constraint::Min(10)]).areas(left)
    };

    draw_repos(f, app, repos_area);
    draw_files(f, app, files_area);
    draw_diff(f, app, diff_area);
    draw_status(f, app, status);
    if app.show_help {
        draw_help(f);
    }
}

fn block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    let style = if focused { Style::new().fg(ACCENT) } else { Style::new().fg(DIM) };
    Block::bordered()
        .border_type(if focused { BorderType::Thick } else { BorderType::Rounded })
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

fn draw_repos(f: &mut Frame, app: &App, area: Rect) {
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
                    head.push(Span::styled(format!(" {} ", s.branch), Style::new().fg(Color::Magenta)));
                    if s.files.is_empty() {
                        head.push(Span::styled("clean", Style::new().fg(DIM)));
                    } else {
                        head.push(Span::styled(format!("{}f ", s.files.len()), Style::new().fg(DIM)));
                        head.extend(counts(Some(a), Some(r)));
                    }
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
                    Span::styled(who, if p.is_agent() { style } else { Style::new().fg(DIM) }),
                    Span::raw(" "),
                    Span::styled(truncate(&detail, width.saturating_sub(used)), Style::new().fg(DIM)),
                ]));
            }
            ListItem::new(Text::from(lines))
        })
        .collect();

    let empty = if app.loading { "loading…" } else { "no herdr panes inside git repos" };
    if items.is_empty() {
        let msg = app.herdr_error.clone().unwrap_or_else(|| empty.to_string());
        f.render_widget(
            Paragraph::new(msg).fg(DIM).wrap(Wrap { trim: true }).block(block(title, focused)),
            area,
        );
        return;
    }
    let list = List::new(items)
        .block(block(title, focused))
        .highlight_style(highlight(focused))
        .highlight_symbol("▌");
    let mut state = ListState::default().with_selected(Some(app.repo_idx));
    f.render_stateful_widget(list, area, &mut state);
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(Color::Rgb(40, 44, 60))
    } else {
        Style::new().bg(Color::Rgb(30, 30, 36))
    }
}

fn draw_files(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Files;
    let Some(g) = app.repo() else {
        f.render_widget(block(" Files ", focused), area);
        return;
    };
    let stats = match &g.stats {
        Ok(s) => s,
        Err(e) => {
            f.render_widget(
                Paragraph::new(e.clone()).fg(DEL_FG).wrap(Wrap { trim: true }).block(block(" Files ", focused)),
                area,
            );
            return;
        }
    };
    let title = format!(" Files ({}) vs {} ", stats.files.len(), stats.base);
    if stats.files.is_empty() {
        f.render_widget(
            Paragraph::new(format!("no {} changes", app.mode.label())).fg(DIM).block(block(title, focused)),
            area,
        );
        return;
    }
    let width = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = stats
        .files
        .iter()
        .map(|file| {
            let color = match file.status {
                Status::Added | Status::Untracked => ADD_FG,
                Status::Deleted => DEL_FG,
                Status::Modified => Color::Yellow,
                _ => Color::Magenta,
            };
            let cnt = if file.is_dir() {
                vec![Span::styled("repo", Style::new().fg(DIM))]
            } else {
                counts(file.added, file.removed)
            };
            let cnt_len: usize = cnt.iter().map(|s| s.content.chars().count()).sum();
            let path_w = width.saturating_sub(cnt_len + 3);
            let mut spans = vec![
                Span::styled(format!("{} ", file.status.letter()), Style::new().fg(color).bold()),
                Span::raw(format!("{:<path_w$} ", truncate_left(&file.path, path_w))),
            ];
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
}

fn draw_diff(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Diff;
    app.diff_height = area.height.saturating_sub(2) as usize;
    let Some(view) = &app.diff else {
        let msg = if app.file().is_some() { "loading…" } else { "" };
        f.render_widget(Paragraph::new(msg).fg(DIM).block(block(" Diff ", focused)), area);
        return;
    };
    let lines = match &view.lines {
        Ok(l) => l,
        Err(e) => {
            f.render_widget(
                Paragraph::new(e.clone()).fg(DEL_FG).wrap(Wrap { trim: true }).block(block(" Diff ", focused)),
                area,
            );
            return;
        }
    };
    let pos = if lines.is_empty() { 0 } else { view.scroll + 1 };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(view.path.clone(), Style::new().bold()),
        Span::styled(format!(" {pos}/{} ", lines.len()), Style::new().fg(DIM)),
    ]);
    let inner_w = area.width.saturating_sub(2) as usize;
    let num_w = lines
        .iter()
        .filter_map(|l| l.old_no.max(l.new_no))
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
        .max(3);
    let height = app.diff_height;

    let rendered: Vec<Line> = lines
        .iter()
        .skip(view.scroll)
        .take(height)
        .map(|l| {
            let no = |n: Option<u32>| n.map(|n| format!("{n:>num_w$}")).unwrap_or_else(|| " ".repeat(num_w));
            let (sign, fg, bg) = match l.kind {
                LineKind::Added => ("+", ADD_FG, Some(ADD_BG)),
                LineKind::Removed => ("-", DEL_FG, Some(DEL_BG)),
                LineKind::Context => (" ", Color::Reset, None),
                LineKind::Hunk => ("", ACCENT, None),
                LineKind::Meta => ("", DIM, None),
                LineKind::NoNewline => ("", DIM, None),
            };
            let body: String = l.text.chars().skip(view.hscroll).collect();
            match l.kind {
                LineKind::Meta | LineKind::Hunk | LineKind::NoNewline => {
                    let style = if l.kind == LineKind::Hunk {
                        Style::new().fg(fg).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(fg)
                    };
                    Line::from(Span::styled(body, style))
                }
                _ => {
                    let gutter = format!("{} {} ", no(l.old_no), no(l.new_no));
                    let used = gutter.chars().count() + 1;
                    let text_style = match bg {
                        Some(bg) => Style::new().fg(Color::Reset).bg(bg),
                        None => Style::new(),
                    };
                    // Pad changed lines so the background spans the full width.
                    let body = if bg.is_some() {
                        format!("{body:<w$}", w = inner_w.saturating_sub(used))
                    } else {
                        body
                    };
                    Line::from(vec![
                        Span::styled(gutter, Style::new().fg(DIM)),
                        Span::styled(sign, Style::new().fg(fg).bold().bg(bg.unwrap_or(Color::Reset))),
                        Span::styled(body, text_style),
                    ])
                }
            }
        })
        .collect();

    f.render_widget(Paragraph::new(rendered).block(block(title, focused)), area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(format!(" {} ", app.mode.label()), Style::new().fg(Color::Black).bg(ACCENT).bold()),
        Span::raw(" "),
    ];
    if let Some(e) = &app.herdr_error {
        spans.push(Span::styled(format!("herdr: {} ", truncate(e, 60)), Style::new().fg(DEL_FG)));
    }
    if app.loading {
        spans.push(Span::styled("refreshing… ", Style::new().fg(Color::Yellow)));
    } else if let Some(t) = app.last_refresh {
        spans.push(Span::styled(format!("updated {}s ago ", t.elapsed().as_secs()), Style::new().fg(DIM)));
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
        spans.push(Span::styled(format!("· {msg} "), Style::new().fg(Color::Yellow)));
    }
    let hint = " m mode  a agent  e edit  ? help  q quit ";
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(used + hint.len());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(hint, Style::new().fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(f: &mut Frame) {
    const KEYS: &[(&str, &str)] = &[
        ("tab / l / enter", "focus next panel"),
        ("shift-tab / h / esc", "focus previous panel"),
        ("j k / ↓ ↑", "move selection (scroll in diff)"),
        ("[ ]", "previous / next file"),
        ("J K", "scroll diff by line"),
        ("space b / pgdn pgup", "scroll diff by page"),
        ("ctrl-d ctrl-u", "scroll diff half page"),
        ("g G", "diff top / bottom"),
        ("n N", "next / previous hunk"),
        ("H L", "scroll diff horizontally"),
        ("m", "cycle mode: uncommitted → unstaged → staged → branch"),
        ("r", "refresh now"),
        ("a", "focus repo's agent pane in herdr (repeat to cycle)"),
        ("e", "open file in $EDITOR at change"),
        ("q / ctrl-c", "quit"),
    ];
    let area = f.area();
    let w = 80.min(area.width);
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
        Paragraph::new(lines).block(block(" herdiff keys (any key to close) ", true).padding(ratatui::widgets::Padding::vertical(1))),
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
    use crate::git::{FileChange, Mode, RepoStats};
    use crate::model::{PaneRef, RepoGroup};
    use crate::worker::{DiffResult, Refreshed};
    use ratatui::{Terminal, backend::TestBackend};

    fn app() -> App {
        let mut app = App::new(Mode::Uncommitted);
        let file = FileChange { path: "src/lib.rs".into(), status: Status::Modified, added: Some(1), removed: Some(1) };
        app.apply_refresh(Refreshed {
            mode: Mode::Uncommitted,
            groups: vec![RepoGroup {
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
                stats: Ok(RepoStats { branch: "main".into(), base: "HEAD".into(), files: vec![file] }),
            }],
            non_repo_panes: 0,
            herdr_error: None,
        });
        app.apply_diff(DiffResult {
            root: "/r/proj".into(),
            mode: Mode::Uncommitted,
            path: "src/lib.rs".into(),
            lines: Ok(parse_unified("@@ -1,2 +1,2 @@\n ctx\n-old line\n+new line\n")),
        });
        app
    }

    fn render(w: u16, h: u16) -> String {
        let mut app = app();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
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
