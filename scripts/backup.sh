#!/usr/bin/env bash
# Back up the GitHub Universe 2026 badge (RP2350B, 16 MB flash) before flashing anything else.
#
#   scripts/backup.sh flash   Full flash dump: firmware, apps and all filesystems. This is the
#                             one to restore from (scripts/restore.sh).
#   scripts/backup.sh files   Copy of the BADGER USB drive (/system) for reading the stock
#                             apps. Not restorable. Put the badge in USB drive mode first.
#
# Backups contain your Wi-Fi password and contact details (/secrets.py). Never share or commit
# them; backups/ is gitignored and every file is made private and read-only.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
dir="$repo/backups/$(date +%Y%m%d-%H%M%S)"
umask 077

die() { echo "error: $*" >&2; exit 1; }
warn_private() {
  echo
  echo "Backup written to $dir"
  echo "It contains your Wi-Fi password and contact details. Don't share or commit it."
}

# Get the badge into BOOTSEL: it may already be there, running the Ratatui firmware (which
# answers picotool -f), or running stock MicroPython (which needs mpremote).
bootsel() {
  if picotool info >/dev/null 2>&1 && picotool info | grep -q 'RP2350'; then return; fi
  if picotool reboot -f -u >/dev/null 2>&1; then sleep 2; return; fi
  local port ports
  ports=(/dev/cu.usbmodem*)
  port="${ports[0]}"; [[ -e "$port" ]] || port=""
  if [[ -n "$port" ]] && command -v uvx >/dev/null; then
    echo "Asking MicroPython on $port to reboot into BOOTSEL…"
    uvx mpremote connect "$port" exec "import machine; machine.bootloader()" >/dev/null 2>&1 || true
    sleep 3
  fi
  picotool info 2>/dev/null | grep -q 'RP2350' \
    || die "no badge in BOOTSEL mode. Hold the BOOT button while pressing RESET, then retry."
}

case "${1:-}" in
  flash)
    command -v picotool >/dev/null || die "picotool not found (brew install picotool)"
    bootsel
    count="$(picotool info 2>/dev/null | grep -c 'RP2350' || true)"
    [[ "$count" == 1 ]] || die "expected exactly one RP2350 in BOOTSEL mode, found $count"
    info="$(picotool info -a)"
    grep -q 'flash size: *16384K' <<<"$info" || die "expected 16 MB flash; is this the badge?"
    grep -q 'pico_board: *pimoroni_tufty2350' <<<"$info" \
      || echo "note: the board isn't reporting the stock pimoroni_tufty2350 firmware; backing up anyway"
    [[ -e "$dir" ]] && die "$dir already exists"
    mkdir -p "$dir"
    echo "$info" >"$dir/info.txt"
    echo "Reading 16 MB (about a minute)…"
    picotool save -r 0x10000000 0x11000000 "$dir/flash-full.bin"
    echo "Verifying against the badge…"
    picotool verify "$dir/flash-full.bin" -r 0x10000000 0x11000000 || die "verify failed; don't trust $dir"
    size="$(stat -f %z "$dir/flash-full.bin")"
    [[ "$size" == 16777216 ]] || die "dump is $size bytes, expected 16777216"
    (cd "$dir" && shasum -a 256 flash-full.bin >flash-full.sha256)
    chmod a-w "$dir"/*
    picotool reboot >/dev/null 2>&1 || true
    warn_private
    ;;
  files)
    [[ -d /Volumes/BADGER ]] || die "/Volumes/BADGER not mounted; choose USB drive mode on the badge first"
    [[ -e "$dir" ]] && die "$dir already exists"
    mkdir -p "$dir"
    rsync -a --exclude '.Spotlight-V100' --exclude '.fseventsd' --exclude '.Trashes' --exclude '._*' \
      /Volumes/BADGER/ "$dir/system/"
    (cd "$dir/system" && find . -type f -exec shasum -a 256 {} + >../system.sha256)
    chmod -R a-w "$dir"
    warn_private
    echo "This copy is for reading the stock apps. To restore, use a flash backup."
    ;;
  *)
    sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
