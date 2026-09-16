//! Syntax highlighting for diff lines (syntect with bat's syntax and theme sets).

use std::path::Path;

use anyhow::{Result, anyhow};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::SyntaxSet;
use two_face::theme::EmbeddedThemeName;

use crate::diff::{DiffLine, LineKind};

pub const DEFAULT_THEME: &str = "Monokai Extended";

/// Skip highlighting past these limits; large generated files are slow and unreadable anyway.
const MAX_LINES: usize = 10_000;
const MAX_LINE_LEN: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fg {
    Rgb(u8, u8, u8),
    /// Terminal palette color (from the `ansi` / `base16` themes).
    Indexed(u8),
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlSpan {
    pub fg: Fg,
    pub bold: bool,
    pub italic: bool,
    pub text: String,
}

/// Highlighted spans per diff line (same indices as the lines). `None` = render plain.
pub type Highlights = Vec<Option<Vec<HlSpan>>>;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new(theme_name: &str) -> Result<Self> {
        let themes = two_face::theme::extra();
        let name = find_theme(theme_name).ok_or_else(|| {
            anyhow!(
                "unknown theme {theme_name:?}; available: {}",
                theme_names().join(", ")
            )
        })?;
        Ok(Self {
            syntaxes: two_face::syntax::extra_newlines(),
            theme: themes.get(name).clone(),
        })
    }

    pub fn highlight(&self, path: &str, lines: &[DiffLine]) -> Highlights {
        let mut out: Highlights = vec![None; lines.len()];
        let Some(syntax) = self.syntax_for(path) else {
            return out;
        };
        if lines.len() > MAX_LINES {
            return out;
        }
        // Old and new sides are separate streams so parser state (open comments,
        // strings) follows each version of the file. Context lines feed both.
        let mut old = HighlightLines::new(syntax, &self.theme);
        let mut new = HighlightLines::new(syntax, &self.theme);
        for (i, line) in lines.iter().enumerate() {
            match line.kind {
                LineKind::Hunk => {
                    // Hunks are not contiguous; restart so state can't leak across gaps.
                    old = HighlightLines::new(syntax, &self.theme);
                    new = HighlightLines::new(syntax, &self.theme);
                }
                LineKind::Removed => out[i] = self.line(&mut old, &line.text),
                LineKind::Added => out[i] = self.line(&mut new, &line.text),
                LineKind::Context => {
                    self.line(&mut old, &line.text);
                    out[i] = self.line(&mut new, &line.text);
                }
                LineKind::Meta | LineKind::NoNewline => {}
            }
        }
        out
    }

    fn line(&self, h: &mut HighlightLines, text: &str) -> Option<Vec<HlSpan>> {
        if text.len() > MAX_LINE_LEN {
            return None;
        }
        let with_nl = format!("{text}\n");
        let ranges = h.highlight_line(&with_nl, &self.syntaxes).ok()?;
        Some(
            ranges
                .into_iter()
                .map(|(style, s)| HlSpan {
                    fg: convert(style.foreground),
                    bold: style.font_style.contains(FontStyle::BOLD),
                    italic: style.font_style.contains(FontStyle::ITALIC),
                    text: s.trim_end_matches('\n').to_string(),
                })
                .filter(|s| !s.text.is_empty())
                .collect(),
        )
    }

    fn syntax_for(&self, path: &str) -> Option<&syntect::parsing::SyntaxReference> {
        let p = Path::new(path);
        let file_name = p.file_name()?.to_str()?;
        p.extension()
            .and_then(|e| e.to_str())
            .and_then(|e| self.syntaxes.find_syntax_by_extension(e))
            // Extension-less names like `Makefile` or `Dockerfile` are registered as extensions too.
            .or_else(|| self.syntaxes.find_syntax_by_extension(file_name))
            .filter(|s| s.name != "Plain Text")
    }
}

/// bat's `ansi`/`base16` themes encode palette colors in the alpha channel.
fn convert(c: syntect::highlighting::Color) -> Fg {
    match c.a {
        0 => Fg::Indexed(c.r),
        1 => Fg::Default,
        _ => Fg::Rgb(c.r, c.g, c.b),
    }
}

fn find_theme(name: &str) -> Option<EmbeddedThemeName> {
    two_face::theme::EmbeddedLazyThemeSet::theme_names()
        .iter()
        .copied()
        .find(|t| t.as_name().eq_ignore_ascii_case(name))
}

pub fn theme_names() -> Vec<&'static str> {
    two_face::theme::EmbeddedLazyThemeSet::theme_names()
        .iter()
        .map(|t| t.as_name())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified;

    #[test]
    fn highlights_rust_by_extension() {
        let h = Highlighter::new(DEFAULT_THEME).unwrap();
        let lines =
            parse_unified("@@ -1,2 +1,2 @@\n fn main() {\n-    let a = 1;\n+    let b = \"x\";\n");
        let hl = h.highlight("src/main.rs", &lines);
        assert!(hl[0].is_none(), "hunk header stays plain");
        for i in 1..=3 {
            let spans = hl[i].as_ref().expect("code line highlighted");
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, lines[i].text);
            let colors: std::collections::HashSet<_> = spans.iter().map(|s| s.fg).collect();
            assert!(colors.len() > 1, "expected several colors in {spans:?}");
        }
    }

    #[test]
    fn toml_and_unknown_files() {
        let h = Highlighter::new("github").unwrap();
        let lines = parse_unified("@@ -0,0 +1 @@\n+name = \"x\"\n");
        assert!(h.highlight("Cargo.toml", &lines)[1].is_some());
        assert!(h.highlight("notes.unknownext", &lines)[1].is_none());
    }

    #[test]
    fn rejects_unknown_theme() {
        assert!(Highlighter::new("nope").is_err());
    }
}
