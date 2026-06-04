"""armoury_tui - a Linux TUI replacement for ASUS Armoury Crate.

Built and verified against an ASUS TUF Gaming A15 (FA506NFR):
AMD Ryzen (k10temp) + NVIDIA RTX 2050, asusd / asusctl ecosystem.

The package is split to mirror the three implementation modules:

    scanner.py    -> Module 1: hardware discovery (dynamic hwmon mapping)
    telemetry.py  -> live readers (CPU/GPU/fans/mem/storage/battery/net)
    control.py    -> Module 2: safe state controller (asusctl + sysfs fallback)
    app.py        -> Module 3: the reactive Textual render loop

Everything degrades gracefully: missing hardware/daemons yield ``None``
fields rather than crashes, so the tool stays usable on other ASUS models.
"""

__version__ = "1.0.0"
__all__ = ["__version__"]
