# /// script
# requires-python = ">=3.11"
# dependencies = ["pyserial"]
# ///
"""Talk to the badge over USB serial.

    uv run tools/badge.py log            # follow the event log
    uv run tools/badge.py mirror         # live screen mirror; arrows/enter drive the badge, q quits
    uv run tools/badge.py screen         # print one text snapshot
    uv run tools/badge.py key left       # press a key remotely
    uv run tools/badge.py demo           # start the booth demo
    uv run tools/badge.py bootsel        # reboot into BOOTSEL
"""
import glob, os, select, sys, termios, time, tty

import serial


def port():
    ports = sorted(glob.glob("/dev/cu.usbmodem*"))
    if not ports:
        sys.exit("badge not found (no /dev/cu.usbmodem*)")
    return serial.Serial(ports[0], timeout=0)


def send(s, line):
    s.write((line + "\n").encode())


def follow(s, seconds=None):
    end = time.time() + seconds if seconds else None
    while end is None or time.time() < end:
        data = s.read(4096)
        if data:
            sys.stdout.buffer.write(data)
            sys.stdout.flush()
        else:
            time.sleep(0.01)


def mirror(s):
    keys = {b"\x1b[A": "up", b"\x1b[B": "down", b"\x1b[D": "left", b"\x1b[C": "right", b"\r": "select", b" ": "select"}
    old = termios.tcgetattr(sys.stdin)
    sys.stdout.write("\x1b[?25l\x1b[2J")
    send(s, "mirror")
    try:
        tty.setcbreak(sys.stdin)
        while True:
            if select.select([sys.stdin], [], [], 0)[0]:
                ch = os.read(sys.stdin.fileno(), 8)
                if ch in (b"q", b"\x03"):
                    break
                if ch in keys:
                    send(s, "key " + keys[ch])
            data = s.read(65536)
            if data:
                sys.stdout.buffer.write(data)
                sys.stdout.flush()
            else:
                time.sleep(0.005)
    finally:
        send(s, "mirror")
        termios.tcsetattr(sys.stdin, termios.TCSADRAIN, old)
        sys.stdout.write("\x1b[0m\x1b[?25h\n")


def main():
    cmd = sys.argv[1:] or ["log"]
    s = port()
    if cmd[0] == "log":
        follow(s)
    elif cmd[0] == "mirror":
        mirror(s)
    elif cmd[0] in ("screen", "bootsel", "key", "demo"):
        send(s, " ".join(cmd))
        follow(s, 0.5)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
