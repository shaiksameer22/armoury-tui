"""Rich renderable builders for the cyberpunk dashboard.

Kept separate from the Textual wiring so the look-and-feel lives in one place.
Every function takes a plain :class:`Snapshot` (or its sub-records) and returns
a Rich renderable that a ``Static`` widget can display. We draw our own block
meters and sparklines rather than leaning on widget APIs, which keeps the
visuals identical regardless of the installed Textual version.
"""

from __future__ import annotations

from rich.align import Align
from rich.console import Group, RenderableType
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

from .scanner import HardwareMap
from .telemetry import Snapshot

# Fan-RPM full-scale for the speedometer needle. Kept fixed (not auto-scaled)
# so the needle's resting position is comparable from one moment to the next —
# a dial whose maximum jumps around is not a dial. ~6500 covers TUF A15 fans.
FAN_RPM_MAX = 6500

# -- neon palette -----------------------------------------------------------
NEON_GREEN = "#00ff9c"
CYAN = "#36f9f6"
MAGENTA = "#ff2e88"
AMBER = "#f3d000"
RED = "#ff3355"
DIM = "#4b5263"
TEXT = "#c9d1d9"
BLUE = "#5ac8fa"

_BLOCKS = " ▁▂▃▄▅▆▇█"
_BAR_FULL = "█"
_BAR_EMPTY = "─"


# -- small formatters -------------------------------------------------------

def human_bytes(n: float) -> str:
    for unit in ("B", "K", "M", "G", "T"):
        if abs(n) < 1024 or unit == "T":
            return f"{n:.0f}{unit}" if unit == "B" else f"{n:.1f}{unit}"
        n /= 1024
    return f"{n:.1f}T"


def fmt_rate(bps: float) -> str:
    """Bytes/second -> human string (KB/s, MB/s ...)."""
    n = bps
    for unit in ("B/s", "KB/s", "MB/s", "GB/s"):
        if abs(n) < 1024 or unit == "GB/s":
            return f"{n:5.1f} {unit}"
        n /= 1024
    return f"{n:.1f} GB/s"


def _grade(frac: float, hot: bool) -> str:
    """Pick a colour for a 0..1 fraction. ``hot`` = high is bad (temps/load)."""
    if not hot:
        return CYAN
    if frac >= 0.85:
        return RED
    if frac >= 0.6:
        return AMBER
    return NEON_GREEN


def meter(value: float | None, maxv: float, width: int = 18,
          hot: bool = True) -> Text:
    """A coloured horizontal bar: ``████████──────────``."""
    if value is None or maxv <= 0:
        return Text("  n/a", style=DIM)
    frac = max(0.0, min(1.0, value / maxv))
    filled = int(round(frac * width))
    colour = _grade(frac, hot)
    t = Text()
    t.append(_BAR_FULL * filled, style=colour)
    t.append(_BAR_EMPTY * (width - filled), style=DIM)
    return t


def sparkline(values: list[float], width: int = 24, hot: bool = False) -> Text:
    """Unicode-block sparkline of the most recent *width* samples."""
    if not values:
        return Text(_BLOCKS[0] * width, style=DIM)
    data = values[-width:]
    lo, hi = min(data), max(data)
    span = (hi - lo) or 1.0
    t = Text()
    for v in data:
        idx = int((v - lo) / span * (len(_BLOCKS) - 1))
        frac = (v - lo) / span
        t.append(_BLOCKS[idx], style=_grade(frac, hot))
    if len(data) < width:
        t.append(_BLOCKS[0] * (width - len(data)), style=DIM)
    return t


# Eighth-block ramp for the partial cell at the top of each chart column.
_COL_BLOCKS = " ▁▂▃▄▅▆▇█"
_CHART_W = 60     # columns of history shown (matches the app's HIST buffer)
_CHART_H = 7      # rows tall


