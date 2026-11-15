<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/06-spurmarke/logo.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo/06-spurmarke/logo-on-light.svg">
    <img alt="komponist" src="assets/logo/06-spurmarke/logo-on-light.svg" width="420">
  </picture>
</p>

<p align="center"><b>A MIDI DAW for Linux. Fast, small, and made for writing music.</b></p>

<p align="center">
  <a href="LICENSE"><img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue"></a>
  <img alt="Platform: Linux" src="https://img.shields.io/badge/platform-Linux-informational">
  <img alt="Status: early, v0.1" src="https://img.shields.io/badge/status-early%20(v0.1)-orange">
  <img alt="Written in Rust" src="https://img.shields.io/badge/written%20in-Rust-b7410e">
</p>

![komponist with the demo project "Nachtfahrt": arrangement, piano roll and mixer meters while the transport plays](docs/media/hero.png)

komponist is a MIDI DAW written in Rust with [egui](https://github.com/emilk/egui). You arrange clips, edit notes and
steps, shape sounds with built-in instruments and effects, host CLAP and VST3 plugins, and mix it all on the same
screen. Everything above is the demo project that ships with the app (`komponist --demo`).

## Features

### Arrangement, piano roll, step sequencer

Arrange MIDI and step clips on tracks, with a snapping grid, loop range, split, duplicate, and a full undo history.
Open a clip in the dock below to edit it.

![The arrangement of the demo project with the playhead running](docs/media/arrangement.png)

The piano roll has pointer, pencil and eraser tools, a velocity lane, quantize and transpose. Draw, drag, done:

![Drawing four notes with the pencil, moving one, deleting one](docs/media/piano-roll.gif)

The step sequencer is made for drums and Sampler kits. Toggle steps while the pattern plays:

![Toggling steps in the step sequencer while the pattern loops](docs/media/step-sequencer.gif)

### Built-in instruments

No plugins needed to make a sound.

**hexa** is a six-operator FM synth with envelopes, LFOs and a large preset library. Here with the EQ, Delay and Hall
devices behind it:

![hexa (FM synth) with the EQ, Delay and Hall devices behind it](docs/media/device-hexa.png)

**knurr** is a subtractive synth with two oscillators, a filter and a sub oscillator:

![knurr, a subtractive synth, with an EQ](docs/media/device-knurr.png)

The **Sampler** is a pad sampler with layers, envelopes, filters and slicing. The demo kit is synthesized, so it needs
no sample files. Next to it, the presse compressor with its sidechain chip:

![Sampler with its pads, and the presse compressor with its sidechain chip](docs/media/device-sampler-presse.png)

### Built-in effects

**EQ**, **Delay**, **Hall** (reverb), **presse** (compressor with sidechain) and **Limiter**, plus the MIDI effects
**Arpeggiator** and **Tonleiter** (scale). The EQ shows the live spectrum of the signal it processes:

![The EQ's spectrum moving while the lead plays](docs/media/eq-spectrum.gif)

Add a device from the search menu in the dock, or drag it from the browser onto a track:

![Searching for "lim" in the add-device menu and adding the Limiter](docs/media/add-device.gif)

### Sidechains

Key presse from another track's signal: choose the source in the **SC** chip of the device. Here, presse on the bass is
keyed by the drums:

![Adding presse to the bass, choosing the drums as sidechain source and watching the gain reduction](docs/media/sidechain.gif)

### CLAP and VST3 plugins

komponist hosts **CLAP** and **VST3** plugins. Every plugin runs in a bridge process of its own, so a crashing plugin
does not take your project down with it. Plugin windows open as native X11 windows.

### Mixer, routing, recording

- Record MIDI from any connected MIDI input (or the computer keyboard) into a take.
- Route tracks into **groups** (Ctrl+G) and **effect buses** with pre/post-fader sends; sidechain presse from any
  track or bus.
- Every edit is undoable, including parameter changes.

The mixer shows every track with its inserts, pan, mute/solo, faders and level meters, next to the master channel:

![The mixer with the arrangement above it, playing](docs/media/mixer.png)

## Status

komponist is **early software (v0.1)**. The MIDI workflow works end to end: arrange, edit, play, record, mix, save,
export. Expect rough edges and file-format changes.

What works: MIDI and step clips, piano roll, step sequencer, hexa / knurr / Sampler, the built-in effects, sidechains,
CLAP and VST3 hosting, mixer with groups, buses and sends, recording, undo and redo, project files, export.

What is coming: **audio tracks**, automation.

## Install

Prebuilt packages do not exist yet. Build from source, see [docs/INSTALL.md](docs/INSTALL.md) for the details
(executable, icon, desktop entry, working on klang-dsp together with komponist).

```bash
git clone https://github.com/klang-studio/komponist
cd komponist
cargo build --release -p komponist-app --bin komponist
target/release/komponist --demo
```

You need the Rust toolchain from [`rust-toolchain.toml`](rust-toolchain.toml) (`rustup` picks it up) and the system
development packages behind the audio and window crates. On Debian or Ubuntu these are typically:

```bash
sudo apt install build-essential pkg-config clang libasound2-dev libjack-jackd2-dev libpipewire-0.3-dev \
    libdbus-1-dev libxkbcommon-dev libx11-dev libwayland-dev
```

At runtime you also need a Vulkan driver (wgpu renders with Vulkan) and an X11 or XWayland session. Package names
differ on other distributions. `komponist --help` lists the options (`--demo`, `--null-audio`, `--lang de|en`, `--scale`).

## Contributing

Issues and pull requests are welcome. Before you send a change, run `scripts/check.sh`: `cargo fmt --check`, `clippy -D
warnings` and the tests. Translations are the easiest way in: add or complete `crates/komponist-i18n/locales/<lang>/*.ftl`
as described in [docs/I18N.md](docs/I18N.md). Performance notes are in [docs/PERFORMANCE.md](docs/PERFORMANCE.md).

### Regenerating the screenshots

The pictures in this README come from the demo project and are produced by a script, so they follow the UI:

```bash
scripts/readme-media.sh            # everything into docs/media/
scripts/readme-media.sh hero mixer # only these scenarios
```

It runs komponist under a private Xvfb with `--null-audio` (no audio or MIDI hardware, no sound) and drives the UI
through its accessibility tree. It needs `xvfb-run`, `ffmpeg`, ImageMagick, Python 3 and `egui-mcp`.

## Architecture

A Rust workspace: model and undo, a realtime engine, built-in devices, plugin hosting and bridges, and the egui app.
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) describes the design and the decisions behind it.

## License

komponist is licensed under the **GNU General Public License, version 3 or (at your option) any later version**, see
[LICENSE](LICENSE).

- `crates/hexa-core` (hexa's FM engine) is a copy of the `hexa-core` crate of pisynth, and `crates/sampler-core`
  (the Sampler engine) is a copy of the `dsp` crate of klopfer. Both authors license the copies under
  GPL-3.0-or-later; the crates' READMEs record the provenance. The projects continue separately.
- The DSP library [klang-dsp](https://github.com/klang-studio/klang-dsp) is a git dependency.
- The UI uses **IBM Plex Sans** and **IBM Plex Mono** (© IBM Corp.), unmodified, under the
  [SIL Open Font License 1.1](assets/fonts/OFL.txt). See [assets/fonts/README.md](assets/fonts/README.md).
