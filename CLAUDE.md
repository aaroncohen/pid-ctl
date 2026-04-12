# Claude Code Rules

## Git Hooks

Pre-commit hooks run `cargo fmt --check` and `cargo clippy -D warnings`. They must never be bypassed. Do not use `--no-verify`, `HUSKY_SKIP_HOOKS`, or any other mechanism to skip them. If a hook fails, fix the underlying issue and recommit.