def area_chart(values: list[float], maxv: float, width: int = _CHART_W,
               height: int = _CHART_H, hot: bool = True,
               colour: str | None = None) -> list[Text]:
    """A filled column/area graph of a time series, newest sample on the right.

    Each column's height is the value as a fraction of ``maxv``; full cells are
    solid blocks and the topmost cell uses an eighth-block for sub-row
    resolution. Columns are coloured by height (green → amber → red) unless a
    fixed ``colour`` is given, so the graph shows both the trend over time and
    the current intensity at a glance.
    """
    data = [float(v) for v in values][-width:]
    if maxv <= 0:
        maxv = 1.0
    grid: list[list[tuple[str, str]]] = [[(" ", DIM)] * width for _ in range(height)]
    offset = width - len(data)               # right-align: history grows leftward
    for i, v in enumerate(data):
        col = offset + i
        if col < 0:
            continue
        frac = max(0.0, min(1.0, v / maxv))
        filled = frac * height
        full = int(filled)
        rem = filled - full
        cell = colour or _grade(frac, hot)
        for r in range(height):
            level = height - 1 - r           # 0 = bottom row
            if level < full:
                grid[r][col] = ("█", cell)
            elif level == full and rem > 1e-6:
                grid[r][col] = (_COL_BLOCKS[int(rem * 8) or 1], cell)
    return [
        Text().join(Text(ch, style=st) for ch, st in row)
        for row in grid
    ]


def fan_graph_panel(snap: Snapshot, hist: dict[str, list[float]]) -> Panel:
    """CPU/GPU fan RPM as stacked time-series area graphs."""
    if not snap.fans:
        return _panel(Text("no ASUS fan channels", style=DIM), "FAN SPEED", BLUE)
    blocks: list[RenderableType] = []
    for n, f in enumerate(snap.fans):
        series = hist.get(f.label) or [float(f.rpm)]
        frac = min(1.0, f.rpm / FAN_RPM_MAX) if FAN_RPM_MAX else 0.0
        title = Text()
        title.append(f.label.replace("_", " ").upper(), style=CYAN)
        title.append(f"   {f.rpm} ", style=_grade(frac, True) if f.rpm else DIM)
        title.append("rpm", style=DIM)
        title.append(f"      scale 0–{FAN_RPM_MAX} rpm", style=DIM)
        chart = area_chart(series, FAN_RPM_MAX)
        axis = Text("└" + "─" * (_CHART_W - 1), style=DIM)
        if n:
            blocks.append(Text(""))
        blocks.append(Group(title, *chart, axis))
    return _panel(Group(*blocks), "FAN SPEED  (live trend)", BLUE)


def _tile(value: str, label: str, colour: str) -> Table:
    """One headline KPI: a big coloured value over a dim label."""
    g = Table.grid(expand=True)
    g.add_column(justify="center")
    g.add_row(Text(value, style=f"bold {colour}"))
    g.add_row(Text(label, style=DIM))
    return g


def headline_strip(snap: Snapshot) -> Panel:
    """A row of at-a-glance KPI tiles across the top of the dashboard."""
    c, g = snap.cpu, snap.gpu
    tiles: list[Table] = []

    ct = c.temp_c
    tiles.append(_tile(f"{ct:.0f}°C" if ct is not None else "—",
                       "CPU TEMP", _grade((ct or 0) / 95, True)))
    tiles.append(_tile(f"{c.overall:.0f}%", "CPU LOAD",
                       _grade(c.overall / 100, True)))
    if g.present:
        gt = g.temp_c
        tiles.append(_tile(f"{gt:.0f}°C" if gt is not None else "—",
                           "GPU TEMP", _grade((gt or 0) / 90, True)))
        tiles.append(_tile(f"{g.util:.0f}%" if g.util is not None else "—",
                           "GPU LOAD", _grade((g.util or 0) / 100, True)))

    fmax = max((f.rpm for f in snap.fans), default=0)
    tiles.append(_tile(f"{fmax}", "FAN RPM", _grade(fmax / FAN_RPM_MAX, True)))

    # Power: on battery the discharge rate is whole-system draw; otherwise show
    # GPU draw (the one power figure we can read directly).
    b = snap.battery
    if b.present and b.rate_w is not None and b.rate_w < 0:
        tiles.append(_tile(f"{abs(b.rate_w):.0f}W", "SYS DRAW", AMBER))
    elif g.present and g.power_w is not None:
        tiles.append(_tile(f"{g.power_w:.0f}W", "GPU PWR", AMBER))
    else:
        tiles.append(_tile(f"{snap.mem.percent:.0f}%", "RAM", CYAN))

    grid = Table.grid(expand=True)
    for _ in tiles:
        grid.add_column(justify="center", ratio=1)
    grid.add_row(*tiles)
    return _panel(grid, "SYSTEM AT A GLANCE", NEON_GREEN)


