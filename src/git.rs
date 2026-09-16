//! Git access by shelling out to the `git` CLI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// SHA of the empty tree, used as base when a repo has no commits yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const MAX_UNTRACKED_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Working tree (incl. untracked) vs HEAD.
    Uncommitted,
    /// Working tree vs index.
    Unstaged,
    /// Index vs HEAD.
    Staged,
    /// Working tree vs merge-base with the default branch.
    Branch,
}

impl Mode {
    pub const ALL: [Mode; 4] = [
        Mode::Uncommitted,
        Mode::Unstaged,
        Mode::Staged,
        Mode::Branch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Uncommitted => "uncommitted",
            Mode::Unstaged => "unstaged",
            Mode::Staged => "staged",
            Mode::Branch => "branch",
        }
    }

    pub fn next(self) -> Mode {
        let i = Mode::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Mode::ALL[(i + 1) % Mode::ALL.len()]
    }

    pub fn includes_untracked(self) -> bool {
        self != Mode::Staged
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Modified,
    Deleted,
    TypeChanged,
    Untracked,
    Other(char),
}

impl Status {
    pub fn letter(self) -> char {
        match self {
            Status::Added => 'A',
            Status::Modified => 'M',
            Status::Deleted => 'D',
            Status::TypeChanged => 'T',
            Status::Untracked => '?',
            Status::Other(c) => c,
        }
    }

