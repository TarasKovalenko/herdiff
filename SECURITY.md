# Security policy

## Supported versions

Security fixes go into the latest release. Older releases don't get backports.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting:
https://github.com/TarasKovalenko/herdiff/security/advisories/new

If that isn't available to you, open an issue asking for a private contact channel.
Leave out exploit details. Don't post sensitive reports in public issues.

In the private report, include the herdiff and herdr versions, your OS, a minimal
reproduction, and what you expected versus what happened.

## What herdiff does and doesn't do

herdiff runs locally with your user's permissions. It connects to the herdr socket you
point it at (by default `$HERDR_SOCKET_PATH` or `~/.config/herdr/herdr.sock`) and runs
`git` in the repos your herdr panes are working in.

- It only reads from git. Every call runs with `GIT_OPTIONAL_LOCKS=0`, so it doesn't
  write `index.lock` or refresh the index.
- The only herdr commands it sends are `session.snapshot`, `events.subscribe` and
  `pane.focus` (when you press `a`).
- `e` starts `$VISUAL` or `$EDITOR` on the selected file. Treat those variables with the
  same care as any shell configuration.
- Git runs with your git config. A repository's own config can point at external
  programs such as `core.fsmonitor`. Only open repos you trust, the same as running
  `git status` in them yourself.
- No network access and no telemetry.

Installed as a herdr plugin, `herdr plugin install` runs `cargo build --release --locked`
in herdr's managed checkout. That downloads the crates pinned in `Cargo.lock` from
crates.io and compiles them, build scripts included. The plugin's actions only call
`herdr plugin pane open` for herdiff's own panes. It has no startup hooks and no event
hooks, and it keeps no state in herdr's plugin config or state directories.