def thermal_graph_panel(snap: Snapshot, cpu_hist: list[float],
                        gpu_hist: list[float]) -> Panel:
    """CPU & GPU temperature as stacked time-series graphs (0–100°C)."""
    blocks: list[RenderableType] = []
    axis = Text("└" + "─" * (_CHART_W - 1), style=DIM)

    ct = snap.cpu.temp_c
    title = Text()
    title.append("CPU °C", style=CYAN)
    title.append(f"   {ct:.0f}°C" if ct is not None else "   —",
                 style=_grade((ct or 0) / 95, True))
    title.append("      scale 0–100°C", style=DIM)
    blocks.append(Group(title, *area_chart(cpu_hist or [ct or 0.0], 100.0,
                                           height=5), axis))

    if snap.gpu.present:
        gt = snap.gpu.temp_c
        t2 = Text()
        t2.append("GPU °C", style=MAGENTA)
        t2.append(f"   {gt:.0f}°C" if gt is not None else "   —",
                  style=_grade((gt or 0) / 90, True))
        t2.append("      scale 0–100°C", style=DIM)
        blocks.append(Text(""))
        blocks.append(Group(t2, *area_chart(gpu_hist or [gt or 0.0], 100.0,
                                            height=5), axis))
    return _panel(Group(*blocks), "THERMAL TREND  (live)", RED)


def fan_curve_panel(curves: list, profile: str, active: bool) -> Panel:
    """Show the temp→fan% points of a profile's CPU/GPU curves as a shape."""
    if not curves:
        body = Text("no fan-curve data — needs asusctl/asusd", style=DIM)
        return _panel(body, f"FAN CURVE · {profile}", AMBER)

    body = Table.grid(padding=(0, 1))
    body.add_column()
    tag = Text()
    tag.append(profile.upper(), style=f"bold {MAGENTA}")
    tag.append("  (active)" if active else "  (edit only — not the live profile)",
               style=NEON_GREEN if active else DIM)
    body.add_row(tag)

    for c in curves:
        pcts = c.pwm_pcts()
        temps = [t for t, _ in c.points]
        head = Text()
        head.append(f"{c.fan:<4}", style=CYAN)
        head.append("curve enabled" if c.enabled else "curve disabled (firmware default)",
                    style=NEON_GREEN if c.enabled else DIM)
        body.add_row(head)
        tline = Text("  temp ", style=DIM)
        tline.append(" ".join(f"{t:>3}" for t in temps), style=TEXT)
        tline.append(" °C", style=DIM)
        body.add_row(tline)
        fline = Text("  fan  ", style=DIM)
        for p in pcts:
            fline.append(f"{p:>3} ", style=_grade(p / 100, True))
        fline.append("%", style=DIM)
        body.add_row(fline)
        body.add_row(Text("  shape ", style=DIM)
                     + sparkline([float(p) for p in pcts], len(pcts) * 2, hot=True))
        body.add_row(Text(""))
    return _panel(body, f"FAN CURVE · {profile}", MAGENTA)


def _kv(label: str, value: str, vstyle: str = TEXT) -> Text:
    t = Text()
    t.append(f"{label:<9}", style=DIM)
    t.append(value, style=vstyle)
    return t