    fn from_letter(c: char) -> Status {
        match c {
            'A' => Status::Added,
            'M' => Status::Modified,
            'D' => Status::Deleted,
            'T' => Status::TypeChanged,
            other => Status::Other(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub status: Status,
    /// `None` for binary files.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

impl FileChange {
    /// Untracked nested repos are listed by git as `dir/`.
    pub fn is_dir(&self) -> bool {
        self.path.ends_with('/')
    }
}

#[derive(Debug, Clone, Default)]
pub struct RepoStats {
    pub branch: String,
    /// Human description of the diff base (e.g. `HEAD`, `main@abc1234`).
    pub base: String,
    pub files: Vec<FileChange>,
}

impl RepoStats {
    pub fn totals(&self) -> (u32, u32) {
        self.files.iter().fold((0, 0), |(a, r), f| {
            (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0))
        })
    }
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off", "-c", "color.ui=false"])
        // Never take index.lock: agents are committing in these repos concurrently.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// Run git; exit codes in `ok_codes` count as success.
fn run(dir: &Path, args: &[&str], ok_codes: &[i32]) -> Result<Vec<u8>> {
    let out = git(dir)
        .args(args)
        .output()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    let code = out.status.code().unwrap_or(-1);
    if !ok_codes.contains(&code) {
        bail!(
            "git {} failed ({code}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn run_str(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&run(dir, args, &[0])?)
        .trim()
        .to_string())
}

pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    run_str(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn branch_name(root: &Path) -> String {
    run_str(root, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            run_str(root, &["rev-parse", "--short", "HEAD"])
                .ok()
                .map(|s| format!("detached@{s}"))
        })
        .unwrap_or_else(|| "(no commits)".into())
}

fn has_head(root: &Path) -> bool {
    run(root, &["rev-parse", "--verify", "-q", "HEAD"], &[0]).is_ok()
}

/// Resolve the diff base revision for a mode. `None` means "index" (unstaged mode).
fn base_rev(root: &Path, mode: Mode) -> Result<(Option<String>, String)> {
    let (head, head_label) = if has_head(root) {
        ("HEAD".to_string(), "HEAD")
    } else {
        (EMPTY_TREE.to_string(), "empty tree")
    };
    Ok(match mode {
        Mode::Unstaged => (None, "index".into()),
        Mode::Uncommitted | Mode::Staged => (Some(head), head_label.into()),
        Mode::Branch => {
            let default = default_branch(root);
            match default
                .as_deref()
                .map(|b| run_str(root, &["merge-base", "HEAD", b]))
            {
                Some(Ok(mb)) if !mb.is_empty() => {
                    let short = &mb[..mb.len().min(7)];
                    (Some(mb.clone()), format!("{}@{short}", default.unwrap()))
                }
                _ => (Some(head), format!("{head_label} (no default branch)")),
            }
        }
    })
}

fn default_branch(root: &Path) -> Option<String> {
    if let Ok(r) = run_str(
        root,
        &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
    ) && !r.is_empty()
    {
        return Some(r);
    }
    ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|b| run(root, &["rev-parse", "--verify", "-q", b], &[0]).is_ok())
        .map(String::from)
}

/// Arguments selecting what to compare, shared by numstat/name-status/patch.
fn diff_args(mode: Mode, base: &Option<String>) -> Vec<String> {
    let mut args = vec![
        "diff".to_string(),
        "--no-ext-diff".into(),
        "--no-renames".into(),
    ];
    if mode == Mode::Staged {
        args.push("--cached".into());
    }
    if let Some(b) = base {
        args.push(b.clone());
    }
    args
}

pub fn repo_stats(root: &Path, mode: Mode) -> Result<RepoStats> {
    let (base, base_label) = base_rev(root, mode)?;
    let args = diff_args(mode, &base);
    let run_diff = |extra: &[&str]| {
        let mut a: Vec<&str> = args.iter().map(String::as_str).collect();
        a.extend_from_slice(extra);
        run(root, &a, &[0])
    };
    let name_status = run_diff(&["--name-status", "-z"])?;
    let numstat = run_diff(&["--numstat", "-z"])?;

    let mut files: BTreeMap<String, FileChange> = parse_name_status(&name_status)
        .into_iter()
        .map(|(status, path)| {
            (
                path.clone(),
                FileChange {
                    path,
                    status,
                    added: Some(0),
                    removed: Some(0),
                },
            )
        })
        .collect();
    for (added, removed, path) in parse_numstat(&numstat) {
        if let Some(f) = files.get_mut(&path) {
            f.added = added;
            f.removed = removed;
        }
    }

    if mode.includes_untracked() {
        let untracked = run(
            root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            &[0],
        )?;
        for path in split_z(&untracked) {
            let lines = count_lines(&root.join(&path));
            files.insert(
                path.clone(),
                FileChange {
                    path,
                    status: Status::Untracked,
                    added: lines,
                    removed: lines.map(|_| 0),
                },
            );
        }
    }

    Ok(RepoStats {
        branch: branch_name(root),
        base: base_label,
        files: files.into_values().collect(),
    })
}

/// Unified diff text for one file.
pub fn file_diff(root: &Path, mode: Mode, file: &FileChange) -> Result<String> {
    if file.is_dir() {
        return Ok(format!(
            "untracked directory (nested git repo): {}\n",
            file.path
        ));
    }
    if file.status == Status::Untracked {
        let out = run(
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-index",
                "--",
                "/dev/null",
                &file.path,
            ],
            &[0, 1],
        )?;
        return Ok(String::from_utf8_lossy(&out).into_owned());
    }
    let (base, _) = base_rev(root, mode)?;
    let mut args = diff_args(mode, &base);
    args.extend(["--".to_string(), file.path.clone()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(String::from_utf8_lossy(&run(root, &refs, &[0])?).into_owned())
}

fn split_z(buf: &[u8]) -> Vec<String> {
    buf.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// `--name-status -z --no-renames`: `M\0path\0A\0path\0`
fn parse_name_status(buf: &[u8]) -> Vec<(Status, String)> {
    let parts = split_z(buf);
    parts
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| {
            (
                Status::from_letter(c[0].chars().next().unwrap_or('?')),
                c[1].clone(),
            )
        })
        .collect()
}

/// `--numstat -z --no-renames`: `12\t3\tpath\0` (`-\t-\t` for binary)
fn parse_numstat(buf: &[u8]) -> Vec<(Option<u32>, Option<u32>, String)> {
    split_z(buf)
        .into_iter()
        .filter_map(|rec| {
            let mut it = rec.splitn(3, '\t');
            let a = it.next()?.parse().ok();
            let r = it.next()?.parse().ok();
            Some((a, r, it.next()?.to_string()))
        })
        .collect()
}

/// Line count of a text file; `None` for binary or huge files.
fn count_lines(path: &Path) -> Option<u32> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_UNTRACKED_BYTES {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    if data.contains(&0) {
        return None;
    }
    let newlines = data.iter().filter(|b| **b == b'\n').count();
    let trailing = usize::from(!data.is_empty() && data.last() != Some(&b'\n'));
    Some((newlines + trailing) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_name_status_and_numstat() {
        let ns = b"M\0src/a.rs\0D\0gone.txt\0";
        assert_eq!(
            parse_name_status(ns),
            vec![
                (Status::Modified, "src/a.rs".into()),
                (Status::Deleted, "gone.txt".into())
            ]
        );
        let num = b"3\t1\tsrc/a.rs\0-\t-\timg.png\0";
        assert_eq!(
            parse_numstat(num),
            vec![
                (Some(3), Some(1), "src/a.rs".into()),
                (None, None, "img.png".into())
            ]
        );
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    fn setup() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        sh(d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(d.join("b.txt"), "keep\n").unwrap();
        sh(d, &["add", "."]);
        sh(d, &["commit", "-q", "-m", "init"]);
        sh(d, &["checkout", "-q", "-b", "feature"]);
        fs::write(d.join("c.txt"), "committed on branch\n").unwrap();
        sh(d, &["add", "."]);
        sh(d, &["commit", "-q", "-m", "feat"]);
        // staged edit
        fs::write(d.join("b.txt"), "keep\nstaged\n").unwrap();
        sh(d, &["add", "b.txt"]);
        // unstaged edit
        fs::write(d.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        // untracked
        fs::write(d.join("new.md"), "x\ny").unwrap();
        tmp
    }

    fn paths(s: &RepoStats) -> Vec<(&str, char, Option<u32>, Option<u32>)> {
        s.files
            .iter()
            .map(|f| (f.path.as_str(), f.status.letter(), f.added, f.removed))
            .collect()
    }

    #[test]
    fn modes_select_expected_files() {
        let tmp = setup();
        let root = repo_root(tmp.path()).unwrap();

        let s = repo_stats(&root, Mode::Uncommitted).unwrap();
        assert_eq!(s.branch, "feature");
        assert_eq!(
            paths(&s),
            vec![
                ("a.txt", 'M', Some(1), Some(1)),
                ("b.txt", 'M', Some(1), Some(0)),
                ("new.md", '?', Some(2), Some(0))
            ]
        );
        assert_eq!(s.totals(), (4, 1));

        let s = repo_stats(&root, Mode::Unstaged).unwrap();
        assert_eq!(
            paths(&s).iter().map(|p| p.0).collect::<Vec<_>>(),
            vec!["a.txt", "new.md"]
        );

        let s = repo_stats(&root, Mode::Staged).unwrap();
        assert_eq!(
            paths(&s).iter().map(|p| p.0).collect::<Vec<_>>(),
            vec!["b.txt"]
        );

        let s = repo_stats(&root, Mode::Branch).unwrap();
        assert!(s.base.starts_with("main@"), "{}", s.base);
        assert_eq!(
            paths(&s).iter().map(|p| p.0).collect::<Vec<_>>(),
            vec!["a.txt", "b.txt", "c.txt", "new.md"]
        );
    }

    #[test]
    fn file_diffs() {
        let tmp = setup();
        let root = repo_root(tmp.path()).unwrap();
        let s = repo_stats(&root, Mode::Uncommitted).unwrap();
        let a = file_diff(&root, Mode::Uncommitted, &s.files[0]).unwrap();
        assert!(a.contains("-two") && a.contains("+TWO"), "{a}");
        let n = file_diff(&root, Mode::Uncommitted, &s.files[2]).unwrap();
        assert!(n.contains("+x") && n.contains("+y"), "{n}");
    }

    #[test]
    fn repo_without_commits() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), &["init", "-q"]);
        fs::write(tmp.path().join("f"), "hi\n").unwrap();
        sh(tmp.path(), &["add", "f"]);
        let root = repo_root(tmp.path()).unwrap();
        let s = repo_stats(&root, Mode::Staged).unwrap();
        assert_eq!(paths(&s), vec![("f", 'A', Some(1), Some(0))]);
        assert!(!s.branch.is_empty());
    }
}
