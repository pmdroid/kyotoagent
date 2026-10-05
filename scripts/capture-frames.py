#!/usr/bin/env python3
"""Screenshot the three kyotoagent session screens from a real terminal.

Runs `cargo run --example screens` under a pty at 76 by 24, waits for each
frame to settle, replays the output through a terminal emulator, and writes one
PNG per frame plus a single page holding all three.

The point is proof, not decoration: the pixels come from the bytes the real
terminal received, and the script checks the captured text against the golden
files in tests/screens before it writes anything. If the screen and the goldens
ever disagree, the script fails instead of producing a picture of the wrong
thing.

    python3 scripts/capture-frames.py                 # needs: pip install pyte
    python3 scripts/capture-frames.py --check         # verify only, no PNGs
    python3 scripts/capture-frames.py --out build/screens

Requires `pyte` and a Chromium binary. The PNG step shells out to Chromium; set
CHROME to point at a different one.
"""
import argparse
import fcntl
import glob
import html
import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import termios
import time

COLS, ROWS = 76, 24
# The example repaints only changed cells, so a frame is "settled" once the
# terminal has been silent this long.
QUIET_SECONDS = 0.45
PUMP_LIMIT = 4.0

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN_DIR = os.path.join(REPO, "tests", "screens")

BG_DEFAULT = "#000000"
FG_DEFAULT = "#c8d0e0"
NAMED = {
    "black": "#1c1f28", "red": "#e05c6c", "green": "#6cc47c",
    "brown": "#d8a657", "yellow": "#d8a657", "blue": "#6b9fe0",
    "magenta": "#c07be0", "cyan": "#5ec5cf", "white": "#d6dae4",
    "brightblack": "#6a7180", "default": None,
}
PALETTE = [
    "#1c1f28", "#e05c6c", "#6cc47c", "#d8a657",
    "#6b9fe0", "#c07be0", "#5ec5cf", "#d6dae4",
]
ORDER = [
    "waiting",
    "idle",
    "working",
    "closeout-run",
    "closeout-asked",
    "result-markdown",
    "closeout-requirements",
    "todos",
]
CAPTIONS = {
    "waiting": "A session is blocked on a write permission. The diff is on screen "
               "and the bottom line is how you answer it.",
    "idle": "The turn finished. The permission reads Allowed, the chosen answer is "
            "marked, and the proof lists what changed and every closeout check "
            "that ran.",
    "working": "Another session is still working. Its ask and current action remain visible. "
               "A closeout retry looks the same, because a closeout_run event draws no card.",
    "closeout-run": "The model asked for the check test by id. The harness built the "
                    "command, so the card names the argv it is about to run, and "
                    "nothing has started yet.",
    "closeout-asked": "The check used all three failed attempts. Another run is refused, "
                      "so the harness opens a question instead. There is no result card.",
    "result-markdown": "The result card draws the agent's markdown: a heading, a list, "
                       "a rust block, and a link.",
    "closeout-requirements": "Touched paths determine which checks are required. "
                             "Untouched checks say they will not run.",
    "todos": "The model is keeping a todo list. The right pane is open, the progress "
             "row names the current step, and the bar shows how much is left.",
}
CSS = """
:root { color-scheme: dark; }
body { background:#11131a; margin:0; padding:30px;
       font-family:system-ui,-apple-system,'Segoe UI',sans-serif; }
header { color:#e6ebf5; font-size:16px; font-weight:600; }
.note { color:#7c869c; font-size:12.5px; margin:6px 0 22px; }
h2 { color:#e6ebf5; font-size:13.5px; font-weight:600; margin:24px 0 9px;
     letter-spacing:.03em; }
.term { background:#000; padding:13px 15px; border-radius:9px;
        border:1px solid #262b38; box-shadow:0 10px 34px rgba(0,0,0,.55);
        display:inline-block; }
pre { margin:0; font-size:13px; line-height:1.30; white-space:pre;
      font-family:'DejaVu Sans Mono','Liberation Mono',monospace;
      font-variant-ligatures:none; }
"""


def fail(message):
    print(f"capture-frames: {message}", file=sys.stderr)
    raise SystemExit(1)