def _panel(body: RenderableType, title: str, colour: str) -> Panel:
    return Panel(
        body,
        title=f"[b]{title}[/]",
        title_align="left",
        border_style=colour,
        padding=(0, 1),
    )


# -- subsystem panels -------------------------------------------------------

def cpu_panel(snap: Snapshot, hist: list[float]) -> Panel:
    c = snap.cpu
    rows = Table.grid(padding=(0, 1))
    rows.add_column(justify="left")
    rows.add_column(justify="left")

    temp = f"{c.temp_c:.0f}°C" if c.temp_c is not None else "n/a"
    temp_style = _grade((c.temp_c or 0) / 95, True)
    avg_freq = sum(c.freqs_mhz) / len(c.freqs_mhz) if c.freqs_mhz else 0
    rows.add_row(_kv("usage", f"{c.overall:5.1f}%", CYAN), meter(c.overall, 100))
    rows.add_row(_kv("temp", temp, temp_style),
                 _kv("clock", f"{avg_freq/1000:.2f} GHz", BLUE))
    load = f"{c.load1:.2f}" if c.load1 is not None else "n/a"
    rows.add_row(_kv("load1", load, TEXT), _kv("cores", str(c.cores), TEXT))

    # per-core block strip, one cell per logical core
    strip = Text("cores  ", style=DIM)
    for u in c.per_core:
        strip.append(_BLOCKS[int(u / 100 * (len(_BLOCKS) - 1))],
                     style=_grade(u / 100, True))
    body = Group(rows, Text(""), strip, Text(""),
                 Text("history ", style=DIM) + sparkline(hist, 28, hot=True))
    return _panel(body, "CPU", NEON_GREEN)


def gpu_panel(snap: Snapshot, hist: list[float]) -> Panel:
    g = snap.gpu
    if not g.present:
        return _panel(Text(f"no readable GPU ({g.vendor})", style=DIM),
                      "GPU", MAGENTA)
    rows = Table.grid(padding=(0, 1))
    rows.add_column()
    rows.add_column()
    util = g.util if g.util is not None else 0
    rows.add_row(_kv("usage", f"{util:5.1f}%", MAGENTA), meter(util, 100, hot=False))
    temp = f"{g.temp_c:.0f}°C" if g.temp_c is not None else "n/a"
    rows.add_row(_kv("temp", temp, _grade((g.temp_c or 0) / 90, True)),
                 _kv("power", f"{g.power_w:.1f} W" if g.power_w else "n/a", AMBER))
    if g.mem_total_mb:
        memtxt = f"{g.mem_used_mb:.0f}/{g.mem_total_mb:.0f} MB"
        rows.add_row(_kv("vram", memtxt, CYAN),
                     meter(g.mem_used_mb, g.mem_total_mb, hot=False))
    rows.add_row(_kv("clock", f"{g.clock_mhz:.0f} MHz" if g.clock_mhz else "n/a", BLUE),
                 _kv("fan", f"{g.fan_pct:.0f}%" if g.fan_pct is not None else "n/a", TEXT))
    name = Text(g.name, style=DIM)
    body = Group(rows, Text(""), name, Text(""),
                 Text("history ", style=DIM) + sparkline(hist, 28, hot=False))
    return _panel(body, "GPU", MAGENTA)


def mem_panel(snap: Snapshot) -> Panel:
    m = snap.mem
    rows = Table.grid(padding=(0, 1))
    rows.add_column()
    rows.add_column()
    used = f"{human_bytes(m.used)} / {human_bytes(m.total)}"
    rows.add_row(_kv("ram", f"{m.percent:4.1f}%", CYAN), meter(m.percent, 100))
    rows.add_row(_kv("", used, TEXT), Text(""))
    if m.swap_total:
        swp = (m.swap_used / m.swap_total) * 100
        rows.add_row(_kv("swap", f"{swp:4.1f}%", TEXT), meter(swp, 100))
        rows.add_row(_kv("", f"{human_bytes(m.swap_used)} / "
                             f"{human_bytes(m.swap_total)}", DIM), Text(""))
    return _panel(rows, "MEMORY", CYAN)


