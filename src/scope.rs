//! Which herdr workspace the viewer shows: everything, the one you're looking at, or its own.

use crate::herdr::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every repo in every workspace.
    All,
    /// The workspace herdr focus was last on, ignoring focus on herdiff's own pane.
    Follow,
    /// The workspace herdiff itself runs in.
    Here,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Follow => "follow",
            Scope::Here => "here",
        }
    }

    /// Next scope for the `w` key. `Here` is skipped when herdiff isn't inside herdr.
    pub fn next(self, here_available: bool) -> Scope {
        match self {
            Scope::All => Scope::Follow,
            Scope::Follow if here_available => Scope::Here,
            Scope::Follow | Scope::Here => Scope::All,
        }
    }
}

/// herdiff's own location in herdr, from the environment herdr injects into every pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfPane {
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
}

impl SelfPane {
    pub fn from_env() -> Self {
        let var = |k| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            pane_id: var("HERDR_PANE_ID"),
            workspace_id: var("HERDR_WORKSPACE_ID"),
        }
    }

    pub fn inside_herdr(&self) -> bool {
        self.pane_id.is_some() || self.workspace_id.is_some()
    }

    /// Current workspace of our pane: from the snapshot when our pane ID is in it, else the
    /// env. A pane moved to another workspace gets a new ID that its process never learns,
    /// so after a move neither source is right; that case isn't handled.
    fn workspace(&self, snap: &Snapshot) -> Option<String> {
        self.pane_id
            .as_ref()
            .and_then(|id| snap.panes.iter().find(|p| &p.pane_id == id))
            .map(|p| p.workspace_id.clone())
            .or_else(|| self.workspace_id.clone())
    }
}

/// The resolved scope for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub scope: Scope,
    /// `None` = all workspaces.
    pub workspace_id: Option<String>,
    /// Workspace label for the status bar.
    pub label: Option<String>,
}

impl Target {
    /// Key for remembering per-scope selection.
    pub fn key(&self) -> String {
        match &self.workspace_id {
            Some(id) => format!("ws:{id}"),
            None => "all".into(),
        }
    }

    pub fn includes(&self, workspace_id: &str) -> bool {
        self.workspace_id
            .as_deref()
            .is_none_or(|w| w == workspace_id)
    }
}

/// Resolves scopes against snapshots, remembering what `Follow` last followed.
#[derive(Debug, Default)]
pub struct Resolver {
    followed: Option<String>,
    last: Option<Target>,
}

impl Resolver {
    /// Record where herdr focus is. Called for every snapshot whatever the scope, so
    /// switching to follow starts from the workspace you were actually in.
    pub fn observe(&mut self, snap: &Snapshot, me: &SelfPane) {
        let focus_is_me = me.pane_id.is_some() && snap.focused_pane_id == me.pane_id;
        if !focus_is_me && let Some(ws) = &snap.focused_workspace_id {
            self.followed = Some(ws.clone());
        }
        // Forget a workspace that has been closed.
        if let Some(ws) = &self.followed
            && !snap.workspaces.iter().any(|w| &w.workspace_id == ws)
        {
            self.followed = None;
        }
    }

