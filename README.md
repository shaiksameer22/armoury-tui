# armoury-tui

A terminal replacement for the Windows **ASUS Armoury Crate**, for Linux ROG/TUF
laptops. Live hardware telemetry plus the controls that actually matter —
performance profiles, battery charge limit, keyboard backlight, Aura RGB and fan
curves — in a single cyberpunk TUI you drive with the **keyboard or the mouse**.

Written in **Rust** (`ratatui` + `tokio`), talking to the kernel and the
`asusd` daemon through native interfaces — sysfs, NVML and D-Bus — rather than
shelling out to CLIs and scraping their text. Built and verified on an **ASUS TUF
Gaming A15 (FA506NFR)**: AMD Ryzen (`k10temp`) + NVIDIA RTX 2050, Ubuntu 24.04,
kernel 6.17, `asusd` 6.3.1. It auto-detects hardware, so other ASUS laptops
(AMD/Intel CPU, NVIDIA/AMD GPU) degrade gracefully rather than break.

```
┌─ ACTIVE PROFILE ─┐   ┌─ CPU ──────────────┐   ┌─ GPU ──────────────┐
│   🚀 PERFORMANCE │   │ usage  5.0% ███──── │   │ usage 25% ████───── │
└──────────────────┘   │ temp 61°C  clock…  │   │ temp 59°C  power 7W │
                        │ cores ▁▃▂▅▁▂▁▁…    │   │ vram 475/4096 MB    │
                        └────────────────────┘   └────────────────────┘
```

---

## Install & run

The installer works on **any ASUS ROG/TUF laptop, any distro** — it builds a
standalone binary, puts it on your `PATH` as `armoury`, and adds a desktop entry.
It offers to install Rust (via rustup) if you don't have it, and checks for the
`asusd` daemon the control features need.

```bash
cd armoury-tui
./install.sh            # user install  → ~/.local/bin/armoury
./install.sh --system   # system-wide   → /usr/local/bin/armoury  (uses sudo)
./install.sh --uninstall

armoury                 # launch the TUI
armoury --probe         # print the discovered hardware map and exit
armoury --once          # print one text telemetry snapshot and exit
armoury --json          # one snapshot as JSON (status bars / scripting)
armoury -i 2            # 2-second refresh
armoury --log run.csv   # TUI + append one telemetry row per tick to a CSV
armoury --replay run.csv # summarise a logged CSV (min/avg/max + sparklines)
```

`make install` / `make uninstall` / `make run` / `make probe` wrap the above.

For development, run from source without installing: `cd rust && cargo run --
--probe`, `cargo test`, etc. The repo also ships an `./armoury` launcher that
builds-and-runs from the working tree (handy while hacking).

The tool **auto-detects hardware** and degrades gracefully — absent sensors or a
missing daemon show "n/a" / disabled controls rather than failing — so it runs on
AMD/Intel CPUs and NVIDIA/AMD GPUs across the ASUS ROG/TUF range, not just the
reference machine.

