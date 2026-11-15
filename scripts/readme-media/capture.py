#!/usr/bin/env python3
"""Drives a running komponist (started here, `--demo --null-audio`) and writes the README media.

Called by `scripts/readme-media.sh` inside `xvfb-run`; do not run it on a real desktop. It starts the app itself,
talks to it through `egui-mcp` (AccessKit names -> coordinates, synthesized egui input; see `egui_client.py`), and
records the GIFs with `ffmpeg -f x11grab` from the private Xvfb display.

    capture.py --bin target/release/komponist --out docs/media --work <scratch dir> [--only NAME ...]

The UI runs in English (`--lang en`, owner decision: README media in English), so the locators below are the
English accessible names (`crates/komponist-i18n/locales/en`; `--lang de` needs the German ones).
When a view changes, fix the locator or the layout constant of the one scenario that broke; every scenario is a
function at the bottom of this file and is independent of the others (each starts from a fresh app state).
"""
import argparse
import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from egui_client import Client, ToolError  # noqa: E402

W, H = 1920, 1080
PORT = 57311
# The dock is dragged up to this y (px) for the dock scenarios; the arrangement keeps 5 track rows above it.
DOCK_Y = 380


class Session:
    """One app process plus its egui-mcp client."""

    def __init__(self, args):
        self.args = args
        self.app = None
        self.c = None
        self.rec = None
        self.rec_file = None

    # -- process -------------------------------------------------------------------------------------------------
    def start(self):
        a = self.args
        home = os.path.join(a.work, "home")
        shutil.rmtree(home, ignore_errors=True)
        for d in ("cfg", "data", "cache", "run"):
            os.makedirs(os.path.join(home, d), exist_ok=True)
        os.chmod(os.path.join(home, "run"), 0o700)
        env = dict(os.environ)
        env.update(
            HOME=home,
            XDG_CONFIG_HOME=home + "/cfg",
            XDG_DATA_HOME=home + "/data",
            XDG_CACHE_HOME=home + "/cache",
            XDG_RUNTIME_DIR=home + "/run",
            EGUI_INSPECTION=f"127.0.0.1:{PORT}",
            WAYLAND_DISPLAY="",
            KOMPONIST_NULL_AUDIO="1",
        )
        self.app = subprocess.Popen(
            # No plugin scan: the browser shows only the built-in devices, not the plugins of this machine.
            [a.bin, "--demo", "--null-audio", "--no-plugin-scan", "--scale", "1", "--lang", "en"],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.c = Client(PORT, 60)
        self.wait_for(role="Button", label="Play")
        time.sleep(1.5)

    def stop(self):
        if self.rec:
            self.rec_stop()
        if self.c:
            self.c.close()
        if self.app:
            self.app.terminate()
            try:
                self.app.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.app.kill()

    # -- lookup --------------------------------------------------------------------------------------------------
    def flat(self, **loc):
        res = self.c.call("widget_tree", visible_only=True, limit=3000, **loc)
        out = []

        def walk(n):
            out.append(n)
            for ch in n.get("children", []):
                walk(ch)

        for n in res.get("nodes", []):
            walk(n)
        return out

    def find(self, role=None, label=None, contains=None, nth=0):
        """Visible nodes whose accessible name equals `label` (or contains `contains`) and has `role`."""
        hits = []
        for n in self.flat():
            name = n.get("label") or n.get("value") or ""
            if role and n.get("role") != role:
                continue
            if label is not None and name != label:
                continue
            if contains is not None and contains not in name:
                continue
            hits.append(n)
        if len(hits) <= nth:
            raise ToolError(f"not found: role={role} label={label} contains={contains}")
        return hits[nth]

    def wait_for(self, timeout=30, **kw):
        end = time.time() + timeout
        while True:
            try:
                return self.find(**kw)
            except ToolError:
                if time.time() > end:
                    raise
                time.sleep(0.3)

    def bounds(self, **kw):
        n = self.find(**kw)
        return self.c.call("get_widget", id=n["id"])["node"]["bounds"]

    def center(self, **kw):
        b = self.bounds(**kw)
        return {"x": b["x"] + b["w"] / 2, "y": b["y"] + b["h"] / 2}

    # -- input ---------------------------------------------------------------------------------------------------
    def click(self, count=1, button="primary", **kw):
        n = self.wait_for(**kw)
        self.c.call("click", id=n["id"], count=count, button=button)

    def click_pos(self, pos, count=1, button="primary"):
        self.c.call("click", pos=pos, count=count, button=button)

    def drag(self, a, b, steps=12):
        self.c.call("drag", start={"pos": a}, end={"pos": b}, steps=steps)

    def key(self, key, **mods):
        self.c.call("press_key", key=key, modifiers=mods)

    def type(self, text):
        self.c.call("type_text", text=text)

    def hover(self, **kw):
        n = self.wait_for(**kw)
        self.c.call("hover", id=n["id"])

    def pause(self, s):
        time.sleep(s)

    # -- output --------------------------------------------------------------------------------------------------
    def shot(self, name, crop=None):
        """PNG screenshot of the app frame (1:1, 1920x1080); `crop` = (x, y, w, h) cuts a region."""
        raw = os.path.join(self.args.work, f"{name}.raw.png")
        self.c.call("screenshot", save_path=raw, pixels_per_point=1.0)
        dst = os.path.join(self.args.out, f"{name}.png")
        cmd = [shutil.which("magick"), raw] if shutil.which("magick") else ["convert", raw]
        if crop:
            cmd += ["-crop", "%dx%d+%d+%d" % (crop[2], crop[3], crop[0], crop[1]), "+repage"]
        cmd += ["-strip", dst]
        subprocess.run(cmd, check=True)
        self.report(dst)

    def rec_start(self):
        self.rec_file = os.path.join(self.args.work, "rec.mkv")
        if os.path.exists(self.rec_file):
            os.remove(self.rec_file)
        self.rec = subprocess.Popen(
            ["ffmpeg", "-loglevel", "error", "-y", "-f", "x11grab", "-draw_mouse", "0", "-framerate", "15",
             "-video_size", f"{W}x{H}", "-i", os.environ["DISPLAY"] + ".0+0,0",
             "-c:v", "libx264", "-qp", "0", "-preset", "ultrafast", self.rec_file],
            stdin=subprocess.PIPE,
        )
        time.sleep(0.6)

    def rec_stop(self):
        self.rec.stdin.write(b"q")
        self.rec.stdin.flush()
        self.rec.wait(timeout=30)
        self.rec = None

    def gif(self, name, crop, width, fps=12, trim=(0.0, None)):
        """Encodes the last recording: crop (x, y, w, h), scaled to `width`, palette-optimized."""
        self.rec_stop()
        dst = os.path.join(self.args.out, f"{name}.gif")
        vf = (f"crop={crop[2]}:{crop[3]}:{crop[0]}:{crop[1]},fps={fps},scale={width}:-1:flags=lanczos,"
              "split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];"
              "[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle")
        cmd = ["ffmpeg", "-loglevel", "error", "-y", "-ss", str(trim[0])]
        if trim[1]:
            cmd += ["-t", str(trim[1])]
        cmd += ["-i", self.rec_file, "-filter_complex", vf, "-loop", "0", dst]
        subprocess.run(cmd, check=True)
        self.report(dst)

    def report(self, path):
        kb = os.path.getsize(path) / 1024
        print(f"  wrote {os.path.relpath(path)}  ({kb:.0f} KB)", flush=True)
        self.args.produced.append(path)


# -- UI helpers ------------------------------------------------------------------------------------------------------
def open_dock(s, y=DOCK_Y):
    """Drags the dock splitter up to `y`."""
    pos = s.center(role="Splitter", contains="Dock")
    s.drag(pos, {"x": pos["x"], "y": y})
    s.pause(0.5)


def select_track(s, name):
    """Selects a track by its header (AccessKit row "Track <name>, <instrument>").

    The row's centre is not safe: tracks inside a group are indented and the centre then lands on the M/S
    toggles (it muted Bass and never selected it). Click on the name, 36 px right of the row's left edge.
    """
    b = s.bounds(role="Row", contains=f"Track {name},")
    s.click_pos({"x": b["x"] + 36, "y": b["y"] + b["h"] / 2})
    s.pause(0.3)


def open_devices(s, track):
    select_track(s, track)
    s.click(role="Tab", label="Devices")
    s.pause(0.8)


def play(s):
    s.click(role="Button", label="Play")


def stop(s):
    s.click(role="Button", label="Stop")


DOCK_X = 261  # left edge of the dock (right of the browser at its default width)


def dock_top(s):
    """y of the dock's top edge, from its first tab (the tab strip sits 6 px below the edge)."""
    return int(s.bounds(role="Tab", label="Devices")["y"]) - 6


# -- scenarios ---------------------------------------------------------------------------------------------------------
# Every scenario: a short docstring (printed), one fresh app, its own output files. GIF crops are (x, y, w, h) in
# the 1920x1080 frame; the dock's top edge is read from the app, the rest is relative to it.

def hero(s):
    """Arrangement with a piano roll open and the transport running (README hero shot)."""
    s.click(role="GridCell", contains="Clip Lead B", count=2)
    open_dock(s, 450)
    play(s)
    s.pause(4.5)
    s.shot("hero")
    stop(s)


def arrangement(s):
    """The arrangement alone (dock folded), playing."""
    s.click(role="Button", label="Collapse dock")
    s.pause(0.5)
    play(s)
    s.pause(3.5)
    s.shot("arrangement")
    stop(s)


def piano_roll(s):
    """Piano roll: draw four notes with the pencil, move one, delete one (GIF + PNG)."""
    s.click(role="GridCell", contains="Clip Bass,", count=2)
    open_dock(s, 480)
    s.pause(0.6)
    # Grid geometry from the existing notes: x of bar 1 / bar 2 start, y of a few piano keys.
    n1 = s.bounds(role="GridCell", label="Note E2, 1.1.000")
    n2 = s.bounds(role="GridCell", label="Note D2, 2.1.000")
    bar = n2["x"] - n1["x"]
    x0 = n1["x"]
    beat = bar / 4

    def key_y(name):
        b = s.bounds(role="Button", label=f"Key {name}")
        return b["y"] + b["h"] / 2

    s.rec_start()
    s.pause(0.6)
    s.click(role="RadioButton", label="Pencil")
    s.pause(0.4)
    melody = [("C3", 0.0), ("D3", 1.0), ("C3", 2.0), ("B2", 3.0)]
    for key, b in melody:
        y = key_y(key)
        s.drag({"x": x0 + b * beat + 6, "y": y}, {"x": x0 + (b + 0.75) * beat, "y": y}, steps=6)
        s.pause(0.45)
    s.click(role="RadioButton", label="Pointer")
    s.pause(0.5)
    # move the third note down a semitone, then remove the last one
    s.drag({"x": x0 + 2 * beat + 20, "y": key_y("C3")}, {"x": x0 + 2 * beat + 20, "y": key_y("A#2")}, steps=8)
    s.pause(0.8)
    s.click_pos({"x": x0 + 3 * beat + 20, "y": key_y("B2")})
    s.pause(0.5)
    s.key("Delete")
    s.pause(1.0)
    top = dock_top(s)
    s.gif("piano-roll", (DOCK_X, top, 1040, 300), 1040, fps=12, trim=(0.3, None))


def step_sequencer(s):
    """Step sequencer: toggle steps on three rows while the pattern plays (GIF + PNG)."""
    s.click(role="GridCell", contains="Clip Beat A, bars 1", count=2)
    open_dock(s, 380)
    s.pause(0.6)
    top = dock_top(s)
    s.rec_start()
    play(s)
    s.pause(0.8)
    for row, steps in (("Clap", (5, 13)), ("Tom", (10, 11, 12)), ("Rim", (3, 7, 11, 15))):
        for st in steps:
            s.click(role="CheckBox", contains=f"Step {st}, {row}, off")
            s.pause(0.3)
    s.pause(1.5)
    stop(s)
    s.gif("step-sequencer", (DOCK_X, top, 1379, 320), 1040, fps=12, trim=(0.3, None))


def devices(s):
    """Device chains: hexa, volt, Sampler + presse with the SC chip (PNGs)."""
    s.click(role="Button", label="Browser")  # fold the browser: the dock gets its full width
    open_dock(s, 380)
    for track, name in (("Lead", "device-hexa"), ("Bass", "device-volt"), ("Drums", "device-sampler-presse")):
        open_devices(s, track)
        top = dock_top(s)
        s.shot(name, crop=(26, top + 28, 1612, 300))


def sidechain(s):
    """Sidechain: presse on the bass keyed by the drums (SC popover PNG), then play and record the pumping (GIF)."""
    s.click(role="Button", label="Browser")  # fold the browser: the whole presse fits in the dock
    open_dock(s, 380)
    open_devices(s, "Bass")
    top = dock_top(s)
    s.pause(0.4)
    # The demo already has presse on the Bass, keyed by the Drums (pre-fader): open the SC chip's popover briefly.
    s.click(role="Button", label="Sidechain: Drums")
    s.wait_for(role="Menu", label="Sidechain source")
    s.pause(0.5)
    chip = s.bounds(role="Button", label="Sidechain: Drums")
    s.key("Escape")
    s.pause(0.5)
    s.click_pos({"x": 553, "y": 83})  # ruler, bar 5 (browser folded): drums and bass play
    s.pause(0.3)
    s.rec_start()
    s.pause(0.4)
    play(s)
    s.pause(1.5)
    s.pause(4.0)
    stop(s)
    s.gif("sidechain", (650, top + 28, 880, 300), 880, fps=12, trim=(0.5, 6.0))


def eq_spectrum(s):
    """EQ with the live spectrum while the lead plays (GIF)."""
    s.click_pos({"x": 1068, "y": 83})  # ruler, bar 9: the lead starts there
    open_dock(s, 380)
    open_devices(s, "Lead")
    top = dock_top(s)
    s.pause(0.5)
    s.rec_start()
    play(s)
    s.pause(5.5)
    stop(s)
    s.gif("eq-spectrum", (DOCK_X + 620, top + 60, 760, 262), 760, fps=12, trim=(0.6, None))


def mixer(s):
    """Mixer tab with the arrangement above, playing (PNG)."""
    open_dock(s, 380)
    s.click(role="Tab", label="Mixer")
    s.click_pos({"x": 1068, "y": 83})
    play(s)
    s.pause(4.0)
    s.shot("mixer")
    stop(s)


def add_device(s):
    """Add-device menu: search, pick an effect (GIF)."""
    open_dock(s, 450)
    open_devices(s, "Keys")
    top = dock_top(s)
    s.rec_start()
    s.pause(0.8)
    s.click(role="Button", label="+ Device")
    s.pause(0.8)
    for ch in "lim":
        s.type(ch)
        s.pause(0.35)
    s.pause(0.9)
    s.click(role="ListBoxOption", label="Limiter")
    s.pause(1.6)
    s.gif("add-device", (DOCK_X + 300, top, 1340, 330), 1040, fps=12, trim=(0.4, None))


SCENARIOS = [hero, arrangement, piano_roll, step_sequencer, devices, sidechain, eq_spectrum, mixer, add_device]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--only", nargs="*", default=[])
    args = ap.parse_args()
    args.produced = []
    os.makedirs(args.out, exist_ok=True)
    os.makedirs(args.work, exist_ok=True)
    failed = []
    for fn in SCENARIOS:
        if args.only and fn.__name__ not in args.only:
            continue
        print(f"[{fn.__name__}] {fn.__doc__.splitlines()[0]}", flush=True)
        s = Session(args)
        try:
            s.start()
            fn(s)
        except Exception as e:  # keep going: one broken scenario must not lose the others
            print(f"  FAILED: {e}", flush=True)
            failed.append(fn.__name__)
        finally:
            s.stop()
    print(f"\n{len(args.produced)} files written, {len(failed)} scenario(s) failed: {', '.join(failed) or '-'}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
