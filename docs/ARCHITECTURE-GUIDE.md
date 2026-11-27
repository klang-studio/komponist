# komponist – A Guided Tour of the Architecture

Status: written for newcomers, checked against the repository on 2026-11-22.

komponist has more than 330,000 lines of Rust across about 30 crates. This guide gives you a map
before you open any of them. It assumes you know Rust. It does **not** assume you know anything
about digital audio, realtime programming or how a DAW works: Part I explains the concepts you need,
Part II walks through the code and follows real data on its way through it, Part III tells you how
to change things without breaking them.

[ARCHITECTURE.md](ARCHITECTURE.md) is the reference: dense, complete and organised by subsystem.
This guide is the tour: selective, explanatory and organised by the questions a newcomer asks.
When they disagree, the code wins; please fix the document that is wrong.

File and type names are given so that you can search for them (`rg 'struct EngineSync'`). Line
numbers are left out on purpose because they change with every commit.

**Contents**

- Part I – Concepts
  - [1. Digital audio in ten minutes](#1-digital-audio-in-ten-minutes)
  - [2. The realtime thread and its rules](#2-the-realtime-thread-and-its-rules)
  - [3. MIDI and musical time](#3-midi-and-musical-time)
  - [4. A little DSP](#4-a-little-dsp)
  - [5. What a DAW is made of](#5-what-a-daw-is-made-of)
  - [6. Plugins](#6-plugins)
- Part II – The code
  - [7. The big picture](#7-the-big-picture)
  - [8. The crates](#8-the-crates)
  - [9. The document: `komponist-model`](#9-the-document-komponist-model)
  - [10. Walk-through: from a mouse drag to sound](#10-walk-through-from-a-mouse-drag-to-sound)
  - [11. The engine: `komponist-engine`](#11-the-engine-komponist-engine)
  - [12. Audio and MIDI hardware: `komponist-audio` and `komponist-rt`](#12-audio-and-midi-hardware-komponist-audio-and-komponist-rt)
  - [13. Built-in devices](#13-built-in-devices)
  - [14. External plugins and the bridge](#14-external-plugins-and-the-bridge)
  - [15. The user interface](#15-the-user-interface)
  - [16. Audio files, streaming, recording and export](#16-audio-files-streaming-recording-and-export)
- Part III – Working on komponist
  - [17. Tests and the gate](#17-tests-and-the-gate)
  - [18. Recipes: "I want to …"](#18-recipes-i-want-to-)
  - [19. Pitfalls](#19-pitfalls)
  - [20. Glossary](#20-glossary)
  - [21. Where to read next](#21-where-to-read-next)

---

# Part I – Concepts

## 1. Digital audio in ten minutes

### 1.1 Sound as numbers

Sound is air pressure that changes over time. A microphone turns it into a voltage, and a converter
measures that voltage at regular intervals. Each measurement is a **sample**. komponist stores
samples as `f32`, where `0.0` is silence and `-1.0 ..= 1.0` is the range the hardware can play.
Values beyond ±1.0 are allowed inside the engine, but they **clip** (distort) at the output.

The **sample rate** says how many samples there are per second, typically 44,100 or 48,000 Hz.
Everything that is "time" inside the engine is ultimately counted in samples.

Stereo sound has two **channels**, left and right. A **frame** is one sample per channel, i.e. one
moment in time. "512 frames of stereo" means 1024 `f32` values. Audio can be stored
**interleaved** (`L R L R …`, how sound cards deliver it) or **planar** (`L L L …` and `R R R …`
as separate slices, how komponist's engine works internally). `StereoIo` in the engine holds
planar input and output: `[&[f32]; 2]`.

### 1.2 Blocks and the callback

The sound card does not ask for samples one at a time. It asks for a **block** (also called buffer
or period) of, say, 256 frames, and it asks again and again, forever. The operating system calls a
function in komponist for every block: the **audio callback**. The callback must fill the block and
return **before the sound card runs out of the previous one**.

The arithmetic you will see everywhere:

| Block size | at 48 kHz | Meaning |
|---:|---:|---|
| 64 frames | 1.3 ms | very low latency, very little time to compute |
| 256 frames | 5.3 ms | a common default |
| 2048 frames | 42.7 ms | komponist's `MAX_BLOCK`; larger device buffers are split |

Formula: `block duration = frames / sample_rate`.

The block size is the main source of **latency**, the delay between "you press a key" and "you hear
the note". Smaller blocks feel more immediate but leave less time per callback. Musicians want
latency below about 10 ms.

### 1.3 Xruns

If the callback is late, the sound card has nothing to play. You hear a click or a dropout. This
is called an **xrun** (buffer underrun or overrun) and it is the one failure a DAW must never cause
itself. komponist counts xruns and shows them in the status bar.

## 2. The realtime thread and its rules

### 2.1 Why "fast on average" is not enough

A callback that takes 1 ms on average but 8 ms once every ten seconds produces a click every ten
seconds. For audio, the **worst case** matters, not the average. Code on the audio thread therefore
has to be **bounded**: you must be able to say how long it takes at most.

Many ordinary operations have no such bound:

| Operation | Why it is forbidden on the audio thread |
|---|---|
| Allocating or freeing memory (`Box::new`, `Vec::push` beyond capacity, dropping a `Vec`) | The allocator can take a lock or ask the kernel for memory. |
| Taking a lock (`Mutex`, `RwLock`) | If the UI thread holds it, the audio thread waits for the UI thread: *priority inversion*. |
| File or network IO, logging, `println!` | The kernel may block for milliseconds or more. |
| Most system calls | Same reason. |

komponist states this as AGENTS.md **rule 4**: inside `Engine::process`, `Processor::process` and
the bridge's realtime loop there is no allocation or freeing, no locks, no logging, no IO and no
blocking system call except the specified wait with a deadline.

### 2.2 How komponist lives with these rules

- **Preallocate everything.** Buffers, event lists and per-track state are created on the UI thread
  with fixed capacities: `MAX_TRACKS = 128`, `MAX_SLOTS = 1024`, `MAX_EVENTS_PER_BLOCK = 1024`,
  `MAX_PDC_FRAMES = 8192` (all in [`komponist-engine/src/lib.rs`](../crates/komponist-engine/src/lib.rs)).
  If something does not fit, it is dropped and counted rather than allocated.
- **Lock-free queues between threads.** UI and audio thread talk through `rtrb` SPSC ring buffers
  (single producer, single consumer): pushing and popping is a few atomic operations, nobody ever
  waits. The command queue holds `COMMAND_CAPACITY = 1024` commands, the event queue
  `EVENT_CAPACITY = 4096` events.
- **Atomics for values the UI only reads.** Playhead position, transport state and meter peaks are
  plain atomic integers that the engine writes and the UI reads every frame (`Telemetry`,
  `TrackMeter` in [`api.rs`](../crates/komponist-engine/src/api.rs)).
- **Never drop on the audio thread.** When the engine is done with an old `Program` or `Processor`,
  it does not drop it. It sends it back to the UI thread as `EngineEvent::Retired(Garbage)`, and
  the UI thread drops it ([`garbage.rs`](../crates/komponist-engine/src/garbage.rs)).
- **Ownership moves, nothing is shared mutably.** A `Processor` is created and activated on the UI
  thread, *moved* into the engine with `Command::InsertProcessor`, used only there, then moved back
  as garbage. The type system (`Send`, no `Sync` needed) enforces most of this.
- **Higher priority.** Audio threads ask the OS for realtime scheduling (`SCHED_FIFO` on Linux, time
  constraint policy and audio workgroups on macOS, MMCSS "Pro Audio" on Windows), see
  [`komponist-rt`](../crates/komponist-rt/README.md).
- **Checked in tests.** In debug builds a global allocator (`assert_no_alloc`) aborts when code
  allocates inside a guarded region. The live callback, the worker threads and every engine test
  wrap processing in it.

### 2.3 Denormals

Very small floating-point numbers (around `1e-38`) are "denormal" and can be 100× slower to compute
on x86. They appear naturally when a reverb or filter tail fades out. The callback therefore sets
the CPU's **flush-to-zero / denormals-are-zero** flags (`enable_flush_to_zero` in
[`komponist-audio/src/lib.rs`](../crates/komponist-audio/src/lib.rs)), and worker threads copy that
floating-point mode. Device DSP also flushes its feedback paths explicitly (`flush()` in the device
core).

### 2.4 Deadlines

Some work happens outside the audio thread but must still finish within the block: the engine's
worker threads and the plugin bridge processes. The engine computes a **deadline** for each
callback: `callback start + 0.6 × block duration` (`DEADLINE_SHARE` in
[`processor.rs`](../crates/komponist-processor/src/processor.rs)). The two kinds of helpers handle
lateness differently:

- **A late bridge** is not waited for. Its block is replaced (an instrument is silent, an effect
  passes its input through), the device's late counter rises and the UI marks the device "zu spät".
  The stale result is discarded by its sequence number (section 14.4).
- **A late worker** is waited for, because a running job owns its track's processors and state and
  can be neither abandoned nor run twice. The audio thread takes every job no worker has started
  yet, so a worker that wakes late costs nothing; a job that runs too long delays the callback and
  can cause an xrun. Such waits are counted in `WorkerStats::late` (section 11.4).

## 3. MIDI and musical time

### 3.1 MIDI

MIDI is a protocol for musical *events*, not sound. The important messages are:

- **Note on** (key 0–127, velocity 1–127, channel 0–15): a key goes down. Middle C is key 60, shown
  as `C4` in komponist.
- **Note off**: the key goes up. Every note-on needs exactly one matching note-off; a lost note-off
  is a "stuck note" that rings forever. A lot of engine code exists to guarantee note-offs across
  loops, stops, edits and program changes.
- **Control change, pitch bend, channel pressure**: continuous controllers (mod wheel, …).

Inside the engine, MIDI becomes `Event { offset, kind }` with `EventKind::NoteOn`, `NoteOff`,
`Midi([u8; 3])`, `Param { id, value }` or `AllNotesOff`. The `offset` is a frame position inside the
current block, so a note can start at frame 117 of a 256-frame block: this is **sample-accurate**
timing. Live MIDI from a keyboard arrives with timestamps, and `komponist-audio` maps those to
offsets.

An **instrument** turns note events into audio (a synthesizer, a sampler). A **MIDI effect**
transforms events before the instrument (komponist has an Arpeggiator and Scale).

### 3.2 Two kinds of time

A DAW must convert between two clocks constantly:

- **Musical time**: bars, beats, notes. komponist uses `Tick(i64)` at **960 PPQ** (pulses per quarter
  note): a quarter note is 960 ticks, a sixteenth 240, a 4/4 bar 3840
  ([`time.rs`](../crates/komponist-model/src/time.rs)). Clips, notes and automation live in ticks,
  so they stay in place when the tempo changes.
- **Sample time**: what the sound card counts.

`TempoMap` converts between them. Example: at 120 BPM a quarter note lasts 0.5 s, which is 24,000
samples at 48 kHz, so one tick is 25 samples. The project tempo is constant (40–240 BPM) and so is
the meter.

The **transport** is the "tape machine": play, stop, record, the playhead position, loop range and
count-in. Negative transport positions are the count-in before bar 1.

## 4. A little DSP

DSP (digital signal processing) is arithmetic on sample streams. You do not need to be an expert to
work on most of komponist, but these terms appear everywhere.

### 4.1 Gain and decibels

Volume is a multiplication: `out = in × gain`. People think in **decibels** because hearing is
logarithmic: `gain = 10^(dB / 20)`. So 0 dB is `× 1.0`, −6 dB is about `× 0.5`, −∞ dB is `× 0.0`.
**dBFS** (dB relative to full scale) means "relative to 1.0". The fader shows dB, the engine
multiplies. komponist prints negative values with the typographic minus `−` (U+2212), not `-`.

**Pan** distributes a signal between left and right (komponist uses a balance law from
`klang_dsp::pan`). A **meter** shows the peak level of recent blocks; the engine only stores peaks
in atomics, the UI does the "ballistics" (how fast the bar falls).

### 4.2 Smoothing

If a gain jumps from 0.2 to 0.8 between two samples, the waveform gets a step, and you hear a click
("zipper noise"). DSP code therefore **smooths** parameter changes over a few milliseconds. In the
device code, a parameter has a `target` and the processor moves towards it at control rate
(`CONTROL = 32` samples in [`builtin-devices/core/src/dsp.rs`](../crates/builtin-devices/core/src/dsp.rs)).
The engine also ramps fader and send changes, and fades audio in and out briefly when you stop,
locate or split clips.

### 4.3 Filters, delay lines, oscillators

- A **delay line** is a ring buffer of past samples. Echoes, reverbs, chorus and latency
  compensation are all built from delay lines.
- A **filter** changes the balance of frequencies (low-pass removes highs, high-pass removes lows).
  The simplest one, a one-pole filter, is one multiply-add per sample with remembered state.
- An **oscillator** generates a periodic waveform (sine, saw, square); synthesizers combine
  oscillators, filters and envelopes. Hexa uses FM synthesis: oscillators modulating each other's
  frequency.

Shared primitives for all of this live in the external **klang-dsp** crate (a pinned git
dependency).

### 4.4 Latency and delay compensation (PDC)

Some effects need to look ahead: a limiter must see a peak before it arrives to turn it down in
time. Such an effect delays its output by N samples and reports `latency() = N`. If one track has
a 64-sample effect and another does not, they would play out of sync. **Plugin delay compensation
(PDC)** delays all other paths by the difference so that everything meets in time at the master.
The engine's compiler computes these delays and the engine applies them with preallocated delay
lines.

### 4.5 Sample-rate conversion and time stretching

An audio file recorded at 44.1 kHz must be **resampled** to play at 48 kHz (komponist uses `rubato`).
Playing audio faster also raises its pitch, like a record at the wrong speed. To follow the project
tempo *without* changing pitch, you need **time stretching** ("Warp" in komponist), done by the
vendored Signalsmith Stretch library in [`komponist-stretch`](../crates/komponist-stretch/README.md).
Both are far too expensive and allocation-heavy for the audio callback, so they run on a
**read-ahead** thread (section 16).

## 5. What a DAW is made of

A DAW (digital audio workstation) is a multitrack recorder, sequencer and mixer in one program. The
vocabulary, as komponist uses it:

```text
Project
 ├── Tracks (rows in the arrangement, strips in the mixer)
 │    ├── MIDI track:  clips with notes → MIDI effects → instrument → audio effects → fader/pan → output
 │    ├── Audio track: audio clips + live input ───────────────────→ audio effects → fader/pan → output
 │    └── Bus:         sum of other tracks' outputs/sends ─────────→ audio effects → fader/pan → output
 └── Master: sum of everything → master effects → master fader → sound card
```

- **Clip**: a region on a track's timeline. MIDI clips hold notes (in named **takes**, one active),
  step clips hold a drum-machine grid, audio clips reference an audio file.
- **Device**: an instrument or effect in a track's **chain** (also called inserts). Built-in or
  external plugin.
- **Channel**: a track's mixer strip: devices, volume (fader), pan, mute, solo.
- **Bus**: a track without clips that sums signals. A **group** is a bus shown as a folder of the
  tracks that output into it.
- **Send**: a copy of a track's signal into a bus, e.g. several tracks sending to one reverb bus.
  **Pre-fader** sends tap before the fader, **post-fader** after it.
- **Sidechain**: a second audio input of a device, fed from another track. Typical use: a kick drum
  that makes a compressor duck the bass.
- **Arm / monitor**: an armed track records its input; a monitored track lets you hear its input live.
- **Automation**: parameter values that change over time, drawn as curves (lanes) on the timeline.
- **Export / render / bounce**: computing the whole song into an audio file, faster than realtime.
- **Arrangement**: the timeline view with tracks and clips. **Piano roll**: note editor.
  **Step sequencer**: grid editor for drum patterns. **Mixer**: strips with faders.

The demo project "Night Drive" (`komponist --demo`) shows all of these. Open it early and click
around; most code makes more sense when you know what it draws or plays.

## 6. Plugins

Third parties write instruments and effects as **plugins**: shared libraries that a host (the DAW)
loads. komponist supports two plugin standards:

- **CLAP**, a modern open standard, hosted through the `clack-host` crate.
- **VST3**, Steinberg's standard, hosted through the `vst3` crate.

Plugin code is foreign code. It can crash, leak, hang or be slow. Many DAWs load plugins into their
own process, so a crashing plugin takes the whole session down. komponist runs **every third-party
plugin instance in its own process** (the *bridge*), communicating through shared memory. A
crashed plugin only silences its own device; you can reload it. The price is the extra cost of
moving audio between processes every block, which section 14 explains.

komponist's twelve shipped instruments and effects each have a CLAP bundle. The app verifies the
bundle inventory and loads each instance **in process** when the project needs it.

---

# Part II – The code

## 7. The big picture

komponist is one executable that plays three roles: the app, the plugin bridge and the plugin
scanner. While it runs, these threads and processes do the work:

```text
komponist (main process)
│
├── UI thread ─────────────── egui frames (~60/s): Document, UiState, actions, EngineSync,
│     │                       PluginManager, project IO. May allocate, block, do IO.
│     │  Command (rtrb queue)            ▲ EngineEvent + Garbage (rtrb queue)
│     ▼                                  │ Telemetry (atomics: playhead, meters)
├── Audio callback thread ─── Engine::process every block: transport, sequencer, mixing.
│     │                       Realtime: no alloc, no locks, no IO.
│     ├── worker threads ──── run independent track jobs of the same block in parallel
│     ├── read-ahead thread ─ decodes/resamples/stretches audio clips into per-track rings
│     └── recording writer ── writes captured input to WAV files
│
├── background threads ────── plugin scan, sample loading, waveform peaks, tempo analysis, export
│
└── one bridge process per external plugin:  komponist --bridge --instance
      ├── main thread ─────── control messages, plugin state, plugin GUI
      └── RT thread ───────── waits for a block in shared memory, runs the plugin, signals back
```

Three ideas carry the whole design. If you keep them in mind, most code explains itself.

1. **The UI owns the document; the engine plays a compiled copy.** The editable project lives only on
   the UI thread, as an immutable `Arc` tree. After each change it is *compiled* into a `Program`,
   a flat, preallocated, play-ready form, and sent to the engine. The engine never sees the
   editable project and never waits for the UI.
2. **The realtime boundary is explicit.** Everything that crosses between UI and audio thread is a
   message in a lock-free queue or an atomic value. Ownership of big objects moves across and moves
   back for destruction.
3. **Foreign code is isolated.** External plugins run in separate processes behind a deadline. The
   engine treats them like any other device through the same `Processor` trait.

## 8. The crates

### 8.1 Layers

The diagram shows selected direct workspace dependencies. Crate names omit the `komponist-` prefix.

```text
app             → audio, bridge, engine, plugins, ui
bridge          → hosting, ipc, plugin-api, ui
plugins         → engine, hosting, ipc, plugin-api, processor
engine          → audiofile, model, processor, rt, stretch
hosting         → ipc, plugin-api
ipc             → plugin-api, rt (macOS)
audio           → rt
ui              → device-dsp, i18n, model, plugin-api, tokens
processor       → model
plugin-api      → model
native-clap-sdk → device-core, model, plugin-api
device-core     → device-dsp, i18n, model, processor

model, device-dsp, i18n, tokens have no normal workspace dependencies.
```

Rules that follow from this picture:

- **`komponist-model` knows nothing about audio, UI or plugins.** It is pure data and pure
  functions. The UI uses the design tokens to convert device grid coordinates into pixel bounds.
- **`komponist-engine` knows neither egui, nor IPC, nor CLAP/VST3.** It sees devices only as
  `Box<dyn Processor>`.
- **`komponist-audio` does not know the engine.** It drives anything that implements its
  `AudioProcess` trait; the app supplies the adapter (`EngineProcess`).
- **`komponist-device-dsp` shares device math.** The device crates and UI graphs use the same curves.
- **`komponist-plugin-api` defines shared extension and frame data.** The UI and SDK use these
  contracts directly. The bridge protocol uses the same types as message payloads.
- **`komponist-hosting` implements the plugin formats.** In the app process, `komponist-plugins`
  uses it to host verified shipped CLAP bundles. The bridge uses it for third-party CLAP and VST3
  instances and for scanning. The app interacts with both paths through `komponist-plugins`.

### 8.2 Sizes and where to start

| Crate | Lines | Read first |
|---|---:|---|
| [`komponist-app`](../crates/komponist-app/README.md) | ~122k (41k tests) | `main.rs`, `app.rs`, `actions.rs`, `engine_sync.rs` |
| [`builtin-devices/*`](../crates/builtin-devices/README.md) | ~47k (Hexa alone 23k) | `core/src/processor.rs`, `delay/src/lib.rs` |
| [`komponist-engine`](../crates/komponist-engine/README.md) | ~35k | `api.rs`, `compile.rs`, `program.rs`, `engine.rs` |
| [`komponist-ui`](../crates/komponist-ui/README.md) | ~31k | `lib.rs`, `widgets/`, `timeline/mod.rs` |
| [`komponist-model`](../crates/komponist-model/README.md) | ~22k | `project.rs`, `time.rs`, `document.rs`, `edits/mod.rs` |
| [`komponist-plugins`](../crates/komponist-plugins/README.md) | ~15k | `manager.rs`, `external.rs`, `bridge.rs` |
| [`komponist-hosting`](../crates/komponist-hosting/README.md) | ~13k | `plugin.rs` |
| [`komponist-bridge`](../crates/komponist-bridge/README.md) | ~12k | `lib.rs`, `instance.rs`, `rt.rs` |
| [`komponist-audio`](../crates/komponist-audio/README.md) | ~10k | `process.rs`, `slot.rs`, `system.rs` |
| [`komponist-ipc`](../crates/komponist-ipc/README.md) | ~8k | `shm.rs`, `control.rs` |
| [`komponist-audiofile`](../crates/komponist-audiofile/README.md) | ~4k | `pool.rs` |
| [`komponist-tokens`](../crates/komponist-tokens/README.md), [`komponist-i18n`](../crates/komponist-i18n/README.md) | ~3k, ~2k | their READMEs |
| [`komponist-rt`](../crates/komponist-rt/README.md), [`komponist-stretch`](../crates/komponist-stretch/README.md) | ~1.5k, ~1k | their READMEs |

Every crate has a README with a structure list; most modules start with a `//!` doc comment that
explains the module's job and its threading rules. Read those first. Comments often name the
thread a function runs on ("Non-RT", "RT", "UI thread only"): take them seriously.

Not part of the main workspace: [`test-plugins/`](../test-plugins/README.md) (CLAP/VST3 fixtures
for tests, own workspace) and [`crates/plugin-sdk/`](../crates/plugin-sdk/README.md) (SDK for
third-party CLAP devices with native komponist panels, own workspace). `vendor/` holds the
Signalsmith Stretch sources and the ASIO SDK.

## 9. The document: `komponist-model`

### 9.1 The project tree

The whole song is one value of type `Project`
([`project.rs`](../crates/komponist-model/src/project.rs)):

```rust
pub struct Project {
    pub name: String,
    pub tempo: Tempo,
    pub meter: Meter,
    pub loop_range: TickRange,
    pub loop_on: bool,
    pub metronome: bool,
    pub count_in_bars: u8,
    pub tracks: Vec<Arc<Track>>,     // display order
    pub master: Arc<Channel>,
    pub master_automation: Arc<TrackAutomation>,
    pub next_id: u64,                // one counter for all ids, only grows
}
```

A `Track` has an id, name, colour (`Hue`), `kind: TrackKind` (MIDI, audio or bus), a `channel:
Channel` (devices, volume, pan, mute, solo), `clips: Vec<Arc<Clip>>`, its output, sends, arm and
monitor flags. A `Device` has an id, a `role` (`Instrument`, `Effect`, `MidiEffect`), a
`PluginRef` (format + id + display name), `bypass`, an optional `sidechain` and an opaque
`state: Option<Arc<[u8]>>` (Hexa's patch, a CLAP plugin's state blob, …).

Ids (`TrackId`, `ClipId`, `NoteId`, `DeviceId`, …) are stable `u64`s. The engine uses them to keep
runtime state (held notes, delay lines, a running plugin) across program changes.

### 9.2 Immutable snapshots and `Arc::make_mut`

Every node that can change independently is behind an `Arc`. An edit never mutates the current
project. It creates a new one that shares all untouched nodes with the old one:

```text
before:  Project ─┬─ Arc<Track A> ── clips ── Arc<Clip 1> ── notes
                  └─ Arc<Track B> ── …
after:   Project'─┬─ Arc<Track A'> ─ clips ── Arc<Clip 1'> ─ notes'   (copied: the path to the change)
                  └─ Arc<Track B> ── …                               (shared with "before")
```

Edits are pure functions in [`edits/`](../crates/komponist-model/src/edits/) with the shape
`fn(&Project, …) -> Result<Project, EditError>`. For example `edits::move_notes(project, clip,
notes, delta, pitch_delta)` clones the project shallowly, then walks down with `Arc::make_mut`,
which copies a node only if it is shared. Only the touched track, clip and note list are copied.

Edits also enforce the rules: a MIDI chain is MIDI effects → at most one instrument → audio
effects; audio clips on one track never overlap; the routing graph never has a cycle
([`routing.rs`](../crates/komponist-model/src/routing.rs)). An edit that would break a rule returns
an error and nothing changes.

### 9.3 Undo, redo and gestures

`Document` ([`document.rs`](../crates/komponist-model/src/document.rs)) holds
`current: Arc<Project>`, an undo stack and a redo stack of whole snapshots (at most `UNDO_LIMIT =
500`), and a `revision` counter. Because snapshots share structure, keeping 500 of them is cheap.
Undo and redo just swap roots.

A drag must become *one* undo step, not one per mouse movement. That is a **gesture**:

```text
begin_gesture("Noten verschieben")   remembers the root "before"
apply_in_gesture(edit with total delta)   computes from "before", replaces current
apply_in_gesture(edit with total delta)   …again, still from "before"
end_gesture()                        pushes one undo step (only if something changed)
cancel_gesture()                     (Esc) restores "before"
```

Drag actions carry the **total** delta since the drag started and are applied to the "before"
state, so clamping at the edges never accumulates errors. Recording a take works the same way.

`UiState` ([`ui_state.rs`](../crates/komponist-app/src/ui_state.rs), in the app crate) holds
everything that is *not* part of the song: selection, focus, zoom, scroll, open panels and dialogs.
It is never undone and never saved with the project.

### 9.4 Files

A project is a `*.komponist` JSON file (`format: "komponist"`, version 6) written by
[`persist.rs`](../crates/komponist-model/src/persist.rs). Plugin states are base64. Audio files
are *linked* by relative and absolute path, never embedded. Below 1.0 exactly one version is valid:
any other version is rejected, nothing is migrated (D10). If you change the file format, read ARCHITECTURE §5.5 first.

## 10. Walk-through: from a mouse drag to sound

This section follows two real interactions through the code: dragging notes in the piano roll
(a structural change that needs a recompile) and dragging a mixer fader (a fast path).

### 10.1 The frame loop

egui is an **immediate mode** GUI: there are no widget objects that persist. Every frame (up to
~60 per second, or when something changes), the app redraws everything from its state, and
widgets report interactions as return values. komponist's frame is `App::show` in
[`app.rs`](../crates/komponist-app/src/app.rs):

```text
App::show(ui)                          once per frame, UI thread
 1. poll_engine()                      drain EngineEvents (recorded notes, plugin param changes,
                                       garbage to drop), update meters from telemetry
 2. handle_keys()                      keyboard → KeyChord → Keymap::lookup → Action
 3. draw_shell()                       transport bar, browser, arrangement, inspector, dock
                                       (piano roll / step grid / waveform / devices / mixer),
                                       status bar, dialogs. Views only READ the project and
                                       queue Actions.
 4. flush_queue()                      App::dispatch(action) for each queued action
 5. engine.sync(&doc)                  if the document revision changed: compile + send to engine
```

The rule behind steps 3 and 4: **views never change the document.** They send `Action`s, and one
dispatcher applies them. This keeps every interaction testable without pixels: tests can call
`app.dispatch(Action::…)` directly.

### 10.2 Step 1 – the view emits actions

`Action` ([`actions.rs`](../crates/komponist-app/src/actions.rs)) is "every user intention as data":

```rust
pub enum Action {
    Transport(TransportAction),   // play, stop, record, loop, live notes
    Edit(EditAction),             // document changes and history
    Automation(…), Macro(…),
    Ui(UiAction),                 // panels, focus, zoom, selection, dialogs
    Plugin(PluginAction),         // add, open GUI, reload, parameters
    Project(ProjectAction),       // new, open, save, export
    Settings(SettingsAction),
}
```

Each view gets a `ViewCtx` with read-only access to the project, and calls `ctx.send(action)`.

- **Piano roll** ([`views/piano_roll/canvas.rs`](../crates/komponist-app/src/views/piano_roll/canvas.rs)):
  on the first mouse movement of a drag it sends `EditAction::BeginGesture`, then on every movement
  `EditAction::MoveNotes { clip, notes, delta, pitch_delta }` with the total delta, and on release
  `EndGesture` (or `CancelGesture` when the notes ended where they started).
- **Mixer fader** ([`views/mixer/`](../crates/komponist-app/src/views/mixer/)): sends
  `SetTrackProp { prop: ChannelProp::VolumeDb(db) }`, also bracketed by `BeginGesture`/`EndGesture`.

### 10.3 Step 2 – dispatch edits the document

`App::dispatch` routes `Action::Edit` to `edit_action`
([`app/edit_dispatch.rs`](../crates/komponist-app/src/app/edit_dispatch.rs)).
`MoveNotes` becomes `doc.apply_in_gesture_with(…, |p| edits::move_notes(p, …))`. The document
now has a new `current` root and an incremented `revision`.

### 10.4 Step 3 – `EngineSync` compiles a `Program`

`EngineSync` ([`engine_sync.rs`](../crates/komponist-app/src/engine_sync.rs)) is the UI thread's
side of the engine. It coordinates `EngineIo` (`engine_sync/io.rs`), which owns the stream,
MIDI ports, engine queues and export handoff, and `ProgramSync` (`engine_sync/program.rs`),
which owns playback revisions, the last program sent and the `DeviceHost` that creates
processors. Stream rate and MIDI port changes invalidate the compiled program.

`EngineSync::sync(&doc)` returns early when the revision has not changed. Otherwise:

1. `compile::build_with_diagnostics(project, previous, env)`
   ([`compile.rs`](../crates/komponist-engine/src/compile.rs)) turns the `Project` into a `Program`
   ([`program.rs`](../crates/komponist-engine/src/program.rs)): sorted note events per track,
   loops expanded, automation curves resolved, processor slots assigned, the routing graph in
   topological order, latency compensation computed. This is **incremental**: a track whose
   `Arc<Track>` is pointer-equal to last time reuses its compiled `TrackProgram`. Moving notes in one
   clip recompiles one track.
2. `compile::slot_changes(previous, next)` lists devices that appeared or disappeared.
3. Commands go out in a strict order: `InsertProcessor` for new devices (created and activated here,
   on the UI thread), then `SetProgram(Box<Program>)`, then `RemoveProcessor` for vanished ones.
   So a slot is filled before a program uses it, and emptied only after no program uses it.

Compilation never fails: what the engine cannot play (too many tracks, a device in a wrong
position) is left out and reported as a `Diagnostic` in the status bar and inspector.

During the piano-roll drag, every frame recompiles at most once, because `sync` runs once per
frame. You hear the moved notes while you drag.

### 10.5 Step 4 – the command crosses to the audio thread

`EngineHandle` ([`api.rs`](../crates/komponist-engine/src/api.rs)) holds the producer end of the
`rtrb` command queue. `EngineIo` keeps waiting commands in order. It retries a full queue on the
next poll and flushes commands queued during export when the original handle returns. If it
replaces the engine after a failure, it discards the old engine's pending commands. Nothing blocks.

At the start of every callback, the engine drains up to `MAX_COMMANDS_PER_BLOCK = 64` commands
(`Core::drain_commands` in [`engine.rs`](../crates/komponist-engine/src/engine.rs)).
`SetProgram` swaps in the new program; per-track runtime state (sounding notes, delay lines) is
carried over by id. The old program goes into the garbage outbox.

### 10.6 Step 5 – sound

The next blocks render with the new program (section 11). The old program travels back as
`EngineEvent::Retired(Garbage::Program(…))`, and the UI thread drops it in `poll_engine` in the next
frame.

### 10.7 The fast path: a fader drag

Recompiling for every fader pixel would be wasteful. For fader, pan, mute and solo during a gesture,
`set_track_prop` ([`app/track_edits.rs`](../crates/komponist-app/src/app/track_edits.rs)) still
edits the document (so undo works), but also sends a direct command such as
`Command::SetTrackGain { track, gain }` via `EngineSync::fast_path`. That marks the revision as
already handled, so `sync` skips the compile until the gesture ends; then it compiles once. Device
parameters (knobs) work similarly with `Command::SetParam`: the running processor already has the
value.

### 10.8 The way back: events and telemetry

| Channel | Carries | Read by the UI |
|---|---|---|
| `EngineEvent` queue | recorded notes, transport transitions, count-in done, processor failures, plugin parameter changes, `Retired(Garbage)` | `App::poll_engine` → `engine_event` |
| `Telemetry` atomics | playhead, transport state, master meter and scope tap, held input keys, dropped events | `ctx.engine.playhead()`, `Meters::update` |
| `TrackMeter` atomics (per track) | peak levels; `take()` reads and resets | the mixer strip, track headers |

### 10.9 The whole loop in one picture

```text
 UI thread                                         audio thread
 ─────────                                         ────────────
 view ── ctx.send(Action) ──► queue
                                │ flush_queue
 App::dispatch ── edits::… ──► Document (revision+1)
                                │ engine.sync
 EngineSync ── compile::build ─► Program
            ── InsertProcessor / SetProgram / RemoveProcessor ──rtrb──► Core::drain_commands
                                                                         install program
                                                                         render blocks
 poll_engine ◄── EngineEvent::Retired(old program) ◄──rtrb─────────────── outbox
 meters/playhead ◄──────── atomics ◄────────────────────────────────────── telemetry
```

## 11. The engine: `komponist-engine`

### 11.1 The `Processor` trait

Every device, built-in or external, is a `Processor`
([`processor.rs`](../crates/komponist-processor/src/processor.rs)). Simplified:

```rust
pub trait Processor: Send {
    // Non-RT (UI thread): prepare for a sample rate and maximum block size. May allocate.
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError>;

    // RT: process ctx.frames frames. `events` are sorted by offset.
    // Instruments write io.output; effects read io.input and write io.output.
    fn process(&mut self, ctx: &ProcessContext, events: &[Event], io: &mut StereoIo<'_>)
        -> ProcessStatus;

    // RT, optional: the same with a sidechain input, aligned in time by the engine.
    fn process_sidechain(&mut self, ctx, events, io, sidechain: [&[f32]; 2]) -> ProcessStatus;

    // RT, optional: start the block early (bridged instruments, section 14).
    fn begin(&mut self, ctx, events, input: [&[f32]; 2]) -> bool;

    fn latency(&self) -> u32;   // frames of delay this device adds (for PDC)
    fn reset(&mut self);        // RT: clear tails and state without allocating
    fn event_processor(&mut self) -> Option<&mut dyn EventProcessor>;  // MIDI effects
}
```

`ProcessContext` carries `frames`, `sample_rate`, `TransportInfo` (playing, recording, looping,
tempo, meter, position in ticks, loop range), whether
this is an offline render, and the realtime `deadline`. If `process` returns
`ProcessStatus::Failed`, the engine reports it once and treats the slot as bypassed.

The trait's doc comment states the lifecycle, which you should memorise: *created and activated on
a non-RT thread, sent to the engine with `Command::InsertProcessor`, processed on the audio thread,
sent back through the garbage queue and dropped on the UI thread. A processor is never dropped on
the audio thread.*

### 11.2 One callback, step by step

`Engine::process_with_audio` → `Core::run` ([`engine.rs`](../crates/komponist-engine/src/engine.rs)):

```text
Core::run(n frames)
 1. drain up to 64 commands; compute the deadline (start + 0.6 × n / rate)
 2. cut the callback into segments: at most MAX_BLOCK, and split where the loop wraps
    or the count-in ends (transport.next_segment)
 3. for each segment → Core::segment:
    a. per track, in order: prepare automation, prepare audio-clip voices,
       collect sequenced notes for this segment (sequencer.rs) plus live MIDI,
       capture input for recording; call begin() early on bridged instruments
    b. run the track/bus jobs (in parallel when possible, 11.4). One job:
         bus: sum inputs │ audio track: read clip audio from the ring + monitored input
         → MIDI effects (stage.rs) → instrument → audio effects (slots.rs: run_chain)
         → gain ramp → pan → mute/solo gates → meters, taps, PDC → output buffer
    c. master: sum outputs in a fixed order → master effects → master fader → meter
    d. metronome click, after the master fader
    e. advance the transport; on loop wrap end notes and "chase" notes at the new position
 4. short fade on hard stop; publish telemetry (playhead, state, meters)
```

**Note chasing**: if you start playback in the middle of a long note, the engine starts that note
anyway (like Ableton does). An active-note table owns all note-offs across loops and program
swaps, so no note gets stuck.

### 11.3 Routing graph and latency compensation

Tracks and buses form a directed acyclic graph: outputs, sends, sidechains and bus inputs are edges,
the master is the final node. `compile/graph.rs` orders the jobs topologically, lists each node's
inputs in a fixed order (so sums are deterministic) and computes latency per edge: a node's input
latency is the latest arrival among its inputs, and each earlier input gets a delay line to match.
[`graph.rs`](../crates/komponist-engine/src/graph.rs) holds the per-edge delay lines and ramped send
gains. `routing::gates` in the model computes which outputs and sends are audible under mute and
solo.

### 11.4 The worker pool

Independent tracks can be processed in parallel. The pool
([`workers.rs`](../crates/komponist-engine/src/workers.rs)) is a fixed set of realtime threads
created outside the callback. For each segment:

- The ready jobs are bits in two `AtomicU64` words. Any thread claims a job by clearing its bit with
  `fetch_and`; whoever clears it owns the job. No locks.
- When a job finishes, it decrements the "missing inputs" counter of its dependents and publishes
  those that reach zero.
- The audio thread works along with the workers and waits (futex on Linux, `os_sync` on macOS,
  `WaitOnAddress` on Windows; see `sys.rs`) only for jobs another thread already owns. A late job is
  counted, never abandoned or run twice.
- Short segments (`< 32` frames), single jobs and `worker_threads = 0` use the serial path. Serial
  and parallel output are bit-identical, which tests check.

### 11.5 Other engine modules

| Module | Job |
|---|---|
| `transport.rs` | playhead, loop, count-in, segments |
| `sequencer.rs`, `note_schedule.rs` | which notes start/stop in a segment; active-note table; chasing |
| `stage.rs` | the MIDI-effect stage before the instrument |
| `slots.rs` | processor slot table and running a device chain |
| `mixer.rs`, `route.rs`, `graph.rs`, `tap.rs` | gain/pan, sends, aligned inputs, audio taps for visualisations |
| `automation.rs`, `engine/automation_capture.rs` | evaluating curves on a fixed sample grid; recording automation |
| `macros.rs` | track macros (one control driving several parameters) |
| `metronome.rs` | the click |
| `audio/` | audio clips: plan, read-ahead streamer, rings, voices, stretch (section 16) |
| `recording.rs` | capturing input and the writer thread |
| `export.rs`, `export/track.rs` | offline rendering to WAV, track flattening |
| `garbage.rs` | the outbox that sends retired objects back to the UI thread |
| `testing.rs` | test processors (`TestSine`, `TestGain`, …) |

## 12. Audio and MIDI hardware: `komponist-audio` and `komponist-rt`

`komponist-audio` turns sound-card callbacks into calls of one trait
([`process.rs`](../crates/komponist-audio/src/process.rs)):

```rust
pub trait AudioProcess: Send + 'static {
    fn prepare(&mut self, params: &StreamParams);   // non-RT: before the first callback
    fn process(&mut self, out: &mut OutputBuffer<'_>, info: &CallbackInfo, midi: &mut MidiIn<'_>);
    fn process_input(&mut self, out, info, midi, input: &InputBuffer<'_>);  // with captured audio
}
```

The app's implementation is `EngineProcess` in `engine_sync/io.rs`. It converts live MIDI to engine
input, calls `Engine::process_with_audio`, and writes the engine's planar stereo into the device's
interleaved buffer.

- **Backends**: cpal drives PipeWire/JACK/ALSA, CoreAudio, WASAPI and optional ASIO; Linux has a
  native JACK path for explicit routing and capture; MIDI uses midir. The **null backend**
  (`--null-audio`, `NullStream`) runs the callback from a plain thread at the right pace, without
  hardware. Tests and headless runs use it.
- **`EngineSlot<P>`** ([`slot.rs`](../crates/komponist-audio/src/slot.rs)) is how the engine moves
  in and out of a stream without locks: single-item mailboxes built on `AtomicPtr`. When you change
  the audio device or buffer size, the old stream is torn down, its slot hands the engine back, and
  the *same* engine object goes into the new stream. Export uses the same mechanism to borrow the
  engine for an offline render.
- **The callback wrapper** (`StreamCallback` in [`backend.rs`](../crates/komponist-audio/src/backend.rs))
  splits device buffers larger than the engine's maximum block, converts sample formats and counts
  xruns; the slot enables flush-to-zero and measures the callback load before it calls the engine.
- **`komponist-rt`** sets realtime priority per OS and wraps macOS audio workgroups.

## 13. Built-in devices

### 13.1 Structure

Every built-in device lives in [`crates/builtin-devices/<name>/`](../crates/builtin-devices/README.md):
its DSP, parameter table, layout, state handling, presets, tests and a `clap/` package that builds
the same device as a standalone CLAP plugin. Shared pieces:

- [`core/`](../crates/builtin-devices/core/) (`komponist-device-core`): the device API.
  `Spec`/`CatalogSpec` describe a device (catalog id, parameters, layout, localized names);
  the `Dsp` trait is the simple interface a device implements; `InternalProcessor` adapts a `Dsp`
  to the engine's `Processor` (splitting blocks at parameter events, feeding meters); `dsp.rs` has
  shared helpers (smoothing, one-pole filters, ring buffers, `flush`).
- The bundle manifest lists the shipped CLAP libraries. The catalog reads their descriptors and
  localized metadata through CLAP. Device crates belong to the bundles' build, outside the app's
  dependency tree.
- `hexa/core` and `sampler-shared/core`: large, separately developed DSP engines (FM synthesis and
  sample playback), vendored into this repository.

### 13.2 Reading a device: the Delay

[`delay/src/lib.rs`](../crates/builtin-devices/delay/src/lib.rs) is about 400 lines and a good
first device to read. It implements `Dsp`:

```rust
pub trait Dsp: Send {
    fn activate(&mut self, sample_rate: f32);                 // non-RT: allocate delay lines
    fn set_param(&mut self, index: usize, value: f64);        // RT: just store a target value
    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]);   // RT: the samples
    fn reset(&mut self);                                      // RT: clear tails
    fn latency(&self) -> u32 { 0 }
    // … set_tempo, process_sidechain, meters
}
```

Things to notice in its `process` loop:

- Every 32 samples (`CONTROL`) it recomputes smoothed parameter values; between those it
  interpolates (`self.time.at(done)`). That avoids zipper noise and saves CPU.
- `set_param` only sets `target`s; the audio changes gradually.
- The delay lines are ring buffers allocated in `activate`, read with interpolation
  (`read_hermite`) so that changing the delay time does not click.
- Input is sanitized with `finite()`, feedback is passed through `flush()` to kill denormals.
- No allocation, no locks, no branches that depend on anything but the samples and parameters.

### 13.3 Device ids are forever

The catalog id and parameter ids are stored in project files and automation lanes. Below 1.0 a
change breaks the projects that hold the old id, because there is no migration (D10); at 1.0 they
become permanent. Hexa uses `org.klang-studio.komponist.hexa` and Klopfer uses
`org.klang-studio.komponist.sampler`. The device checklist in
[builtin-devices/README.md](../crates/builtin-devices/README.md) lists everything a new device needs.

## 14. External plugins and the bridge

### 14.1 The pieces

| Crate | Process | Job |
|---|---|---|
| `komponist-plugins` | app | catalog and scan cache, `PluginManager` (lifecycle, status, state), `Bridge` (one child process), `ExternalProcessor` (the engine-side `Processor`) |
| `komponist-ipc` | both | shared-memory layout, ring buffers, wait/wake primitives, control protocol, process watching |
| `komponist-bridge` | bridge | instance mode (load a plugin, serve audio) and scan mode |
| `komponist-hosting` | app and bridge | CLAP (clack-host) for verified shipped bundles in the app, and CLAP/VST3 hosting and scans in the bridge |

The bridge is the same executable: `main()` in
[`komponist-app/src/main.rs`](../crates/komponist-app/src/main.rs) checks for `--bridge` before
anything else starts and hands over to `komponist_bridge::run`.

### 14.2 Scanning

Which plugins are installed? `PluginManager::start_scan` walks the plugin folders and, for every
bundle not in the cache (`plugins.json`), runs `komponist --bridge --scan <bundle>` with a 10 s
timeout. The child loads the plugin, describes it as JSON (`PluginDescriptor`: format, id, name,
vendor, kind, features, sidechain) on stdout and exits. A plugin that crashes during the scan only
kills the scan process.

### 14.3 Loading an instance

When the document gets a device with an external plugin, `EngineSync` asks the `DeviceHost` for a
processor, which ends in `PluginManager::create_processor`:

1. Spawn `komponist --bridge --instance --control-fd N` with a socket pair (Linux/macOS) or a named
   pipe (Windows) as the **control channel**.
2. Create the **shared memory** region and pass it to the child.
3. Queue control messages: `Load`, then `SetState` (if the project has a saved state), then
   `Activate { sample_rate, max_block }`.
4. Return an `ExternalProcessor` *immediately*. Until the bridge reports `Activated` (plus a short
   warm-up), the device status is `Loading`: an effect passes audio through, an instrument is silent.

Control messages (`ControlMsg` app→bridge, `BridgeMsg` bridge→app,
[`control.rs`](../crates/komponist-ipc/src/control.rs)) are encoded with `postcard` and framed;
`PROTOCOL_VERSION` is checked in the bridge's `Hello`.

### 14.4 One audio block across two processes

The shared memory ([`shm.rs`](../crates/komponist-ipc/src/shm.rs), `ShmLayout`, `#[repr(C)]`)
holds a header with two sequence counters (`request_seq`, `done_seq`), an input and an output event
ring (1024 events each), and stereo input, sidechain and output buffers of `MAX_BLOCK` frames.

```text
Engine audio thread (app)               shared memory          Bridge RT thread
ExternalProcessor::begin                                       loop: wait for request_seq
 | write audio_in, push input events ─► buffers, ring           |
 | request_seq = n (release) ─────────► request_seq ──wake────► | read events, run the plugin
 |                                                              | (CLAP or VST3 process call)
 | … engine processes other tracks …                            | write audio_out, events
ExternalProcessor::process                                      |
 | wait on done_seq until the deadline ◄──wake── done_seq = n ◄─ |
 | on time → copy audio_out                                     |
 | late    → effect: pass input through, instrument: silence;   |
 |           the stale result is ignored by its sequence number |
```

The wait is a futex on Linux (`memfd` shared memory), shared-address waits on macOS, and events plus
a high-resolution timer on Windows ([`signal.rs`](../crates/komponist-ipc/src/signal.rs)).

`begin` is the trick that hides the cross-process cost: the engine calls it for instruments at the
head of a chain as soon as their notes for the segment are known, then processes other tracks while
the bridge works. Offline export uses a 2 s timeout instead of the realtime deadline.

### 14.5 When plugins misbehave

`PluginManager::poll` runs every UI frame and checks each bridge:

- **Crash**: the process watcher (pidfd on Linux, kqueue on macOS, process handle on Windows)
  reports the exit → status `Crashed(Exited { … })`.
- **Hang**: the app sends `Ping` every second; no `Pong` within 5 s → `Crashed(Unresponsive)`.
- **Reload** ("Neu laden"): starts a new bridge with the last known state and installs the new
  processor with `Command::InsertProcessor`.

`DeviceStatus` is `Loading`, `Ok`, `Crashed(FailReason)` or `Missing` (plugin not installed; the
device keeps its state so nothing is lost). Status is runtime information and never saved.

### 14.6 Plugin windows

A plugin's own editor is a native window the plugin draws itself; egui cannot see inside it.

- **Linux** (`cfg(x11_embed)`): the app opens an eframe viewport with komponist's header bar, creates
  an X11 child window and sends its id to the bridge; the plugin draws into it.
- **macOS and Windows** (`cfg(bridge_frame)`): the bridge owns the whole window and draws the same
  header; header state and user actions travel over the control channel.

These cfgs are set by the build scripts of the app, bridge and hosting crates.

## 15. The user interface

### 15.1 Startup

`main()` ([`main.rs`](../crates/komponist-app/src/main.rs)):

```text
main
 ├── "--bridge" first?  → komponist_bridge::run (plugin process, no UI)
 └── run_app
      load settings → choose audio (null with --null-audio) → PluginHost → demo project (--demo)
      → start plugin scan → EngineSync::new → App::new(Document, EngineSync, settings)
      → komponist_ui::run(eframe) with a Shell that calls App::show every frame
```

Useful flags: `--demo`, `--null-audio`, `--lang de|en`, `--scale F`, `--open-plugin NAME`,
`--no-plugin-scan`. `scripts/run.sh` builds a release binary and starts it.

### 15.2 The app crate

[`crates/komponist-app/src/`](../crates/komponist-app/src/) is the largest crate. Its main parts:

| Path | What lives there |
|---|---|
| `app.rs`, `app/` | `App`: owns `Document`, `UiState`, `EngineSync`, views, dialogs; `show` and `dispatch` |
| `actions.rs`, `keymap.rs`, `application_menu.rs` | actions, keyboard chords per focus area, menus (native on macOS) |
| `views/` (~39k lines) | arrangement, piano roll, step sequencer, audio editor, mixer, devices, inspector, browser, automation, routing |
| `dialogs/`, `windows/`, `panels/` | settings, export, file dialogs; plugin frame windows; transport bar, status bar, dock tabs |
| `shell.rs`, `layout.rs` | the main window layout |
| `engine_sync.rs`, `engine_sync/` | program compilation and audio/MIDI I/O coordination |
| `device_host.rs` | creating processors for verified bundles and bridged plugins |
| `project_io.rs`, `settings.rs`, `dirs.rs` | files, settings, platform folders |
| `record.rs`, `recording/` | MIDI capture and coordination of MIDI, audio and automation takes |
| `audio_settings.rs` | driver and device inventory for audio settings |
| `export*.rs` | WAV export coordination |
| `showcase.rs` | the "Night Drive" demo project (`showcase_project()`) |
| `bin/komponist-play.rs` | headless demo player |

Views implement the `View` trait (`views/mod.rs`) and receive a `ViewCtx`.

### 15.3 The design system: tokens, widgets, canvases

- **Tokens** ([`komponist-tokens`](../crates/komponist-tokens/README.md)): `design/tokens.json` and
  `design/device-tokens.json` are turned into Rust constants at build time (`color::SURFACE_0`,
  sizes, radii, type styles). **No hard-coded colours or sizes in UI code** (rule 2); component
  metrics that are not tokens go into `komponist_ui::widgets::metrics` with a comment on their source.
- **Widgets** ([`komponist-ui/src/widgets/`](../crates/komponist-ui/src/widgets/)): buttons, knobs,
  faders, meters, number fields, dropdowns, device cards. Run the gallery to see them all:
  `cargo run -p komponist-ui --bin komponist-gallery`.
- **Canvases**: the arrangement, piano roll, step grid and mixer are not built from widgets. They paint
  directly with egui's `Painter`, compute their own geometry and hit testing, and draw only what is
  visible. Pure paint helpers live in [`komponist-ui/src/timeline/`](../crates/komponist-ui/src/timeline/).

### 15.4 Texts and languages

No UI text is written as a literal in the app (rule 1). German is the source language, English the
translation, both as Fluent files in
[`komponist-i18n/locales/{de,en}/`](../crates/komponist-i18n/locales/). A build script generates one
typed method per message:

```ftl
# locales/de/shell.ftl
shell-no-clip-open = Kein Clip geöffnet
```

```rust
ui.label(self.lang.shell_no_clip_open());                    // &'static str
self.lang.status_audio_import_failed(path.display(), why)    // messages with variables → String
```

Widgets get the language with `komponist_ui::lang(ctx)`. A message missing in one language fails
the build.
Read [I18N.md](I18N.md) before adding texts; it has the style guide and glossary.

### 15.5 Accessibility is part of the contract

Every interactive element has an AccessKit role and a localized name (rule 12). Canvases create
nodes for their **visible** objects ("Note C4, 1.1.000", "Clip Beat A, bars 1–4"); see
`PianoRoll::access` and `komponist_ui::timeline::Access`. This is not only for screen readers:
tests (`egui_kittest`) and the `egui` MCP server find and click UI elements through exactly these
nodes. If you cannot find your widget in a test, it probably lacks an accessible name.

## 16. Audio files, streaming, recording and export

### 16.1 Audio files

[`komponist-audiofile`](../crates/komponist-audiofile/README.md) decodes WAV (hound) and AIFF, FLAC,
MP3, Ogg (symphonia), resamples (rubato), builds waveform peaks for display and estimates a file's
tempo. A shared `FilePool` makes sure that one file is decoded and analysed once, however many
clips and samplers use it. None of this runs on the audio thread.

### 16.2 Playing audio clips: the read-ahead thread

The audio thread must never wait for the disk. So:

```text
compile:      audio clips → AudioPlan (which source frames play at which song frames; no file IO)
read-ahead:   "komponist-stream" thread: open, decode, resample, stretch (Warp), apply clip gain
              and fades → per-track SPSC ring (32768 frames)
audio thread: Voice::render reads the ring; missing samples become silence + underrun telemetry
```

When the playhead jumps (locate, loop), the audio thread sends a small message with a new **epoch**,
and the read-ahead thread refills from the new position; ring data from an old epoch is ignored. The
first 100 ms of every segment are cached, so playback can start instantly after a jump. Offline
export fills the rings synchronously before each block, so it never underruns.

### 16.3 Recording

Recording audio must also avoid disk IO on the audio thread. Track jobs copy captured input into
bounded per-track queues (`Recorder::capture`); a writer thread (`komponist-record`) drains them
into WAV files and publishes progress for the live waveform. MIDI recording sends recorded notes as
engine events; the app grows the take inside a gesture, so the whole take is one undo step.

### 16.4 Export

Export renders faster than realtime with the same engine code:

1. Detach the engine from the audio stream (the stream outputs silence meanwhile).
2. On a worker thread: reactivate processors at the export rate, switch plugins to offline mode,
   recompile latency compensation, render the range plus tail with `Engine::render`, remove the
   project latency, optionally normalize, dither for 16/24-bit, write WAV.
3. Write into a staging folder and publish only when complete; always reattach the engine.

---

# Part III – Working on komponist

## 17. Tests and the gate

### 17.1 What is tested where

| Level | Where | Typical style |
|---|---|---|
| Model | `komponist-model` unit tests, proptests | pure functions: edit → assert project |
| Engine | `komponist-engine/tests/{engine,compile,golden}` | render offline, assert exact samples; serial = parallel; `assert_no_alloc` around every `process` |
| Devices | each device's `tests.rs` | parameter/state round trips, realtime allocation |
| Hosting/IPC | plugin tests with the `test-plugins/` fixtures | real bridges with fixture plugins that crash, hang, add latency or use sidechains |
| App | `komponist-app/tests/it/` | `App::headless(project)` with the null backend; `app.dispatch(Action::…)`; kittest UI flows |
| UI pixels | `tests/snapshots/*.png` | kittest renders views; Linux (lavapipe) is the reference |

Each crate has **one** integration test binary, `tests/it/main.rs`, with one module per topic,
because every test binary links egui and wgpu again (slow to build). Add new tests as modules
there. Run one module with `cargo test -p komponist-app --test it piano_roll::`.

### 17.2 UI tests and snapshots

A UI test builds a headless app, drives it through AccessKit and optionally compares a PNG:

```rust
// tests/it/acceptance_flow1.rs (shortened)
let mut h = harness(App::headless(demo_project()));
ctrl(&mut h, Key::T);                              // Ctrl+T: new armed Hexa track
let p = h.state().project();
assert_eq!(p.tracks[0].channel.devices[0].plugin.name, "Hexa");
h.get_by_role_and_label(Role::Row, &format!("Spur {}, Hexa", p.tracks[0].name));
```

Snapshot tests call `h.snapshot("shell-s01-1x")`, which compares with
`tests/snapshots/shell-s01-1x.png`.

Look at the PNGs you produce (rule 7), in German *and* English (`-en` snapshots): nothing may be
cut off. Only Linux updates reference images; never run `UPDATE_SNAPSHOTS` on macOS or Windows.

### 17.3 The gate

```bash
scripts/build-test-plugins.sh   # once, when you touch plugins, devices, catalog, browser or engine
scripts/check.sh                # fmt --check, clippy -D warnings, cargo test --workspace
```

While working, iterate narrowly (`cargo check -p <crate>`, one test module). Run `check.sh` once
before you report; it takes a shared build lock so that parallel builds do not freeze the machine.
See AGENTS.md for worktrees and build load rules.

### 17.4 Seeing your change live

`scripts/run.sh --demo` starts the app. With `EGUI_INSPECTION=1` the app exposes its AccessKit tree,
input and screenshots to the `egui` MCP server, so an agent or a script can click through it.
`--null-audio` runs without audio hardware.

## 18. Recipes: "I want to …"

**… add a user action (a button, a shortcut).** Add a variant to the right sub-enum in
`actions.rs`; handle it in `App::dispatch` (or `app/edit_dispatch.rs` for edits); send it from the
view with `ctx.send`; for a shortcut, add a binding in `keymap.rs`. If it edits the document, the
edit belongs in `komponist-model/src/edits/` as a pure function with its own tests. Write a
dispatch test with `App::headless`.

**… add a UI text.** Add the key to the matching file in `locales/de/` and `locales/en/` in the same
change, then call the generated method on `Lang`. Follow [I18N.md](I18N.md).

**… add a new kind of engine command.** Add a variant to `Command` in `api.rs` and handle it in
`Core::apply` in `engine.rs`. The command must not allocate when applied: anything big travels
preallocated inside the command (`Box` created on the UI thread) and leaves as `Garbage`. Add an
engine test that runs inside `assert_no_alloc`.

**… change what the engine plays for a document feature.** Usually: model field → compiler
(`compile.rs` writes it into `TrackProgram`/`Program`) → engine reads it during `segment`. Keep
expensive work in the compiler, keep the engine's per-sample work simple. Check that unchanged
tracks still reuse their compiled program (pointer equality).

**… add a built-in instrument or effect.** Follow the checklist in
[builtin-devices/README.md](../crates/builtin-devices/README.md): a crate with `Spec`, `Dsp`
implementation, stable CLAP and parameter IDs, DE/EN names, packaging, and a musically sensible
use in the demo project (`bundled_demo()`, rule 15) with a balanced mix.

**… change the project file.** Add the field (optional data with `#[serde(default)]`), bump
`FORMAT_VERSION` in `persist.rs` when files of the other version must be rejected, and update every
user in the same change. There is no migration and no test against old files (rule 16). Read
ARCHITECTURE §5.5.

**… build new UI.** Use the existing widgets and tokens. A new screen or a larger change needs a
proposal with mockups in the internal notes and the owner's approval first (AGENTS.md, rule 3).

## 19. Pitfalls

- **Dropping something on the audio thread.** Replacing a `Vec` or `Box` field in the engine drops
  the old value right there. Send it back as garbage instead. `assert_no_alloc` catches frees too,
  but only in code paths a test exercises.
- **"It's just one `Vec::push`."** Even with spare capacity today, growth tomorrow allocates. Use
  fixed capacities and drop-and-count overflow.
- **Locks hidden in libraries.** Logging macros, `Arc<Mutex<…>>` inside a helper, channel types that
  allocate per message: check what a function does before calling it from RT code.
- **Forgetting the note-off.** Any code that removes, mutes or reroutes notes must make sure sounding
  notes end. Look at how the sequencer and the MIDI stage use note guards.
- **Reading the language inside a `widget_info` closure.** Read it once before; see the pitfall in
  [I18N.md](I18N.md).
- **Hard-coded design values.** Colours, sizes and font sizes come from tokens or `metrics`, with a
  source comment.
- **Snapshots on the wrong platform.** Only Linux writes reference PNGs.
- **New `tests/*.rs` files.** They create another heavy test binary; use a module in `tests/it/`.
- **Stable ids.** Never rename a catalog id, parameter id or serialized field name; projects and
  automation depend on them.
- **`cargo clean` in a worktree.** It deletes the `target` symlink to the fast disk (AGENTS.md).

## 20. Glossary

| Term | Meaning |
|---|---|
| **Block / buffer / period** | The number of frames processed per audio callback. |
| **Bridge** | The separate process (`komponist --bridge --instance`) that hosts one external plugin instance. |
| **Bus** | A track without clips that sums other tracks; a **group** is a bus shown as a folder. |
| **Callback** | The function the audio system calls for every block; komponist's engine runs inside it. |
| **Chain** | The ordered devices of a channel: MIDI effects → instrument → audio effects. |
| **Chase** | Starting notes that should already sound when playback starts or jumps mid-note. |
| **CLAP / VST3** | Plugin standards komponist hosts. |
| **Command** | A UI → engine message (`Command` in `api.rs`). |
| **Compile** | Turning a `Project` into a `Program` (`compile.rs`). |
| **dB / dBFS** | Logarithmic level; `gain = 10^(dB/20)`; dBFS is relative to full scale (1.0). |
| **Deadline** | Start of the callback + 60 % of its duration; bridges and workers must finish by then. |
| **Denormal** | A tiny float that is slow on some CPUs; avoided with flush-to-zero. |
| **Device** | An instrument or effect in a chain, built-in or external. |
| **Document** | The editable project plus undo/redo (`Document` in the model). |
| **DSP** | Digital signal processing: arithmetic on samples. |
| **Epoch** | A counter that marks streaming data as belonging to a particular playhead jump. |
| **Frame** | One sample per channel: one moment of stereo audio. |
| **Garbage** | Objects the engine is done with, sent back to the UI thread to be dropped. |
| **Gesture** | A sequence of edits (a drag) that becomes one undo step. |
| **Instrument** | A device that turns notes into audio. |
| **Latency** | Delay, e.g. of the audio buffer or of a look-ahead effect. |
| **MIDI effect** | A device that transforms note events before the instrument. |
| **PDC** | Plugin delay compensation: delaying paths so all signals stay in sync. |
| **PPQ / Tick** | Pulses per quarter note; komponist uses 960 ticks per quarter. |
| **Processor** | The engine's trait for a device (`processor.rs`). |
| **Program** | The compiled, play-ready form of the project that the engine runs. |
| **Read-ahead** | The thread that prepares audio-clip samples ahead of the playhead. |
| **RT / realtime** | Code that must finish within the block deadline; no alloc, locks or IO. |
| **Sample / sample rate** | One measurement of the waveform; measurements per second. |
| **Send** | A copy of a track's signal into a bus. |
| **Sidechain** | An extra audio input to a device from another track (e.g. for ducking). |
| **Slot** | The engine's numbered place for one processor (`SlotId`). |
| **SPSC queue** | Single-producer single-consumer lock-free ring buffer (`rtrb`). |
| **Take** | One recorded or alternative version inside a clip; one is active. |
| **Telemetry** | Atomic values the engine publishes for the UI (playhead, meters). |
| **Transport** | Play/stop/record, playhead, loop and count-in. |
| **Warp** | Following the project tempo with an audio clip without changing pitch (time stretching). |
| **Xrun** | A dropout because the callback was late. |

## 21. Where to read next

- [ARCHITECTURE.md](ARCHITECTURE.md): the full reference, including routing semantics, automation,
  audio-track details, platform specifics and decisions.
- [PERFORMANCE.md](PERFORMANCE.md): measurements, the RT-safety sweep and how to re-run benchmarks.
- [I18N.md](I18N.md): texts, languages, style guide and glossary.
- [INSTALL.md](INSTALL.md): building, installing, working on klang-dsp locally.
- The crate READMEs and the `//!` module docs, especially
  `komponist-engine/src/{api,compile,processor}.rs`, `komponist-app/src/{actions,engine_sync}.rs` and
  `komponist-model/src/document.rs`.
- The test fixtures in [test-plugins/README.md](../test-plugins/README.md) show what the plugin
  host must survive.

A good first exercise: run `komponist --demo --null-audio`, set a breakpoint (or a `dbg!`) in
`EngineSync::sync`, move a note in the piano roll and follow it through sections 10 and 11 in the
debugger.
