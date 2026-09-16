//! Unified diff parsing into typed, numbered lines for rendering.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// `diff --git`, `index`, `---`, `+++`, mode lines, etc.
    Meta,
    Hunk,
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// Display text: without the `+`/`-`/` ` prefix, tabs expanded, no trailing `\r`.
    pub text: String,
}

pub fn parse_unified(input: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut in_hunk = false;

    for raw in input.split_terminator('\n') {
        let shown = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.starts_with("@@") {
            if let Some((o, n)) = parse_hunk_header(raw) {
                old_no = o;
                new_no = n;
                in_hunk = true;
            }
            out.push(line(LineKind::Hunk, None, None, shown));
            continue;
        }
        if raw.starts_with("diff --git") {
            in_hunk = false;
        }
        if !in_hunk {
            out.push(line(LineKind::Meta, None, None, shown));
            continue;
        }
        let body = shown.get(1..).unwrap_or("");
        match raw.as_bytes().first() {
            Some(b'+') => {
                out.push(line(LineKind::Added, None, Some(new_no), body));
                new_no += 1;
            }
            Some(b'-') => {
                out.push(line(LineKind::Removed, Some(old_no), None, body));
                old_no += 1;
            }
            Some(b'\\') => out.push(line(LineKind::NoNewline, None, None, shown)),
            Some(b' ') => {
                out.push(line(LineKind::Context, Some(old_no), Some(new_no), body));
                old_no += 1;
                new_no += 1;
            }
            None => {
                // Some tools strip the leading space from empty context lines.
                out.push(line(LineKind::Context, Some(old_no), Some(new_no), ""));
                old_no += 1;
                new_no += 1;
            }
            _ => {
                in_hunk = false;
                out.push(line(LineKind::Meta, None, None, shown));
            }
        }
    }
    out
}

/// Index of the hunk header that line `at` belongs to: the one at or above it, or the first
/// hunk when `at` is still in the file header. This is the hunk `space` stages.
pub fn hunk_start(lines: &[DiffLine], at: usize) -> Option<usize> {
    let at = at.min(lines.len().checked_sub(1)?);
    let start = (0..=at)
        .rev()
        .find(|&i| lines[i].kind == LineKind::Hunk)
        .or_else(|| (at..lines.len()).find(|&i| lines[i].kind == LineKind::Hunk))?;
    if start < at
        && lines[start + 1..=at]
            .iter()
            .any(|l| l.kind == LineKind::Meta)
    {
        return None; // `at` is past the hunk, in another file's header
    }
    Some(start)
}

/// How many hunks come before line `start` (the hunk's position in its diff).
pub fn hunk_index(lines: &[DiffLine], start: usize) -> usize {
    lines[..start.min(lines.len())]
        .iter()
        .filter(|l| l.kind == LineKind::Hunk)
        .count()
}

/// One screen row of a diff view, referencing indices into the parsed lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Spans the full width: every line in unified view; headers and hunks in split view.
    Full(usize),
    /// Split view: old side (context/removed) and new side (context/added).
    Pair(Option<usize>, Option<usize>),
}

impl Row {
    /// Line used as the scroll anchor for this row.
    pub fn anchor(self) -> usize {
        match self {
            Row::Full(i) | Row::Pair(Some(i), _) | Row::Pair(None, Some(i)) => i,
            Row::Pair(None, None) => 0,
        }
    }
}

/// Rows for one layout plus a reverse map from line index to row index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rows {
    pub rows: Vec<Row>,
    pub line_row: Vec<usize>,
}

impl Rows {
    fn from_rows(rows: Vec<Row>, line_count: usize) -> Self {
        let mut line_row = vec![0; line_count];
        for (r, row) in rows.iter().enumerate() {
            match *row {
                Row::Full(i) => line_row[i] = r,
                Row::Pair(a, b) => {
                    for i in [a, b].into_iter().flatten() {
                        line_row[i] = r;
                    }
                }
            }
        }
        Self { rows, line_row }
    }

    pub fn unified(lines: &[DiffLine]) -> Self {
        Self::from_rows((0..lines.len()).map(Row::Full).collect(), lines.len())
    }

