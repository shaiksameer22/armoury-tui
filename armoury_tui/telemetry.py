"""Live telemetry collectors.

A single :class:`Telemetry` instance owns the discovered :class:`HardwareMap`
and any cross-tick state (previous network/CPU counters needed to compute
rates). Its :meth:`Telemetry.snapshot` returns an immutable :class:`Snapshot`
describing the whole machine at one instant.

``snapshot()`` may block for tens of milliseconds (it shells out to
``nvidia-smi`` and ``upower``), so the TUI always calls it via
``asyncio.to_thread`` -- never on the event loop. Every field is optional:
absent hardware yields ``None`` so the renderer can show "n/a" instead of
dying.
"""

from __future__ import annotations

import re
import shutil
import socket
import subprocess
import time
from dataclasses import dataclass, field

import psutil

from . import sysfs
from .scanner import ACPI_PROFILE, KBD_LED, HardwareMap

# ---------------------------------------------------------------------------
# Per-subsystem snapshot records
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class CpuSample:
    per_core: list[float] = field(default_factory=list)   # % utilisation
    freqs_mhz: list[float] = field(default_factory=list)   # current MHz / core
    overall: float = 0.0
    temp_c: float | None = None
    load1: float | None = None
    cores: int = 0


@dataclass(frozen=True)
class GpuSample:
    present: bool = False
    vendor: str = "none"
    name: str = ""
    util: float | None = None
    mem_used_mb: float | None = None
    mem_total_mb: float | None = None
    temp_c: float | None = None
    power_w: float | None = None
    clock_mhz: float | None = None
    fan_pct: float | None = None


@dataclass(frozen=True)
class FanSample:
    label: str
    rpm: int


@dataclass(frozen=True)
class MemSample:
    used: int = 0
    total: int = 0
    percent: float = 0.0
    swap_used: int = 0
    swap_total: int = 0


@dataclass(frozen=True)
class StorageSample:
    nvme_temp_c: float | None = None
    root_used: int = 0
    root_total: int = 0
    root_percent: float = 0.0
    smart_ok: bool | None = None   # None = unknown / needs root


@dataclass(frozen=True)
class BatterySample:
    present: bool = False
    percent: float | None = None
    status: str = "unknown"          # Charging / Discharging / Not charging / Full
    rate_w: float | None = None      # signed: +charging, -discharging
    charge_limit: int | None = None
    ac_online: bool | None = None


@dataclass(frozen=True)
class NetIface:
    name: str
    up_bps: float
    down_bps: float
    is_up: bool
    is_virtual: bool
    ipv4: str = ""
    mac: str = ""
    mtu: int = 0
    speed: int = 0            # link speed Mbps, 0 = unknown
    rx_total: int = 0         # bytes received since boot
    tx_total: int = 0         # bytes sent since boot
    errin: int = 0
    errout: int = 0
    dropin: int = 0
    dropout: int = 0


@dataclass(frozen=True)
class NetConn:
    proto: str                # tcp / udp
    laddr: str
    raddr: str
    status: str
    pid: int | None
    pname: str


@dataclass(frozen=True)
class ProcInfo:
    pid: int
    name: str
    cpu: float        # percent (psutil scale: may exceed 100 on multi-core)
    mem_mb: float
    mem_pct: float


@dataclass(frozen=True)
class GpuProc:
    pid: int
    name: str
    mem_mb: float


@dataclass(frozen=True)
class Snapshot:
    ts: float
    uptime_s: float
    profile: str | None
    kbd_brightness: int | None
    cpu: CpuSample
    gpu: GpuSample
    fans: list[FanSample]
    mem: MemSample
    storage: StorageSample
    battery: BatterySample
    net: list[NetIface]
    procs_cpu: list[ProcInfo]
    procs_mem: list[ProcInfo]
    procs_all: list[ProcInfo]
    gpu_procs: list[GpuProc]


# Interface name prefixes we consider "virtual" (shown but de-emphasised).
_VIRTUAL_IFACE = ("lo", "docker", "veth", "br-", "virbr", "vmnet", "mpqemu", "tun", "tap")