def fan_panel(snap: Snapshot, hist: dict[str, list[float]]) -> Panel:
    if not snap.fans:
        return _panel(Text("no ASUS fan channels", style=DIM), "FANS", BLUE)
    rows = Table.grid(padding=(0, 1))
    rows.add_column(justify="left")
    rows.add_column(justify="right")
    rows.add_column(justify="left")
    # ~5000 rpm is a sane laptop-fan ceiling for the bar scale.
    for f in snap.fans:
        h = hist.get(f.label, [])
        rows.add_row(
            Text(f.label.replace("_", " "), style=BLUE),
            Text(f"{f.rpm:>5} rpm", style=NEON_GREEN if f.rpm else DIM),
            sparkline(h, 16, hot=True),
        )
    return _panel(rows, "FANS", BLUE)


def battery_panel(snap: Snapshot) -> Panel:
    b = snap.battery
    if not b.present:
        return _panel(Text("no battery", style=DIM), "BATTERY", AMBER)
    rows = Table.grid(padding=(0, 1))
    rows.add_column()
    rows.add_column()
    pct = b.percent or 0
    pcol = RED if pct < 20 else AMBER if pct < 40 else NEON_GREEN
    rows.add_row(_kv("charge", f"{pct:4.0f}%", pcol), meter(pct, 100, hot=False))
    rows.add_row(_kv("state", b.status, CYAN),
                 _kv("ac", "online" if b.ac_online else "offline",
                     NEON_GREEN if b.ac_online else DIM))
    if b.rate_w is not None:
        arrow = "▲" if b.rate_w > 0 else "▼" if b.rate_w < 0 else "•"
        rcol = NEON_GREEN if b.rate_w > 0 else AMBER
        rows.add_row(_kv("power", f"{arrow} {abs(b.rate_w):.1f} W", rcol),
                     _kv("limit", f"{b.charge_limit}%" if b.charge_limit else "—",
                         MAGENTA))
    else:
        rows.add_row(_kv("limit", f"{b.charge_limit}%" if b.charge_limit else "—",
                         MAGENTA), Text(""))
    return _panel(rows, "BATTERY", AMBER)


def storage_panel(snap: Snapshot) -> Panel:
    s = snap.storage
    rows = Table.grid(padding=(0, 1))
    rows.add_column()
    rows.add_column()
    nvme = f"{s.nvme_temp_c:.0f}°C" if s.nvme_temp_c is not None else "n/a"
    rows.add_row(_kv("nvme", nvme, _grade((s.nvme_temp_c or 0) / 80, True)),
                 _kv("root", f"{s.root_percent:.0f}%", CYAN))
    rows.add_row(_kv("disk", f"{human_bytes(s.root_used)} / "
                            f"{human_bytes(s.root_total)}", TEXT),
                 meter(s.root_percent, 100, hot=False))
    return _panel(rows, "STORAGE", BLUE)


def system_panel(hw: HardwareMap, snap: Snapshot) -> Panel:
    up = int(snap.uptime_s)
    d, rem = divmod(up, 86400)
    h, rem = divmod(rem, 3600)
    mnt = rem // 60
    uptime = (f"{d}d " if d else "") + f"{h}h {mnt}m"
    rows = Table.grid(padding=(0, 1))
    rows.add_column()
    rows.add_row(_kv("host", hw.hostname, NEON_GREEN))
    rows.add_row(_kv("model", hw.product, TEXT))
    rows.add_row(_kv("distro", hw.distro, TEXT))
    rows.add_row(_kv("kernel", hw.kernel, DIM))
    rows.add_row(_kv("uptime", uptime, CYAN))
    return _panel(rows, "SYSTEM", NEON_GREEN)


