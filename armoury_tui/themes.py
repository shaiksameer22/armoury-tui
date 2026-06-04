"""Colour themes for the TUI.

A :class:`Theme` carries two things: the **Rich palette** used by every
renderable in ``render.py`` (panels, meters, graphs, gauges, tables), and the
**Textual design tokens** used by the built-in chrome (header, footer, tab bar,
buttons, inputs, the process table). Keeping both in one object means a theme
is a single source of truth — ``render.apply_theme`` reassigns the palette and
``App.theme`` switches the chrome, and the two stay in sync.

The default ``cyberpunk`` theme reproduces the original hardcoded colours
exactly, so switching themes off and back on is lossless.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Theme:
    key: str
    label: str
    # Rich palette (render.py globals)
    neon: str        # primary accent
    cyan: str
    magenta: str
    amber: str
    red: str
    dim: str
    text: str
    blue: str
    # Textual chrome tokens
    background: str
    surface: str     # panel / cell background
    panel: str       # header / footer / table-header background


THEMES: dict[str, Theme] = {
    "cyberpunk": Theme(
        "cyberpunk", "Cyberpunk",
        neon="#00ff9c", cyan="#36f9f6", magenta="#ff2e88", amber="#f3d000",
        red="#ff3355", dim="#4b5263", text="#c9d1d9", blue="#5ac8fa",
        background="#0a0e14", surface="#0c1320", panel="#0d1b2a",
    ),
    "synthwave": Theme(
        "synthwave", "Synthwave",
        neon="#ff6ac1", cyan="#36f9f6", magenta="#b967ff", amber="#ffcc55",
        red="#ff4d6d", dim="#5b5277", text="#f0e6ff", blue="#7aa2ff",
        background="#1a1033", surface="#241546", panel="#2a1850",
    ),
    "matrix": Theme(
        "matrix", "Matrix",
        neon="#00ff66", cyan="#33ff99", magenta="#88ff88", amber="#aaff55",
        red="#ff5555", dim="#1f5f3f", text="#b8ffcf", blue="#33ffaa",
        background="#000800", surface="#001a0d", panel="#002613",
    ),
    "amber": Theme(
        "amber", "Amber CRT",
        neon="#ffb000", cyan="#ffcc66", magenta="#ff8c42", amber="#ffd700",
        red="#ff5e3a", dim="#6b4a1f", text="#ffe6b3", blue="#ffaa55",
        background="#140d00", surface="#1f1500", panel="#2a1d00",
    ),
    "ice": Theme(
        "ice", "Ice",
        neon="#5ac8fa", cyan="#7fffd4", magenta="#a0c4ff", amber="#ffd166",
        red="#ff6b6b", dim="#46586b", text="#e0f0ff", blue="#4d96ff",
        background="#08111a", surface="#0c1a26", panel="#102433",
    ),
}

DEFAULT = "cyberpunk"


def get(key: str | None) -> Theme:
    """Return a theme by key, falling back to the default."""
    return THEMES.get(key or DEFAULT, THEMES[DEFAULT])
