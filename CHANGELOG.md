# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] — 2026-06-09

### Added
- Full **Rust rewrite** using ratatui + tokio, replacing the Python/Textual original.
- Native data sources — NVML (GPU), UPower D-Bus (battery wattage), sysinfo (CPU/memory/processes) — no CLI scraping.
- Native D-Bus controls via zbus to asusd 6.x (`xyz.ljones.*` API) for:
  - Performance profile switching (Quiet / Balanced / Performance)
  - Battery charge limit adjustment
  - Keyboard backlight brightness
  - Aura LED effect mode cycling and colour editing
  - Fan curve editor (cooler/quieter/enable/disable/default)
- Five interactive TUI tabs:
  - **Dashboard** — KPI tiles, CPU/GPU panels with sparklines, thermal trend graphs, memory/fans/battery/storage.
  - **Power / Fans** — profile selection, charge limit, fan curve editor with live CPU/GPU fan RPM graphs.
  - **Lighting (Aura)** — brightness control, effect mode cycling, colour editing.
  - **Network** — live throughput area graphs, per-NIC table with rates/counters/errors, connections list.
  - **Processes** — sortable/filterable process table, detail card, kill with confirmation, GPU process list.
- Full mouse support — clickable chips, tab switching, scroll.
- Five colour themes: Cyberpunk (default), Synthwave, Matrix, Amber CRT, Ice.
- Headless CLI modes: `--probe`, `--once`, `--json`, `--replay`.
- CSV telemetry logging (`--log`) and replay summariser (`--replay`).
- User config at `~/.config/armoury-tui/config.toml` with:
  - Theme persistence and startup tab selection.
  - Alert thresholds (CPU/GPU temp, battery low, fan stall).
  - Power presets (named bundles of profile/charge/brightness).
  - Auto-rules (apply preset when battery drops below a level).
- Dynamic hwmon discovery — never hardcodes `hwmonN` paths.
- Graceful degradation — missing hardware/daemons show "n/a", never crash.
- Cross-distro installer (`install.sh`) with desktop entry and PATH check.
- `--version` flag.
- MIT license.

### Changed
- Replaced all `asusctl` shell-outs with typed D-Bus property reads/writes.
- Replaced `nvidia-smi` CSV parsing with native NVML via `nvml-wrapper`.
- Replaced `upower -i` regex with UPower D-Bus properties via `zbus`.
- Themes are now actually wired (the Python original defined but never applied them).

### Fixed
- CPU usage readings no longer report since-boot averages on first tick (sysinfo is primed on startup).
- Network rate calculations handle interface counter wraps correctly.

## [0.1.0] — 2026-05-xx (Python original)

### Added
- Initial Python implementation using Textual + psutil + Rich.
- Dashboard, Power, Lighting, Network, Processes tabs.
- Hardware scanner for dynamic hwmon discovery.
- Shell-out controls via `asusctl` CLI.
