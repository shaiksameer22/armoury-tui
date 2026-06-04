"""CLI entry point: ``python -m armoury_tui``.

Three modes:

    (default)      launch the full Textual dashboard
    --probe        print the discovered HardwareMap and exit (no TUI)
    --once         print one plain-text telemetry snapshot and exit

``--probe`` / ``--once`` are invaluable for debugging on a headless session or
over SSH without a usable terminal UI, and they exercise the data layer with
zero Textual involvement.
"""

from __future__ import annotations

import argparse
import sys


def _probe() -> int:
    from .scanner import scan
    hw = scan()
    print("=== armoury-tui hardware probe ===")
    for line in hw.summary_lines():
        print("  " + line)
    print("\n  hwmon nodes discovered:")
    for name, path in sorted(hw.hwmon_by_name.items()):
        print(f"    {name:<28} {path}")
    return 0


def _once() -> int:
    from .scanner import scan
    from .telemetry import Telemetry
    from .render import fmt_rate, human_bytes
    import time

    hw = scan()
    tel = Telemetry(hw)
    # Two reads 0.4s apart so CPU% and network rates are meaningful.
    tel.snapshot()
    time.sleep(0.4)
    s = tel.snapshot()

    print(f"profile     : {s.profile}")
    print(f"cpu         : {s.cpu.overall:.1f}%  "
          f"{(s.cpu.temp_c or 0):.0f}°C  {s.cpu.cores} cores")
    if s.gpu.present:
        print(f"gpu         : {s.gpu.name}  {s.gpu.util or 0:.0f}%  "
              f"{s.gpu.temp_c or 0:.0f}°C  {s.gpu.power_w or 0:.1f}W")
    for f in s.fans:
        print(f"fan         : {f.label:<10} {f.rpm} rpm")
    print(f"memory      : {human_bytes(s.mem.used)}/{human_bytes(s.mem.total)} "
          f"({s.mem.percent:.0f}%)")
    if s.storage.nvme_temp_c is not None:
        print(f"nvme        : {s.storage.nvme_temp_c:.0f}°C")
    b = s.battery
    if b.present:
        rate = f"{b.rate_w:+.1f}W" if b.rate_w is not None else "n/a"
        print(f"battery     : {b.percent:.0f}%  {b.status}  rate={rate}  "
              f"limit={b.charge_limit}%")
    print(f"kbd light   : {s.kbd_brightness}")
    for i in s.net:
        if i.is_up and not i.is_virtual:
            print(f"net         : {i.name:<8} ↓{fmt_rate(i.down_bps)} "
                  f"↑{fmt_rate(i.up_bps)}")
    top = ", ".join(f"{p.name}({p.cpu:.0f}%)" for p in s.procs_cpu[:3])
    print(f"top cpu     : {top}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="armoury-tui",
        description="ASUS Armoury-Crate-style monitor/control TUI for Linux.",
    )
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--probe", action="store_true",
                       help="print discovered hardware map and exit")
    group.add_argument("--once", action="store_true",
                       help="print one telemetry snapshot and exit")
    parser.add_argument("-i", "--interval", type=float, default=1.0,
                        metavar="SEC", help="UI refresh interval (default 1.0s)")
    parser.add_argument("--log", metavar="CSV",
                        help="append a telemetry CSV row each tick to this file")
    args = parser.parse_args(argv)

    if args.probe:
        return _probe()
    if args.once:
        return _once()

    try:
        from .app import run
    except ModuleNotFoundError as exc:
        print(f"error: missing dependency ({exc.name}). "
              f"Install with:  pip install -r requirements.txt", file=sys.stderr)
        return 1
    run(refresh=max(0.25, args.interval), log_path=args.log)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
