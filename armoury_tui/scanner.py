"""Module 1 - the hardware scanner.

The single most important correctness rule for hwmon code: **never hardcode
``hwmon5`` / ``hwmon7``**. The kernel assigns those numbers in probe order, so
they shuffle between boots and between machines. Instead we enumerate every
``/sys/class/hwmon/hwmonN`` once at startup, read its ``name`` file, and resolve
the *file paths* we actually care about (CPU package temp, CPU/GPU fan RPM,
NVMe composite temp, ...). The telemetry layer then reads those fixed paths
each tick with zero re-scanning.

On the reference machine the relevant nodes are:

    name=asus                 -> fan1_input (cpu_fan), fan2_input (gpu_fan)
    name=k10temp              -> temp1_input (Tctl)            [AMD Ryzen]
    name=nvme                 -> temp1_input (Composite)
    name=BAT1 / ACAD          -> battery / AC adapter

but we discover all of that at runtime so a coretemp/zenpower/amdgpu machine
works too.
"""

from __future__ import annotations

import platform
import shutil
import socket
from dataclasses import dataclass, field
from pathlib import Path

from . import sysfs

HWMON_ROOT = Path("/sys/class/hwmon")
PLATFORM_DIR = Path("/sys/devices/platform/asus-nb-wmi")
KBD_LED = Path("/sys/class/leds/asus::kbd_backlight")
ACPI_PROFILE = Path("/sys/firmware/acpi/platform_profile")

# hwmon ``name`` values we treat as a CPU package/die temperature, in priority
# order. Whichever appears first wins.
CPU_TEMP_CHIPS = ("k10temp", "zenpower", "coretemp", "cpu_thermal")
# Labels within a CPU chip that represent the headline package temperature.
CPU_TEMP_LABELS = ("Tctl", "Tdie", "Package id 0", "Tccd1")


@dataclass
class TempChannel:
    """One ``tempN_input`` node plus its human label."""

    path: Path
    label: str


@dataclass
class FanChannel:
    path: Path
    label: str


@dataclass
class HardwareMap:
    """Resolved, ready-to-read paths discovered once at startup."""

    # Identity
    hostname: str = ""
    product: str = ""
    board: str = ""
    distro: str = ""
    kernel: str = ""

    # CPU
    cpu_temp: TempChannel | None = None
    cpu_chip: str = ""

    # GPU
    gpu_vendor: str = "none"          # "nvidia" | "amd" | "intel" | "none"
    amd_gpu_hwmon: Path | None = None  # populated only for AMD discrete/iGPU

    # Fans (ASUS platform fans)
    fans: list[FanChannel] = field(default_factory=list)

    # Storage
    nvme_temp: TempChannel | None = None

    # Battery / power supplies
    battery: Path | None = None        # /sys/class/power_supply/BAT*
    ac_adapter: Path | None = None
    charge_limit_node: Path | None = None

    # Controls availability
    has_asusctl: bool = False
    has_asusd: bool = False
    has_platform_profile: bool = False
    has_kbd_backlight: bool = False
    kbd_max_brightness: int = 0

    # Raw map of name -> hwmon dir, kept for diagnostics / the --probe view.
    hwmon_by_name: dict[str, str] = field(default_factory=dict)

    def summary_lines(self) -> list[str]:
        """Compact human description used by the ``--probe`` text view."""
        gpu = self.gpu_vendor.upper() if self.gpu_vendor != "none" else "none"
        return [
            f"host      : {self.hostname}",
            f"product   : {self.product}",
            f"board     : {self.board}",
            f"distro    : {self.distro}",
            f"kernel    : {self.kernel}",
            f"cpu temp  : {self.cpu_chip or '?'} "
            f"({self.cpu_temp.label if self.cpu_temp else 'n/a'})",
            f"gpu       : {gpu}",
            f"fans      : {', '.join(f.label for f in self.fans) or 'none'}",
            f"nvme temp : {'yes' if self.nvme_temp else 'no'}",
            f"battery   : {self.battery.name if self.battery else 'none'}",
            f"chg limit : {'writable via daemon' if self.charge_limit_node else 'n/a'}",
            f"asusctl   : {self.has_asusctl}   asusd: {self.has_asusd}",
            f"profiles  : {self.has_platform_profile}",
            f"kbd led   : {self.has_kbd_backlight} (max {self.kbd_max_brightness})",
        ]


def _detect_identity(hw: HardwareMap) -> None:
    hw.hostname = socket.gethostname()
    hw.product = sysfs.read_text("/sys/class/dmi/id/product_name", "unknown")
    hw.board = sysfs.read_text("/sys/class/dmi/id/board_name", "")
    hw.kernel = platform.release()
    # Parse /etc/os-release PRETTY_NAME without importing distro libs.
    pretty = None
    for line in (sysfs.read_text("/etc/os-release", "") or "").splitlines():
        if line.startswith("PRETTY_NAME="):
            pretty = line.split("=", 1)[1].strip().strip('"')
            break
    hw.distro = pretty or platform.system()