`--probe` / `--once` are headless — perfect for SSH/debugging, and they exercise
the whole data layer with zero TUI involvement. (`--probe` output is
byte-for-byte identical to the original Python tool's.)

For the **control** features you also need the asus-linux stack:

```bash
# https://asus-linux.org/  — provides asusd (daemon) + asusctl
systemctl status asusd          # the daemon must be running for any control to work
```

`nvidia-smi`/the NVIDIA driver (for NVML) enables full GPU telemetry; `upower`
gives the accurate battery wattage. Both are optional — the tool falls back to
sysfs and shows "n/a" for anything missing.

---

## Keys & mouse

Everything clickable is also a key, and vice-versa. **Click a tab or any
control chip** with the mouse, or:

| Key | Action | Key | Action |
|-----|--------|-----|--------|
| `1`–`5` | switch tab | `r` | force refresh |
| `t` | cycle colour theme | `q` / `Esc` | quit |

**Power / Fans tab**

| Key | Action | Key | Action |
|-----|--------|-----|--------|
| `p` | cycle performance profile | `[` `]` | charge limit −/＋5% |
| `s` | switch fan-curve profile | `c` `v` | curve cooler / quieter (±5%) |
| `e` `d` | enable / disable curve | `x` | restore firmware default curve |

**Lighting tab** — `+`/`-` keyboard brightness, `m` cycle Aura effect, `t` theme.

**Processes tab** — `c`/`m`/`p`/`n` sort by CPU/MEM/PID/NAME, `/` filter (type,
`Esc` to leave), `↑`/`↓` select, `k` kill (SIGTERM), `K` force-kill (SIGKILL).
Kills always go through a confirmation dialog.

---

## The tabs

**⬢ Dashboard** — at-a-glance KPI tiles (CPU/GPU temp & load, fan RPM, power),
system identity, the active profile, live CPU/GPU panels with meters + history
sparklines, a CPU/GPU thermal-trend area graph, and memory/fans/battery/storage.

**⚡ Power / Fans** — the controls. Pick a **profile** (live-highlighted), nudge
the **battery charge limit**, and edit **fan curves**: pick a profile to load its
CPU & GPU `temp → %` curves, then *Cooler/Quieter* to bias every point (clamped
0–100 %, temps fixed), *Enable/Disable* the custom curve, or *Default* to restore
firmware. A curve only drives the fans while its profile is the live one — the
panel tells you which is active. Below: live CPU/GPU fan-RPM trend graphs.

**✦ Lighting (Aura)** — keyboard backlight brightness and the Aura **effect**
(the tool reads the keyboard's supported modes from the daemon and cycles
through them). The panel shows the live brightness, current/supported modes and
the active colour theme. *(Per-effect colour editing is not yet ported — see
caveats.)*

**⇅ Network** — total download/upload throughput as live auto-scaled area
graphs, plus a per-NIC table: state, IPv4, ↓/↑ rate, cumulative RX/TX, and
error/drop counts. Virtual interfaces (docker/veth/…) are de-emphasised.

**☰ Processes** — an interactive table: sort by CPU/MEM/PID/NAME, filter as you
type, select a row (a detail card shows user, status, parent, start time and the
full command line), and kill the selection. PID 1 and the app's own
process/parent are refused; root-owned processes report a clear permission
error. A side panel lists GPU compute processes.

---

## Why it talks to a daemon instead of writing sysfs

Every interesting control node on a stock ASUS laptop is **root-owned**:

```
ro/needpriv  /sys/firmware/acpi/platform_profile
ro/needpriv  /sys/class/power_supply/BAT1/charge_control_end_threshold
ro/needpriv  /sys/class/leds/asus::kbd_backlight/brightness
```

So this tool runs **as your normal user, never as root**. Reads (sysfs, NVML,
UPower, `/proc`) never need privileges. All *state changes* are delegated to the
`asusd` system daemon over **D-Bus**, which performs the privileged write itself
after a polkit check. If `asusd` isn't reachable, the controls simply report as
disabled — nothing is forced.

Concretely, on the reference machine the daemon is **asusd 6.3.1** with the
modern property API (bus `xyz.ljones.Asusd`, not the older
`org.asuslinux.Daemon`). The Rust tool calls it directly via `zbus`:

| Control | D-Bus |
|---|---|
| performance profile | `xyz.ljones.Platform` → `PlatformProfile` (int enum) |
| battery charge limit | `xyz.ljones.Platform` → `ChargeControlEndThreshold` |
| keyboard brightness | `xyz.ljones.Aura` → `Brightness` |
| Aura effect | `xyz.ljones.Aura` → `LedMode` |
| fan curves | `xyz.ljones.FanCurves` → `FanCurveData` / `SetFanCurve` / … |

This is the main reason for the rewrite: the Python original shelled out to
`asusctl` and parsed its RON/CSV output with regexes — fragile across daemon
versions. The Rust tool reads typed values straight off the bus.

---

## How it works (the ideas worth knowing)

**1. Dynamic hwmon mapping (`scanner.rs`).** The kernel numbers `hwmonN` nodes in
*probe order*, so they reshuffle between boots — hardcoding `hwmon5` is a bug
waiting to happen. The scanner reads each node's `name` once at startup (`asus`,
`k10temp`, `nvme`, …) and resolves the real file paths. Run `--probe` to see the
live map.

**2. Native sources, no CLI scraping (`telemetry.rs`).** CPU/memory/processes
come from `sysinfo`; fans, temps, battery and network counters from sysfs; the
GPU from **NVML** (`nvml-wrapper`, not `nvidia-smi` text); battery wattage from
**UPower** over D-Bus. Every field is optional — absent hardware yields `None`
and the renderer shows "n/a" instead of crashing.