def profile_banner(snap: Snapshot) -> Panel:
    prof = snap.profile or "unknown"
    icon = {"Quiet": "🌙", "Balanced": "⚖", "Performance": "🚀"}.get(prof, "•")
    colour = {"Quiet": CYAN, "Balanced": NEON_GREEN,
              "Performance": MAGENTA}.get(prof, TEXT)
    txt = Text(f"{icon}  {prof.upper()}", style=f"bold {colour}")
    return _panel(Align.center(txt), "ACTIVE PROFILE", colour)


def proc_cpu_panel(snap: Snapshot) -> Panel:
    table = Table(expand=True, box=None, padding=(0, 1))
    table.add_column("pid", justify="right", style=DIM, no_wrap=True)
    table.add_column("process", style=TEXT, no_wrap=True)
    table.add_column("cpu%", justify="right")
    table.add_column("mem", justify="right", style=CYAN)
    for p in snap.procs_cpu:
        table.add_row(
            str(p.pid),
            p.name[:20],
            Text(f"{p.cpu:5.1f}", style=_grade(min(p.cpu, 100) / 100, True)),
            f"{p.mem_mb:.0f}M",
        )
    return _panel(table, "TOP — CPU", NEON_GREEN)


def proc_mem_panel(snap: Snapshot) -> Panel:
    table = Table(expand=True, box=None, padding=(0, 1))
    table.add_column("pid", justify="right", style=DIM, no_wrap=True)
    table.add_column("process", style=TEXT, no_wrap=True)
    table.add_column("mem", justify="right", style=CYAN)
    table.add_column("mem%", justify="right")
    for p in snap.procs_mem:
        table.add_row(
            str(p.pid),
            p.name[:20],
            f"{p.mem_mb:.0f}M",
            Text(f"{p.mem_pct:4.1f}", style=_grade(p.mem_pct / 100, True)),
        )
    return _panel(table, "TOP — MEMORY", CYAN)


def proc_detail_panel(detail: dict | None) -> Panel:
    """Detail card for the currently-selected process."""
    if not detail:
        return _panel(Text("select a process to inspect", style=DIM),
                      "PROCESS DETAIL", CYAN)
    import time as _time
    rows = Table.grid(padding=(0, 1))
    rows.add_column(justify="right", style=DIM, no_wrap=True)
    rows.add_column()

    def add(label: str, value, vstyle: str = TEXT) -> None:
        rows.add_row(label, Text(str(value), style=vstyle))

    add("pid", detail.get("pid", "—"), NEON_GREEN)
    add("name", detail.get("name", "—"), CYAN)
    add("status", detail.get("status", "—"),
        AMBER if detail.get("status") not in ("running", "sleeping") else TEXT)
    add("user", detail.get("user", "—"))
    add("ppid", detail.get("ppid", "—"))
    add("threads", detail.get("threads", "—"))
    cpu = detail.get("cpu")
    if cpu is not None:
        add("cpu", f"{cpu:.1f}%", _grade(min(cpu, 100) / 100, True))
    mem = detail.get("mem_mb")
    if mem is not None:
        add("memory", f"{mem:.0f} MB", CYAN)
    created = detail.get("create")
    if created:
        add("started", _time.strftime("%H:%M:%S", _time.localtime(created)))
    cmd = detail.get("cmdline") or detail.get("exe") or "—"
    rows.add_row("cmd", Text(str(cmd)[:120], style=DIM))
    return _panel(rows, "PROCESS DETAIL", CYAN)


def gpu_proc_panel(snap: Snapshot) -> Panel:
    if not snap.gpu.present:
        return _panel(Text("no GPU", style=DIM), "GPU PROCESSES", MAGENTA)
    if not snap.gpu_procs:
        return _panel(Text("no GPU compute processes", style=DIM),
                      "GPU PROCESSES", MAGENTA)
    table = Table(expand=True, box=None, padding=(0, 1))
    table.add_column("pid", justify="right", style=DIM, no_wrap=True)
    table.add_column("process", style=TEXT)
    table.add_column("vram", justify="right", style=MAGENTA)
    for p in snap.gpu_procs:
        table.add_row(str(p.pid), p.name[:32], f"{p.mem_mb:.0f}M")
    return _panel(table, "GPU PROCESSES", MAGENTA)


