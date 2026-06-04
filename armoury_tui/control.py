"""Module 2 - the state controller (safe writes).

On a stock ASUS laptop every control node under /sys is **root-owned**:

    ro/needpriv /sys/class/leds/asus::kbd_backlight/brightness
    ro/needpriv /sys/firmware/acpi/platform_profile
    ro/needpriv /sys/class/power_supply/BAT1/charge_control_end_threshold

So this TUI must *never* be launched as root. Instead each change is delegated
to ``asusd`` through the ``asusctl`` CLI, which authenticates over D-Bus/polkit
and performs the privileged write itself. If ``asusctl`` is missing we fall
back to a ``pkexec`` (GUI polkit) sysfs write, and finally to ``sudo``.

Every public method returns a :class:`ControlResult` and swallows all
exceptions -- a failed control action must surface as a toast in the UI, not a
traceback that tears down the render loop.
"""

from __future__ import annotations

import re
import shutil
import subprocess
from dataclasses import dataclass

from .scanner import ACPI_PROFILE, KBD_LED, HardwareMap

# asusctl profile names <-> the ACPI platform_profile tokens used as fallback.
PROFILE_TO_ACPI = {"Quiet": "quiet", "Balanced": "balanced", "Performance": "performance"}
# Keyboard brightness: asusctl word <-> sysfs integer (max_brightness == 3).
BRIGHTNESS_WORDS = {0: "off", 1: "low", 2: "med", 3: "high"}
# Aura effects exposed by `asusctl aura` on TUF/older-ROG hardware.
AURA_MODES = ("static", "breathe", "rainbow-cycle", "rainbow-wave", "highlight")
AURA_SPEEDS = ("low", "med", "high")            # breathe/rainbow/highlight
AURA_DIRECTIONS = ("up", "down", "left", "right")  # rainbow-wave only
_HEX_RE = re.compile(r"^[0-9a-fA-F]{6}$")

# Fans that asusctl exposes a per-fan curve for. "mid" only exists on some ROG.
CURVE_FANS = ("cpu", "gpu", "mid")
# A `--data` string: "30c:10%,55c:40%,..." (the '%' may be dropped → 0-255 raw,
# but we always emit it). Whitespace is stripped before matching.
_CURVE_DATA_RE = re.compile(r"^(\d{1,3}c:\d{1,3}%?)(,\d{1,3}c:\d{1,3}%?)*$")
# One fan entry inside `asusctl fan-curve --mod-profile <P>` RON-ish output.
# Fields always appear in this order, so non-greedy hops between them are safe.
_CURVE_BLOCK_RE = re.compile(
    r"fan:\s*(\w+).*?pwm:\s*\(([^)]*)\).*?temp:\s*\(([^)]*)\)"
    r".*?enabled:\s*(true|false)",
    re.S,
)


@dataclass(frozen=True)
class ControlResult:
    ok: bool
    message: str


@dataclass(frozen=True)
class FanCurve:
    """One fan's temperature→PWM curve for a given performance profile."""

    profile: str
    fan: str                              # "CPU" | "GPU" | "MID"
    points: tuple[tuple[int, int], ...]   # (temp °C, pwm 0-255), low→high
    enabled: bool

    def pwm_pcts(self) -> list[int]:
        """PWM duty as 0-100% (asusctl's `--data` unit)."""
        return [round(p / 255 * 100) for _, p in self.points]

    def data_str(self) -> str:
        """Render back to an asusctl `--data` string."""
        return ",".join(f"{t}c:{round(p / 255 * 100)}%" for t, p in self.points)


