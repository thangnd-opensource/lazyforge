"""Records the README demo (docs/demo.gif) against the fake server in server.py.

    cargo build --release
    python3 scripts/demo/record.py      # needs `agg` (brew install agg) for the GIF

lazyforge runs in a pseudo-terminal with a throwaway config directory, so the recording
never uses your accounts, tokens or keychain. Keys are pressed on a fixed schedule; the
output is saved as an asciicast (docs/demo.cast) and turned into a GIF with agg.
"""

import codecs
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / "target" / "release" / "lazyforge"
CAST = ROOT / "docs" / "demo.cast"
GIF = ROOT / "docs" / "demo.gif"
PORT = 8080
HOST = f"github.acme.localhost:{PORT}"
COLS, ROWS = 118, 30

# (seconds to wait first, keys). One step per key, so each change is visible.
SCRIPT = [
    (3.0, "j"), (0.7, "j"), (0.7, "k"), (0.7, "k"),      # the repository's pull requests
    (0.9, "]"), (1.4, "["),                              # another tab, and back
    (1.2, "\r"),                                         # open #128: overview
    (2.6, "]"),                                          # files and diff
    (1.6, "\r"),                                         # focus the diff...
    (0.8, "j"), (0.25, "j"), (0.25, "j"), (0.25, "j"), (0.25, "j"),  # ...and scroll it
    (1.0, "\x1b"), (0.6, "j"),                           # back to the file list, next file
    (1.4, "]"),                                          # discussion
    (2.4, "]"),                                          # Actions: the run's jobs
    (1.4, "j"), (0.5, "j"), (0.8, "\r"),                 # the failed job's log
    (3.2, "\x1b"),                                       # back to the list
    (1.0, "2"),                                          # view 2: Actions runs
    (2.4, "3"),                                          # view 3: branches
    (1.6, "j"), (0.8, "\r"),                             # compare feat/rate-limit with main
    (2.6, "b"),                                          # ...or with another branch
    (1.0, "r"), (0.25, "e"), (0.25, "l"),                # type to filter
    (1.2, "\r"),                                         # compare with release/1.4
    (2.4, "\x1b"), (1.0, "1"),                           # back to pull requests
    (2.0, "q"),
]


def main():
    if not BINARY.exists():
        sys.exit(f"{BINARY} not found: run `cargo build --release` first")
    server = subprocess.Popen([sys.executable, str(Path(__file__).with_name("server.py")), str(PORT)],
                              stdout=subprocess.DEVNULL)
    time.sleep(0.8)
    try:
        with tempfile.TemporaryDirectory(prefix="lazyforge-demo-") as tmp:
            events = record(Path(tmp))
    finally:
        server.terminate()

    CAST.parent.mkdir(exist_ok=True)
    with CAST.open("w") as cast:
        header = {"version": 2, "width": COLS, "height": ROWS, "env": {"TERM": "xterm-256color"}}
        cast.write(json.dumps(header) + "\n")
        for at, text in events:
            cast.write(json.dumps([round(at, 3), "o", text]) + "\n")
    print(f"wrote {CAST}")
    subprocess.run(["agg", "--theme", "github-dark", "--font-size", "15", "--idle-time-limit", "3",
                    "--last-frame-duration", "2", str(CAST), str(GIF)], check=True)
    print(f"wrote {GIF}")


def record(tmp: Path):
    config_dir = tmp / "config" / "lazyforge"
    config_dir.mkdir(parents=True)
    # Named like the host, so the header shows the server once.
    (config_dir / "config.toml").write_text(
        f'[profiles."{HOST}"]\nhost = "http://{HOST}"\nkind = "github"\ntoken_env = "DEMO_TOKEN"\n')
    env = {
        "PATH": os.environ["PATH"], "HOME": str(tmp), "TERM": "xterm-256color", "LANG": "en_US.UTF-8",
        "XDG_CONFIG_HOME": str(tmp / "config"), "XDG_STATE_HOME": str(tmp / "state"),
        "DEMO_TOKEN": "demo-token",
    }
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(str(BINARY), [str(BINARY), "--profile", HOST, "--project", "acme/api"], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    events, start = [], time.monotonic()

    def pump(seconds):
        end = time.monotonic() + seconds
        while (left := end - time.monotonic()) > 0:
            if select.select([fd], [], [], min(left, 0.05))[0]:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return False
                if text := decoder.decode(data):
                    events.append((time.monotonic() - start, text))
        return True

    for wait, key in SCRIPT:
        if not pump(wait):
            break
        os.write(fd, key.encode())
    pump(1.5)
    os.waitpid(pid, 0)
    return start_at_first_screen(events)


def start_at_first_screen(events):
    """Drops the blank start: everything up to the first drawn list becomes frame 0, so the
    GIF (and its still preview) opens on the app rather than on an empty terminal."""
    first = next(i for i, (_, text) in enumerate(events) if "Updated" in text)
    offset = events[first][0]
    head = "".join(text for _, text in events[: first + 1])
    return [(0.0, head)] + [(at - offset, text) for at, text in events[first + 1 :]]


if __name__ == "__main__":
    main()
