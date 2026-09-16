# herdiff

A terminal diff viewer for [herdr](https://herdr.dev). It finds every git repo your herdr
panes are sitting in and shows what changed, grouped by repo, while your agents keep
editing. Leave it open in a side pane and you can watch the work land.

Diffs are syntax highlighted and switch to a side-by-side layout when there's room.

```
cargo install --git https://github.com/TarasKovalenko/herdiff
```

Or from a local checkout: `cargo install --path .`

## Usage

```
herdiff                   # uncommitted changes (the default)
herdiff -m branch         # changes since you branched off main/master
herdiff -d ~/src/other    # also show a repo that has no herdr pane
herdiff --view split      # always side-by-side (auto, unified, split)
herdiff --theme Nord      # pick a highlighting theme
herdiff list --json       # print once and exit, handy for scripts
```

It talks to herdr over its socket, so it runs in any terminal, not only inside herdr. The
usual setup is a split next to your agents:

```
herdr pane split --current --direction right --no-focus
```

Then start `herdiff` in the new pane.

`--view auto` (the default) goes side-by-side once the diff panel is 140 columns wide
and falls back to unified below that. Press `s` to override it for the session.

Highlighting uses the syntax definitions and themes that ship with
[bat](https://github.com/sharkdp/bat), so most languages and config formats work out of
the box. The default theme is Monokai Extended. `herdiff --list-themes` prints the rest;
`ansi` follows your terminal's palette, which is the one to try on a light background.
`--no-highlight` turns it off.

`--socket PATH` points it at a different herdr socket. By default it uses
`$HERDR_SOCKET_PATH`, then `~/.config/herdr/herdr.sock`. `--interval SECS` sets how often
it polls git (2s by default). herdr events trigger a refresh on top of that.

## Modes

Press `m` to cycle through them.

- **uncommitted**: working tree against `HEAD`, untracked files included
- **unstaged**: working tree against the index
- **staged**: index against `HEAD`
- **branch**: working tree against the merge-base with `origin/HEAD`, `main` or `master`

## Keys

| key | action |
| --- | --- |
| `tab` `l` `enter` / `shift-tab` `h` `esc` | next / previous panel |
| `j` `k` | move the selection, or scroll when the diff has focus |
| `[` `]` | previous / next file |
| `J` `K`, `space` `b`, `ctrl-d` `ctrl-u`, `g` `G` | scroll the diff |
| `n` `N` | next / previous hunk |
| `H` `L` | scroll sideways |
| `s` | toggle side-by-side / unified |
| `m` | switch mode |
| `r` | refresh now |
| `a` | jump to the repo's agent pane in herdr (press again for the next one) |
| `e` | open the file in `$VISUAL` or `$EDITOR` at the change |
| `?` | help |
| `q` | quit |

## How it works

herdiff asks herdr for a session snapshot, takes each pane's working directory and groups
panes by `git rev-parse --show-toplevel`. It subscribes to herdr events (panes and tabs
opening or closing, agent status changes) so the list stays current, and polls git in the
background to catch edits that happen between events.

Highlighting runs on the same background thread as git, so a large file never stalls
the UI. Old and new sides are highlighted as separate streams, which keeps a comment or
string opened in a removed line from bleeding into the added ones. Files over 10,000 diff
lines are shown plain.

All diffs come from the `git` CLI, so your git config and worktrees behave as usual. Every
git call runs with `GIT_OPTIONAL_LOCKS=0`. That way herdiff never grabs `index.lock` while
an agent is trying to commit.

Nested repos inside a repo (agent worktrees under `.claude/worktrees/`, for example) show
up as one entry marked `repo`. Open them with `-d` if you want their diffs.

The design notes are in [PLAN.md](PLAN.md).
