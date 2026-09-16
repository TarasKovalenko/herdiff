//! Git access by shelling out to the `git` CLI.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};

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
    /// `git status --porcelain` X: index vs HEAD (`' '` = nothing staged, `'?'` = untracked).
    pub index: char,
    /// `git status --porcelain` Y: working tree vs index (`' '` = nothing unstaged).
    pub worktree: char,
}

impl FileChange {
    /// Untracked nested repos are listed by git as `dir/`.
    pub fn is_dir(&self) -> bool {
        self.path.ends_with('/')
    }

    pub fn has_staged(&self) -> bool {
        !matches!(self.index, ' ' | '?')
    }

    pub fn has_unstaged(&self) -> bool {
        self.worktree != ' '
    }
}

#[derive(Debug, Clone, Default)]
pub struct RepoStats {
    pub branch: String,
    /// Human description of the diff base (e.g. `HEAD`, `main@abc1234`).
    pub base: String,
    pub files: Vec<FileChange>,
    /// Files with staged changes, whatever the mode shows (what a commit would include).
    pub staged: usize,
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
    // Pin the output format against user config: hunk staging turns this diff back into a
    // patch, and `diff.noprefix`, `diff.mnemonicPrefix`, `diff.relative` or a textconv
    // driver would produce one that `git apply` can't use.
    let mut args: Vec<String> = [
        "diff",
        "--no-ext-diff",
        "--no-renames",
        "--no-textconv",
        "--no-relative",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]
    .map(String::from)
    .to_vec();
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
                    index: ' ',
                    worktree: ' ',
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
                    index: '?',
                    worktree: '?',
                },
            );
        }
    }

    // Stage state per file. Untracked files are already marked above.
    let status = run(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=no",
        ],
        &[0],
    )?;
    let mut staged = 0;
    for (x, y, path) in parse_porcelain(&status) {
        staged += usize::from(!matches!(x, ' ' | '?'));
        if let Some(f) = files.get_mut(&path) {
            f.index = x;
            f.worktree = y;
        }
    }

    Ok(RepoStats {
        branch: branch_name(root),
        base: base_label,
        files: files.into_values().collect(),
        staged,
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
                "--no-textconv",
                "--src-prefix=a/",
                "--dst-prefix=b/",
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

/// `status --porcelain=v1 -z --no-renames`: `XY path\0`
fn parse_porcelain(buf: &[u8]) -> Vec<(char, char, String)> {
    split_z(buf)
        .into_iter()
        .filter_map(|rec| {
            let mut chars = rec.chars();
            let (x, y) = (chars.next()?, chars.next()?);
            let path = rec.get(3..)?;
            Some((x, y, path.to_string()))
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

// ---------------------------------------------------------------------------------------
// Writes. Only ever run on an explicit key press, never by the refresh loop.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(Vec<String>),
    Unstage(Vec<String>),
    StageAll,
    UnstageAll,
    /// Stage hunk number `hunk` of the file's unstaged diff, or with `reverse` unstage it from
    /// the staged diff. `header` is the `@@` line the user saw; if the diff has changed
    /// since, the hunk isn't applied.
    ApplyHunk {
        path: String,
        hunk: usize,
        header: String,
        reverse: bool,
    },
    Commit {
        message: String,
    },
}

impl GitOp {
    pub fn describe(&self) -> String {
        let files = |p: &[String]| match p {
            [one] => one.clone(),
            many => format!("{} files", many.len()),
        };
        match self {
            GitOp::Stage(p) => format!("staged {}", files(p)),
            GitOp::Unstage(p) => format!("unstaged {}", files(p)),
            GitOp::StageAll => "staged all changes".into(),
            GitOp::UnstageAll => "unstaged everything".into(),
            GitOp::ApplyHunk { reverse: false, .. } => "staged hunk".into(),
            GitOp::ApplyHunk { reverse: true, .. } => "unstaged hunk".into(),
            GitOp::Commit { .. } => "committed".into(),
        }
    }
}

/// How long to keep retrying while another git process (usually an agent) holds the index lock.
const LOCK_RETRIES: u32 = 10;
const LOCK_WAIT: Duration = Duration::from_millis(150);

/// Run a git operation. Returns a one-line summary for the status bar (for a commit, git's
/// `[branch sha] subject` line).
pub fn apply_op(root: &Path, op: &GitOp) -> Result<String> {
    let head = has_head(root);
    match op {
        GitOp::Stage(paths) => {
            if let Some(dir) = paths.iter().find(|p| p.ends_with('/')) {
                bail!("{dir} is a nested git repository; stage it from inside that repo");
            }
            write(root, &with_paths(&["add", "-A", "--"], paths), None)?;
        }
        GitOp::Unstage(paths) if head => {
            write(
                root,
                &with_paths(&["restore", "--staged", "--"], paths),
                None,
            )?;
        }
        // No commits yet: there's no HEAD to restore from, so drop the paths from the index.
        // `-f` is needed for files edited after staging; with `--cached` it only touches the index.
        GitOp::Unstage(paths) => {
            write(
                root,
                &with_paths(&["rm", "-r", "-q", "-f", "--cached", "--"], paths),
                None,
            )?;
        }
        GitOp::StageAll => write(root, &["add", "-A"], None).map(drop)?,
        GitOp::UnstageAll if head => write(root, &["reset", "-q"], None).map(drop)?,
        GitOp::UnstageAll => write(
            root,
            &["rm", "-r", "-q", "-f", "--cached", "--ignore-unmatch", "."],
            None,
        )
        .map(drop)?,
        GitOp::ApplyHunk {
            path,
            hunk,
            header,
            reverse,
        } => {
            // Rebuild the patch from git's raw bytes, not the decoded text on screen: a
            // lossy UTF-8 round trip would put U+FFFD into the index.
            let mode = if *reverse {
                Mode::Staged
            } else {
                Mode::Unstaged
            };
            let (base, _) = base_rev(root, mode)?;
            let mut args = diff_args(mode, &base);
            args.extend(["--".to_string(), path.clone()]);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let raw = run(root, &refs, &[0])?;
            let (patch, found) = hunk_patch(&raw, *hunk)
                .with_context(|| format!("{path} has no hunk {} any more", hunk + 1))?;
            ensure!(
                normalize_header(&found) == normalize_header(header.as_bytes()),
                "{path} changed since the diff was shown; nothing was staged. Try again."
            );
            let mut args = vec!["apply", "--cached", "--whitespace=nowarn"];
            if *reverse {
                args.push("--reverse");
            }
            args.push("-");
            write(root, &args, Some(&patch))?;
        }
        GitOp::Commit { message } => {
            ensure!(!message.trim().is_empty(), "commit message is empty");
            let out = write(root, &["commit", "-F", "-"], Some(message.as_bytes()))?;
            let first = out.lines().next().unwrap_or("").trim();
            return Ok(if first.is_empty() {
                "committed".into()
            } else {
                first.to_string()
            });
        }
    }
    Ok(op.describe())
}

/// Header plus hunk `n` of a single-file diff, as raw bytes, and that hunk's `@@` line.
pub fn hunk_patch(diff: &[u8], n: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let lines: Vec<&[u8]> = diff.split_inclusive(|b| *b == b'\n').collect();
    let hunks: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with(b"@@"))
        .collect();
    let start = *hunks.get(n)?;
    let end = hunks.get(n + 1).copied().unwrap_or(lines.len());
    let header = &lines[..*hunks.first()?];
    if !header.iter().any(|l| l.starts_with(b"+++ ")) {
        return None;
    }
    let mut patch: Vec<u8> = header.concat();
    patch.extend(lines[start..end].concat());
    if !patch.ends_with(b"\n") {
        patch.push(b'\n');
    }
    let at = lines[start];
    let at = at.strip_suffix(b"\n").unwrap_or(at);
    Some((patch, at.to_vec()))
}

/// Compare `@@` lines the way the UI shows them (lossy UTF-8, tabs expanded, no `\r`).
fn normalize_header(h: &[u8]) -> String {
    let s = String::from_utf8_lossy(h);
    s.trim_end_matches(['\r', '\n']).replace('\t', "    ")
}

fn with_paths<'a>(args: &[&'a str], paths: &'a [String]) -> Vec<&'a str> {
    args.iter()
        .copied()
        .chain(paths.iter().map(String::as_str))
        .collect()
}

/// Run a writing git command, feeding `stdin` if given. Retries while `index.lock` is held
/// by someone else and never removes the lock itself.
fn write(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
    let mut attempt = 0;
    loop {
        let mut cmd = git(root);
        // Run without a controlling terminal. The TUI owns it: a signing passphrase prompt
        // or a hook reading the terminal would otherwise hang behind the UI. Now they fail
        // at once and the error suggests `C`, which commits in the real terminal.
        cmd.env_remove("GPG_TTY");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            // SAFETY: setsid is async-signal-safe, which is all pre_exec requires.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        cmd.args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn git {}", args.join(" ")))?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input)?;
        }
        let out = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if stderr.contains("index.lock") && attempt < LOCK_RETRIES {
            attempt += 1;
            std::thread::sleep(LOCK_WAIT);
            continue;
        }
        let detail = [stderr.trim(), stdout.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let lower = stderr.to_lowercase();
        let needs_terminal = [
            "tty",
            "passphrase",
            "pinentry",
            "gpg failed",
            "signing failed",
        ]
        .iter()
        .any(|k| lower.contains(k));
        let hint = if stderr.contains("index.lock") {
            "\n\nAnother git process (maybe an agent) is holding the index lock. Try again in a moment."
        } else if needs_terminal {
            "\n\nThis needs the terminal (a passphrase prompt or an interactive hook). Press C to run git commit in the terminal instead."
        } else {
            ""
        };
        bail!("git {} failed:\n{detail}{hint}", args.join(" "));
    }
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

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn state(root: &Path, path: &str) -> (char, char) {
        let s = repo_stats(root, Mode::Uncommitted).unwrap();
        let f = s.files.iter().find(|f| f.path == path).unwrap();
        (f.index, f.worktree)
    }

    #[test]
    fn stage_state_and_file_ops() {
        let tmp = setup();
        let root = repo_root(tmp.path()).unwrap();
        assert_eq!(state(&root, "a.txt"), (' ', 'M'));
        assert_eq!(state(&root, "b.txt"), ('M', ' '));
        assert_eq!(state(&root, "new.md"), ('?', '?'));
        assert_eq!(repo_stats(&root, Mode::Uncommitted).unwrap().staged, 1);

        apply_op(&root, &GitOp::Stage(vec!["a.txt".into(), "new.md".into()])).unwrap();
        assert_eq!(state(&root, "a.txt"), ('M', ' '));
        assert_eq!(state(&root, "new.md"), ('A', ' '));
        assert_eq!(repo_stats(&root, Mode::Uncommitted).unwrap().staged, 3);

        apply_op(&root, &GitOp::Unstage(vec!["b.txt".into()])).unwrap();
        assert_eq!(state(&root, "b.txt"), (' ', 'M'));

        apply_op(&root, &GitOp::UnstageAll).unwrap();
        assert_eq!(repo_stats(&root, Mode::Uncommitted).unwrap().staged, 0);
        apply_op(&root, &GitOp::StageAll).unwrap();
        assert_eq!(repo_stats(&root, Mode::Uncommitted).unwrap().staged, 3);
    }

    /// What the UI sends for `space` on the hunk at diff line index `at`.
    fn hunk_op(root: &Path, path: &str, mode: Mode, at: usize) -> GitOp {
        let file = repo_stats(root, mode)
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == path)
            .unwrap();
        let lines = crate::diff::parse_unified(&file_diff(root, mode, &file).unwrap());
        let start = crate::diff::hunk_start(&lines, at).unwrap();
        GitOp::ApplyHunk {
            path: path.into(),
            hunk: crate::diff::hunk_index(&lines, start),
            header: lines[start].text.clone(),
            reverse: mode == Mode::Staged,
        }
    }

    /// Repo with one committed file and two separate unstaged edits to it.
    fn two_hunk_repo(first: &[u8], second: &[u8]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        sh(d, &["init", "-q", "-b", "main"]);
        let mut body = Vec::new();
        for i in 1..=30 {
            body.extend(format!("line {i}\n").as_bytes());
        }
        fs::write(d.join("f.txt"), &body).unwrap();
        sh(d, &["add", "."]);
        sh(d, &["commit", "-q", "-m", "init"]);
        let text = String::from_utf8(body).unwrap();
        let mut edited = Vec::new();
        for line in text.split_inclusive('\n') {
            match line {
                "line 2\n" => edited.extend(first),
                "line 28\n" => edited.extend(second),
                other => edited.extend(other.as_bytes()),
            }
        }
        fs::write(d.join("f.txt"), edited).unwrap();
        tmp
    }

    #[test]
    fn stage_and_unstage_single_hunk() {
        let tmp = two_hunk_repo(b"line 2\tchanged\n", b"LINE 28\n");
        let d = tmp.path();
        let root = repo_root(d).unwrap();
        // Diff line 20 sits in the second hunk.
        apply_op(&root, &hunk_op(&root, "f.txt", Mode::Unstaged, 20)).unwrap();

        let cached = git_out(d, &["diff", "--cached"]);
        let unstaged = git_out(d, &["diff"]);
        assert!(
            cached.contains("+LINE 28") && !cached.contains("changed"),
            "{cached}"
        );
        assert!(
            unstaged.contains("+line 2\tchanged") && !unstaged.contains("LINE 28"),
            "{unstaged}"
        );
        assert_eq!(state(&root, "f.txt"), ('M', 'M'));

        apply_op(&root, &hunk_op(&root, "f.txt", Mode::Staged, 0)).unwrap();
        assert_eq!(git_out(d, &["diff", "--cached"]), "");
    }

    #[test]
    fn hunk_staging_ignores_diff_config() {
        let tmp = two_hunk_repo(b"line 2 changed\n", b"LINE 28\n");
        let root = repo_root(tmp.path()).unwrap();
        for (k, v) in [("diff.noprefix", "true"), ("diff.mnemonicPrefix", "true")] {
            sh(&root, &["config", k, v]);
        }
        fs::create_dir(root.join("sub")).unwrap();
        sh(&root, &["config", "diff.relative", "true"]);
        apply_op(&root, &hunk_op(&root, "f.txt", Mode::Unstaged, 0)).unwrap();
        assert!(git_out(&root, &["diff", "--cached"]).contains("line 2 changed"));
    }

    #[test]
    fn hunk_staging_keeps_non_utf8_bytes() {
        let tmp = two_hunk_repo(b"caf\xe9 latin-1\n", b"LINE 28\n");
        let root = repo_root(tmp.path()).unwrap();
        apply_op(&root, &hunk_op(&root, "f.txt", Mode::Unstaged, 0)).unwrap();
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["show", ":f.txt"])
            .output()
            .unwrap()
            .stdout;
        assert!(
            out.windows(4).any(|w| w == b"caf\xe9"),
            "byte 0xE9 must reach the index"
        );
        assert!(!String::from_utf8_lossy(&out).contains("LINE 28"));
    }

    #[test]
    fn stale_hunk_header_is_refused() {
        let tmp = two_hunk_repo(b"line 2 changed\n", b"LINE 28\n");
        let root = repo_root(tmp.path()).unwrap();
        let op = hunk_op(&root, "f.txt", Mode::Unstaged, 0);
        // An agent edits the file between rendering and the key press.
        let body = fs::read_to_string(root.join("f.txt")).unwrap();
        fs::write(root.join("f.txt"), format!("new first line\n{body}")).unwrap();
        let err = apply_op(&root, &op).unwrap_err().to_string();
        assert!(err.contains("changed since the diff was shown"), "{err}");
        assert_eq!(git_out(&root, &["diff", "--cached"]), "");
    }

    #[test]
    fn hook_needing_a_terminal_fails_fast() {
        let tmp = setup();
        let root = repo_root(tmp.path()).unwrap();
        sh(&root, &["config", "user.name", "t"]);
        sh(&root, &["config", "user.email", "t@t"]);
        let hook = root.join(".git/hooks/pre-commit");
        fs::write(
            &hook,
            "#!/bin/sh\nread answer < /dev/tty || { echo 'no tty' >&2; exit 1; }\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let r = root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(apply_op(
                &r,
                &GitOp::Commit {
                    message: "x".into(),
                },
            ));
        });
        let err = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("commit hung waiting for a terminal")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Press C"), "{err}");
    }

    #[test]
    fn commit_uses_message_and_reports_summary() {
        let tmp = setup();
        let root = repo_root(tmp.path()).unwrap();
        sh(&root, &["config", "user.name", "t"]);
        sh(&root, &["config", "user.email", "t@t"]);
        sh(&root, &["config", "commit.gpgsign", "false"]);
        let summary = apply_op(
            &root,
            &GitOp::Commit {
                message: "Stage b\n\nBody line".into(),
            },
        )
        .unwrap();
        assert!(
            summary.starts_with("[feature ") && summary.ends_with("] Stage b"),
            "{summary}"
        );
        assert_eq!(
            git_out(&root, &["log", "-1", "--format=%B"]).trim(),
            "Stage b\n\nBody line"
        );
        assert_eq!(repo_stats(&root, Mode::Uncommitted).unwrap().staged, 0);

        let err = apply_op(
            &root,
            &GitOp::Commit {
                message: "nothing".into(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("git commit"), "{err}");
        assert!(
            apply_op(
                &root,
                &GitOp::Commit {
                    message: "  ".into()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn unstage_without_commits_and_nested_repo_guard() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), &["init", "-q"]);
        fs::write(tmp.path().join("f"), "hi\n").unwrap();
        let root = repo_root(tmp.path()).unwrap();
        apply_op(&root, &GitOp::StageAll).unwrap();
        assert_eq!(state(&root, "f"), ('A', ' '));
        apply_op(&root, &GitOp::Unstage(vec!["f".into()])).unwrap();
        assert_eq!(state(&root, "f"), ('?', '?'));
        apply_op(&root, &GitOp::StageAll).unwrap();
        apply_op(&root, &GitOp::UnstageAll).unwrap();
        assert_eq!(state(&root, "f"), ('?', '?'));

        // Staged, then edited again (`AM`): unstaging must still work without a HEAD.
        apply_op(&root, &GitOp::StageAll).unwrap();
        fs::write(tmp.path().join("f"), "hi again\n").unwrap();
        assert_eq!(state(&root, "f"), ('A', 'M'));
        apply_op(&root, &GitOp::Unstage(vec!["f".into()])).unwrap();
        assert_eq!(state(&root, "f"), ('?', '?'));
        apply_op(&root, &GitOp::StageAll).unwrap();
        fs::write(tmp.path().join("f"), "third\n").unwrap();
        apply_op(&root, &GitOp::UnstageAll).unwrap();
        // Nothing staged at all is fine too.
        apply_op(&root, &GitOp::UnstageAll).unwrap();

        let err = apply_op(&root, &GitOp::Stage(vec!["vendor/".into()])).unwrap_err();
        assert!(err.to_string().contains("nested git repository"), "{err}");
    }

    #[test]
    fn waits_for_a_released_index_lock_but_not_forever() {
        let tmp = setup();
        let root = repo_root(tmp.path()).unwrap();
        let lock = root.join(".git/index.lock");

        fs::write(&lock, "").unwrap();
        let release = {
            let lock = lock.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(400));
                fs::remove_file(lock).unwrap();
            })
        };
        apply_op(&root, &GitOp::Stage(vec!["a.txt".into()])).unwrap();
        release.join().unwrap();
        assert_eq!(state(&root, "a.txt"), ('M', ' '));

        fs::write(&lock, "").unwrap();
        let err = apply_op(&root, &GitOp::UnstageAll).unwrap_err().to_string();
        assert!(err.contains("holding the index lock"), "{err}");
        assert!(
            lock.exists(),
            "herdiff must never delete someone else's lock"
        );
    }
}
