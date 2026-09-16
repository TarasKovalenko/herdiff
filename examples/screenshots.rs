//! Regenerate the README screenshots.
//!
//!     cargo run --example screenshots
//!
//! Every screen goes through the real code: demo git repos are created in a temp directory,
//! diffed and highlighted by herdiff's git and highlight modules, and drawn by `ui::draw`
//! into ratatui's test backend, which is then written out as SVG. Only the herdr session
//! is invented, so no herdr needs to be running and no real workspace, path or repo ever
//! ends up in a picture.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdiff::app::{App, Focus, View};
use herdiff::diff::parse_unified;
use herdiff::git::{self, Mode};
use herdiff::herdr::Snapshot;
use herdiff::highlight::{DEFAULT_THEME, Highlighter};
use herdiff::model::{self, RepoGroup};
use herdiff::scope::{Scope, Target};
use herdiff::ui;
use herdiff::worker::{DiffResult, Refreshed};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use serde_json::json;

const OUT: &str = "docs/screenshots";
/// Cell size in SVG pixels.
const CW: f64 = 8.4;
const CH: f64 = 18.0;
const FONT: &str = "ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace";
const BG: &str = "#15161c";
const FG: &str = "#d6d8e0";

fn main() -> Result<()> {
    // Same output on every machine: ignore the runner's git config.
    // SAFETY: single-threaded start-up, before anything reads the environment.
    unsafe {
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    }
    std::fs::create_dir_all(OUT)?;
    let tmp = tempfile::tempdir()?;
    let demo = Demo::create(tmp.path())?;
    let hl = Highlighter::new(DEFAULT_THEME)?;

    // 1. Follow scope on the payments workspace, side-by-side Rust diff.
    let mut app = demo.app(Scope::Follow, Some("w1"), View::Split)?;
    select(&mut app, &hl, "src/refund.rs")?;
    app.focus = Focus::Diff;
    shot(&mut app, "split", 160, 40)?;

    // 2. Every workspace, narrow terminal, unified TypeScript diff.
    let mut app = demo.app(Scope::All, None, View::Unified)?;
    app.repo_idx = 1;
    select(&mut app, &hl, "src/components/RevenueChart.tsx")?;
    app.focus = Focus::Files;
    shot(&mut app, "unified", 110, 42)?;

    // 3. Help overlay.
    let mut app = demo.app(Scope::Follow, Some("w1"), View::Split)?;
    select(&mut app, &hl, "src/refund.rs")?;
    app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    shot(&mut app, "help", 160, 40)?;

    // 4. Staging and committing: part of the work staged, commit message being written.
    let mut app = demo.app(Scope::Follow, Some("w1"), View::Split)?;
    select(&mut app, &hl, "src/refund.rs")?;
    app.focus = Focus::Files;
    app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    app.on_paste(
        "Document partial refunds\n\nBump to 0.5.0 and describe how repeated refunds work.",
    );
    shot(&mut app, "commit", 160, 40)?;

    println!("wrote {OUT}/split.svg, unified.svg, help.svg, commit.svg");
    Ok(())
}

struct Demo {
    snapshot: Snapshot,
}

impl Demo {
    fn create(dir: &Path) -> Result<Self> {
        let payments = dir.join("payments-api");
        let dashboard = dir.join("web-dashboard");
        let infra = dir.join("infra");
        payments_repo(&payments)?;
        dashboard_repo(&dashboard)?;
        infra_repo(&infra)?;

        let cwd = |p: &PathBuf| p.to_string_lossy().into_owned();
        let snapshot = serde_json::from_value(json!({
            "focused_workspace_id": "w1",
            "focused_pane_id": "w1:p1",
            "workspaces": [
                {"workspace_id": "w1", "label": "payments", "number": 1},
                {"workspace_id": "w2", "label": "dashboard", "number": 2},
                {"workspace_id": "w3", "label": "infra", "number": 3}
            ],
            "tabs": [
                {"tab_id": "w1:t1", "label": "agents"},
                {"tab_id": "w2:t1", "label": "ui"},
                {"tab_id": "w3:t1", "label": "ops"}
            ],
            "panes": [
                {"pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1", "cwd": cwd(&payments),
                 "agent": "claude", "agent_status": "working",
                 "terminal_title_stripped": "Partial refunds"},
                {"pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t1", "cwd": cwd(&payments),
                 "agent": "codex", "agent_status": "done",
                 "terminal_title_stripped": "Refund tests"},
                {"pane_id": "w2:p1", "workspace_id": "w2", "tab_id": "w2:t1", "cwd": cwd(&dashboard),
                 "agent": "claude", "agent_status": "blocked",
                 "terminal_title_stripped": "Chart tooltips"},
                {"pane_id": "w3:p1", "workspace_id": "w3", "tab_id": "w3:t1", "cwd": cwd(&infra),
                 "agent": "opencode", "agent_status": "idle",
                 "terminal_title_stripped": "Bump replicas"},
                {"pane_id": "w3:p2", "workspace_id": "w3", "tab_id": "w3:t1", "cwd": dir.to_string_lossy()}
            ]
        }))?;
        Ok(Self { snapshot })
    }

