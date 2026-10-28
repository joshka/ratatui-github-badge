# Ratatui on the GitHub Universe badge

Rust firmware that turns the GitHub Universe 2026 badge into a [Ratatui] showcase. Every
pixel on screen is a terminal cell, drawn by [Mousefood] onto the badge's LCD.

- **Pages:** the Ratatui mascot and wordmark, live sensor charts, a tilt maze, a widget tour
  with [tachyonfx] effects, QR codes for ratatui.rs and the GitHub repo, and your contact
  details.
- **Interaction:** the screen flips 180° when you lift the badge to read it, and parts of the
  UI drift with the tilt. Hold Select for a second to start a hands-free booth demo, which
  includes a [tui-big-text] marquee, Doom fire and Conway's Life.
- **USB:** the badge shows up as a serial port. `cargo badge` mirrors its screen in your
  terminal (as a Ratatui app, of course) and drives it with the arrow keys.

The badge is a [Pimoroni Tufty 2350] variant: an RP2350B with 16 MB flash, a 320×240 LCD,
five buttons, an IMU, touch pads, four case LEDs and a battery. A terminal of 6×10 px cells
gives 53×24 cells.

[Ratatui]: https://ratatui.rs
[Mousefood]: https://github.com/ratatui/mousefood
[tachyonfx]: https://github.com/ratatui/tachyonfx
[tui-big-text]: https://github.com/ratatui/tui-widgets
[Pimoroni Tufty 2350]: https://shop.pimoroni.com/products/tufty-2350

## Before you flash: back up the stock firmware

Flashing replaces the stock firmware. Take a full backup first, so you can put the badge back
exactly as it came:

```sh
scripts/backup.sh flash      # ~75 s: dumps and verifies all 16 MB into backups/<timestamp>/
scripts/restore.sh backups/<timestamp>   # later, to go back to stock
```

> [!WARNING]
> A backup contains your Wi-Fi password and the contact details you entered in Settings
> (`/secrets.py` on the badge), plus app state. Never share or commit one. `backups/` is
> gitignored, and the script makes the files private and read-only.

What the scripts check:

- **Backup:**
  - exactly one RP2350 is connected, and it has 16 MB flash
  - the dump verifies against the badge and is the right size
  - it records a SHA-256 and `picotool info` next to the dump, and never overwrites an
    existing backup
- **Restore:**
  - the size and SHA-256 match
  - the connected badge's chip ID matches the backup (`--other-badge` overrides), since a
    backup carries one badge's Wi-Fi password
  - it asks before writing (`--dry-run` only checks), always verifies, and skips sectors
    that haven't changed

`scripts/backup.sh files` copies the badge's USB drive (`/system`) instead, so you can read
the stock apps. It can't be restored from.

Getting into BOOTSEL (the RP2350's USB flashing mode) is handled for you:

- **Ratatui firmware:** it answers `picotool -f` and reboots itself. You can also hold
  Left + Select for a second.
- **Stock firmware:** the scripts ask MicroPython to reboot with
  `mpremote … exec "import machine; machine.bootloader()"`. The serial error afterwards is
  expected.
- **Anything else, or a frozen badge:** hold BOOT, tap RESET, then release BOOT.

## Build and flash

You need:
- Rust with the `thumbv8m.main-none-eabihf` target
- [picotool] 2.x (`brew install picotool`)
- [uv], only for the stock-firmware steps above

```sh
rustup target add thumbv8m.main-none-eabihf
cp badge.example.toml badge.toml   # your name, links and photo; see below
cargo run --release                # builds, reboots the badge into BOOTSEL, flashes, verifies
```

The runner is `picotool load -u -v -x -f -t elf`, so a rebuild only rewrites the sectors that
changed (about 700 KB). The firmware only touches the first ~700 KB of flash, leaving the
stock filesystems in place, but restore from your backup to get the stock firmware back.

[picotool]: https://github.com/raspberrypi/picotool
[uv]: https://docs.astral.sh/uv/

## Your details

`badge.toml` (gitignored) holds everything personal:
- your name, handle and role
- the status lines you cycle with Up/Down
- an optional photo (any PNG or JPEG)
- the QR codes on the "me" page: URLs, or a vCard so phones offer to save you as a contact

`build.rs` turns it into Rust at build time, so changing it means rebuilding and flashing.
Without one, the build uses `badge.example.toml` and warns. Point `BADGE_CONFIG` at a file to
keep it somewhere else.

## Host tools

```sh
cargo badge             # live mirror + log. Arrows/Enter drive the badge, d starts the demo
cargo badge log         # follow the event and perf log
cargo badge screen      # print the screen as text
cargo badge key left    # press a button remotely
cargo badge demo
cargo badge bootsel
cargo preview /tmp/out  # render every page to PNG on your laptop, no badge needed
cargo test-ui           # host tests for the UI crate
```

The badge enumerates as a USB CDC serial port with the Pico SDK's IDs (`2E8A:0009`) and its
chip ID as the serial number. Those IDs are what `picotool -f` looks for.

## Layout

| Path | What |
|---|---|
| `src/` | Firmware: display driver, sensors, LEDs, USB, the main loop |
| `ui/` | The Ratatui app. Hardware-free, so it runs in host tests and the preview |
| `host/` | `cargo badge`, the USB mirror and remote control |
| `scripts/` | Backup and restore |

## Hardware notes

Worked out from the [Tufty 2350 repo], the stock firmware, and trial and error.

| Function | Pins |
|---|---|
| LCD, ST7789, 8-bit parallel (8080) | CS 27, DC 28, WR 30, RD 31, D0–D7 32–39, backlight 26 |
| Power enable | 41: **must be driven high**, or the LCD, IMU and touch stay unpowered |
| Buttons, active low | Up 11, Down 6, Left (A) 7, Right (B) 9, Select (C) 10 |
| Internal I2C1, 400 kHz | SDA 18, SCL 19: LSM6DS3TR-C IMU at `0x6A`, CAP1208 touch at `0x28` |
| ADC | battery on 40 (×2 divider, by measurement), light sensor on 43 |
| Case LEDs | 0–3, PWM slices 0 and 1 |

Things that took a while:

- **The panel is natively 240×320 portrait.** The driver keeps a landscape framebuffer and
  sends dirty rectangles column by column, which is fast enough for most pages to run under
  16 ms per frame at 200 MHz.
- **WR timing:** bit-banging the bus faster than ~8 cycles per write strobe garbles pixels.
- **USB on the main executor stalls** whenever a frame takes tens of milliseconds. It runs on
  an interrupt executor instead.
- **Stock flash layout:**
  - `0x10000000`: MicroPython (~1.5 MB)
  - `0x10200000`: a 1 MB ROMFS
  - `0x10300000`–`0x11000000`: a 13 MB FAT filesystem

**The stock firmware** is Pimoroni's MicroPython build for the `pimoroni_tufty2350` board. It
reports itself as "GitHub Badger 2350". It includes:
- the badgeware libraries, Wi-Fi, BLE and NEC IR
- "sparkle", the signed light shows
- the apps: a GitHub profile badge, contact pairing, Mona Pet, Mona Paint, Flappy Mona,
  Merge Conflict, a roguelike, an ISS tracker and more

The apps are plain Python on the badge's filesystem. See [badger/home] for the official
docs and source.

[Tufty 2350 repo]: https://github.com/pimoroni/tufty2350
[badger/home]: https://github.com/badger/home

## License

MIT or Apache-2.0, at your option.
