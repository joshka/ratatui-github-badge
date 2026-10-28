#!/usr/bin/env bash
# Restore a full flash backup made by scripts/backup.sh, putting the badge back exactly as it
# was (stock firmware, apps, settings and files).
#
#   scripts/restore.sh backups/20261027-115637            check everything, ask, then write
#   scripts/restore.sh --dry-run backups/20261027-115637  only run the checks
#
# Refuses if the hash or size is wrong, or if the connected badge isn't the one the backup
# came from (each backup holds that badge's Wi-Fi password). --other-badge overrides that.
set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

dry_run=false other_badge=false yes=false dir=""
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=true ;;
    --other-badge) other_badge=true ;;
    --yes) yes=true ;;
    -*) die "unknown option $arg" ;;
    *) dir="$arg" ;;
  esac
done
[[ -n "$dir" ]] || { sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

bin="$dir/flash-full.bin"
for f in "$bin" "$dir/flash-full.sha256" "$dir/info.txt"; do
  [[ -f "$f" ]] || die "missing $f; is $dir a flash backup from scripts/backup.sh?"
done
command -v picotool >/dev/null || die "picotool not found (brew install picotool)"

size="$(stat -f %z "$bin")"
[[ "$size" == 16777216 ]] || die "$bin is $size bytes, expected 16777216"
echo "Checking SHA-256…"
(cd "$dir" && shasum -a 256 -c flash-full.sha256 >/dev/null) || die "SHA-256 mismatch; the backup is damaged"

# The badge's USB serial number is its chip ID, both in BOOTSEL and in the Ratatui firmware,
# so picotool only ever touches the badge this backup came from.
chipid="$(sed -n 's/^ *chipid: *0x\([0-9a-fA-F]*\).*/\1/p' "$dir/info.txt" | head -1 | tr a-f A-F)"
[[ -n "$chipid" ]] || die "no chipid in $dir/info.txt"
select=(--ser "$chipid")
$other_badge && select=()

cmd=(picotool load -u -v -x "$bin" -o 0x10000000 "${select[@]}")
echo "Backup OK (16 MB, hash matches, from badge $chipid)."
if $dry_run; then
  echo "Would run: ${cmd[*]}"
  exit 0
fi

if ! $yes; then
  read -r -p "Overwrite the whole badge flash with this backup? [y/N] " answer
  [[ "$answer" == [yY]* ]] || die "cancelled"
fi

in_bootsel() { picotool info "${select[@]}" 2>/dev/null | grep -q RP2350; }
# The Ratatui firmware answers picotool -f (which can't be combined with --ser, hence the
# check afterwards). Stock MicroPython doesn't; ask it to reboot into BOOTSEL itself.
if ! in_bootsel && ! { picotool reboot -f -u >/dev/null 2>&1 && sleep 2; }; then
  ports=(/dev/cu.usbmodem*)
  port="${ports[0]}"; [[ -e "$port" ]] || port=""
  if [[ -n "$port" ]] && command -v uvx >/dev/null; then
    uvx mpremote connect "$port" exec "import machine; machine.bootloader()" >/dev/null 2>&1 || true
    sleep 3
  fi
fi
in_bootsel || die "badge $chipid isn't in BOOTSEL mode \
(hold BOOT while pressing RESET). If this backup is from another badge, see --other-badge."

echo "Writing and verifying (unchanged sectors are skipped)…"
"${cmd[@]}" || die "picotool failed; the badge may be half-written. Run this again."
echo "Restored. The badge is rebooting into the backed-up firmware."