    /// App state as the worker would deliver it for this scope.
    fn app(&self, scope: Scope, workspace: Option<&str>, view: View) -> Result<App> {
        let label = workspace.and_then(|id| {
            self.snapshot
                .workspaces
                .iter()
                .find(|w| w.workspace_id == id)
                .map(|w| w.label.clone())
        });
        let target = Target {
            scope,
            workspace_id: workspace.map(String::from),
            label,
        };
        let topo = model::group_panes(&self.snapshot, &target, None, git::repo_root);
        let groups = topo
            .repos
            .into_iter()
            .map(|(root, panes)| RepoGroup {
                name: model::repo_name(&root),
                stats: git::repo_stats(&root, Mode::Uncommitted).map_err(|e| e.to_string()),
                root,
                panes,
            })
            .collect();
        let mut app = App::new(Mode::Uncommitted, view).with_scope(scope, true);
        app.apply_refresh(Refreshed {
            mode: Mode::Uncommitted,
            target,
            groups,
            non_repo_panes: topo.non_repo_panes,
            herdr_error: None,
        });
        Ok(app)
    }
}

/// Select `path` in the current repo and load its diff the way the worker does.
fn select(app: &mut App, hl: &Highlighter, path: &str) -> Result<()> {
    app.file_idx = app
        .files()
        .iter()
        .position(|f| f.path == path)
        .with_context(|| format!("{path} not changed in demo repo"))?;
    let root = app.repo().context("no repo selected")?.root.clone();
    let file = app.file().context("no file")?.clone();
    let lines = parse_unified(&git::file_diff(&root, Mode::Uncommitted, &file)?);
    let highlights = hl.highlight(&file.path, &lines);
    app.apply_diff(DiffResult {
        root,
        mode: Mode::Uncommitted,
        path: file.path,
        lines: Ok(lines),
        highlights,
    });
    Ok(())
}

fn shot(app: &mut App, name: &str, width: u16, height: u16) -> Result<()> {
    // Always "updated 0s ago", however long the demo setup took.
    app.last_refresh = Some(std::time::Instant::now());
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|f| ui::draw(f, app))?;
    let svg = svg(terminal.backend().buffer());
    std::fs::write(Path::new(OUT).join(format!("{name}.svg")), svg)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Demo repositories

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Demo", "-c", "user.email=demo@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()?;
    ensure!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn write(dir: &Path, path: &str, content: &str) -> Result<()> {
    let full = dir.join(path);
    std::fs::create_dir_all(full.parent().unwrap())?;
    std::fs::write(full, content)?;
    Ok(())
}

fn commit_all(dir: &Path, msg: &str) -> Result<()> {
    git(dir, &["add", "-A"])?;
    git(dir, &["commit", "-q", "-m", msg])
}

