# Contributing to herdiff

Thanks for looking. Bug reports, small fixes and new features are all welcome.
This page covers what a change needs before it can be merged.

## Before you start

For a bug, open an issue with the herdiff and herdr versions, your OS and terminal, and
the steps that show it. For a feature, open an issue first and say what you're trying to
do. It's cheaper to agree on the shape before the code exists.

Security problems don't go in issues. See [SECURITY.md](SECURITY.md).

## Setting up

You need a recent stable Rust (the crate uses edition 2024), `git` on your `PATH`, and a
Unix-like OS. herdr talks over a Unix socket, so Windows isn't supported.

```sh
git clone https://github.com/TarasKovalenko/herdiff
cd herdiff
cargo run
```

herdr doesn't have to be running. Without it herdiff shows the socket error in the status
bar, and you can still point it at repos with `-d`:

```sh
cargo run -- -d .
```

## Checks

CI runs these on Linux and macOS, and a pull request needs all of them green:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The git tests build throwaway repos in a temp directory, so they never touch your own.

## Writing the change

- `git.rs` is the only module that runs git and `herdr.rs` the only one that talks to
  the socket. `app.rs` holds state and key handling, `ui.rs` only draws. Keep it that way.
- Git and socket calls stay off the UI thread. Add work to `worker.rs`.
- Never drop `GIT_OPTIONAL_LOCKS=0` or add a git command that writes. herdiff must not
  take `index.lock` while an agent is committing.
- New behaviour comes with tests. Rendering is tested against ratatui's `TestBackend`
  at several sizes, down to 40×12.
- A new key goes in the help screen (`ui.rs`) and the README key table.

## Commits and pull requests

Commit subjects follow [Conventional Commits](https://www.conventionalcommits.org):
`feat:`, `fix:`, `docs:`, `chore:`, `test:`. The body says why, not what the diff
already shows.

Keep a pull request to one change. Say what it does, how you tested it, and anything
you weren't sure about. The template will ask for this.

Don't bump the version in a pull request. That happens at release time.

## Code of conduct

Everyone taking part is expected to follow the [code of conduct](CODE_OF_CONDUCT.md).
