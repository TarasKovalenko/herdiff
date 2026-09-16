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
    pub text: String,
}

pub fn parse_unified(input: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut in_hunk = false;

    for raw in input.lines() {
        if raw.starts_with("@@") {
            if let Some((o, n)) = parse_hunk_header(raw) {
                old_no = o;
                new_no = n;
                in_hunk = true;
            }
            out.push(line(LineKind::Hunk, None, None, raw));
            continue;
        }
        if raw.starts_with("diff --git") {
            in_hunk = false;
        }
        if !in_hunk {
            out.push(line(LineKind::Meta, None, None, raw));
            continue;
        }
        match raw.as_bytes().first() {
            Some(b'+') => {
                out.push(line(LineKind::Added, None, Some(new_no), &raw[1..]));
                new_no += 1;
            }
            Some(b'-') => {
                out.push(line(LineKind::Removed, Some(old_no), None, &raw[1..]));
                old_no += 1;
            }
            Some(b'\\') => out.push(line(LineKind::NoNewline, None, None, raw)),
            Some(b' ') => {
                out.push(line(LineKind::Context, Some(old_no), Some(new_no), &raw[1..]));
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
                out.push(line(LineKind::Meta, None, None, raw));
            }
        }
    }
    out
}

fn line(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: &str) -> DiffLine {
    DiffLine { kind, old_no, new_no, text: text.replace('\t', "    ") }
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
            vec![Meta, Meta, Meta, Meta, Hunk, Context, Removed, Added, Added, Context, NoNewline]
        );
        assert_eq!(lines[6].old_no, Some(2));
        assert_eq!(lines[7].new_no, Some(2));
        assert_eq!(lines[8].new_no, Some(3));
        assert_eq!((lines[9].old_no, lines[9].new_no), (Some(3), Some(4)));
        assert_eq!(lines[7].text, "B");
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
