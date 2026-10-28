#!/usr/bin/env bash
# Back up the GitHub Universe 2026 badge (RP2350B, 16 MB flash).
#   ./backup.sh msc    - badge in Mass Storage mode: copy /Volumes/BADGER (= /system only)
#   ./backup.sh flash  - badge in BOOTSEL mode: dump full 16 MB flash (firmware + all filesystems)
set -euo pipefail
dir="$(cd "$(dirname "$0")" && pwd)/backups/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$dir"
case "${1:-}" in
  msc)
    rsync -a --exclude '.Spotlight-V100' --exclude '.fseventsd' --exclude '.Trashes' /Volumes/BADGER/ "$dir/system/"
    (cd "$dir/system" && find . -type f -exec shasum -a 256 {} + > ../system.sha256)
    ;;
  flash)
    picotool info -a > "$dir/info.txt"
    picotool save -r 0x10000000 0x11000000 "$dir/flash-full.bin"
    picotool verify "$dir/flash-full.bin" -r 0x10000000 0x11000000 || true
    shasum -a 256 "$dir/flash-full.bin" > "$dir/flash-full.sha256"
    ;;
  *) echo "usage: $0 msc|flash" >&2; exit 2 ;;
esac
echo "backup written to $dir"
