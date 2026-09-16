## What this changes

<!-- What it does and why. Link the issue if there is one: Closes #123 -->

## How it was tested

<!-- Tests added, and anything you checked by hand. Say which herdr version if you ran it live. -->

## Checklist

- [ ] `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --all-targets` pass
- [ ] New behaviour has tests
- [ ] Refreshes stay read-only (`GIT_OPTIONAL_LOCKS=0`); any git write goes through `git::apply_op` and respects `--read-only`
- [ ] New keys are in the help screen and the README
