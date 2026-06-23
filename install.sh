#!/usr/bin/env bash
# armoury-tui installer — works on any ASUS ROG/TUF laptop, any distro.
#
#   ./install.sh            # user install  → ~/.local/bin/armoury
#   ./install.sh --system   # system install → /usr/local/bin/armoury  (uses sudo)
#   ./install.sh --uninstall # remove what this script installed
#
# Builds a standalone release binary (no repo needed afterwards), drops a desktop
# entry, and checks for the asusd daemon the control features need. The tool
# auto-detects hardware, so it runs on AMD/Intel + NVIDIA/AMD ASUS laptops and
# degrades gracefully where a sensor or the daemon is absent.
set -euo pipefail

REPO="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
SYSTEM=0
UNINSTALL=0
for arg in "$@"; do
  case "$arg" in
    --system) SYSTEM=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

c_g=$'\e[32m'; c_y=$'\e[33m'; c_r=$'\e[31m'; c_d=$'\e[2m'; c_0=$'\e[0m'
say()  { printf '%s==>%s %s\n' "$c_g" "$c_0" "$*"; }
warn() { printf '%s!! %s%s\n' "$c_y" "$*" "$c_0"; }
err()  { printf '%sxx %s%s\n' "$c_r" "$*" "$c_0" >&2; }

if [ "$SYSTEM" = 1 ]; then
  BINDIR=/usr/local/bin
  DESKTOPDIR=/usr/share/applications
  SUDO="sudo"
else
  BINDIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
  DESKTOPDIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  SUDO=""
fi
DESKTOP="$DESKTOPDIR/armoury-tui.desktop"
BIN="$BINDIR/armoury"

# -- uninstall --------------------------------------------------------------
if [ "$UNINSTALL" = 1 ]; then
  say "Removing armoury-tui"
  $SUDO rm -fv "$BIN" "$DESKTOP"
  echo "Config left at ~/.config/armoury-tui/ (remove by hand if you want)."
  exit 0
fi

# -- 1. Rust toolchain ------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  warn "Rust toolchain (cargo) not found."
  read -rp "Install Rust now via rustup? [y/N] " ans
  if [[ "$ans" =~ ^[Yy]$ ]]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
  else
    err "cargo is required. Install Rust from https://rustup.rs and re-run."
    exit 1
  fi
fi

# -- 2. Build ---------------------------------------------------------------
say "Building release binary (this can take a minute the first time)…"
cargo build --release --manifest-path "$REPO/rust/Cargo.toml"

# -- 3. Install binary ------------------------------------------------------
say "Installing → $BIN"
$SUDO install -Dm755 "$REPO/rust/target/release/armoury-tui" "$BIN"

# -- 4. Desktop entry -------------------------------------------------------
say "Installing desktop entry → $DESKTOP"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
cat >"$tmp" <<EOF
[Desktop Entry]
Type=Application
Name=Armoury TUI
GenericName=ASUS laptop control
Comment=Telemetry and control for ASUS ROG/TUF laptops
Exec="$BIN"
Icon=utilities-terminal
Terminal=true
Categories=System;Monitor;HardwareSettings;
Keywords=asus;rog;tuf;armoury;fan;rgb;battery;
EOF
$SUDO install -Dm644 "$tmp" "$DESKTOP"

# -- 5. PATH check ----------------------------------------------------------
if ! printf '%s' "$PATH" | tr ':' '\n' | grep -qx "$BINDIR"; then
  warn "$BINDIR is not on your PATH."
  echo "   Add this to your shell rc (~/.bashrc, ~/.zshrc):"
  echo "       export PATH=\"$BINDIR:\$PATH\""
fi

# -- 6. asusd stack check (controls) ----------------------------------------
if command -v asusctl >/dev/null 2>&1 || systemctl is-active --quiet asusd 2>/dev/null; then
  say "asusd detected — control features (profile/charge/RGB/fans) available."
else
  warn "asusd not found. Telemetry works now; controls need the asus-linux stack:"
  if   command -v pacman >/dev/null 2>&1; then echo "   Arch:    sudo pacman -S asusctl   (or the AUR)"
  elif command -v dnf    >/dev/null 2>&1; then echo "   Fedora:  sudo dnf copr enable lukenukem/asus-linux && sudo dnf install asusctl"
  elif command -v apt    >/dev/null 2>&1; then echo "   Debian/Ubuntu: build from https://asus-linux.org/ (no official apt pkg)"
  fi
  echo "   Docs: https://asus-linux.org/  — then: systemctl enable --now asusd"
fi

say "Done. Run it with:  armoury        (or  armoury --probe  to see detected hardware)"
