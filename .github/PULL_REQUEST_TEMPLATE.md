## What does this PR do?

A clear, concise description of the change.

## Why?

The motivation — link to an issue if one exists (e.g. `Fixes #42`).

## How to test

Steps to verify the change works:

1. `cargo test --manifest-path rust/Cargo.toml`
2. `cargo run --manifest-path rust/Cargo.toml -- --probe`
3. ...

## Checklist

- [ ] `cargo fmt --manifest-path rust/Cargo.toml` — code is formatted
- [ ] `cargo clippy --manifest-path rust/Cargo.toml -- -D warnings` — no warnings
- [ ] `cargo test --manifest-path rust/Cargo.toml` — all tests pass
- [ ] Tests added/updated for new behaviour
- [ ] `CHANGELOG.md` updated (if user-facing)