def _temp_channels(hwmon_dir: Path) -> list[TempChannel]:
    chans: list[TempChannel] = []
    for inp in sorted(hwmon_dir.glob("temp*_input")):
        label_file = inp.with_name(inp.name.replace("_input", "_label"))
        label = sysfs.read_text(label_file) or inp.name.replace("_input", "")
        chans.append(TempChannel(path=inp, label=label))
    return chans


def _pick_cpu_temp(chips: dict[str, Path]) -> tuple[str, TempChannel | None]:
    for chip in CPU_TEMP_CHIPS:
        if chip not in chips:
            continue
        chans = _temp_channels(chips[chip])
        if not chans:
            continue
        # Prefer a named package/die channel; otherwise take the first.
        for wanted in CPU_TEMP_LABELS:
            for ch in chans:
                if ch.label == wanted:
                    return chip, ch
        return chip, chans[0]
    # Last-ditch fallback: ACPI thermal zone.
    if "acpitz" in chips:
        chans = _temp_channels(chips["acpitz"])
        if chans:
            return "acpitz", chans[0]
    return "", None


def _detect_gpu(hw: HardwareMap, name_to_dir: dict[str, Path]) -> None:
    if shutil.which("nvidia-smi"):
        hw.gpu_vendor = "nvidia"
        return
    # AMD discrete/integrated GPUs expose an "amdgpu" hwmon node.
    if "amdgpu" in name_to_dir:
        hw.gpu_vendor = "amd"
        hw.amd_gpu_hwmon = name_to_dir["amdgpu"]
        return
    if "i915" in name_to_dir or "xe" in name_to_dir:
        hw.gpu_vendor = "intel"
        return
    hw.gpu_vendor = "none"


def _detect_battery(hw: HardwareMap) -> None:
    ps_root = Path("/sys/class/power_supply")
    if not ps_root.exists():
        return
    for entry in sorted(ps_root.iterdir()):
        ps_type = sysfs.read_text(entry / "type", "")
        name = entry.name.upper()
        if ps_type == "Battery" or name.startswith("BAT"):
            hw.battery = entry
            limit = entry / "charge_control_end_threshold"
            if limit.exists():
                hw.charge_limit_node = limit
        elif ps_type == "Mains" or name.startswith(("ACAD", "AC", "ADP")):
            hw.ac_adapter = entry


def scan() -> HardwareMap:
    """Discover the machine and return a fully-resolved :class:`HardwareMap`."""
    hw = HardwareMap()
    _detect_identity(hw)

    # 1. Enumerate every hwmon node by its declared name.
    name_to_dir: dict[str, Path] = {}
    cpu_chip_dirs: dict[str, Path] = {}
    for node in sorted(HWMON_ROOT.glob("hwmon*")):
        name = sysfs.read_text(node / "name")
        if not name:
            continue
        # Several nodes can share a name (e.g. two spd5118 RAM sensors); keep
        # the first for the simple map but remember CPU chips explicitly.
        name_to_dir.setdefault(name, node)
        hw.hwmon_by_name[name] = str(node)
        if name in CPU_TEMP_CHIPS or name == "acpitz":
            cpu_chip_dirs.setdefault(name, node)

    # 2. CPU package temperature.
    hw.cpu_chip, hw.cpu_temp = _pick_cpu_temp(cpu_chip_dirs)

    # 3. ASUS platform fans (name == "asus") -> labelled fan channels.
    asus_dir = name_to_dir.get("asus")
    if asus_dir:
        for inp in sorted(asus_dir.glob("fan*_input")):
            label_file = inp.with_name(inp.name.replace("_input", "_label"))
            label = sysfs.read_text(label_file) or inp.stem
            hw.fans.append(FanChannel(path=inp, label=label))

    # 4. NVMe composite temperature.
    nvme_dir = name_to_dir.get("nvme")
    if nvme_dir:
        for ch in _temp_channels(nvme_dir):
            if ch.label.lower().startswith("composite"):
                hw.nvme_temp = ch
                break
        if hw.nvme_temp is None:
            chans = _temp_channels(nvme_dir)
            hw.nvme_temp = chans[0] if chans else None

    # 5. GPU vendor + battery.
    _detect_gpu(hw, name_to_dir)
    _detect_battery(hw)

    # 6. Control surface availability.
    hw.has_asusctl = shutil.which("asusctl") is not None
    hw.has_asusd = shutil.which("asusd") is not None
    hw.has_platform_profile = ACPI_PROFILE.exists()
    if KBD_LED.exists():
        hw.has_kbd_backlight = True
        hw.kbd_max_brightness = sysfs.read_int(KBD_LED / "max_brightness", 0) or 0

    return hw