fn payments_repo(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "-q", "-b", "main"])?;
    write(
        dir,
        "Cargo.toml",
        r#"[package]
name = "payments-api"
version = "0.4.2"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
thiserror = "2"
"#,
    )?;
    write(
        dir,
        "src/refund.rs",
        r#"use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::money::Money;
use crate::payment::{Payment, PaymentStatus};

#[derive(Debug, Deserialize)]
pub struct RefundRequest {
    pub payment_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Refund {
    pub payment_id: String,
    pub amount: Money,
}

#[derive(Debug, Error)]
pub enum RefundError {
    #[error("payment {0} is not captured")]
    NotCaptured(String),
}

/// Refund a captured payment in full.
pub fn refund(payment: &Payment, req: RefundRequest) -> Result<Refund, RefundError> {
    if payment.status != PaymentStatus::Captured {
        return Err(RefundError::NotCaptured(req.payment_id));
    }
    Ok(Refund {
        payment_id: req.payment_id,
        amount: payment.amount,
    })
}
"#,
    )?;
    write(
        dir,
        "src/lib.rs",
        "pub mod money;\npub mod payment;\npub mod refund;\n",
    )?;
    commit_all(dir, "Initial refunds")?;
    git(dir, &["checkout", "-q", "-b", "feat/partial-refunds"])?;

    // The agent's uncommitted work.
    write(
        dir,
        "src/refund.rs",
        r#"use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::money::Money;
use crate::payment::{Payment, PaymentStatus};

#[derive(Debug, Deserialize)]
pub struct RefundRequest {
    pub payment_id: String,
    /// Amount to refund. `None` refunds whatever is left.
    pub amount: Option<Money>,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Refund {
    pub payment_id: String,
    pub amount: Money,
    pub remaining: Money,
}

#[derive(Debug, Error)]
pub enum RefundError {
    #[error("payment {0} is not captured")]
    NotCaptured(String),
    #[error("refund of {requested} exceeds the {remaining} left on the payment")]
    ExceedsRemaining { requested: Money, remaining: Money },
}

/// Refund a captured payment, fully or in part.
pub fn refund(payment: &Payment, req: RefundRequest) -> Result<Refund, RefundError> {
    if !matches!(payment.status, PaymentStatus::Captured | PaymentStatus::PartiallyRefunded) {
        return Err(RefundError::NotCaptured(req.payment_id));
    }
    let remaining = payment.amount - payment.refunded;
    let requested = req.amount.unwrap_or(remaining);
    if requested > remaining {
        return Err(RefundError::ExceedsRemaining { requested, remaining });
    }
    Ok(Refund {
        payment_id: req.payment_id,
        amount: requested,
        remaining: remaining - requested,
    })
}
"#,
    )?;
    write(
        dir,
        "Cargo.toml",
        r#"[package]
name = "payments-api"
version = "0.5.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
thiserror = "2"
tracing = "0.1"
"#,
    )?;
    write(
        dir,
        "docs/refunds.md",
        "# Partial refunds\n\nA payment can be refunded several times until nothing is left.\n",
    )?;
    // Some of it already staged, as it would be mid-review.
    git(dir, &["add", "Cargo.toml", "docs/refunds.md"])?;
    Ok(())
}

fn dashboard_repo(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "-q", "-b", "main"])?;
    write(
        dir,
        "src/components/RevenueChart.tsx",
        r##"import { LineChart, Line, XAxis, YAxis } from "recharts";

type Point = { day: string; revenue: number };

export function RevenueChart({ data }: { data: Point[] }) {
  return (
    <LineChart width={640} height={240} data={data}>
      <XAxis dataKey="day" />
      <YAxis />
      <Line type="monotone" dataKey="revenue" stroke="#6366f1" />
    </LineChart>
  );
}
"##,
    )?;
    write(
        dir,
        "src/api/revenue.ts",
        "export async function fetchRevenue(range: string) {\n  const res = await fetch(`/api/revenue?range=${range}`);\n  return res.json();\n}\n",
    )?;
    commit_all(dir, "Revenue chart")?;

    write(
        dir,
        "src/components/RevenueChart.tsx",
        r##"import { LineChart, Line, XAxis, YAxis, Tooltip } from "recharts";

import { formatCurrency } from "../lib/format";

type Point = { day: string; revenue: number; refunds: number };

export function RevenueChart({ data }: { data: Point[] }) {
  return (
    <LineChart width={640} height={240} data={data}>
      <XAxis dataKey="day" />
      <YAxis tickFormatter={formatCurrency} />
      <Tooltip formatter={(value: number) => formatCurrency(value)} />
      <Line type="monotone" dataKey="revenue" stroke="#6366f1" />
      <Line type="monotone" dataKey="refunds" stroke="#f43f5e" dot={false} />
    </LineChart>
  );
}
"##,
    )?;
    write(
        dir,
        "src/lib/format.ts",
        "const usd = new Intl.NumberFormat(\"en-US\", { style: \"currency\", currency: \"USD\" });\n\nexport const formatCurrency = (value: number) => usd.format(value);\n",
    )?;
    Ok(())
}

