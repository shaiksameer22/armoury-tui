# armoury-tui

A terminal replacement for the Windows **ASUS Armoury Crate**, for Linux ROG/TUF
laptops. Live hardware telemetry plus the controls that actually matter —
performance profiles, battery charge limit, keyboard backlight and Aura RGB —
in a single cyberpunk TUI.

Built and verified on an **ASUS TUF Gaming A15 (FA506NFR)**: AMD Ryzen
(`k10temp`) + NVIDIA RTX 2050, Ubuntu 24.04, kernel 6.17, with the
`asusd`/`asusctl` stack installed. It auto-detects hardware, so other ASUS
laptops (AMD/Intel CPU, NVIDIA/AMD GPU) degrade gracefully rather than break.

```
┌─ ACTIVE PROFILE ─┐   ┌─ CPU ──────────────┐   ┌─ GPU ──────────────┐
│   🚀 PERFORMANCE │   │ usage  5.0% ███──── │   │ usage 25% ████───── │
└──────────────────┘   │ temp 61°C  clock…  │   │ temp 59°C  power 7W │
                        │ cores ▁▃▂▅▁▂▁▁…    │   │ vram 475/4096 MB    │
                        └────────────────────┘   └────────────────────┘
```

---

## Why it talks to a daemon instead of writing sysfs

Every interesting control node on a stock ASUS laptop is **root-owned**:

```
ro/needpriv  /sys/firmware/acpi/platform_profile
ro/needpriv  /sys/class/power_supply/BAT1/charge_control_end_threshold
ro/needpriv  /sys/class/leds/asus::kbd_backlight/brightness
```

So this tool is designed to run **as your normal user, never as root**. All
state changes are delegated to the `asusd` system daemon through the `asusctl`
CLI, which performs the privileged write itself after a polkit check. If
`asusctl` is missing, it falls back to a `pkexec` (graphical polkit) sysfs
write, and finally to `sudo -n`. Reads (sysfs, `nvidia-smi`, `upower`, psutil)
never need privileges.

---

## Fan control (Power / Fans tab)

Two things live here beyond the profile buttons:

* **Trend graphs.** The CPU and GPU fans are drawn as live time-series area
  graphs — newest sample on the right, one column per tick over the last 60
  readings. Each column's height is the RPM as a fraction of full scale and is
  coloured by intensity (green → amber → red), so you read both the trend and
  the current level at a glance. The scale is fixed (`FAN_RPM_MAX`, ~6500) so
  the graph's vertical meaning stays constant from one moment to the next.

* **Fan-curve editor.** Pick a profile (Quiet / Balanced / Performance) to load
  its CPU & GPU `temp → fan%` curves. From there you can:
  - **Cooler +5% / Quieter −5%** — shift every duty-cycle point, clamped to
    0–100 %, temps untouched. The safe, one-button way to bias a curve.
  - **Enable / Disable** — toggle the custom curve vs. the firmware default.
  - **Default** — restore the firmware curve for that profile.
  - **Apply CPU / Apply GPU** — type an exact `30c:10%,55c:40%,…` string for
    full control; it's validated before being sent.

  All edits go through `asusctl fan-curve` (daemon + polkit), and a written
  curve is auto-enabled so it actually takes effect. A curve only drives the
  fans while its profile is the active one — the panel tells you which is live.

## Lighting (Aura) tab

Pick an **effect** (static, breathe, rainbow-cycle, rainbow-wave, highlight) and
it applies instantly with the current settings below it:

* **Colours** — a primary and a second colour (breathe uses both), with
  one-click neon **swatches** so you don't have to type hex.
* **Speed** (low/med/high) for breathe, rainbow and highlight.
* **Direction** (up/down/left/right) for rainbow-wave.
* **Zone** — all, or one of the 4 zones on the TUF keyboard.

Changing any knob re-applies the current effect, so the keyboard updates live.
Each mode sends exactly the flags `asusctl` requires (breathe needs two colours
*and* a speed; rainbow-wave needs a direction *and* speed; etc.) — getting that
wrong is why effects silently failed before.

## Network tab

* **Bandwidth graph** — total download (green) and upload (amber) throughput as
  live area graphs, auto-scaled to the recent peak (shown in the title).
* **Interfaces** — per-NIC state, IPv4, ↓/↑ rate, cumulative RX/TX, and
  error/drop counts. Virtual interfaces (docker/veth/vmnet/…) are de-emphasised.
* **Connections** — active TCP/UDP sockets (ESTABLISHED first, then listening)
  with local/remote address and the owning process. Sockets owned by other
  users show `—` for the process (mapping them would need root). Enumeration
  reads `/proc`, so it runs in a worker thread to keep the UI smooth.

## Processes tab

An interactive process table (`psutil`):

* **Sort** by CPU / MEM / PID / NAME (buttons). Sort by PID or NAME for a stable
  list while you navigate; CPU/MEM re-rank live.
* **Filter** by name or PID as you type.
* **Select** a row with the arrow keys; a detail card shows user, status, parent,
  threads, start time and full command line.
* **Kill** the selection with `k` (SIGTERM, graceful) or `K` (SIGKILL, force) —
  always behind a confirmation dialog. PID 1 and the app's own process/parent are
  refused, and root-owned processes report a clear permission error.

## Install

```bash
cd armoury-tui
python3 -m pip install -r requirements.txt      # textual, psutil, rich
#   – or –  pip install -e .                      # installs the `armoury-tui` command
```

For the **control** features you also want the asus-linux stack (already present
on the reference machine):

