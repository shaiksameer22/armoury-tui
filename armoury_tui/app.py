"""Module 3 - the reactive Textual render loop.

Design goals, in priority order:

* **Never block the UI.** ``Telemetry.snapshot`` shells out to nvidia-smi /
  upower, so it runs in a thread (``asyncio.to_thread``) every tick. A
  ``_collecting`` guard drops a tick rather than queueing if collection ever
  runs long, so the loop can't pile up and spike CPU.
* **Never crash on a control action.** Buttons call the daemon-backed
  :class:`Controller` (also off-thread) and surface the result as a toast.
* **Degrade gracefully.** Panels show "n/a" for absent hardware.
"""

from __future__ import annotations

import asyncio
from collections import defaultdict, deque

from textual import on
from textual.app import App, ComposeResult
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import (
    Button,
    DataTable,
    Footer,
    Header,
    Input,
    Label,
    Static,
    TabbedContent,
    TabPane,
)

from . import render
from .control import (
    AURA_DIRECTIONS,
    AURA_MODES,
    AURA_SPEEDS,
    BRIGHTNESS_WORDS,
    Controller,
    FanCurve,
)
from .scanner import HardwareMap, scan
from .telemetry import Snapshot, Telemetry, kill_process, process_detail

HIST = 60          # samples kept for sparklines
CPU_TEMP_ALERT = 90.0   # °C — rising-edge notification thresholds
GPU_TEMP_ALERT = 87.0
CURVE_BIAS = 5          # %-points shifted per Cooler/Quieter nudge
PROC_ROWS = 40          # max rows shown in the process table
PROC_SORTS = ("cpu", "mem", "pid", "name")

# Quick-pick colour swatches for the Aura editor (name, rrggbb) — neon palette.
AURA_SWATCHES = (
    ("pink", "ff2e88"), ("cyan", "36f9f6"), ("green", "00ff9c"),
    ("amber", "f3d000"), ("red", "ff3355"), ("blue", "5ac8fa"),
    ("violet", "9d4edd"), ("white", "ffffff"),
)


class ConfirmKill(ModalScreen[bool]):
    """A small yes/no dialog so a process is never killed by a stray keypress."""

    def __init__(self, pid: int, name: str, force: bool) -> None:
        super().__init__()
        self.pid = pid
        self.pname = name
        self.force = force

    def compose(self) -> ComposeResult:
        sig = "SIGKILL — force, no cleanup" if self.force else "SIGTERM — graceful"
        with Vertical(id="dialog"):
            yield Label("⚠  KILL PROCESS", id="dlg_title")
            yield Label(f"{self.pname}  (PID {self.pid})", id="dlg_proc")
            yield Label(f"signal: {sig}", id="dlg_sig")
            with Horizontal(id="dlg_btns"):
                yield Button("Cancel  (Esc)", id="cancel", classes="ctl")
                yield Button("Kill  (Enter)", id="confirm", classes="ctl danger")

    @on(Button.Pressed)
    def _pressed(self, event: Button.Pressed) -> None:
        self.dismiss(event.button.id == "confirm")

    def on_key(self, event) -> None:
        if event.key == "escape":
            self.dismiss(False)
        elif event.key == "enter":
            self.dismiss(True)