class Controller:
    def __init__(self, hw: HardwareMap) -> None:
        self.hw = hw
        self._asusctl = shutil.which("asusctl")
        self._pkexec = shutil.which("pkexec")
        self._sudo = shutil.which("sudo")

    # -- process plumbing -------------------------------------------------

    def _run(self, argv: list[str], timeout: int = 10) -> tuple[int, str, str]:
        try:
            p = subprocess.run(
                argv, capture_output=True, text=True, timeout=timeout
            )
            return p.returncode, p.stdout.strip(), p.stderr.strip()
        except subprocess.TimeoutExpired:
            return 124, "", f"timed out after {timeout}s"
        except OSError as exc:
            return 127, "", str(exc)

    def _asusctl_run(self, args: list[str], timeout: int = 10) -> ControlResult:
        if not self._asusctl:
            return ControlResult(False, "asusctl not installed")
        rc, out, err = self._run([self._asusctl, *args], timeout)
        if rc == 0:
            return ControlResult(True, out or "ok")
        return ControlResult(False, (err or out or f"asusctl rc={rc}").splitlines()[0])

    def _priv_write(self, path, value: str) -> ControlResult:
        """Escalated write to a root-owned sysfs node (fallback path)."""
        cmd = f"printf %s {value!r} > {str(path)!r}"
        if self._pkexec:
            rc, _out, err = self._run([self._pkexec, "sh", "-c", cmd], timeout=60)
            tool = "pkexec"
        elif self._sudo:
            # -n: never prompt; a TUI has no tty for an interactive password.
            rc, _out, err = self._run([self._sudo, "-n", "sh", "-c", cmd])
            tool = "sudo -n"
        else:
            return ControlResult(False, "no asusctl, pkexec or sudo available")
        if rc == 0:
            return ControlResult(True, f"wrote {value} via {tool}")
        return ControlResult(False, f"{tool} failed: {err or rc}")

    # -- power profiles ---------------------------------------------------

    def list_profiles(self) -> list[str]:
        if self._asusctl:
            rc, out, _ = self._run([self._asusctl, "profile", "list"])
            if rc == 0 and out:
                names = [ln.strip() for ln in out.splitlines() if ln.strip()]
                if names:
                    return names
        # Fallback: derive from the ACPI choices file.
        choices_path = ACPI_PROFILE.with_name("platform_profile_choices")
        try:
            raw = choices_path.read_text().split()
        except OSError:
            return list(PROFILE_TO_ACPI)
        return [c.capitalize() for c in raw]

    def set_profile(self, name: str) -> ControlResult:
        if self._asusctl:
            res = self._asusctl_run(["profile", "set", name])
            if res.ok:
                return ControlResult(True, f"profile → {name}")
            # fall through to sysfs if the daemon rejected it
        token = PROFILE_TO_ACPI.get(name, name.lower())
        res = self._priv_write(ACPI_PROFILE, token)
        return ControlResult(res.ok, f"profile → {name}" if res.ok else res.message)

    # -- battery charge limit --------------------------------------------

    def set_charge_limit(self, percent: int) -> ControlResult:
        percent = max(20, min(100, int(percent)))
        if self._asusctl:
            res = self._asusctl_run(["battery", "limit", str(percent)])
            if res.ok:
                return ControlResult(True, f"charge limit → {percent}%")
        if self.hw.charge_limit_node is None:
            return ControlResult(False, "no charge-limit node on this machine")
        res = self._priv_write(self.hw.charge_limit_node, str(percent))
        return ControlResult(res.ok,
                             f"charge limit → {percent}%" if res.ok else res.message)

    # -- keyboard brightness ---------------------------------------------

    def set_brightness(self, level: int) -> ControlResult:
        level = max(0, min(self.hw.kbd_max_brightness or 3, int(level)))
        word = BRIGHTNESS_WORDS.get(level, "med")
        if self._asusctl:
            res = self._asusctl_run(["leds", "set", word])
            if res.ok:
                return ControlResult(True, f"brightness → {word}")
        if not self.hw.has_kbd_backlight:
            return ControlResult(False, "no keyboard backlight node")
        res = self._priv_write(KBD_LED / "brightness", str(level))
        return ControlResult(res.ok,
                             f"brightness → {word}" if res.ok else res.message)

    # -- aura / RGB -------------------------------------------------------

    def set_aura(
        self,
        mode: str,
        colour: str | None = None,
        colour2: str | None = None,
        speed: str = "med",
        direction: str = "right",
        zone: str | None = None,
    ) -> ControlResult:
        """Apply an Aura effect, building the exact args each mode requires.

        The CLI is strict: ``breathe`` needs two colours *and* a speed,
        ``rainbow-cycle`` needs a speed, ``rainbow-wave`` needs a direction and
        speed, ``highlight`` needs a colour and speed, ``static`` just a colour.
        Missing a required flag makes asusctl error out, so we assemble exactly
        the right set per mode rather than a one-size-fits-all command.
        """
        if mode not in AURA_MODES:
            return ControlResult(False, f"unknown aura mode '{mode}'")
        if not self._asusctl:
            return ControlResult(False, "aura modes require asusctl/asusd")
        speed = speed if speed in AURA_SPEEDS else "med"
        direction = direction if direction in AURA_DIRECTIONS else "right"

        def clean(c: str | None, default: str) -> str | None:
            c = (c or default).lstrip("#")
            return c if _HEX_RE.match(c) else None

        args = ["aura", mode]
        if mode == "static":
            c = clean(colour, "ff00ff")
            if c is None:
                return ControlResult(False, f"bad hex colour '{colour}'")
            args += ["-c", c]
        elif mode == "highlight":
            c = clean(colour, "ff00ff")
            if c is None:
                return ControlResult(False, f"bad hex colour '{colour}'")
            args += ["-c", c, "--speed", speed]
        elif mode == "breathe":
            c1, c2 = clean(colour, "ff00ff"), clean(colour2, "00ffff")
            if c1 is None or c2 is None:
                return ControlResult(False, "bad hex colour (need two for breathe)")
            args += ["--colour", c1, "--colour2", c2, "--speed", speed]
        elif mode == "rainbow-cycle":
            args += ["--speed", speed]
        elif mode == "rainbow-wave":
            args += ["--direction", direction, "--speed", speed]

        if zone is not None and str(zone) != "":
            args += ["--zone", str(zone)]
        res = self._asusctl_run(args)
        return ControlResult(res.ok,
                             f"aura → {mode}" if res.ok else res.message)

    # -- fan curves -------------------------------------------------------

    def get_fan_curves(self, profile: str) -> list[FanCurve]:
        """Parse the active curves for *profile* (read-only; no privilege)."""
        if not self._asusctl:
            return []
        rc, out, _ = self._run(
            [self._asusctl, "fan-curve", "--mod-profile", profile]
        )
        if rc != 0 or not out:
            return []
        return self._parse_curves(profile, out)

    @staticmethod
    def _parse_curves(profile: str, text: str) -> list[FanCurve]:
        curves: list[FanCurve] = []
        for m in _CURVE_BLOCK_RE.finditer(text):
            fan = m.group(1).upper()
            try:
                pwm = [int(x) for x in m.group(2).split(",") if x.strip()]
                temp = [int(x) for x in m.group(3).split(",") if x.strip()]
            except ValueError:
                continue
            points = tuple(zip(temp, pwm))
            if points:
                curves.append(FanCurve(profile, fan, points, m.group(4) == "true"))
        return curves

    def set_fan_curve(self, profile: str, fan: str, data: str,
                      enable: bool = True) -> ControlResult:
        """Write a new temp→% curve for one fan on *profile*.

        With ``enable`` (the default) the per-fan curve is also switched on, so
        a single edit takes effect. Callers writing several fans at once pass
        ``enable=False`` and enable them together afterwards — one daemon call
        instead of one per fan.
        """
        fan = fan.lower()
        if fan not in CURVE_FANS:
            return ControlResult(False, f"unknown fan '{fan}'")
        if not self._asusctl:
            return ControlResult(False, "fan curves require asusctl/asusd")
        data = data.replace(" ", "")
        if not _CURVE_DATA_RE.match(data):
            return ControlResult(False, "bad curve data (use 30c:10%,55c:40%,…)")
        res = self._asusctl_run(
            ["fan-curve", "--mod-profile", profile, "--fan", fan, "--data", data]
        )
        if not res.ok:
            return res
        if enable:
            # A written curve does nothing until its per-fan curve is enabled.
            self._asusctl_run(["fan-curve", "--mod-profile", profile,
                               "--fan", fan, "--enable-fan-curve", "true"])
        return ControlResult(True, f"{fan.upper()} curve set on {profile}")

    def set_curve_enabled(self, profile: str, enabled: bool,
                          fan: str | None = None) -> ControlResult:
        """Toggle custom curve(s) on a profile (firmware default when off)."""
        if not self._asusctl:
            return ControlResult(False, "fan curves require asusctl/asusd")
        val = "true" if enabled else "false"
        if fan:
            args = ["fan-curve", "--mod-profile", profile, "--fan", fan.lower(),
                    "--enable-fan-curve", val]
        else:
            args = ["fan-curve", "--mod-profile", profile, "--enable-fan-curves", val]
        res = self._asusctl_run(args)
        verb = "enabled" if enabled else "disabled"
        return ControlResult(res.ok,
                             f"{profile} curves {verb}" if res.ok else res.message)

    def reset_fan_curve(self, profile: str) -> ControlResult:
        """Restore the firmware-default curve for *profile*."""
        if not self._asusctl:
            return ControlResult(False, "fan curves require asusctl/asusd")
        res = self._asusctl_run(["fan-curve", "--mod-profile", profile, "--default"])
        return ControlResult(res.ok,
                             f"{profile} curve reset to default" if res.ok
                             else res.message)