    /// Side-by-side rows: a run of removed lines is paired with the added lines that follow it.
    pub fn split(lines: &[DiffLine]) -> Self {
        let mut rows = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            match lines[i].kind {
                LineKind::Context => {
                    rows.push(Row::Pair(Some(i), Some(i)));
                    i += 1;
                }
                LineKind::Removed | LineKind::Added => {
                    let removed: Vec<usize> = (i..lines.len())
                        .take_while(|&j| lines[j].kind == LineKind::Removed)
                        .collect();
                    i += removed.len();
                    let added: Vec<usize> = (i..lines.len())
                        .take_while(|&j| lines[j].kind == LineKind::Added)
                        .collect();
                    i += added.len();
                    for k in 0..removed.len().max(added.len()) {
                        rows.push(Row::Pair(removed.get(k).copied(), added.get(k).copied()));
                    }
                }
                _ => {
                    rows.push(Row::Full(i));
                    i += 1;
                }
            }
        }
        Self::from_rows(rows, lines.len())
    }
}

fn line(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: &str) -> DiffLine {
    DiffLine {
        kind,
        old_no,
        new_no,
        text: text.replace('\t', "    "),
    }
}

/// `@@ -12,5 +12,7 @@ fn x` → (12, 12)
fn parse_hunk_header(h: &str) -> Option<(u32, u32)> {
    let mut parts = h.split_whitespace().skip(1);
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |s: &str| s.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/f.rs b/f.rs
index 1..2 100644
--- a/f.rs
+++ b/f.rs
@@ -1,3 +1,4 @@ fn main
 a
-b
+B
+c
 d
\\ No newline at end of file
";

    #[test]
    fn numbers_lines() {
        let lines = parse_unified(SAMPLE);
        let kinds: Vec<_> = lines.iter().map(|l| l.kind).collect();
        use LineKind::*;
        assert_eq!(
            kinds,
            vec![
                Meta, Meta, Meta, Meta, Hunk, Context, Removed, Added, Added, Context, NoNewline
            ]
        );
        assert_eq!(lines[6].old_no, Some(2));
        assert_eq!(lines[7].new_no, Some(2));
        assert_eq!(lines[8].new_no, Some(3));
        assert_eq!((lines[9].old_no, lines[9].new_no), (Some(3), Some(4)));
        assert_eq!(lines[7].text, "B");
    }

    #[test]
    fn split_rows_pair_removed_with_added() {
        let lines = parse_unified(SAMPLE);
        let split = Rows::split(&lines);
        use Row::*;
        assert_eq!(
            split.rows,
            vec![
                Full(0),
                Full(1),
                Full(2),
                Full(3),
                Full(4),
                Pair(Some(5), Some(5)),
                Pair(Some(6), Some(7)),
                Pair(None, Some(8)),
                Pair(Some(9), Some(9)),
                Full(10),
            ]
        );
        assert_eq!(split.line_row[8], 7);
        assert_eq!(split.rows[7].anchor(), 8);
        let unified = Rows::unified(&lines);
        assert_eq!(unified.rows.len(), lines.len());
        assert_eq!(unified.line_row[8], 8);
    }

    #[test]
    fn hunk_start_and_index() {
        let lines = parse_unified(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+A\n@@ -9 +9 @@\n-y\n+Y\n",
        );
        assert_eq!(hunk_start(&lines, 0), Some(3)); // file header: first hunk
        assert_eq!(hunk_start(&lines, 5), Some(3));
        assert_eq!(hunk_start(&lines, 8), Some(6));
        assert_eq!(hunk_index(&lines, 6), 1);
        assert_eq!(lines[4].text, "a");
        assert!(hunk_start(&parse_unified("Binary files differ\n"), 0).is_none());
    }

    #[test]
    fn hunk_header_without_counts() {
        assert_eq!(parse_hunk_header("@@ -0,0 +1 @@"), Some((0, 1)));
    }

    #[test]
    fn meta_lines_are_not_treated_as_changes() {
        let lines = parse_unified("--- a/x\n+++ b/x\n");
        assert!(lines.iter().all(|l| l.kind == LineKind::Meta));
    }
}
