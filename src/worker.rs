//! Background threads: git/herdr work off the UI thread, herdr event subscription, ticker.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::diff::{self, DiffLine};
use crate::git::{self, FileChange, Mode};
use crate::herdr::Client;
use crate::model::{self, RepoGroup};

pub enum Job {
    Refresh { mode: Mode },
    Diff { root: PathBuf, mode: Mode, file: FileChange },
}

pub struct Refreshed {
    pub mode: Mode,
    pub groups: Vec<RepoGroup>,
    pub non_repo_panes: usize,
    pub herdr_error: Option<String>,
}

pub struct DiffResult {
    pub root: PathBuf,
    pub mode: Mode,
    pub path: String,
    pub lines: Result<Vec<DiffLine>, String>,
}

pub enum AppEvent {
    Refreshed(Refreshed),
    Diff(DiffResult),
    /// herdr reported a topology or agent status change.
    HerdrChanged,
    Tick,
}

/// Cache of directory → repo root lookups (roots rarely change).
#[derive(Default)]
struct RootCache {
    map: HashMap<PathBuf, (Option<PathBuf>, Instant)>,
}

impl RootCache {
    const TTL: Duration = Duration::from_secs(30);

    fn resolve(&mut self, dir: &Path) -> Option<PathBuf> {
        if let Some((root, at)) = self.map.get(dir)
            && at.elapsed() < Self::TTL
        {
            return root.clone();
        }
        let root = git::repo_root(dir);
        self.map.insert(dir.to_path_buf(), (root.clone(), Instant::now()));
        root
    }
}

pub fn spawn_git_worker(client: Client, extra_dirs: Vec<PathBuf>, jobs: Receiver<Job>, out: Sender<AppEvent>) {
    thread::spawn(move || {
        let mut roots = RootCache::default();
        while let Ok(first) = jobs.recv() {
            // Coalesce queued jobs: keep only the latest refresh and latest diff.
            let mut refresh = None;
            let mut diff = None;
            for job in std::iter::once(first).chain(jobs.try_iter()) {
                match job {
                    Job::Refresh { mode } => refresh = Some(mode),
                    d @ Job::Diff { .. } => diff = Some(d),
                }
            }
            if let Some(mode) = refresh {
                let r = refresh_all(&client, &extra_dirs, &mut roots, mode);
                if out.send(AppEvent::Refreshed(r)).is_err() {
                    return;
                }
            }
            if let Some(Job::Diff { root, mode, file }) = diff {
                let lines = git::file_diff(&root, mode, &file)
                    .map(|t| diff::parse_unified(&t))
                    .map_err(|e| e.to_string());
                let res = DiffResult { root, mode, path: file.path, lines };
                if out.send(AppEvent::Diff(res)).is_err() {
                    return;
                }
            }
        }
    });
}

fn refresh_all(client: &Client, extra_dirs: &[PathBuf], roots: &mut RootCache, mode: Mode) -> Refreshed {
    let (snap, herdr_error) = match client.snapshot() {
        Ok(s) => (s, None),
        Err(e) => (Default::default(), Some(format!("{e:#}"))),
    };
    let mut topo = model::group_panes(&snap, |d| roots.resolve(d));
    for dir in extra_dirs {
        if let Some(root) = roots.resolve(dir)
            && !topo.repos.iter().any(|(r, _)| *r == root)
        {
            topo.repos.push((root, Vec::new()));
        }
    }

    let groups = thread::scope(|s| {
        let handles: Vec<_> = topo
            .repos
            .into_iter()
            .map(|(root, panes)| {
                s.spawn(move || {
                    let stats = git::repo_stats(&root, mode).map_err(|e| format!("{e:#}"));
                    RepoGroup { name: model::repo_name(&root), root, panes, stats }
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    Refreshed { mode, groups, non_repo_panes: topo.non_repo_panes, herdr_error }
}

/// Subscribe to herdr events; re-subscribes when panes change so per-pane
/// agent status subscriptions stay current. Retries while herdr is down.
pub fn spawn_herdr_listener(client: Client, out: Sender<AppEvent>) {
    thread::spawn(move || {
        loop {
            let pane_ids: Vec<String> = client
                .snapshot()
                .map(|s| s.panes.into_iter().map(|p| p.pane_id).collect())
                .unwrap_or_default();
            let mut closed = false;
            let res = client.subscribe(&pane_ids, |msg: &Value| {
                if out.send(AppEvent::HerdrChanged).is_err() {
                    closed = true;
                    return false;
                }
                // Pane set changed: restart to refresh per-pane subscriptions.
                // Match loosely: event envelopes differ between event kinds.
                let raw = msg.to_string();
                !["pane.created", "pane.closed", "pane.moved", "workspace.closed"]
                    .iter()
                    .any(|k| raw.contains(k))
            });
            if closed {
                return;
            }
            let pause = if res.is_err() { 2000 } else { 100 };
            thread::sleep(Duration::from_millis(pause));
        }
    });
}

pub fn spawn_ticker(every: Duration, out: Sender<AppEvent>) {
    thread::spawn(move || {
        loop {
            thread::sleep(every);
            if out.send(AppEvent::Tick).is_err() {
                return;
            }
        }
    });
}