class Telemetry:
    def __init__(self, hw: HardwareMap) -> None:
        self.hw = hw
        self._have_nvsmi = hw.gpu_vendor == "nvidia" and shutil.which("nvidia-smi")
        self._have_upower = shutil.which("upower") is not None
        self._upower_path: str | None = None
        self._prev_net: dict[str, tuple[int, int]] = {}
        self._prev_net_ts: float | None = None
        # Persisted psutil.Process objects: cpu_percent() is a delta since the
        # *previous call on the same object*, so the cache is what makes the
        # per-process CPU figures meaningful across ticks.
        self._proc_cache: dict[int, psutil.Process] = {}
        # Prime psutil's interval-based CPU percent so the first real call
        # reports a delta rather than a meaningless 0.0/since-boot value.
        psutil.cpu_percent(percpu=True)
        self._resolve_upower_device()

    # -- helpers ----------------------------------------------------------

    def _resolve_upower_device(self) -> None:
        if not self._have_upower:
            return
        try:
            out = subprocess.run(
                ["upower", "-e"], capture_output=True, text=True, timeout=2
            ).stdout
        except (OSError, subprocess.SubprocessError):
            return
        for line in out.splitlines():
            if "battery_BAT" in line or "/battery" in line:
                self._upower_path = line.strip()
                break

    # -- CPU --------------------------------------------------------------

    def _cpu(self) -> CpuSample:
        per_core = psutil.cpu_percent(percpu=True)
        try:
            freqs = [f.current for f in psutil.cpu_freq(percpu=True)]
        except (NotImplementedError, AttributeError, OSError):
            cur = psutil.cpu_freq()
            freqs = [cur.current] if cur else []
        try:
            load1 = psutil.getloadavg()[0]
        except (OSError, AttributeError):
            load1 = None
        temp = sysfs.read_milli(self.hw.cpu_temp.path) if self.hw.cpu_temp else None
        overall = sum(per_core) / len(per_core) if per_core else 0.0
        return CpuSample(
            per_core=per_core,
            freqs_mhz=freqs,
            overall=overall,
            temp_c=temp,
            load1=load1,
            cores=len(per_core),
        )

    # -- GPU --------------------------------------------------------------

    def _gpu(self) -> GpuSample:
        if self._have_nvsmi:
            return self._gpu_nvidia()
        if self.hw.gpu_vendor == "amd" and self.hw.amd_gpu_hwmon:
            return self._gpu_amd()
        return GpuSample(present=False, vendor=self.hw.gpu_vendor)

    def _gpu_nvidia(self) -> GpuSample:
        query = (
            "name,utilization.gpu,memory.used,memory.total,"
            "temperature.gpu,power.draw,clocks.gr,fan.speed"
        )
        try:
            out = subprocess.run(
                ["nvidia-smi", f"--query-gpu={query}",
                 "--format=csv,noheader,nounits"],
                capture_output=True, text=True, timeout=3,
            ).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            return GpuSample(present=False, vendor="nvidia")
        if not out:
            return GpuSample(present=False, vendor="nvidia")
        # Use only the first GPU line.
        parts = [p.strip() for p in out.splitlines()[0].split(",")]

        def num(idx: int) -> float | None:
            try:
                v = parts[idx]
            except IndexError:
                return None
            if not v or v.upper().startswith(("N/A", "[N/A")):
                return None
            try:
                return float(v)
            except ValueError:
                return None

        return GpuSample(
            present=True,
            vendor="nvidia",
            name=parts[0] if parts else "NVIDIA GPU",
            util=num(1),
            mem_used_mb=num(2),
            mem_total_mb=num(3),
            temp_c=num(4),
            power_w=num(5),
            clock_mhz=num(6),
            fan_pct=num(7),
        )

    def _gpu_amd(self) -> GpuSample:
        """Best-effort AMD readout from the amdgpu hwmon/drm sysfs nodes."""
        d = self.hw.amd_gpu_hwmon
        assert d is not None
        temp = sysfs.read_milli(d / "temp1_input")
        power = sysfs.read_milli(d / "power1_average")  # µW -> mW; see below
        if power is not None:
            power = power / 1000.0  # millis() already /1000 -> this gives W
        # GPU busy percent lives under the drm device, a few dirs up.
        busy = None
        try:
            dev = (d / "device").resolve()
            busy = sysfs.read_int(dev / "gpu_busy_percent")
        except OSError:
            pass
        return GpuSample(
            present=True, vendor="amd", name="AMD GPU",
            util=float(busy) if busy is not None else None,
            temp_c=temp, power_w=power,
        )

    # -- fans -------------------------------------------------------------

    def _fans(self) -> list[FanSample]:
        out: list[FanSample] = []
        for fan in self.hw.fans:
            rpm = sysfs.read_int(fan.path)
            if rpm is not None:
                out.append(FanSample(label=fan.label, rpm=rpm))
        return out

    # -- memory -----------------------------------------------------------

    def _mem(self) -> MemSample:
        vm = psutil.virtual_memory()
        sw = psutil.swap_memory()
        return MemSample(
            used=vm.used, total=vm.total, percent=vm.percent,
            swap_used=sw.used, swap_total=sw.total,
        )

    # -- storage ----------------------------------------------------------

    def _storage(self) -> StorageSample:
        temp = sysfs.read_milli(self.hw.nvme_temp.path) if self.hw.nvme_temp else None
        try:
            du = psutil.disk_usage("/")
            used, total, pct = du.used, du.total, du.percent
        except OSError:
            used = total = 0
            pct = 0.0
        return StorageSample(
            nvme_temp_c=temp, root_used=used, root_total=total,
            root_percent=pct, smart_ok=None,
        )

    # -- battery ----------------------------------------------------------

    def _battery(self) -> BatterySample:
        bat = self.hw.battery
        if bat is None:
            return BatterySample(present=False)
        pct = sysfs.read_float(bat / "capacity")
        status = sysfs.read_text(bat / "status", "unknown") or "unknown"
        limit = (
            sysfs.read_int(self.hw.charge_limit_node)
            if self.hw.charge_limit_node else None
        )
        ac = None
        if self.hw.ac_adapter:
            ac_val = sysfs.read_int(self.hw.ac_adapter / "online")
            ac = bool(ac_val) if ac_val is not None else None

        rate = self._battery_rate(bat, status)
        return BatterySample(
            present=True, percent=pct, status=status,
            rate_w=rate, charge_limit=limit, ac_online=ac,
        )

    def _battery_rate(self, bat, status: str) -> float | None:
        """Signed power flow in watts (+ charging, - discharging)."""
        mag: float | None = None
        # Prefer upower's pre-computed, accurate energy-rate.
        if self._upower_path:
            try:
                out = subprocess.run(
                    ["upower", "-i", self._upower_path],
                    capture_output=True, text=True, timeout=2,
                ).stdout
                m = re.search(r"energy-rate:\s*([\d.]+)\s*W", out)
                if m:
                    mag = float(m.group(1))
            except (OSError, subprocess.SubprocessError, ValueError):
                mag = None
        # Fallbacks straight from sysfs.
        if mag is None:
            p = sysfs.read_int(bat / "power_now")          # µW
            if p is not None:
                mag = abs(p) / 1_000_000.0
            else:
                i = sysfs.read_int(bat / "current_now")    # µA
                v = sysfs.read_int(bat / "voltage_now")    # µV
                if i is not None and v is not None:
                    mag = abs(i) * abs(v) / 1e12
        if mag is None:
            return None
        return -mag if status.lower().startswith("dis") else mag

    # -- network ----------------------------------------------------------

    def _net(self) -> list[NetIface]:
        now = time.monotonic()
        counters = psutil.net_io_counters(pernic=True)
        stats = psutil.net_if_stats()
        try:
            addrs = psutil.net_if_addrs()
        except OSError:
            addrs = {}
        dt = (now - self._prev_net_ts) if self._prev_net_ts else None
        result: list[NetIface] = []
        for name, c in counters.items():
            prev = self._prev_net.get(name)
            if prev and dt and dt > 0:
                up = max(0, c.bytes_sent - prev[0]) / dt
                down = max(0, c.bytes_recv - prev[1]) / dt
            else:
                up = down = 0.0
            self._prev_net[name] = (c.bytes_sent, c.bytes_recv)
            st = stats.get(name)
            ipv4 = mac = ""
            for a in addrs.get(name, []):
                if a.family == socket.AF_INET and not ipv4:
                    ipv4 = a.address
                elif a.family == psutil.AF_LINK and not mac:
                    mac = a.address
            result.append(NetIface(
                name=name,
                up_bps=up,
                down_bps=down,
                is_up=bool(st.isup) if st else False,
                is_virtual=name.startswith(_VIRTUAL_IFACE),
                ipv4=ipv4,
                mac=mac,
                mtu=st.mtu if st else 0,
                speed=st.speed if st else 0,
                rx_total=c.bytes_recv,
                tx_total=c.bytes_sent,
                errin=c.errin,
                errout=c.errout,
                dropin=c.dropin,
                dropout=c.dropout,
            ))
        self._prev_net_ts = now
        # Real, up interfaces first; then by throughput.
        result.sort(key=lambda i: (i.is_virtual, not i.is_up,
                                   -(i.up_bps + i.down_bps)))
        return result

    # -- processes --------------------------------------------------------

    def _processes(
        self, top: int = 7
    ) -> tuple[list[ProcInfo], list[ProcInfo], list[ProcInfo]]:
        cache = self._proc_cache
        seen: set[int] = set()
        rows: list[ProcInfo] = []
        for p in psutil.process_iter():
            pid = p.pid
            seen.add(pid)
            proc = cache.get(pid)
            if proc is None:
                try:
                    proc = psutil.Process(pid)
                    proc.cpu_percent(None)  # prime: first reading is always 0
                except psutil.Error:
                    continue
                cache[pid] = proc
            try:
                with proc.oneshot():
                    cpu = proc.cpu_percent(None)
                    name = proc.name()
                    rss = proc.memory_info().rss
                    mem_pct = proc.memory_percent()
            except psutil.Error:
                continue
            rows.append(ProcInfo(pid, name, cpu, rss / (1024 * 1024), mem_pct))
        # Drop cached handles for processes that have exited.
        for pid in list(cache):
            if pid not in seen:
                del cache[pid]
        by_cpu = sorted(rows, key=lambda r: r.cpu, reverse=True)[:top]
        by_mem = sorted(rows, key=lambda r: r.mem_mb, reverse=True)[:top]
        return by_cpu, by_mem, rows

    def _gpu_processes(self) -> list[GpuProc]:
        if not self._have_nvsmi:
            return []
        try:
            out = subprocess.run(
                ["nvidia-smi",
                 "--query-compute-apps=pid,process_name,used_memory",
                 "--format=csv,noheader,nounits"],
                capture_output=True, text=True, timeout=3,
            ).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            return []
        procs: list[GpuProc] = []
        for line in out.splitlines():
            parts = [p.strip() for p in line.split(",")]
            if len(parts) < 3:
                continue
            try:
                procs.append(GpuProc(int(parts[0]), parts[1], float(parts[2])))
            except ValueError:
                continue
        return sorted(procs, key=lambda p: p.mem_mb, reverse=True)

    # -- connections (on-demand; not part of every snapshot) --------------

    def connections(self, limit: int = 60) -> list[NetConn]:
        """Active inet sockets, ESTABLISHED first, with owning process names.

        Reads /proc as the normal user, so sockets we don't own report
        ``pid=None`` (no root) — shown as "—" rather than failing.
        """
        try:
            raw = psutil.net_connections(kind="inet")
        except (psutil.AccessDenied, OSError):
            return []
        names: dict[int, str] = {}
        for pid in {c.pid for c in raw if c.pid}:
            try:
                names[pid] = psutil.Process(pid).name()
            except psutil.Error:
                names[pid] = "?"
        out: list[NetConn] = []
        for c in raw:
            proto = "tcp" if c.type == socket.SOCK_STREAM else "udp"
            laddr = f"{c.laddr.ip}:{c.laddr.port}" if c.laddr else "-"
            raddr = f"{c.raddr.ip}:{c.raddr.port}" if c.raddr else "-"
            out.append(NetConn(proto, laddr, raddr, c.status or "-",
                               c.pid, names.get(c.pid, "—")))
        rank = {"ESTABLISHED": 0, "LISTEN": 1}
        out.sort(key=lambda x: (rank.get(x.status, 2), x.proto))
        return out[:limit]

    # -- misc -------------------------------------------------------------

    def _profile(self) -> str | None:
        raw = sysfs.read_text(ACPI_PROFILE)
        if not raw:
            return None
        return raw.capitalize()  # quiet/balanced/performance -> Quiet/...

    def _brightness(self) -> int | None:
        if not self.hw.has_kbd_backlight:
            return None
        return sysfs.read_int(KBD_LED / "brightness")

    @staticmethod
    def _uptime() -> float:
        raw = sysfs.read_text("/proc/uptime", "0")
        try:
            return float(raw.split()[0])
        except (ValueError, IndexError):
            return 0.0

    # -- public -----------------------------------------------------------

    def snapshot(self) -> Snapshot:
        """Collect one full reading of every subsystem."""
        procs_cpu, procs_mem, procs_all = self._processes()
        return Snapshot(
            ts=time.time(),
            uptime_s=self._uptime(),
            profile=self._profile(),
            kbd_brightness=self._brightness(),
            cpu=self._cpu(),
            gpu=self._gpu(),
            fans=self._fans(),
            mem=self._mem(),
            storage=self._storage(),
            battery=self._battery(),
            net=self._net(),
            procs_cpu=procs_cpu,
            procs_mem=procs_mem,
            procs_all=procs_all,
            gpu_procs=self._gpu_processes(),
        )