class ArmouryApp(App):
    CSS_PATH = "styles.tcss"
    TITLE = "ARMOURY-TUI"
    SUB_TITLE = "ASUS control for Linux"

    BINDINGS = [
        ("q", "quit", "Quit"),
        ("1", "tab('dash')", "Dashboard"),
        ("2", "tab('power')", "Power/Fans"),
        ("3", "tab('light')", "Lighting"),
        ("4", "tab('net')", "Network"),
        ("5", "tab('proc')", "Processes"),
        ("r", "refresh_now", "Refresh"),
        ("k", "kill_proc(False)", "Kill"),
        ("K", "kill_proc(True)", "Force-kill"),
    ]

    def __init__(self, hw: HardwareMap, refresh: float = 1.0,
                 log_path: str | None = None) -> None:
        super().__init__()
        self.hw = hw
        self.telemetry = Telemetry(hw)
        self.control = Controller(hw)
        self.refresh_s = refresh
        self.log_path = log_path
        self._log_fh = None
        self._alert_state = {"cpu": False, "gpu": False}
        self._collecting = False
        self._latest: Snapshot | None = None
        # Serialises control actions so rapid button presses can't stack up
        # concurrent asusctl calls (which would fight over the daemon).
        self._ctl_lock = asyncio.Lock()
        # prefix -> intended button suffix while a write is in flight, so a
        # periodic tick reading stale hardware can't revert the highlight.
        self._pending: dict[str, str] = {}
        # Resolve profile names once, off the render path (calls asusctl).
        self._profiles = self.control.list_profiles()
        # Fan-curve editor state. Curves change only when we write them, so we
        # cache the selected profile's curves rather than re-reading each tick.
        self._curve_profile = "Performance"
        self._curves: list[FanCurve] = []
        # Aura editor state. asusctl can't read the active effect back, so we
        # hold the current selection here and re-apply it whenever any knob
        # (mode / speed / direction / zone / colour) changes.
        self._aura_mode = "static"
        self._aura_speed = "med"
        self._aura_direction = "right"
        self._aura_zone: str | None = None      # None = all zones
        # Process-manager state.
        self._proc_sort = "cpu"
        self._proc_filter = ""
        self._selected_pid: int | None = None
        self._proc_names: dict[int, str] = {}   # pid -> name, for the kill dialog
        # rolling history for sparklines / trend graphs
        self._cpu_hist: deque[float] = deque(maxlen=HIST)
        self._gpu_hist: deque[float] = deque(maxlen=HIST)
        self._cputemp_hist: deque[float] = deque(maxlen=HIST)
        self._gputemp_hist: deque[float] = deque(maxlen=HIST)
        self._fan_hist: dict[str, deque[float]] = defaultdict(lambda: deque(maxlen=HIST))
        self._net_hist: dict[str, deque[float]] = defaultdict(lambda: deque(maxlen=HIST))
        self._net_down_hist: deque[float] = deque(maxlen=HIST)   # total ↓ across real ifaces
        self._net_up_hist: deque[float] = deque(maxlen=HIST)     # total ↑

    # -- layout -----------------------------------------------------------

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        with TabbedContent(initial="dash"):
            with TabPane("⬢ Dashboard", id="dash"):
                yield from self._compose_dashboard()
            with TabPane("⚡ Power / Fans", id="power"):
                yield from self._compose_power()
            with TabPane("✦ Lighting", id="light"):
                yield from self._compose_light()
            with TabPane("⇅ Network", id="net"):
                yield from self._compose_net()
            with TabPane("☰ Processes", id="proc"):
                yield from self._compose_proc()
        yield Footer()

    def _compose_dashboard(self) -> ComposeResult:
        with VerticalScroll():
            with Horizontal(classes="row"):
                yield Static(id="d_headline", classes="cell wide")
            with Horizontal(classes="row"):
                yield Static(id="d_system", classes="cell")
                yield Static(id="d_profile", classes="cell")
            with Horizontal(classes="row"):
                yield Static(id="d_cpu", classes="cell wide")
                yield Static(id="d_gpu", classes="cell wide")
            with Horizontal(classes="row"):
                yield Static(id="d_thermal", classes="cell wide")
            with Horizontal(classes="row"):
                yield Static(id="d_mem", classes="cell")
                yield Static(id="d_fans", classes="cell")
                yield Static(id="d_batt", classes="cell")
                yield Static(id="d_store", classes="cell")

    def _compose_power(self) -> ComposeResult:
        with VerticalScroll():
            yield Label("PERFORMANCE PROFILE", classes="section")
            with Horizontal(classes="btnrow"):
                for name in self._profiles:
                    yield Button(name, id=f"prof-{name}", classes="ctl")
            yield Label("BATTERY CHARGE LIMIT", classes="section")
            with Horizontal(classes="btnrow"):
                for lim in (60, 80, 100):
                    yield Button(f"{lim}%", id=f"lim-{lim}", classes="ctl")

            yield Label("FAN SPEED", classes="section")
            yield Static(id="p_fangraph", classes="cell wide")

            yield Label("FAN CURVE  (pick a profile to view / edit)",
                        classes="section")
            with Horizontal(classes="btnrow"):
                for name in self._profiles:
                    yield Button(name, id=f"curveprof-{name}", classes="ctl")
            yield Static(id="p_curve", classes="cell wide")
            with Horizontal(classes="btnrow"):
                yield Button("Cooler +5%", id="curveadj-cooler", classes="ctl")
                yield Button("Quieter −5%", id="curveadj-quieter", classes="ctl")
                yield Button("Enable", id="curveen-true", classes="ctl")
                yield Button("Disable", id="curveen-false", classes="ctl")
                yield Button("Default", id="curveadj-default", classes="ctl")
            with Horizontal(classes="btnrow"):
                yield Label("cpu:", classes="inlabel")
                yield Input(id="cdata-cpu", placeholder="30c:10%,55c:40%,…",
                            classes="curveinput")
                yield Button("Apply CPU", id="curveapply-cpu", classes="ctl")
            with Horizontal(classes="btnrow"):
                yield Label("gpu:", classes="inlabel")
                yield Input(id="cdata-gpu", placeholder="30c:10%,55c:40%,…",
                            classes="curveinput")
                yield Button("Apply GPU", id="curveapply-gpu", classes="ctl")

            with Horizontal(classes="row"):
                yield Static(id="p_cpu", classes="cell wide")
                yield Static(id="p_gpu", classes="cell wide")
            with Horizontal(classes="row"):
                yield Static(id="p_fans", classes="cell wide")
                yield Static(id="p_batt", classes="cell wide")

    def _compose_light(self) -> ComposeResult:
        with VerticalScroll():
            yield Label("KEYBOARD BRIGHTNESS", classes="section")
            with Horizontal(classes="btnrow"):
                maxb = self.hw.kbd_max_brightness or 3
                for lvl in range(maxb + 1):
                    word = BRIGHTNESS_WORDS.get(lvl, str(lvl))
                    yield Button(word.upper(), id=f"br-{lvl}", classes="ctl")

            yield Label("AURA EFFECT  (click to apply with the settings below)",
                        classes="section")
            with Horizontal(classes="btnrow"):
                for mode in AURA_MODES:
                    yield Button(mode, id=f"aura-{mode}", classes="ctl")

            yield Label("COLOUR  (static / breathe / highlight)", classes="section")
            with Horizontal(classes="btnrow"):
                yield Label("primary:", classes="inlabel")
                yield Input(value="ff2e88", id="aura-color",
                            placeholder="rrggbb", max_length=6, classes="hexinput")
                yield Label("second:", classes="inlabel")
                yield Input(value="36f9f6", id="aura-color2",
                            placeholder="rrggbb", max_length=6, classes="hexinput")
            with Horizontal(classes="btnrow"):
                for name, hexv in AURA_SWATCHES:
                    yield Button(name, id=f"aswatch-{hexv}", classes="ctl swatch")

            yield Label("SPEED  (breathe / rainbow / highlight)", classes="section")
            with Horizontal(classes="btnrow"):
                for sp in AURA_SPEEDS:
                    yield Button(sp, id=f"aspeed-{sp}", classes="ctl")

            yield Label("DIRECTION  (rainbow-wave)", classes="section")
            with Horizontal(classes="btnrow"):
                for d in AURA_DIRECTIONS:
                    yield Button(d, id=f"adir-{d}", classes="ctl")

            yield Label("ZONE  (4-zone keyboard)", classes="section")
            with Horizontal(classes="btnrow"):
                yield Button("all", id="azone-all", classes="ctl")
                for z in range(4):
                    yield Button(f"zone {z + 1}", id=f"azone-{z}", classes="ctl")

            with Horizontal(classes="btnrow"):
                yield Button("◈ RE-APPLY", id="aurago", classes="ctl")
            yield Static(id="l_state", classes="cell wide")

    def _compose_net(self) -> ComposeResult:
        with VerticalScroll():
            yield Static(id="n_graph", classes="cell wide")
            yield Static(id="n_table", classes="cell wide")
            yield Static(id="n_conn", classes="cell wide")

    def _compose_proc(self) -> ComposeResult:
        with Vertical():
            with Horizontal(classes="btnrow"):
                yield Label("filter:", classes="inlabel")
                yield Input(placeholder="name or pid", id="proc_filter",
                            classes="curveinput")
                yield Button("CPU", id="psort-cpu", classes="ctl")
                yield Button("MEM", id="psort-mem", classes="ctl")
                yield Button("PID", id="psort-pid", classes="ctl")
                yield Button("NAME", id="psort-name", classes="ctl")
            with Horizontal(classes="btnrow"):
                yield Button("⏻ Kill  (k)", id="pkill", classes="ctl")
                yield Button("✖ Force-kill  (K)", id="pkillf", classes="ctl danger")
                yield Button("↻ Refresh", id="prefresh", classes="ctl")
            yield DataTable(id="proc_table", classes="cell wide")
            with Horizontal(classes="row"):
                yield Static(id="pr_detail", classes="cell wide")
                yield Static(id="pr_gpu", classes="cell wide")

    # -- lifecycle --------------------------------------------------------

    def on_mount(self) -> None:
        if not (self.hw.has_asusctl or self.control._pkexec or self.control._sudo):
            self.notify("No asusctl/pkexec/sudo found — controls are read-only.",
                        severity="warning", timeout=8)
        self._open_log()
        # Configure the process table once.
        tbl = self.query_one("#proc_table", DataTable)
        tbl.cursor_type = "row"
        tbl.zebra_stripes = True
        tbl.add_columns("PID", "PROCESS", "CPU%", "MEM MB", "MEM%")
        self.set_interval(self.refresh_s, self._tick)
        self.call_after_refresh(self._tick)  # paint immediately
        self.call_after_refresh(self._init_aura_highlights)
        self.call_after_refresh(
            lambda: self._optimistic_active("psort-", self._proc_sort, pending=False))
        if self.hw.has_asusctl:
            self.run_worker(self._load_curves(), exclusive=False)

    def _init_aura_highlights(self) -> None:
        """Reflect the default Aura selection on the buttons at startup."""
        self._optimistic_active("aura-", self._aura_mode, pending=False)
        self._optimistic_active("aspeed-", self._aura_speed, pending=False)
        self._optimistic_active("adir-", self._aura_direction, pending=False)
        self._optimistic_active("azone-", "all", pending=False)

    def on_unmount(self) -> None:
        if self._log_fh:
            self._log_fh.close()
            self._log_fh = None

    async def _tick(self) -> None:
        if self._collecting:
            return
        self._collecting = True
        try:
            snap = await asyncio.to_thread(self.telemetry.snapshot)
        except Exception as exc:  # collection must never kill the loop
            self.notify(f"telemetry error: {exc}", severity="error", timeout=6)
            return
        finally:
            self._collecting = False
        self._latest = snap
        self._push_history(snap)
        self._render(snap)
        self._check_alerts(snap)
        self._write_log(snap)

    def _push_history(self, s: Snapshot) -> None:
        self._cpu_hist.append(s.cpu.overall)
        if s.cpu.temp_c is not None:
            self._cputemp_hist.append(s.cpu.temp_c)
        if s.gpu.present and s.gpu.util is not None:
            self._gpu_hist.append(s.gpu.util)
        if s.gpu.present and s.gpu.temp_c is not None:
            self._gputemp_hist.append(s.gpu.temp_c)
        for f in s.fans:
            self._fan_hist[f.label].append(float(f.rpm))
        tot_down = tot_up = 0.0
        for i in s.net:
            self._net_hist[i.name].append(i.down_bps + i.up_bps)
            if not i.is_virtual and i.is_up:
                tot_down += i.down_bps
                tot_up += i.up_bps
        self._net_down_hist.append(tot_down)
        self._net_up_hist.append(tot_up)

    # -- rendering --------------------------------------------------------

    def _set(self, wid: str, renderable) -> None:
        """Update a Static if it is currently mounted; ignore if absent."""
        try:
            self.query_one(f"#{wid}", Static).update(renderable)
        except Exception:
            pass

    def _render(self, s: Snapshot) -> None:
        fan_h = {k: list(v) for k, v in self._fan_hist.items()}
        net_h = {k: list(v) for k, v in self._net_hist.items()}
        cpu = render.cpu_panel(s, list(self._cpu_hist))
        gpu = render.gpu_panel(s, list(self._gpu_hist))
        fans = render.fan_panel(s, fan_h)
        batt = render.battery_panel(s)

        # Dashboard
        self._set("d_headline", render.headline_strip(s))
        self._set("d_thermal", render.thermal_graph_panel(
            s, list(self._cputemp_hist), list(self._gputemp_hist)))
        self._set("d_system", render.system_panel(self.hw, s))
        self._set("d_profile", render.profile_banner(s))
        self._set("d_cpu", cpu)
        self._set("d_gpu", gpu)
        self._set("d_mem", render.mem_panel(s))
        self._set("d_fans", fans)
        self._set("d_batt", batt)
        self._set("d_store", render.storage_panel(s))
        # Power / Fans
        self._set("p_fangraph", render.fan_graph_panel(s, fan_h))
        self._set("p_cpu", cpu)
        self._set("p_gpu", gpu)
        self._set("p_fans", fans)
        self._set("p_batt", batt)
        # Network
        self._set("n_table", render.net_table(s, net_h))
        self._set("n_graph", render.bandwidth_graph_panel(
            list(self._net_down_hist), list(self._net_up_hist),
            self._net_down_hist[-1] if self._net_down_hist else 0.0,
            self._net_up_hist[-1] if self._net_up_hist else 0.0))
        try:
            on_net = self.query_one(TabbedContent).active == "net"
        except Exception:
            on_net = False
        if on_net:   # connections enumeration reads /proc — keep it off the loop
            self.run_worker(self._refresh_connections(),
                            exclusive=True, group="conns")
        # Processes — only refresh the (interactive) table while it's on screen.
        self._set("pr_gpu", render.gpu_proc_panel(s))
        try:
            on_proc = self.query_one(TabbedContent).active == "proc"
        except Exception:
            on_proc = False
        if on_proc and not isinstance(self.screen, ConfirmKill):
            self._refresh_proc_table(s)
            self._set("pr_detail", render.proc_detail_panel(
                process_detail(self._selected_pid) if self._selected_pid else None))
        # Lighting state
        self._set("l_state", self._lighting_state(s))
        self._mark_active(s)

    async def _refresh_connections(self) -> None:
        conns = await asyncio.to_thread(self.telemetry.connections)
        self._set("n_conn", render.connections_panel(conns))

    def _lighting_state(self, s: Snapshot) -> object:
        from rich.panel import Panel
        from rich.text import Text
        b = s.kbd_brightness
        word = BRIGHTNESS_WORDS.get(b, "?") if b is not None else "n/a"
        try:
            c1 = self.query_one("#aura-color", Input).value.strip() or "—"
            c2 = self.query_one("#aura-color2", Input).value.strip() or "—"
        except Exception:
            c1 = c2 = "—"
        zone = "all" if self._aura_zone is None else f"zone {int(self._aura_zone) + 1}"

        body = Text()

        def row(key: str, val: str, vstyle: str = render.CYAN) -> None:
            body.append(f"{key:<20}", style=render.DIM)
            body.append(f"{val}\n", style=vstyle)

        row("keyboard backlight", f"{word} ({b})" if b is not None else "n/a")
        row("aura mode", self._aura_mode, render.MAGENTA)
        row("speed / direction", f"{self._aura_speed} / {self._aura_direction}")
        row("zone", zone)
        row("colours", f"#{c1}  +  #{c2}")
        row("backend", "asusd via asusctl" if self.hw.has_asusctl else "unavailable",
            render.NEON_GREEN if self.hw.has_asusctl else render.RED)
        return Panel(body, title="[b]LIGHTING STATE[/]", title_align="left",
                     border_style=render.MAGENTA, padding=(0, 1))

    def _mark_active(self, s: Snapshot) -> None:
        """Toggle the 'active' class on whichever control matches live state.

        A press still in flight (recorded in ``_pending``) overrides the
        snapshot value, so a periodic tick reading stale hardware can't flicker
        the highlight back to the old value before the write lands.
        """
        def activate(prefix: str, real: str | None) -> None:
            match = self._pending.get(prefix, real)
            for btn in self.query(Button):
                if btn.id and btn.id.startswith(prefix):
                    want = match is not None and btn.id == f"{prefix}{match}"
                    btn.set_class(want, "active")

        if s.profile or "prof-" in self._pending:
            activate("prof-", s.profile)
        if s.battery.charge_limit is not None or "lim-" in self._pending:
            limit = s.battery.charge_limit
            activate("lim-", str(limit) if limit is not None else None)
        if s.kbd_brightness is not None or "br-" in self._pending:
            kb = s.kbd_brightness
            activate("br-", str(kb) if kb is not None else None)
        activate("curveprof-", self._curve_profile)

    # -- actions / controls ----------------------------------------------

    def action_tab(self, tab_id: str) -> None:
        self.query_one(TabbedContent).active = tab_id

    async def action_refresh_now(self) -> None:
        await self._tick()

    # -- fan-curve editor -------------------------------------------------

    async def _load_curves(self) -> None:
        """Read the selected profile's curves (off-thread) and show them."""
        if not self.hw.has_asusctl:
            self._set("p_curve", render.fan_curve_panel([], self._curve_profile, False))
            return
        prof = self._curve_profile
        try:
            curves = await asyncio.to_thread(self.control.get_fan_curves, prof)
        except Exception as exc:
            self.notify(f"curve read failed: {exc}", severity="error", timeout=5)
            return
        self._curves = curves
        active = bool(self._latest and self._latest.profile == prof)
        self._set("p_curve", render.fan_curve_panel(curves, prof, active))
        # Pre-fill the editable data fields from the live curve.
        for c in curves:
            try:
                self.query_one(f"#cdata-{c.fan.lower()}", Input).value = c.data_str()
            except Exception:
                pass

    async def _adjust_curve(self, kind: str) -> None:
        prof = self._curve_profile
        if kind == "default":
            await self._do(self.control.reset_fan_curve, prof,
                           busy=f"resetting {prof} curve…")
            await self._load_curves()
            return
        if not self._curves:
            self.notify("no curve loaded to adjust", severity="warning")
            return
        delta = CURVE_BIAS if kind == "cooler" else -CURVE_BIAS
        verb = "cooler" if delta > 0 else "quieter"
        self.notify(f"→ {prof} curves {verb} ({delta:+d}%)", timeout=2)
        ok = True
        async with self._ctl_lock:
            for c in self._curves:
                # Shift each point's duty cycle, clamp 0-100, keep temps fixed.
                # enable=False here: we flip them all on once, below.
                temps = [t for t, _ in c.points]
                pcts = [max(0, min(100, p + delta)) for p in c.pwm_pcts()]
                data = ",".join(f"{t}c:{p}%" for t, p in zip(temps, pcts))
                res = await asyncio.to_thread(
                    self.control.set_fan_curve, prof, c.fan, data, False)
                if not res.ok:
                    self.notify(res.message, severity="error", timeout=5)
                    ok = False
                    break
            if ok:
                await asyncio.to_thread(self.control.set_curve_enabled, prof, True)
                self.notify(f"{prof} curves {verb}", timeout=4)
        await self._load_curves()

    # -- aura editor ------------------------------------------------------

    async def _apply_aura(self) -> None:
        """Apply the current Aura selection (mode + colours/speed/dir/zone)."""
        colour = self.query_one("#aura-color", Input).value.strip() or "ff2e88"
        colour2 = self.query_one("#aura-color2", Input).value.strip() or "36f9f6"
        await self._do(
            self.control.set_aura, self._aura_mode, colour, colour2,
            self._aura_speed, self._aura_direction, self._aura_zone,
            busy=f"aura → {self._aura_mode}",
        )

    # -- process manager --------------------------------------------------

    def _sorted_filtered(self, procs: list) -> list:
        """Filter by the search box and sort by the active column (top N)."""
        f = self._proc_filter.lower().strip()
        rows = [p for p in procs
                if not f or f in p.name.lower() or f == str(p.pid)]
        if self._proc_sort == "cpu":
            rows.sort(key=lambda p: p.cpu, reverse=True)
        elif self._proc_sort == "mem":
            rows.sort(key=lambda p: p.mem_mb, reverse=True)
        elif self._proc_sort == "pid":
            rows.sort(key=lambda p: p.pid)
        else:  # name
            rows.sort(key=lambda p: p.name.lower())
        return rows[:PROC_ROWS]

    def _refresh_proc_table(self, s: Snapshot) -> None:
        from rich.text import Text
        try:
            tbl = self.query_one("#proc_table", DataTable)
        except Exception:
            return
        rows = self._sorted_filtered(s.procs_all)
        self._proc_names = {p.pid: p.name for p in rows}
        sel_idx = None
        tbl.clear()
        for i, p in enumerate(rows):
            tbl.add_row(
                str(p.pid),
                p.name[:26],
                Text(f"{p.cpu:.1f}", style=render._grade(min(p.cpu, 100) / 100, True)),
                f"{p.mem_mb:.0f}",
                Text(f"{p.mem_pct:.1f}", style=render._grade(p.mem_pct / 100, True)),
                key=str(p.pid),
            )
            if p.pid == self._selected_pid:
                sel_idx = i
        if sel_idx is not None:
            try:
                tbl.move_cursor(row=sel_idx, animate=False)
            except Exception:
                pass
        elif rows and self._selected_pid is None:
            self._selected_pid = rows[0].pid

    @on(DataTable.RowHighlighted)
    def _proc_row_highlighted(self, event: DataTable.RowHighlighted) -> None:
        key = event.row_key.value if event.row_key else None
        if key is not None:
            try:
                self._selected_pid = int(key)
            except (TypeError, ValueError):
                pass

    @on(Input.Changed, "#proc_filter")
    def _proc_filter_changed(self, event: Input.Changed) -> None:
        self._proc_filter = event.value
        if self._latest is not None:
            self._refresh_proc_table(self._latest)

    async def action_kill_proc(self, force: bool = False) -> None:
        if self.query_one(TabbedContent).active != "proc":
            return
        if isinstance(self.focused, Input):   # don't fire while typing a filter
            return
        pid = self._selected_pid
        if pid is None:
            self.notify("no process selected", severity="warning")
            return
        name = self._proc_names.get(pid, str(pid))

        def after(confirmed: bool | None) -> None:
            if confirmed:
                self.run_worker(self._kill_worker(pid, force), exclusive=False)

        self.push_screen(ConfirmKill(pid, name, force), after)

    async def _kill_worker(self, pid: int, force: bool) -> None:
        ok, msg = await asyncio.to_thread(kill_process, pid, force)
        self.notify(msg, severity="information" if ok else "error", timeout=5)
        if ok:
            self._selected_pid = None
        self.run_worker(self._tick(), exclusive=False)

    async def _do(self, fn, *args, busy: str, pending: str | None = None) -> None:
        """Run a Controller method off-thread and toast the result.

        The slow daemon call is never on the render path: we toast immediately,
        serialise via ``_ctl_lock`` so presses can't stack, and let the
        optimistic highlight plus a non-blocking reconcile reflect the change.
        That's what keeps the button feeling instant even though asusctl can
        take a second or two — the old code awaited the call *and* a full
        telemetry snapshot before redrawing, which is what made it freeze.
        """
        self.notify(busy, timeout=2)
        async with self._ctl_lock:
            try:
                res = await asyncio.to_thread(fn, *args)
            except Exception as exc:
                if pending:
                    self._pending.pop(pending, None)
                self.notify(f"failed: {exc}", severity="error")
                return
        if pending:
            self._pending.pop(pending, None)
        self.notify(res.message,
                    severity="information" if res.ok else "error",
                    timeout=4)
        self.run_worker(self._tick(), exclusive=False)  # reconcile, non-blocking

    def _optimistic_active(self, prefix: str, match: str, *,
                           pending: bool = True) -> None:
        """Light up the pressed control at once, before the daemon replies.

        ``pending`` records the intent so a periodic :meth:`_mark_active` can't
        revert the highlight to stale hardware state mid-write. It's cleared
        when :meth:`_do` finishes (success or failure); the reconciling tick
        then shows the true state — which corrects the highlight if the write
        actually failed.
        """
        if pending:
            self._pending[prefix] = match
        for btn in self.query(Button):
            if btn.id and btn.id.startswith(prefix):
                btn.set_class(btn.id == f"{prefix}{match}", "active")

    @on(Button.Pressed)
    async def _on_button(self, event: Button.Pressed) -> None:
        bid = event.button.id or ""
        kind, _, rest = bid.partition("-")
        if kind == "prof":
            self._optimistic_active("prof-", rest)
            await self._do(self.control.set_profile, rest,
                           busy=f"→ {rest} profile", pending="prof-")
        elif kind == "lim":
            self._optimistic_active("lim-", rest)
            await self._do(self.control.set_charge_limit, int(rest),
                           busy=f"→ charge limit {rest}%", pending="lim-")
        elif kind == "br":
            self._optimistic_active("br-", rest)
            await self._do(self.control.set_brightness, int(rest),
                           busy="→ brightness", pending="br-")
        elif kind == "aura":
            self._aura_mode = rest
            self._optimistic_active("aura-", rest, pending=False)
            await self._apply_aura()
        elif kind == "aspeed":
            self._aura_speed = rest
            self._optimistic_active("aspeed-", rest, pending=False)
            await self._apply_aura()
        elif kind == "adir":
            self._aura_direction = rest
            self._optimistic_active("adir-", rest, pending=False)
            await self._apply_aura()
        elif kind == "azone":
            self._aura_zone = None if rest == "all" else rest
            self._optimistic_active("azone-", rest, pending=False)
            await self._apply_aura()
        elif kind == "aswatch":
            self.query_one("#aura-color", Input).value = rest
            await self._apply_aura()
        elif kind == "aurago":
            await self._apply_aura()
        elif kind == "curveprof":
            self._curve_profile = rest
            self._optimistic_active("curveprof-", rest, pending=False)
            await self._load_curves()
        elif kind == "curveadj":
            await self._adjust_curve(rest)
        elif kind == "curveen":
            await self._do(self.control.set_curve_enabled,
                           self._curve_profile, rest == "true",
                           busy=f"{'enabling' if rest == 'true' else 'disabling'} "
                                f"{self._curve_profile} curves…")
            await self._load_curves()
        elif kind == "curveapply":
            data = self.query_one(f"#cdata-{rest}", Input).value.strip()
            await self._do(self.control.set_fan_curve,
                           self._curve_profile, rest, data,
                           busy=f"applying {rest.upper()} curve…")
            await self._load_curves()
        elif kind == "psort":
            self._proc_sort = rest
            self._optimistic_active("psort-", rest, pending=False)
            if self._latest is not None:
                self._refresh_proc_table(self._latest)
        elif kind == "pkill":
            await self.action_kill_proc(False)
        elif kind == "pkillf":
            await self.action_kill_proc(True)
        elif kind == "prefresh":
            await self._tick()

    # -- temperature alerts ----------------------------------------------

    def _check_alerts(self, s: Snapshot) -> None:
        """Notify only on the rising edge (cool→hot), so we don't spam."""
        self._edge("cpu", s.cpu.temp_c, CPU_TEMP_ALERT, "CPU")
        self._edge("gpu", s.gpu.temp_c if s.gpu.present else None,
                   GPU_TEMP_ALERT, "GPU")

    def _edge(self, key: str, temp: float | None, limit: float, label: str) -> None:
        if temp is None:
            return
        hot = temp >= limit
        if hot and not self._alert_state[key]:
            self.notify(f"⚠ {label} hit {temp:.0f}°C (≥{limit:.0f}°C)",
                        severity="warning", title="THERMAL ALERT", timeout=6)
        self._alert_state[key] = hot

    # -- CSV logging ------------------------------------------------------

    _LOG_COLS = ("ts", "profile", "cpu_pct", "cpu_temp", "gpu_pct", "gpu_temp",
                 "gpu_power_w", "fan_cpu_rpm", "fan_gpu_rpm", "mem_pct",
                 "batt_pct", "batt_rate_w")

    def _open_log(self) -> None:
        if not self.log_path:
            return
        try:
            import os
            new = not os.path.exists(self.log_path) or os.path.getsize(self.log_path) == 0
            self._log_fh = open(self.log_path, "a", buffering=1, encoding="utf-8")
            if new:
                self._log_fh.write(",".join(self._LOG_COLS) + "\n")
            self.notify(f"logging telemetry → {self.log_path}", timeout=4)
        except OSError as exc:
            self._log_fh = None
            self.notify(f"could not open log: {exc}", severity="error", timeout=6)

    def _write_log(self, s: Snapshot) -> None:
        if not self._log_fh:
            return
        fans = {f.label: f.rpm for f in s.fans}

        def g(v, fmt="{:.1f}"):
            return fmt.format(v) if v is not None else ""

        row = [
            f"{s.ts:.0f}", s.profile or "", g(s.cpu.overall), g(s.cpu.temp_c, "{:.0f}"),
            g(s.gpu.util) if s.gpu.present else "", g(s.gpu.temp_c, "{:.0f}"),
            g(s.gpu.power_w), str(fans.get("cpu_fan", "")), str(fans.get("gpu_fan", "")),
            g(s.mem.percent), g(s.battery.percent, "{:.0f}"), g(s.battery.rate_w),
        ]
        try:
            self._log_fh.write(",".join(row) + "\n")
        except OSError:
            pass


def run(refresh: float = 1.0, log_path: str | None = None) -> None:
    hw = scan()
    ArmouryApp(hw, refresh=refresh, log_path=log_path).run()
