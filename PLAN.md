# herdiff design notes

## Goal

A Rust TUI that shows the code changes agents in herdr are making, live, grouped by the
git repo each pane works in.

## Data sources

**herdr socket API.** Lives at `~/.config/herdr/herdr.sock` and speaks newline-delimited
JSON (`{"id","method","params"}`).

- `session.snapshot` returns workspaces, tabs and panes (`cwd`, `agent`, `agent_status`, title).
- `events.subscribe` covers lifecycle events (`pane.created/closed/updated/moved`,
  `workspace.*`, `tab.*`, `worktree.*`) plus `pane.agent_status_changed` per pane. Any
  event schedules a re-snapshot and a git refresh.
- `pane.focus {pane_id}` jumps to an agent's pane.
- The socket path can be overridden with `--socket` or `HERDR_SOCKET_PATH`.

**git CLI.** Shelling out keeps user config and worktrees working and avoids a libgit2 build.

- `git rev-parse --show-toplevel` maps a pane's cwd to a repo root. Panes in the same repo share one entry.
- `git diff --name-status -z` and `--numstat -z` give the file list and counts.
  `git ls-files --others --exclude-standard` adds untracked files.
- `git diff <base> -- <file>` gives the patch. Untracked files use `git diff --no-index /dev/null <file>`.

## Scope

`w` cycles three scopes; `--scope` sets the starting one.

- **follow**: the workspace from the snapshot's `focused_workspace_id`. Focus is recorded
  from every snapshot whatever the scope (focus events in other scopes trigger a snapshot
  with no git work), so switching to follow starts from where you actually were. When
  `focused_pane_id` is herdiff's own pane (`HERDR_PANE_ID`), the previous workspace is
  kept, otherwise looking at the diff would switch the view to herdiff's own workspace.
  Before anything has been followed it falls back to herdiff's workspace. A followed
  workspace that closes is dropped. If a snapshot fails (herdr restarting, a timeout),
  the last target is kept rather than treating the empty result as "every workspace
  closed". Default inside herdr.
- **here**: herdiff's own workspace, from the snapshot entry for its pane, else
  `HERDR_WORKSPACE_ID`. Only offered inside herdr.
- **all**: no filter. Default outside herdr and for `list`.

`scope.rs` holds this as a pure resolver (snapshot + own pane + last followed workspace in,
target out), so the rules are unit tested without a live herdr. Filtering happens before
repo discovery, so out-of-scope panes cost no git calls. `-d` repos are added in every scope.

Focus events (`workspace.focused`, `tab.focused`, `pane.focused`) are debounced 150ms instead
of 300ms and ignored unless the scope is follow. Results carry the scope they were computed
for; a result for a scope you've already left is dropped. Selection (repo, file, scroll) is
saved per target workspace and restored when you come back.

Known gaps: herdr has one focus per server, so with several clients follow tracks whichever
moved last. A pane moved to another workspace gets a new ID its process never learns, so
herdiff stops recognising its own pane until restarted.

## Mouse

Mouse capture is on unless `--no-mouse`. The renderer records each panel's rectangle and
list scroll offset every frame (`HitMap`), and `App::on_mouse` hit-tests against that, so
mouse handling is plain state logic with unit tests. Repo list items are one row plus one
per pane, so clicks walk item heights from the list offset. Motion events are dropped in
the main loop before they cause a redraw. Capture is turned off before the editor starts
and in the panic hook, since ratatui's own hook doesn't know about it.

## Diff modes

1. **uncommitted**: working tree vs `HEAD`, staged, unstaged and untracked together. Default.
2. **unstaged**: working tree vs index.
3. **staged**: index vs `HEAD`.
4. **branch**: working tree vs merge-base with the default branch (`origin/HEAD`, `main`, `master`).

A repo with no commits diffs against the empty tree.

## Layout

```
┌ Repos ──────────────┐┌ Files ───────────┐┌ Diff: src/foo.rs ───────────────┐
│● MedInsight  +42 -3 ││ M src/foo.rs +4-1││ @@ -1,4 +1,7 @@                   │
│   ● claude  working ││ A new.rs    +38  ││  fn a() {                          │
│   ○ claude  idle    ││ ? notes.md       ││ +    b();                          │
│○ herdiff     clean  ││                  ││ -    c();                          │
└─────────────────────┘└──────────────────┘└───────────────────────────────────┘
 uncommitted  updated 2s ago                 m mode  a agent  e edit  ? help  q quit
```

The diff panel has two layouts. Unified shows one column with old and new line numbers.
Split shows old on the left and new on the right: context lines sit on both sides, and a
run of removed lines is paired row by row with the added lines that follow it, leaving a
blank filler where one side is shorter. `--view auto` picks split at 140+ columns; `s`
toggles. The scroll position is stored as a line index, not a row index, so switching
layouts keeps the same code on screen.

At 120 columns or wider, repos and files stack on the left with the diff on the right.
Narrower terminals put the lists on top and the diff underneath.

## Code layout

```
main.rs   CLI, terminal setup, event loop
herdr.rs  socket client, snapshot types, event subscription
git.rs    repo discovery, status and diff commands, output parsers
diff.rs   unified diff parser (typed lines with old/new numbers) and unified/split row layouts
highlight.rs syntect highlighting with bat's syntaxes and themes (two-face)
model.rs  groups in-scope snapshot panes into repos
scope.rs  all / follow / here resolution
worker.rs background git worker, herdr listener, ticker
app.rs    state and key handling
ui.rs     ratatui rendering
```

Git and socket calls never run on the UI thread. The worker merges queued jobs, so a burst
of events turns into one refresh, and the main loop debounces herdr events by 300ms.
Results tagged with an old mode get dropped. The selected repo and file are kept by path
across refreshes, so the cursor doesn't jump when files are added.

The herdr listener subscribes again whenever panes are created, closed or moved, which
keeps the per-pane status subscriptions in sync. If herdr isn't running it retries every 2s.

## Tests

- Unit tests for the numstat, name-status and unified diff parsers, and for grouping panes into repos.
- Integration tests build a temp repo with staged, unstaged, untracked and branch commits and check each mode.
- State tests cover selection surviving a refresh, stale results being ignored, and
  per-workspace selection and scroll being restored.
- Scope tests cover follow ignoring herdiff's own pane, following other panes in its
  workspace, dropping closed workspaces, and running outside herdr.
- Highlighting tests cover language detection, TOML via bat's extra syntaxes, and clipping
  highlighted spans by display width (wide CJK characters included).
- A render test draws the UI into ratatui's `TestBackend` at wide, narrow and tiny sizes.

## Not done yet

Word-level highlighting, staging or reverting hunks
from the viewer, and packaging as a herdr plugin.
