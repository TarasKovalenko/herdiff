# herdiff

[![ci](https://github.com/TarasKovalenko/herdiff/actions/workflows/ci.yml/badge.svg)](https://github.com/TarasKovalenko/herdiff/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A terminal diff viewer for [herdr](https://herdr.dev). It finds every git repo your herdr
panes are sitting in and shows what changed, grouped by repo, while your agents keep
editing. Leave it open in a side pane and you can watch the work land.

Diffs are syntax highlighted and switch to a side-by-side layout when there's room.

```
cargo install --git https://github.com/TarasKovalenko/herdiff
```

Or from a local checkout: `cargo install --path .`

It needs `git` on your `PATH` and runs on macOS and Linux.

## Usage

```
herdiff                   # uncommitted changes (the default)
herdiff -m branch         # changes since you branched off main/master
herdiff --scope all       # every workspace instead of the one you're in
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

The mouse works too: scroll the panel under the pointer, click a repo or file to select it,
click the diff to focus it. Shift+wheel scrolls the diff sideways. While herdiff has the
mouse, your terminal's own click-and-drag selection won't work (most terminals let you
hold Shift or Option to get it back). `--no-mouse` leaves the mouse to the terminal.

`--socket PATH` points it at a different herdr socket. By default it uses
`$HERDR_SOCKET_PATH`, then `~/.config/herdr/herdr.sock`. `--interval SECS` sets how often
it polls git (2s by default). herdr events trigger a refresh on top of that.

## Scope

With several workspaces open, seeing every repo at once gets noisy. Press `w` to cycle
what herdiff shows:

- **follow**: the workspace you're working in. Jump to another workspace in herdr and the
  view switches with you. Focusing herdiff itself doesn't count, so you can move over to
  read a diff without it changing under you. This is the default inside herdr.
- **here**: the workspace herdiff runs in. Handy when you keep one herdiff split beside
  each agent.
- **all**: every repo in every workspace. The default outside herdr and for `herdiff list`.

herdiff remembers the file and scroll position for each workspace, so switching back puts
you where you left off. Only repos in scope are diffed, so a narrow scope also means less
git work. Repos added with `-d` show up in every scope.

herdr keeps one focus for the whole server, so with several clients attached (say, a
second one over SSH), follow tracks whichever client moved last. If you move herdiff's own
pane to another workspace, restart it: the pane gets a new ID that the running process
never sees.

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
| `w` | switch scope: follow, here, all |
| `m` | switch mode |
| `r` | refresh now |
| `a` | jump to the repo's agent pane in herdr (press again for the next one) |
| `e` | open the file in `$VISUAL` or `$EDITOR` at the change |
| mouse | wheel scrolls, click selects, shift+wheel scrolls sideways |
| `?` | help |
| `q` | quit |

## How it works

herdiff asks herdr for a session snapshot, keeps the panes in scope, and groups them by
`git rev-parse --show-toplevel`. It subscribes to herdr events (panes and tabs opening or
closing, focus moving, agent status changes) so the list stays current, and polls git in the
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

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) has the checks a
change needs, and security reports go through [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
