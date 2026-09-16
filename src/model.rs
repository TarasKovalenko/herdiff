//! Join herdr panes with the git repos they work in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::git::RepoStats;
use crate::herdr::Snapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRef {
    pub pane_id: String,
    pub workspace: String,
    pub tab: String,
    pub agent: Option<String>,
    pub status: Option<String>,
    pub title: String,
    pub focused: bool,
}

impl PaneRef {
    pub fn is_agent(&self) -> bool {
        self.agent.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct RepoGroup {
    pub root: PathBuf,
    pub name: String,
    pub panes: Vec<PaneRef>,
    pub stats: Result<RepoStats, String>,
}

impl RepoGroup {
    /// Most urgent agent state in the group, for the repo badge.
    pub fn status(&self) -> Option<&str> {
        const RANK: [&str; 5] = ["blocked", "working", "done", "idle", "unknown"];
        self.panes
            .iter()
            .filter_map(|p| p.status.as_deref())
            .min_by_key(|s| RANK.iter().position(|r| r == s).unwrap_or(RANK.len()))
    }
}

/// Panes grouped by repo root, without git stats yet.
pub struct Topology {
    pub repos: Vec<(PathBuf, Vec<PaneRef>)>,
    pub non_repo_panes: usize,
}

/// Group panes by repo root. `resolve` maps a directory to its repo root (cached by caller).
pub fn group_panes(snap: &Snapshot, mut resolve: impl FnMut(&Path) -> Option<PathBuf>) -> Topology {
    let ws: HashMap<&str, &str> = snap
        .workspaces
        .iter()
        .map(|w| (w.workspace_id.as_str(), w.label.as_str()))
        .collect();
    let ws_order: HashMap<&str, u32> =
        snap.workspaces.iter().map(|w| (w.workspace_id.as_str(), w.number)).collect();
    let tabs: HashMap<&str, &str> =
        snap.tabs.iter().map(|t| (t.tab_id.as_str(), t.label.as_str())).collect();

    let mut panes: Vec<_> = snap.panes.iter().collect();
    panes.sort_by_key(|p| (ws_order.get(p.workspace_id.as_str()).copied().unwrap_or(u32::MAX), p.pane_id.clone()));

    let mut repos: Vec<(PathBuf, Vec<PaneRef>)> = Vec::new();
    let mut non_repo_panes = 0;
    for p in panes {
        let Some(root) = p.work_dir().and_then(|d| resolve(Path::new(d))) else {
            non_repo_panes += 1;
            continue;
        };
        let pref = PaneRef {
            pane_id: p.pane_id.clone(),
            workspace: ws.get(p.workspace_id.as_str()).unwrap_or(&"?").to_string(),
            tab: tabs.get(p.tab_id.as_str()).unwrap_or(&"").to_string(),
            agent: p.agent.clone(),
            status: p.agent_status.clone(),
            title: p.terminal_title_stripped.clone().unwrap_or_default(),
            focused: p.focused,
        };
        match repos.iter_mut().find(|(r, _)| *r == root) {
            Some((_, list)) => list.push(pref),
            None => repos.push((root, vec![pref])),
        }
    }
    Topology { repos, non_repo_panes }
}

pub fn repo_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn groups_by_repo_root_in_workspace_order() {
        let snap: Snapshot = serde_json::from_value(json!({
            "workspaces": [
                {"workspace_id": "w2", "label": "B", "number": 2},
                {"workspace_id": "w1", "label": "A", "number": 1}
            ],
            "tabs": [{"tab_id": "w1:t1", "label": "main"}],
            "panes": [
                {"pane_id": "w2:p1", "workspace_id": "w2", "cwd": "/repo2"},
                {"pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1", "cwd": "/repo1/src", "agent": "claude", "agent_status": "idle"},
                {"pane_id": "w1:p2", "workspace_id": "w1", "cwd": "/repo1", "agent": "codex", "agent_status": "working"},
                {"pane_id": "w1:p3", "workspace_id": "w1", "cwd": "/tmp"}
            ]
        }))
        .unwrap();
        let topo = group_panes(&snap, |d| {
            let s = d.to_str().unwrap();
            s.starts_with("/repo1").then(|| PathBuf::from("/repo1"))
                .or_else(|| s.starts_with("/repo2").then(|| PathBuf::from("/repo2")))
        });
        assert_eq!(topo.non_repo_panes, 1);
        let roots: Vec<_> = topo.repos.iter().map(|(r, p)| (r.to_str().unwrap(), p.len())).collect();
        assert_eq!(roots, vec![("/repo1", 2), ("/repo2", 1)]);
        assert_eq!(topo.repos[0].1[0].workspace, "A");
        assert_eq!(topo.repos[0].1[0].tab, "main");

        let g = RepoGroup {
            root: "/repo1".into(),
            name: "repo1".into(),
            panes: topo.repos[0].1.clone(),
            stats: Ok(Default::default()),
        };
        assert_eq!(g.status(), Some("working"));
    }
}