fn infra_repo(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "-q", "-b", "main"])?;
    write(
        dir,
        "k8s/payments.yaml",
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: payments-api\nspec:\n  replicas: 2\n",
    )?;
    commit_all(dir, "Payments deployment")?;
    write(
        dir,
        "k8s/payments.yaml",
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: payments-api\nspec:\n  replicas: 4\n",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// SVG output

fn svg(buf: &Buffer) -> String {
    let (w, h) = (buf.area.width, buf.area.height);
    let (width, height) = (w as f64 * CW, h as f64 * CH);
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.0} {height:.0}" width="{width:.0}" height="{height:.0}" font-family="{FONT}" font-size="14">"#
    );
    let _ = write!(
        out,
        r#"<rect width="{width:.0}" height="{height:.0}" rx="6" fill="{BG}"/>"#
    );

    // Backgrounds: one rectangle per run of equal colour.
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let bg = hex(buf[(x, y)].style().bg, BG);
            let start = x;
            while x < w && hex(buf[(x, y)].style().bg, BG) == bg {
                x += 1;
            }
            if bg != BG {
                let _ = write!(
                    out,
                    r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{CH:.1}" fill="{bg}"/>"#,
                    start as f64 * CW,
                    y as f64 * CH,
                    (x - start) as f64 * CW
                );
            }
        }
    }

    // Text: one element per run of equal style, stretched to its exact cell count so
    // columns line up whatever monospace font the viewer has.
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let style = text_style(buf[(x, y)].style());
            let start = x;
            let mut text = String::new();
            while x < w && text_style(buf[(x, y)].style()) == style {
                // A wide character's second cell holds an empty symbol.
                text.push_str(buf[(x, y)].symbol());
                x += 1;
            }
            if text.trim().is_empty() {
                continue;
            }
            let fill = hex(style.fg, FG);
            let weight = if style.add_modifier.contains(Modifier::BOLD) {
                r#" font-weight="700""#
            } else {
                ""
            };
            let italic = if style.add_modifier.contains(Modifier::ITALIC) {
                r#" font-style="italic""#
            } else {
                ""
            };
            let _ = write!(
                out,
                r#"<text x="{:.1}" y="{:.1}" fill="{fill}"{weight}{italic} textLength="{:.1}" lengthAdjust="spacingAndGlyphs" xml:space="preserve">{}</text>"#,
                start as f64 * CW,
                y as f64 * CH + CH * 0.75,
                (x - start) as f64 * CW,
                escape(&text)
            );
        }
    }
    out.push_str("</svg>\n");
    out
}

/// Only what affects how text is drawn; background is painted separately.
fn text_style(s: Style) -> Style {
    Style::new()
        .fg(s.fg.unwrap_or(Color::Reset))
        .add_modifier(s.add_modifier)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A ratatui colour as CSS. `Reset` means the terminal default, which a picture has to pick.
fn hex(color: Option<Color>, default: &str) -> String {
    let named = |s: &str| s.to_string();
    match color.unwrap_or(Color::Reset) {
        Color::Reset => default.to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Black => named("#1b1d26"),
        Color::Red => named("#f7768e"),
        Color::Green => named("#9ece6a"),
        Color::Yellow => named("#e0af68"),
        Color::Blue => named("#7aa2f7"),
        Color::Magenta => named("#bb9af7"),
        Color::Cyan => named("#7dcfff"),
        Color::Gray => named("#a9b1d6"),
        Color::DarkGray => named("#636a86"),
        Color::LightRed => named("#ff899d"),
        Color::LightGreen => named("#b9f27c"),
        Color::LightYellow => named("#ffc777"),
        Color::LightBlue => named("#8db0ff"),
        Color::LightMagenta => named("#c7a9ff"),
        Color::LightCyan => named("#a4daff"),
        Color::White => named("#e6e8f0"),
        Color::Indexed(i) => indexed(i),
    }
}

fn indexed(i: u8) -> String {
    const BASE: [&str; 16] = [
        "#1b1d26", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
        "#636a86", "#ff899d", "#b9f27c", "#ffc777", "#8db0ff", "#c7a9ff", "#a4daff", "#e6e8f0",
    ];
    match i {
        0..=15 => BASE[i as usize].to_string(),
        // 6x6x6 colour cube.
        16..=231 => {
            let v = i - 16;
            let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            format!(
                "#{:02x}{:02x}{:02x}",
                level(v / 36),
                level((v / 6) % 6),
                level(v % 6)
            )
        }
        // Grayscale ramp.
        232..=255 => {
            let g = 8 + (i - 232) * 10;
            format!("#{g:02x}{g:02x}{g:02x}")
        }
    }
}