def net_table(snap: Snapshot, hist: dict[str, list[float]]) -> Panel:
    table = Table(expand=True, box=None, padding=(0, 1))
    table.add_column("iface", style=CYAN, no_wrap=True)
    table.add_column("state", justify="center")
    table.add_column("IPv4", style=TEXT, no_wrap=True)
    table.add_column("▼ down", justify="right", style=NEON_GREEN)
    table.add_column("▲ up", justify="right", style=AMBER)
    table.add_column("RX", justify="right", style=DIM)
    table.add_column("TX", justify="right", style=DIM)
    table.add_column("err/drop", justify="right")
    shown = [i for i in snap.net if not i.is_virtual or i.up_bps + i.down_bps > 0]
    for i in shown[:10]:
        state = Text("UP", style=NEON_GREEN) if i.is_up else Text("down", style=DIM)
        errs = i.errin + i.errout + i.dropin + i.dropout
        table.add_row(
            Text(i.name + (" *" if i.is_virtual else ""),
                 style=DIM if i.is_virtual else CYAN),
            state,
            i.ipv4 or "—",
            fmt_rate(i.down_bps),
            fmt_rate(i.up_bps),
            human_bytes(i.rx_total),
            human_bytes(i.tx_total),
            Text(str(errs), style=RED if errs else DIM),
        )
    return _panel(table, "INTERFACES  (* = virtual)", CYAN)


def bandwidth_graph_panel(down_hist: list[float], up_hist: list[float],
                          cur_down: float, cur_up: float) -> Panel:
    """Total download / upload throughput as stacked, auto-scaled graphs."""
    axis = Text("└" + "─" * (_CHART_W - 1), style=DIM)
    dmax = max(max(down_hist or [0.0]), 1024.0)   # floor so idle isn't all-full
    umax = max(max(up_hist or [0.0]), 1024.0)

    dt = Text()
    dt.append("▼ DOWN  ", style=NEON_GREEN)
    dt.append(fmt_rate(cur_down).strip(), style=NEON_GREEN)
    dt.append(f"      peak {fmt_rate(dmax).strip()}", style=DIM)
    ut = Text()
    ut.append("▲ UP    ", style=AMBER)
    ut.append(fmt_rate(cur_up).strip(), style=AMBER)
    ut.append(f"      peak {fmt_rate(umax).strip()}", style=DIM)

    body = Group(
        dt, *area_chart(down_hist or [0.0], dmax, height=5, colour=NEON_GREEN), axis,
        Text(""),
        ut, *area_chart(up_hist or [0.0], umax, height=5, colour=AMBER), axis,
    )
    return _panel(body, "BANDWIDTH  (live trend)", CYAN)


def connections_panel(conns: list) -> Panel:
    if not conns:
        return _panel(Text("no active inet connections", style=DIM),
                      "CONNECTIONS", BLUE)
    est = sum(1 for c in conns if c.status == "ESTABLISHED")
    lis = sum(1 for c in conns if c.status == "LISTEN")
    table = Table(expand=True, box=None, padding=(0, 1))
    table.add_column("proto", style=DIM, no_wrap=True)
    table.add_column("local", style=CYAN, no_wrap=True)
    table.add_column("remote", style=TEXT, no_wrap=True)
    table.add_column("state", no_wrap=True)
    table.add_column("process", style=MAGENTA, no_wrap=True)
    for c in conns[:14]:
        scol = (NEON_GREEN if c.status == "ESTABLISHED"
                else AMBER if c.status == "LISTEN" else DIM)
        table.add_row(c.proto, c.laddr[:26], c.raddr[:26],
                      Text(c.status, style=scol),
                      f"{c.pname}" + (f" ({c.pid})" if c.pid else ""))
    title = f"CONNECTIONS  ({est} established, {lis} listening)"
    return _panel(table, title, BLUE)