# ---------------------------------------------------------------------------
# Process actions (state changes — kept out of the read-only Telemetry class)
# ---------------------------------------------------------------------------


def kill_process(pid: int, force: bool = False) -> tuple[bool, str]:
    """Send SIGTERM (or SIGKILL if *force*) to a process, with guard rails.

    Refuses PID 1 and this app's own process/parent so a stray keypress can't
    take down the session or init. Never raises — returns (ok, message) for the
    UI to toast.
    """
    import os
    if pid in (1, os.getpid(), os.getppid()):
        return False, f"refusing to kill critical/own process {pid}"
    try:
        p = psutil.Process(pid)
        name = p.name()
    except psutil.NoSuchProcess:
        return False, f"process {pid} is already gone"
    except psutil.Error as exc:
        return False, f"cannot access {pid}: {exc}"
    try:
        p.kill() if force else p.terminate()
    except psutil.NoSuchProcess:
        return False, f"process {pid} is already gone"
    except psutil.AccessDenied:
        return False, f"permission denied for {name} ({pid}) — likely root-owned"
    except psutil.Error as exc:
        return False, f"failed to signal {pid}: {exc}"
    sig = "SIGKILL" if force else "SIGTERM"
    return True, f"sent {sig} → {name} ({pid})"


def process_detail(pid: int) -> dict | None:
    """A best-effort detail bundle for one process (each field guarded)."""
    try:
        p = psutil.Process(pid)
    except psutil.Error:
        return None
    d: dict = {"pid": pid}
    try:
        with p.oneshot():
            d["name"] = p.name()
            d["status"] = p.status()
            d["ppid"] = p.ppid()
            d["threads"] = p.num_threads()
            d["cpu"] = p.cpu_percent(None)
            d["mem_mb"] = p.memory_info().rss / (1024 * 1024)
            d["create"] = p.create_time()
    except psutil.Error:
        pass
    for key, getter in (("user", p.username), ("cmdline", p.cmdline),
                        ("exe", p.exe)):
        try:
            val = getter()
            d[key] = " ".join(val) if isinstance(val, list) else val
        except psutil.Error:
            d[key] = "—"
    return d