```bash
# https://asus-linux.org/  — provides asusd (daemon) + asusctl (CLI)
sudo apt install asusctl        # or your distro's package / build from source
systemctl status asusd          # the daemon must be running for controls
```

`nvidia-smi` (driver package) enables full GPU telemetry; `upower` gives the
accurate battery wattage. Both are optional — the tool falls back to sysfs.

---

## Run

```bash
python3 -m armoury_tui                  # launch the TUI
python3 -m armoury_tui -i 2             # 2-second refresh
python3 -m armoury_tui --log run.csv    # TUI + append telemetry to a CSV
python3 -m armoury_tui --probe          # print discovered hardware map, exit
python3 -m armoury_tui --once           # print one text telemetry snapshot, exit
```

`--probe` and `--once` are headless — perfect for SSH/debugging and for seeing
exactly what the data layer reads before the UI is involved.

### Keys

| Key | Action            | Key | Action               |
|-----|-------------------|-----|----------------------|
| `1` | Dashboard         | `r` | Force refresh        |
| `2` | Power / Fans      | `q` | Quit                 |
| `3` | Lighting          | `k` | Kill selected proc   |
| `4` | Network           | `K` | Force-kill (SIGKILL) |
| `5` | Processes         |     |                      |

---

## Features → spec mapping

| Armoury Crate feature        | Here                                            | Source |
|------------------------------|-------------------------------------------------|--------|
| Silent/Balanced/Performance  | Power/Fans tab buttons (live-highlighted)       | `asusctl profile` / `platform_profile` |
| CPU telemetry                | per-core %, clock, `k10temp` temp, load         | `psutil` + hwmon |
| GPU telemetry                | util, VRAM, temp, power, clock                  | `nvidia-smi` (AMD sysfs fallback) |
| Fan RPM (CPU + GPU)          | live RPM trend graphs + sparkline history        | `asus` hwmon `fan*_input` |
| Fan-curve tuning             | view/edit per-profile temp→% curves, Cooler/Quieter nudges, enable/reset | `asusctl fan-curve` |
| Memory / storage             | RAM, swap, NVMe temp, disk usage                | `psutil` + `nvme` hwmon |
| Battery health               | %, charge/discharge W, charge limit             | `upower` + `BAT*` sysfs |
| Battery charge limit         | 60 / 80 / 100 % buttons                         | `asusctl battery limit` |
| Aura / RGB                   | 5 effects + colour swatches, 2nd colour, speed, direction, per-zone | `asusctl aura` / `asusctl leds` |
| Network                      | bandwidth trend graph + interface details (IP/MAC/MTU/totals/errors) + connections | `psutil` net APIs |
| Processes                    | sortable/filterable table, select + kill (SIGTERM/SIGKILL), detail card, GPU apps | `psutil` + `nvidia-smi` |
| Thermal alerts               | rising-edge toast when CPU≥90°C / GPU≥87°C      | telemetry thresholds |
| Telemetry logging            | `--log run.csv` → one row/tick for analysis     | CSV |
| System overview              | host, model, distro, kernel, uptime             | DMI + `/proc` |
| Dashboard glance             | headline KPI tiles + live CPU/GPU thermal-trend graph | aggregated telemetry |

---

## How it works (the two ideas worth knowing)

**1. Dynamic hwmon mapping (`scanner.py`).** The kernel numbers `hwmonN` nodes
in *probe order*, so they reshuffle between boots — hardcoding `hwmon5` is a
bug waiting to happen. Instead the scanner reads each node's `name` file once at
startup (`asus`, `k10temp`, `nvme`, …) and resolves the real file paths. Run
`--probe` to see the live map.

**2. Non-blocking render loop (`app.py`).** `Telemetry.snapshot()` shells out to
`nvidia-smi`/`upower` and can take tens of milliseconds, which would stutter the
UI if run on the event loop. So each tick runs it via `asyncio.to_thread`, with
a `_collecting` guard that drops a tick rather than queueing — that's what keeps
refreshes cheap and the CPU quiet.

## File structure

```
armoury-tui/
├── armoury_tui/
│   ├── sysfs.py       # total (never-raising) sysfs/procfs read primitives
│   ├── scanner.py     # Module 1: dynamic hardware discovery
│   ├── telemetry.py   # live collectors  -> immutable Snapshot
│   ├── control.py     # Module 2: safe state controller (asusctl + fallback)
│   ├── render.py      # Rich renderables: neon meters, sparklines, panels
│   ├── app.py         # Module 3: Textual app + reactive render loop
│   ├── styles.tcss    # cyberpunk theme
│   └── __main__.py    # CLI: TUI / --probe / --once
├── requirements.txt
├── pyproject.toml
└── README.md
```

## Troubleshooting

- **Controls do nothing / "asusctl rc=…":** ensure `asusd` is running
  (`systemctl status asusd`) and your user is allowed by its polkit policy.
- **Charge limit won't stick:** some firmwares only accept specific values;
  `asusctl battery limit 80` is the most reliable. Confirm with `--probe`.
- **RGB colour ignored:** on 4-zone TUF keyboards the colour applies to
  `static`/`breathe`/`highlight`; `rainbow-cycle`/`rainbow-wave` are fixed.
- **GPU shows n/a:** install the NVIDIA driver (for `nvidia-smi`) or, on AMD,
  ensure the `amdgpu` hwmon node exists.

## Status / caveats

The data layer and the TUI render loop are verified end-to-end on the reference
machine (`--probe`, `--once`, and a headless Textual pilot all pass). Control
commands are wired and validated against the installed `asusctl` syntax, but are
**not** auto-fired during testing because they change real hardware state — try
them live from the Power/Fans and Lighting tabs.