def find_chrome():
    if os.environ.get("CHROME"):
        return os.environ["CHROME"]
    for name in ("chromium", "chromium-browser", "google-chrome",
                 "google-chrome-stable"):
        path = shutil.which(name)
        if path:
            return path
    for pattern in ("chromium-*/chrome-linux64/chrome",
                    "chromium-*/chrome-linux/chrome"):
        hits = sorted(glob.glob(os.path.expanduser(
            f"~/.cache/ms-playwright/{pattern}")))
        if hits:
            return hits[-1]
    return None


def build_example():
    result = subprocess.run(
        ["cargo", "build", "--example", "screens"],
        cwd=REPO, capture_output=True, text=True)
    if result.returncode != 0:
        fail(f"cargo build --example screens failed:\n{result.stderr}")
    return os.path.join(REPO, os.environ.get("CARGO_TARGET_DIR", "target"), "debug", "examples", "screens")


def title_of(data):
    """The last `kyotoagent - <name>` window title in an OSC sequence, if any."""
    found = ""
    i = 0
    while i < len(data):
        if data[i:i + 2] == b"\x1b]":
            end = data.find(b"\x07", i)
            if end == -1:
                break
            body = data[i + 2:end].decode("utf-8", "replace")
            text = body.split(";", 1)[1] if ";" in body else ""
            if text.startswith("kyotoagent - "):
                found = text[len("kyotoagent - "):]
            i = end + 1
            continue
        i += 1
    return found


def copy_screen(screen):
    """A detached copy, because the live screen is reused for the next frame."""
    import pyte

    clone = pyte.Screen(screen.columns, screen.lines)
    for y in range(screen.lines):
        for x in range(screen.columns):
            clone.buffer[y][x] = screen.buffer[y][x]
    return clone


def capture(example):
    """Walk the three frames under a pty and snapshot each once it settles."""
    import pyte

    os.environ["TERM"] = "xterm-256color"
    os.environ["COLUMNS"] = str(COLS)
    os.environ["LINES"] = str(ROWS)

    pid, fd = pty.fork()
    if pid == 0:
        os.execv(example, [example])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)
    frames = []
    title = ""

    def pump(quiet=QUIET_SECONDS, limit=PUMP_LIMIT):
        """Feed output until the terminal is silent. Returns True if any came."""
        nonlocal title
        got = False
        deadline = time.time() + limit
        last = time.time()
        while time.time() < deadline:
            ready, _, _ = select.select([fd], [], [], 0.1)
            if fd in ready:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return got
                if not data:
                    return got
                got = True
                last = time.time()
                found = title_of(data)
                if found:
                    title = found
                stream.feed(data)
                continue
            if got and time.time() - last >= quiet:
                return True
        return got

    def snapshot():
        # The title is read after the terminal settles, so it names the frame
        # that is actually painted rather than the one being left behind.
        name = title or f"frame{len(frames)}"
        frames.append((name, copy_screen(screen)))
        return name

    pump()
    snapshot()
    for _ in range(len(ORDER) - 1):
        os.write(fd, b"\r")
        pump()
        snapshot()
    os.write(fd, b"q")
    pump(quiet=0.2, limit=1.0)
    os.waitpid(pid, 0)
    return frames


def rows_of(screen):
    rows = []
    for y in range(screen.lines):
        row = []
        for x in range(screen.columns):
            cell = screen.buffer[y][x]
            row.append({
                "c": cell.data or " ",
                "fg": cell.fg,
                "bg": cell.bg,
                "b": bool(cell.bold),
                "r": bool(cell.reverse),
            })
        rows.append(row)
    return rows


def text_of(rows):
    return "\n".join("".join(c["c"] for c in row).rstrip()
                     for row in rows) + "\n"


def check_against_goldens(frames):
    """The screenshot is only worth anything if it shows the tested screen."""
    problems = []
    by_name = dict(frames)
    for name in ORDER:
        if name not in by_name:
            problems.append(f"the capture has no {name} frame")
            continue
        path = os.path.join(GOLDEN_DIR, f"{name}.txt")
        try:
            golden = open(path).read()
        except OSError as err:
            problems.append(f"{path}: {err}")
            continue
        if text_of(by_name[name]) != golden:
            problems.append(f"{name}: the captured screen differs from {path}")
    for problem in problems:
        print(f"capture-frames: {problem}", file=sys.stderr)
    return not problems