**3. Non-blocking render loop (`app.rs`).** A `tokio` event loop `select!`s over a
periodic tick and the terminal event stream. Each tick collects a `Snapshot`
off-thread (`spawn_blocking`), so a slow NVML/D-Bus/sysfs read never stutters the
UI. Control actions run off-thread too and surface their result as a toast.

**4. Mouse without a widget toolkit.** `ratatui` is immediate-mode and has no
clickable buttons, so controls are rendered as chips into known rectangles that
register click *zones* each frame; a left-click is hit-tested against them. The
same chips double as keyboard hints.

---

## Themes

Five colour themes ship in `theme.rs` — **Cyberpunk** (default), **Synthwave**,
**Matrix**, **Amber CRT** and **Ice**. Press `t` (or the 🎨 button on the
Lighting tab) to cycle; the whole UI re-colours live.

---

## Architecture

```
armoury-tui/
├── armoury                 # launcher: build --release + run, passthrough args
├── install.sh              # cross-distro installer (binary + desktop entry)
├── Makefile                # convenience targets (build/run/test/fmt/probe)
├── CHANGELOG.md            # keepachangelog-format history
├── CONTRIBUTING.md         # build, test, and PR guide
├── LICENSE                 # MIT
├── .editorconfig           # consistent formatting across editors
├── .github/                # CI workflow + issue/PR templates
└── rust/
    ├── Cargo.toml
    ├── tests/              # integration tests (CLI modes)
    └── src/
        ├── sysfs.rs        # total (never-panicking) sysfs/procfs read primitives
        ├── scanner.rs      # dynamic hardware discovery -> HardwareMap
        ├── telemetry.rs    # live collectors (sysinfo/sysfs/NVML/UPower) -> Snapshot
        ├── control.rs      # safe state controller over D-Bus to asusd
        ├── dbus.rs         # zbus proxies for asusd + UPower
        ├── config.rs       # user config (~/.config/armoury-tui/config.toml)
        ├── render.rs       # ratatui renderables: meters, sparklines, area charts, panels
        ├── app.rs          # the reactive render loop, tabs, input (keyboard + mouse)
        ├── theme.rs        # colour themes
        └── main.rs         # CLI: TUI / --probe / --once / --json / --replay
```

The dependency direction is strictly one-way (`sysfs → scanner → telemetry /
control → render → app`); nothing lower imports anything higher, which is why the
headless `--probe` / `--once` paths exercise the data layer with no TUI.

---

## Troubleshooting

- **Controls do nothing / "asusd unreachable":** ensure `asusd` is running
  (`systemctl status asusd`) and your user is allowed by its polkit policy. The
  Power/Lighting tabs show the backend status; `--probe` reports `asusd: true`
  when the daemon is detected.
- **Different asusd version:** the D-Bus interface names have drifted across
  releases. This tool targets the 6.x `xyz.ljones.*` API; on a much older daemon
  the control calls may not match. Reads are unaffected.
- **GPU shows n/a:** install the NVIDIA driver (provides `libnvidia-ml` for NVML)
  or, on AMD, ensure the `amdgpu` hwmon node exists.
- **Charge limit won't stick:** some firmwares only accept specific values; 80 %
  is the most reliable. Confirm the live value with `--probe`.

---

## Status & caveats

The data layer and TUI are verified end-to-end on the reference machine
(`--probe`, `--once`, and live frames all pass; `--probe` matches the Python
original byte-for-byte). Control commands are wired and their read paths verified
against the installed `asusd`, but **writes are not auto-fired during testing**
because they change real hardware state — try them live from the Power/Fans and
Lighting tabs.

Not yet ported from the original: per-Aura-effect **colour** editing (the effect
mode switches, but colour/speed/direction are left as the daemon has them), and
typing an **exact** `30c:10%,…` fan-curve string (the Cooler/Quieter/Enable/
Default nudges cover the common cases).

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, testing, code
style, and the PR workflow. Bug reports, hardware compatibility reports (`armoury
--probe` output), and feature requests are all welcome — GitHub issue templates
are provided.

---

## Python original (legacy)

The repository also contains the **original Python implementation** under
`armoury_tui/` (Textual + psutil), which this Rust version was ported from. It
still runs (`python3 -m armoury_tui`) but is **no longer maintained** — the Rust
build is the one `armoury` launches and where all development continues.
