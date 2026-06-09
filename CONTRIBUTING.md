# Contributing to armoury-tui

Thanks for your interest in improving armoury-tui! This guide covers
everything you need to build, test, and submit changes.

## Prerequisites

- **Rust** stable toolchain (install via [rustup](https://rustup.rs))
- **Linux** (the tool reads `/sys`, `/proc`, and talks to D-Bus)
- **ASUS laptop** (optional — the tool degrades gracefully, and the test suite
  includes a mock sysfs harness)

Optional (for full feature testing):
- `asusd` daemon for control features (see [asus-linux.org](https://asus-linux.org))
- NVIDIA driver (for NVML / GPU telemetry)
- `upower` (for accurate battery wattage)

## Building

```bash
# Debug build (faster compile, slower binary)
cargo build --manifest-path rust/Cargo.toml

# Release build (slower compile, optimised binary)
cargo build --release --manifest-path rust/Cargo.toml

# Or use the Makefile
make build
```

## Running from source

```bash
# Launch the TUI (debug build)
make run

# Or directly:
cargo run --manifest-path rust/Cargo.toml

# Headless modes (great for testing without a terminal):
cargo run --manifest-path rust/Cargo.toml -- --probe
cargo run --manifest-path rust/Cargo.toml -- --once
cargo run --manifest-path rust/Cargo.toml -- --json
```

## Testing

```bash
# Run all tests
cargo test --manifest-path rust/Cargo.toml

# Run a specific test
cargo test --manifest-path rust/Cargo.toml -- test_name

# Run with output (for tests that print diagnostics)
cargo test --manifest-path rust/Cargo.toml -- --nocapture

# Or use the Makefile
make test
```

### Testing without ASUS hardware

The test suite includes a mock sysfs harness that creates fake `/sys/class/hwmon`
trees in temporary directories. Scanner and telemetry helper tests use these
mocks and run on any Linux machine (including CI servers).

D-Bus-dependent tests (e.g. `reads_from_asusd`) skip gracefully when the daemon
is absent.

## Code style

We use `rustfmt` and `clippy` with default settings:

```bash
# Format
cargo fmt --manifest-path rust/Cargo.toml

# Lint
cargo clippy --manifest-path rust/Cargo.toml -- -D warnings

# Or use the Makefile
make fmt
```

CI enforces both — please run them before pushing.

## Architecture

The codebase has a strict one-way dependency graph:

```
sysfs → scanner → telemetry / control → render → app
```

Lower modules never import higher ones. This is why `--probe` and `--once`
work without any TUI involvement.

When adding a new feature:
- **New data source** → add to `telemetry.rs`, extend `Snapshot`
- **New control** → add to `control.rs`, wire in `app.rs`
- **New widget** → add to `render.rs`, call from `app.rs`
- **New CLI flag** → add to `main.rs` (clap derive)

## Pull request workflow

1. Fork the repo and create a feature branch from `master`.
2. Make your changes with clear, focused commits.
3. Ensure `cargo test`, `cargo clippy`, and `cargo fmt --check` all pass.
4. Add or update tests for any new/changed behaviour.
5. Update `CHANGELOG.md` under an `[Unreleased]` section.
6. Open a PR with a clear description of what and why.

## Reporting hardware compatibility

If you have an ASUS ROG/TUF laptop, please run `armoury --probe` and share the
output in a GitHub issue (use the "Hardware Report" template). This helps us
build the compatibility matrix and catch edge cases on different models.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE).