def color_for(value, fallback):
    if value is None or value == "default":
        return fallback
    text = str(value)
    if text.isdigit():
        index = int(text)
        return PALETTE[index] if index < len(PALETTE) else fallback
    if len(text) == 6:
        try:
            int(text, 16)
            return "#" + text
        except ValueError:
            pass
    return NAMED.get(text, fallback)


def css_for(cell):
    fg = color_for(cell["fg"], FG_DEFAULT)
    bg = color_for(cell["bg"], BG_DEFAULT)
    if cell["r"]:
        fg, bg = bg, fg
    style = f"color:{fg}"
    if bg != BG_DEFAULT:
        style += f";background:{bg}"
    if cell["b"]:
        style += ";font-weight:700"
    return style


def row_html(row):
    out, run, run_key = "", [], None
    for cell in row:
        key = (cell["fg"], cell["bg"], cell["b"], cell["r"])
        if run and key != run_key:
            out += (f"<span style=\"{css_for(run[0])}\">"
                    f"{html.escape(''.join(c['c'] for c in run))}</span>")
            run = []
        run_key = key
        run.append(cell)
    if run:
        out += (f"<span style=\"{css_for(run[0])}\">"
                f"{html.escape(''.join(c['c'] for c in run))}</span>")
    return out.rstrip()


def write_page(frames, path, one=False):
    """Render the frames as a terminal-looking page.

    `one` writes a single frame to its own page, which is what the per-frame
    PNGs are shot from. Otherwise all three go on one page.
    """
    parts = [
        "<!doctype html><meta charset='utf-8'>",
        "<title>Kyoto Agent - the three session screens</title>",
        f"<style>{CSS}</style>",
        "<body>",
    ]
    if one:
        parts.append("<header>Kyoto Agent &mdash; a session screen</header>")
    else:
        parts.append(
            f"<header>Kyoto Agent &mdash; the {len(frames)} session screens</header>"
        )
    parts.append(
        "<p class='note'>76 &times; 24, drawn by <code>render()</code> and "
        "captured from a real terminal by <code>cargo run --example screens</code>. "
        "The same states are the golden files in <code>tests/screens/</code>."
        "</p>"
    )
    for name, rows in frames:
        parts.append(f"<h2>{html.escape(name)}</h2>")
        if name in CAPTIONS:
            parts.append(f"<p class='note'>{html.escape(CAPTIONS[name])}</p>")
        parts.append("<div class='term'><pre>")
        for row in rows:
            parts.append(row_html(row) + "\n")
        parts.append("</pre></div>")
    parts.append("</body>")
    with open(path, "w") as f:
        f.write("\n".join(parts))


def shoot(chrome, page, out_png, width, height):
    subprocess.run(
        [chrome, "--headless", "--no-sandbox", "--disable-gpu",
         "--hide-scrollbars", "--force-device-scale-factor=2",
         f"--window-size={width},{height}",
         f"--screenshot={out_png}", f"file://{page}"],
        capture_output=True, check=False)
    if not os.path.exists(out_png):
        fail(f"Chromium wrote no screenshot to {out_png}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default=os.path.join(REPO, "build", "screens"),
                        help="where to write the PNGs (default build/screens)")
    parser.add_argument("--check", action="store_true",
                        help="compare the capture with the goldens and stop")
    args = parser.parse_args()

    try:
        import pyte  # noqa: F401
    except ImportError:
        fail("pyte is required: pip install pyte")

    example = build_example()
    frames = [(name, rows_of(screen)) for name, screen in capture(example)]

    if not check_against_goldens(frames):
        raise SystemExit(1)
    print(
        f"capture-frames: the live screen matches all {len(ORDER)} golden files"
    )
    if args.check:
        return

    chrome = find_chrome()
    if not chrome:
        fail("no Chromium found; set CHROME to a binary")

    os.makedirs(args.out, exist_ok=True)
    page = os.path.join(args.out, "screens.html")
    write_page(frames, page)
    # The single page holds every frame, so its height grows with the list.
    shoot(chrome, page, os.path.join(args.out, "screens.png"),
          width=1000, height=1420 * len(frames) // 3 + 200)
    for name, rows in frames:
        single = os.path.join(args.out, f"{name}.html")
        write_page([(name, rows)], single, one=True)
        shoot(chrome, single, os.path.join(args.out, f"{name}.png"),
              width=1000, height=1060)
    print(f"capture-frames: wrote {page} and one PNG per frame in {args.out}")


if __name__ == "__main__":
    main()
