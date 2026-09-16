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
use crate::herdr::Snapshot;
use crate::highlight::{Highlighter, Highlights};
use crate::model::{self, RepoGroup};
use crate::scope::{Resolver, Scope, SelfPane, Target};

pub enum Job {
    Refresh {
        mode: Mode,
        scope: Scope,
    },
    Diff {
        root: PathBuf,
        mode: Mode,
        file: FileChange,
    },
    /// Record herdr focus without touching git (focus moved while the scope isn't follow).
    ObserveFocus,
}

pub struct Refreshed {
    pub mode: Mode,
    pub target: Target,
    pub groups: Vec<RepoGroup>,
    pub non_repo_panes: usize,
    pub herdr_error: Option<String>,
}

pub struct DiffResult {
    pub root: PathBuf,
    pub mode: Mode,
    pub path: String,
    pub lines: Result<Vec<DiffLine>, String>,
    /// Parallel to `lines`; empty when highlighting is off or the diff failed.
    pub highlights: Highlights,
}

pub enum AppEvent {
    Refreshed(Refreshed),
    Diff(DiffResult),
    /// herdr reported a change. `focus` = only focus moved (matters for `Scope::Follow`).
    HerdrChanged {
        focus: bool,
    },
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
        self.map
            .insert(dir.to_path_buf(), (root.clone(), Instant::now()));
        root
    }
}

pub fn spawn_git_worker(
    client: Client,
    extra_dirs: Vec<PathBuf>,
    highlighter: Option<Highlighter>,
    me: SelfPane,
    jobs: Receiver<Job>,
    out: Sender<AppEvent>,
) {
    thread::spawn(move || {
        let mut roots = RootCache::default();
        let mut resolver = Resolver::default();
        while let Ok(first) = jobs.recv() {
            // Coalesce queued jobs: keep only the latest refresh and latest diff.
            let mut refresh = None;
            let mut diff = None;
            let mut observe = false;
            for job in std::iter::once(first).chain(jobs.try_iter()) {
                match job {
                    Job::Refresh { mode, scope } => refresh = Some((mode, scope)),
                    d @ Job::Diff { .. } => diff = Some(d),
                    Job::ObserveFocus => observe = true,
                }
            }
            // A refresh observes focus itself.
            if observe
                && refresh.is_none()
                && let Ok(snap) = client.snapshot()
            {
                resolver.observe(&snap, &me);
            }
            if let Some((mode, scope)) = refresh {
                let ctx = RefreshCtx {
                    client: &client,
                    extra_dirs: &extra_dirs,
                    me: &me,
                };
                let r = refresh_all(&ctx, &mut roots, &mut resolver, mode, scope);
                if out.send(AppEvent::Refreshed(r)).is_err() {
                    return;
                }
            }
            if let Some(Job::Diff { root, mode, file }) = diff {
                let lines = git::file_diff(&root, mode, &file)
                    .map(|t| diff::parse_unified(&t))
                    .map_err(|e| e.to_string());
                let highlights = match (&highlighter, &lines) {
                    (Some(h), Ok(l)) => h.highlight(&file.path, l),
                    _ => Vec::new(),
                };
                let res = DiffResult {
                    root,
                    mode,
                    path: file.path,
                    lines,
                    highlights,
                };
                if out.send(AppEvent::Diff(res)).is_err() {
                    return;
                }
            }
        }
    });
}

struct RefreshCtx<'a> {
    client: &'a Client,
    extra_dirs: &'a [PathBuf],
    me: &'a SelfPane,
}

fn refresh_all(
    ctx: &RefreshCtx,
    roots: &mut RootCache,
    resolver: &mut Resolver,
    mode: Mode,
    scope: Scope,
) -> Refreshed {
    let (snap, herdr_error) = match ctx.client.snapshot() {
        Ok(s) => (Some(s), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let target = resolver.resolve(scope, snap.as_ref(), ctx.me);
    let snap = snap.unwrap_or_else(Snapshot::default);
    let own_pane = ctx.me.pane_id.as_deref();
    let mut topo = model::group_panes(&snap, &target, own_pane, |d| roots.resolve(d));
    // Repos passed with -d are always shown, whatever the scope.
    for dir in ctx.extra_dirs {
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
                    RepoGroup {
                        name: model::repo_name(&root),
                        root,
                        panes,
                        stats,
                    }
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    Refreshed {
        mode,
        target,
        groups,
        non_repo_panes: topo.non_repo_panes,
        herdr_error,
    }
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
                // Match loosely: event envelopes differ between event kinds.
                let raw = msg.to_string();
                let focus = event_is_focus(&raw);
                if out.send(AppEvent::HerdrChanged { focus }).is_err() {
                    closed = true;
                    return false;
                }
                // Pane set changed: restart to refresh per-pane subscriptions.
                ![
                    "pane.created",
                    "pane.closed",
                    "pane.moved",
                    "workspace.closed",
                ]
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

fn event_is_focus(raw: &str) -> bool {
    ["workspace.focused", "tab.focused", "pane.focused"]
        .iter()
        .any(|k| raw.contains(k))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_focus_events() {
        assert!(event_is_focus(
            r#"{"event":"pane.focused","data":{"pane_id":"w1:p1"}}"#
        ));
        assert!(event_is_focus(r#"{"type":"workspace.focused"}"#));
        assert!(!event_is_focus(r#"{"event":"pane.agent_status_changed"}"#));
    }
}
