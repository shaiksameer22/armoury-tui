"""Low-level sysfs / procfs access primitives.

Reads here are deliberately *total*: a missing file, a permission error or a
malformed value never raises -- it returns ``None`` (or a supplied default).
The TUI refresh loop calls these dozens of times per second, so a single
unreadable node must never be able to take the whole interface down.

Writing is handled separately in :mod:`armoury_tui.control`, because on a
locked-down ASUS laptop every interesting control node is root-only and must
go through a privilege-escalation path rather than a naive ``open(..., "w")``.
"""

from __future__ import annotations

import os
from pathlib import Path


def read_text(path: str | os.PathLike[str], default: str | None = None) -> str | None:
    """Return the stripped contents of *path*, or *default* on any failure."""
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            return fh.read().strip()
    except (OSError, ValueError):
        return default


def read_int(path: str | os.PathLike[str], default: int | None = None) -> int | None:
    """Read *path* as an integer (e.g. a millidegree/RPM sysfs node)."""
    raw = read_text(path)
    if raw is None:
        return default
    try:
        return int(raw)
    except ValueError:
        # Some nodes report hex or have trailing units; try a tolerant parse.
        try:
            return int(raw.split()[0], 0)
        except (ValueError, IndexError):
            return default


def read_float(path: str | os.PathLike[str], default: float | None = None) -> float | None:
    raw = read_text(path)
    if raw is None:
        return default
    try:
        return float(raw)
    except ValueError:
        return default


def read_milli(path: str | os.PathLike[str]) -> float | None:
    """Read a sysfs "millis" node (millidegrees / microvolts-style) ÷ 1000.

    hwmon temperatures and many power-supply fields are stored as the real
    value × 1000, so this is the single most common conversion in the tool.
    """
    val = read_int(path)
    return None if val is None else val / 1000.0


def exists(path: str | os.PathLike[str]) -> bool:
    return Path(path).exists()


def is_writable(path: str | os.PathLike[str]) -> bool:
    """True only if the current user can write *path* without escalation."""
    return os.access(path, os.W_OK)


def list_glob(pattern: str) -> list[Path]:
    """Sorted ``Path.glob`` from the filesystem root, swallowing errors."""
    try:
        # Anchor relative patterns at '/' so callers can pass sysfs globs.
        root = Path("/")
        return sorted(root.glob(pattern.lstrip("/")))
    except OSError:
        return []