    /// `snap` is `None` when herdr couldn't be reached. The last target for the scope is
    /// kept then, so a restart or a timeout doesn't throw away what follow was tracking.
    pub fn resolve(&mut self, scope: Scope, snap: Option<&Snapshot>, me: &SelfPane) -> Target {
        let Some(snap) = snap else {
            if let Some(last) = self.last.clone().filter(|t| t.scope == scope) {
                return last;
            }
            let workspace_id = match scope {
                Scope::All => None,
                Scope::Follow => self.followed.clone().or_else(|| me.workspace_id.clone()),
                Scope::Here => me.workspace_id.clone(),
            };
            let label = workspace_id.clone();
            return Target {
                scope,
                workspace_id,
                label,
            };
        };
        self.observe(snap, me);
        let workspace_id = match scope {
            Scope::All => None,
            Scope::Here => Some(me.workspace(snap).unwrap_or_default()),
            Scope::Follow => Some(
                self.followed
                    .clone()
                    .or_else(|| me.workspace(snap))
                    .or_else(|| snap.focused_workspace_id.clone())
                    .unwrap_or_default(),
            ),
        };
        let label = workspace_id.as_ref().map(|id| {
            snap.workspaces
                .iter()
                .find(|w| &w.workspace_id == id)
                .map(|w| w.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| {
                    if id.is_empty() {
                        "none".into()
                    } else {
                        id.clone()
                    }
                })
        });
        let target = Target {
            scope,
            workspace_id,
            label,
        };
        self.last = Some(target.clone());
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// w1 = project (agent in p1), w2 = viewer (herdiff in p1).
    fn snap(focused_ws: &str, focused_pane: &str) -> Snapshot {
        serde_json::from_value(json!({
            "focused_workspace_id": focused_ws,
            "focused_pane_id": focused_pane,
            "workspaces": [
                {"workspace_id": "w1", "label": "project", "number": 1},
                {"workspace_id": "w2", "label": "viewer", "number": 2}
            ],
            "panes": [
                {"pane_id": "w1:p1", "workspace_id": "w1"},
                {"pane_id": "w2:p1", "workspace_id": "w2"}
            ]
        }))
        .unwrap()
    }

    fn me() -> SelfPane {
        SelfPane {
            pane_id: Some("w2:p1".into()),
            workspace_id: Some("w2".into()),
        }
    }

    #[test]
    fn follow_ignores_focus_on_own_pane() {
        let mut r = Resolver::default();
        // Started while herdiff itself has focus: nothing followed yet, use own workspace.
        let t = r.resolve(Scope::Follow, Some(&snap("w2", "w2:p1")), &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w2"));

        // User jumps to the project.
        let t = r.resolve(Scope::Follow, Some(&snap("w1", "w1:p1")), &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w1"));
        assert_eq!(t.label.as_deref(), Some("project"));

        // Back to herdiff to read the diff: keep showing the project.
        let t = r.resolve(Scope::Follow, Some(&snap("w2", "w2:p1")), &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w1"));
    }

    #[test]
    fn follow_tracks_other_panes_in_own_workspace() {
        let mut r = Resolver::default();
        r.resolve(Scope::Follow, Some(&snap("w1", "w1:p1")), &me());
        // A different pane in herdiff's workspace is a real focus change.
        let t = r.resolve(Scope::Follow, Some(&snap("w2", "w2:p9")), &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w2"));
    }

    #[test]
    fn follow_drops_closed_workspace() {
        let mut r = Resolver::default();
        r.resolve(Scope::Follow, Some(&snap("w1", "w1:p1")), &me());
        let mut s = snap("w2", "w2:p1");
        s.workspaces.retain(|w| w.workspace_id != "w1");
        let t = r.resolve(Scope::Follow, Some(&s), &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w2"));
    }

    #[test]
    fn failed_snapshot_keeps_followed_workspace() {
        let mut r = Resolver::default();
        r.resolve(Scope::Follow, Some(&snap("w1", "w1:p1")), &me());
        // herdr restarts while herdiff has focus.
        let t = r.resolve(Scope::Follow, None, &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w1"));
        assert_eq!(t.label.as_deref(), Some("project"));
        let t = r.resolve(Scope::Follow, Some(&snap("w2", "w2:p1")), &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w1"));
    }

    #[test]
    fn focus_observed_in_other_scopes_seeds_follow() {
        let mut r = Resolver::default();
        r.resolve(Scope::All, Some(&snap("w1", "w1:p1")), &me());
        // Press `w` while herdiff has focus: follow starts from w1, not herdiff's own w2.
        let t = r.resolve(Scope::Follow, Some(&snap("w2", "w2:p1")), &me());
        assert_eq!(t.workspace_id.as_deref(), Some("w1"));
    }

    #[test]
    fn follow_outside_herdr_uses_focus() {
        let mut r = Resolver::default();
        let t = r.resolve(
            Scope::Follow,
            Some(&snap("w1", "w1:p1")),
            &SelfPane::default(),
        );
        assert_eq!(t.workspace_id.as_deref(), Some("w1"));
    }

    #[test]
    fn here_prefers_snapshot_over_env() {
        let mut r = Resolver::default();
        // Snapshot and env disagree: the snapshot is live, so it wins.
        let moved = SelfPane {
            pane_id: Some("w2:p1".into()),
            workspace_id: Some("w9".into()),
        };
        let t = r.resolve(Scope::Here, Some(&snap("w1", "w1:p1")), &moved);
        assert_eq!(t.workspace_id.as_deref(), Some("w2"));
        assert_eq!(t.key(), "ws:w2");
    }

    #[test]
    fn all_and_cycling() {
        let mut r = Resolver::default();
        let t = r.resolve(Scope::All, Some(&snap("w1", "w1:p1")), &me());
        assert_eq!(t.workspace_id, None);
        assert!(t.includes("anything"));
        assert_eq!(t.key(), "all");

        assert_eq!(Scope::All.next(true), Scope::Follow);
        assert_eq!(Scope::Follow.next(true), Scope::Here);
        assert_eq!(Scope::Here.next(true), Scope::All);
        assert_eq!(Scope::Follow.next(false), Scope::All);
    }
}
