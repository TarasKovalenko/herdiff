//! Minimal client for the herdr socket API (newline-delimited JSON).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub number: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tab {
    pub tab_id: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pane {
    pub pane_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub focused: bool,
}

impl Pane {
    /// Directory the pane's foreground process works in.
    pub fn work_dir(&self) -> Option<&str> {
        [&self.foreground_cwd, &self.cwd]
            .into_iter()
            .filter_map(|d| d.as_deref())
            .find(|d| !d.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: Option<PathBuf>) -> Self {
        let socket = socket
            .or_else(|| std::env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from))
            .unwrap_or_else(default_socket);
        Self { socket }
    }

    fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.socket)
            .with_context(|| format!("connect to herdr socket {}", self.socket.display()))
    }

    /// One request/response round trip on a fresh connection.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        let mut stream = self.connect()?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let id = format!("herdiff:{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
        let req = json!({ "id": id, "method": method, "params": params });
        writeln!(stream, "{req}")?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                bail!("herdr closed connection without response to {method}");
            }
            let msg: Value = serde_json::from_str(line.trim())
                .with_context(|| format!("bad herdr response: {line}"))?;
            if msg.get("id").and_then(Value::as_str) != Some(id.as_str())
                && msg.get("error").is_none()
            {
                continue;
            }
            if let Some(err) = msg.get("error") {
                bail!("herdr {method} failed: {err}");
            }
            return msg
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow!("herdr {method}: response without result"));
        }
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        let result = self.request("session.snapshot", json!({}))?;
        let snap = result
            .get("snapshot")
            .cloned()
            .ok_or_else(|| anyhow!("session.snapshot: missing snapshot"))?;
        Ok(serde_json::from_value(snap)?)
    }

    pub fn focus_pane(&self, pane_id: &str) -> Result<()> {
        self.request("pane.focus", json!({ "pane_id": pane_id }))?;
        Ok(())
    }

    /// Subscribe to events and call `on_event` for every pushed message.
    /// Blocks until the connection closes or `on_event` returns false.
    pub fn subscribe(&self, pane_ids: &[String], mut on_event: impl FnMut(&Value) -> bool) -> Result<()> {
        const TOPOLOGY: &[&str] = &[
            "workspace.created",
            "workspace.closed",
            "workspace.renamed",
            "worktree.created",
            "worktree.opened",
            "worktree.removed",
            "tab.created",
            "tab.closed",
            "tab.renamed",
            "pane.created",
            "pane.closed",
            "pane.updated",
            "pane.moved",
            "pane.exited",
            "pane.agent_detected",
        ];
        let mut subs: Vec<Value> = TOPOLOGY.iter().map(|t| json!({ "type": t })).collect();
        subs.extend(
            pane_ids
                .iter()
                .map(|p| json!({ "type": "pane.agent_status_changed", "pane_id": p })),
        );
        let mut stream = self.connect()?;
        let req = json!({
            "id": format!("herdiff:sub:{}", NEXT_ID.fetch_add(1, Ordering::Relaxed)),
            "method": "events.subscribe",
            "params": { "subscriptions": subs },
        });
        writeln!(stream, "{req}")?;
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(err) = msg.get("error") {
                bail!("events.subscribe failed: {err}");
            }
            if msg.pointer("/result/type").and_then(Value::as_str) == Some("subscription_started") {
                continue;
            }
            if !on_event(&msg) {
                break;
            }
        }
        Ok(())
    }
}

fn default_socket() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("herdr").join("herdr.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_snapshot_subset() {
        let raw = json!({
            "workspaces": [{"workspace_id": "w1", "label": "proj", "number": 1, "extra": true}],
            "tabs": [{"tab_id": "w1:t1", "label": "1"}],
            "panes": [{
                "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
                "cwd": "/a", "foreground_cwd": "", "agent": "claude",
                "agent_status": "working", "focused": true
            }]
        });
        let snap: Snapshot = serde_json::from_value(raw).unwrap();
        assert_eq!(snap.panes[0].work_dir(), Some("/a"));
        assert_eq!(snap.workspaces[0].label, "proj");
    }
}
