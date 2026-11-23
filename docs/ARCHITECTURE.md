# komponist – Software Architecture

Status: **implemented architecture**, checked against the repository on 2026-11-19.

komponist is an audio and MIDI DAW for Linux, macOS and Windows. This document describes the
current implementation and the decisions embodied in it. Dependency versions are defined in
`../Cargo.toml` and resolved in `../Cargo.lock`. Installation and packaging instructions are in
[INSTALL.md](INSTALL.md), language and catalog rules in [I18N.md](I18N.md), and measurements in
[PERFORMANCE.md](PERFORMANCE.md). New to the project, or to audio and realtime programming? Start
with the guided tour in [ARCHITECTURE-GUIDE.md](ARCHITECTURE-GUIDE.md).

## 1. Architectural boundaries

- The UI owns the editable document. The realtime engine consumes compiled snapshots, commands
  and timestamped input, and returns events and telemetry.
- The engine is independent of the UI, audio backend and plugin formats. The audio backend can
  rebuild a stream or hand the engine to an offline render without rebuilding the document.
- Every external plugin instance runs in a separate bridge process. A failed plugin produces
  silence for an instrument or passthrough for an effect while the rest of the project runs.
- Built-in instruments and effects run in process through the same engine processor interface.
- Audio decoding, source analysis, sample-rate conversion, pitch-preserving stretch and recording
  file writes run outside the realtime callback.
- UI appearance comes from generated design tokens and shared widgets. The running app and its
  snapshots define the implemented UI; the original design handoff is historical.

## 2. Technology stack and decisions

### 2.1 UI

The UI uses **egui and eframe 0.37**, rendered by **wgpu**. eframe provides the event loop,
viewports, clipboard, input methods, AccessKit and inspection support. Dense editors use custom
`Painter` canvases with their own geometry, hit testing and visible-object accessibility nodes.
Linux uses X11, including XWayland on Wayland desktops, to embed plugin editors.

### 2.2 Audio and MIDI

**cpal 0.18** supplies audio devices and streams; **midir 0.11** supplies MIDI inputs. Linux also
has a native JACK path for explicit routing and capture. Audio and MIDI callback boundaries are
separate from the engine. `NullBackend` and synthetic input sources support operation and testing
without audio or MIDI hardware.

### 2.3 Communication and persistence

**rtrb** SPSC queues connect the UI, input and realtime processing paths. Atomics carry telemetry.
Plugin audio and events use shared memory with OS-specific signalling; **postcard** encodes
control messages. **serde/serde_json** encode projects, settings and caches. Project files are
versioned JSON, with external plugin states stored as base64.

### 2.4 Plugin format layer

The host implements its own format-independent `HostedPlugin` interface over **clack-host 0.2**
for CLAP and **vst3 0.3** for VST3. `komponist-hosting` implements the formats; `komponist-bridge`
runs them. The main app manages instances through IPC rather than calling plugin format APIs.
The **nice-plug** fixtures in `test-plugins/` export both formats from the same test sources.

### 2.5 Core decisions

| ID | Implemented decision |
|---|---|
| D1 | egui/eframe with wgpu; X11 on Linux |
| D2 | cpal and midir, with native JACK routing/capture on Linux |
| D3 | Own hosting layer on clack-host and vst3 |
| D4 | One bridge process per external plugin instance |
| D5 | Klopfer and the Sampler share `sampler-core` and sample loading. The separate Klopfer CLAP plugin is scanned normally. |
| D6 | Internal Hexa backed by `hexa-core` with `alloc` and `serde` |
| D7 | Versioned `*.komponist` JSON project file and linked audio assets |
| D8 | Immutable document snapshots with `Arc` structural sharing for undo/redo |
| D9 | Signed 64-bit musical ticks at 960 PPQ; position display at 240 ticks per beat |

## 3. Processes and threads

```text
komponist (main process)
  UI thread: Document, UiState, actions, EngineSync, PluginManager, windows
      │ commands / compiled Program          ▲ events / retired objects
      ▼                                      │
  Audio callback: Engine ── worker pool: independent routing jobs
      │                ▲
      │                ├── timestamped MIDI and captured audio
      │                └── audio read-ahead rings
      │ shared memory + deadline signalling
      ▼
komponist --bridge --instance (one process per external plugin)
  Main thread: control messages, plugin main-thread work, timers, GUI
  RT thread: input/events → plugin processing → output/events

Background work: scan processes, project IO, sample loading, waveform/tempo analysis,
                audio read-ahead/stretch, recording writer, offline export
```

| Thread | Responsibility | Communication |
|---|---|---|
| Main/UI | Document/history, actions, views, plugin lifecycle | Commands, snapshots, atomics, non-RT IPC |
| Audio callback | Transport, sequencing, recording capture, job dispatch, master | Bounded queues, atomics, preallocated buffers |
| Engine workers | Track/bus processing jobs | Atomic job claims and deadline waits |
| MIDI callbacks | Timestamp input and report device activity | SPSC queues |
| Audio read-ahead | Decode and render clip source audio | Per-track rings and epoch requests |
| Recording writer | Drain captured samples, write/finalize WAVs | Bounded sample and timestamp queues |
| Bridge main | Plugin control, state, host loop and GUI | Control channel and bridge-local queues |
| Bridge RT | Plugin audio processing | Shared memory and request/completion sequence words |

The UI and background threads can allocate and perform file IO. Realtime processing cannot
allocate, free, lock, log or perform file/socket IO. Synchronization waits follow the bounded
bridge and worker protocols described below.

## 4. Workspace and crates

The Cargo workspace uses Rust edition 2024 and resolver 3. The [Plugin SDK](../crates/plugin-sdk/README.md)
and `test-plugins/` have their own workspaces.
A clone builds without sibling repositories: **klang-dsp** is a git dependency pinned to a commit;
**hexa-core** and **sampler-core** are vendored engines developed independently in this repository.
Their READMEs record provenance.

| Crate | Responsibility |
|---|---|
| [`komponist-model`](../crates/komponist-model/README.md) | Project tree, musical time, edits, undo, routing validation, parameter descriptions, persistence |
| [`komponist-engine`](../crates/komponist-engine/README.md) | Processor interfaces, compilation, transport, sequencing, mixer, routing scheduler, clip streaming, recording, export |
| [`komponist-audio`](../crates/komponist-audio/README.md) | Audio/MIDI backends, device enumeration, stream lifecycle, input alignment, statistics |
| [`komponist-rt`](../crates/komponist-rt/README.md) | OS realtime scheduling, workgroups and wait primitives |
| [`komponist-audiofile`](../crates/komponist-audiofile/README.md) | Seekable decoding, resampling, shared file pool, peak cache, source tempo analysis |
| [`komponist-stretch`](../crates/komponist-stretch/README.md) | Worker-only Rust/C++ wrapper around vendored Signalsmith Stretch/Linear |
| [`hexa-core`](../crates/builtin-devices/hexa/core/README.md) | Hexa six-operator FM engine |
| [`sampler-core`](../crates/builtin-devices/sampler-shared/core/README.md) | Shared Klopfer and Sampler engine |
| [`komponist-device-core`](../crates/builtin-devices/core/src/lib.rs) | Unstable device API: catalog and parameter metadata, layouts, state, presets, processor adapters and shared DSP |
| [Device crates](../crates/builtin-devices/README.md) | One directory per instrument, audio effect and MIDI effect, with a standalone CLAP package alongside its DSP |
| [`komponist-builtin`](../crates/builtin-devices/registry/README.md) | Built-in device list, catalog and processor factory |
| [`komponist-ipc`](../crates/komponist-ipc/README.md) | Shared-memory layout, signalling, control protocol, process watching, frame vocabulary |
| [`komponist-hosting`](../crates/komponist-hosting/README.md) | CLAP/VST3 loading, processing, state, parameters, GUI and host services |
| [`komponist-bridge`](../crates/komponist-bridge/README.md) | Bridge library and utility executable; instance and scan modes |
| [`komponist-plugins`](../crates/komponist-plugins/README.md) | App-side catalog, scan cache, bridge lifecycle, `ExternalProcessor` |
| [`komponist-tokens`](../crates/komponist-tokens/README.md) | Generated design tokens, colour conversion and embedded font assets |
| [`komponist-i18n`](../crates/komponist-i18n/README.md) | Generated typed DE/EN text catalog from Fluent sources |
| [`komponist-ui`](../crates/komponist-ui/README.md) | Shared egui theme, widgets, canvases, plugin-frame header, gallery |
| [`komponist-app`](../crates/komponist-app/README.md) | Application shell, actions/keymap, views/dialogs, document/engine synchronization, project IO |

`komponist-model` depends on no other komponist crate and receives localized names as arguments.
The engine knows neither egui nor IPC nor CLAP/VST3. `komponist-audio` defines an `AudioProcess`
callback interface without depending on the engine; the app supplies the adapter.
`komponist-device-core` uses the model, i18n catalog and engine processor interfaces.
Individual device crates build on that API. The `komponist-builtin` registry depends on the
devices and supplies catalog metadata and processor factories to the app and plugin catalog.
App code that works with a specific device also depends directly on that device's crate.
`komponist-audiofile` and `komponist-stretch` have no UI or model dependencies. The bridge is the
runtime boundary for loading external plugin code, even though it ships in the app executable.

The separate [test workspace](../test-plugins/README.md) contains the plugin fixtures
[`kp-test-synth`](../test-plugins/kp-test-synth/README.md),
[`kp-test-fx`](../test-plugins/kp-test-fx/README.md),
[`kp-test-sc`](../test-plugins/kp-test-sc/README.md),
[`kp-test-crash`](../test-plugins/kp-test-crash/README.md) and
[`kp-test-slow`](../test-plugins/kp-test-slow/README.md), along with the
[`fake-host`](../test-plugins/fake-host/README.md) harness and the
[`xtask`](../test-plugins/xtask/README.md) bundling tool.

### 4.1 Platform cfgs

Linux, macOS and Windows have concrete backends. Semantic cfgs generated by the app, bridge and
hosting build scripts distinguish plugin-window ownership:

| cfg | Platforms | Meaning |
|---|---|---|
| `x11_embed` | Linux | App-owned frame with a foreign X11 plugin child |
| `bridge_frame` | macOS, Windows | Bridge-owned frame; header model and actions travel over IPC |

OS-specific modules implement shared memory, waits, control channels, process watching, library
loading, GUI embedding and activation. Unix fd services use `cfg(unix)`; AppKit and Win32 calls
stay in their platform modules. Unsupported platform backends fail at compile time, while the
portable realtime/audio helpers retain their defined fallbacks.

## 5. Data model (`komponist-model`)

### 5.1 Time

`Tick(i64)` uses **960 PPQ**: a 4/4 bar has 3840 ticks and a sixteenth-note step 240 ticks.
Position display uses bar/beat/tick with **240 display ticks per beat**. Negative transport
positions represent count-in; document objects start at or after zero. `TempoMap` converts ticks,
seconds and sample positions. Project tempo is bounded to 40–240 BPM.

### 5.2 Document

`Project` holds tempo, meter, loop/transport settings, tracks, the master channel and an ID counter.
Tracks, clips, notes and devices use shared immutable data. Object IDs are stable `u64` values
allocated from `Project::next_id`; overflow rejects an edit or file instead of wrapping.

- **MIDI tracks** contain MIDI and step clips. MIDI clips hold named takes with one active take;
  step clips hold rows, resolution, swing and step velocity/probability/ratchet data.
- **Audio tracks** contain linked audio clips and hardware/bus input plus monitoring settings.
- **Buses** contain audio effects and routing state, without clips, instruments, MIDI effects or
  record arm. A group is a bus with folder presentation; an effect bus uses the same track kind.
- **Channels** hold ordered devices, fader, pan, mute and solo. A MIDI chain is
  MIDI effects → at most one instrument → audio effects. Audio tracks, buses and the master hold
  audio effects only. Edits enforce these roles and ordering.
- **Devices** store plugin identity, persistent state, bypass and optional sidechain source.
  Loading/crash/missing status belongs to runtime management, outside the saved document.
- **Automation** uses arrangement lanes on tracks and the master, plus envelopes in MIDI and step
  clips. Each lane has a stable ID, a target and its own playback/recording mode (§6.12).

### 5.3 Edits and undo

Edits are pure, fallible functions in `edits/`. They clone the path to changed data with
`Arc::make_mut` and return a new project. `Document` keeps current, undo and redo roots; undo/redo
swaps roots with structural sharing. History is bounded to 500 steps.

A gesture captures its starting root, updates the current root during interaction and adds one
history step on completion. Cancellation restores the starting root. This applies to drags,
wheel bursts and recording. Device parameters, presets, sample changes and external plugin state
changes participate in history. External state snapshots are coalesced per device.

Fader, pan, mute/solo, send levels and reachable device parameters use engine fast paths during
interaction. Parameter undo/redo applies differences to running processors where possible, keeping
held notes alive; changes that cannot be expressed through the parameter interface replace the
processor. Transport settings, save-time asset path rewrites and plugin echoes of restored state
use explicit untracked document operations. Path rewrites preserve each history step's own state.
Selection, focus, zoom and open panels live in `UiState` and do not participate in undo.

### 5.4 Formatting

Pure formatting functions provide musical positions, elapsed time, dB, pan and note names
(`C4` = MIDI 60). They use the UI's typographic conventions, including the U+2212 minus sign and
`−∞`. Localized words are supplied by callers.

### 5.5 Persistence

A `*.komponist` file is JSON with `format: "komponist"`, **version 5** and a `project` object.
Plugin states are base64 and survive a missing plugin unchanged. Loading validates data and
routing, repairs defined dangling-reference cases and rejects cycles or invalid track content.
Explicit migrations give v1 files master outputs and add the audio-capable v3 schema after v2.
Version 4 adds optional track, clip and master automation. The v3 → v4 migration leaves the
project JSON unchanged and missing automation fields default to empty. Version 5 adds an optional
eight-slot macro array to each track. The v4 → v5 migration also leaves the project JSON
unchanged. Newer unsupported file versions are rejected.

Audio assets remain linked by relative and absolute paths. Save As rebases resolving links;
Collect and Save copies audio and Sampler sources into the project's sample folder, deduplicating
identical content. Audio takes and optional Warp metadata extend v3. Legacy active-source fields
remain serialized: older v3 readers play the active source without Warp, can shorten stretched
spans, and discard optional data on save. Compatibility is checked against a pinned pre-Warp reader.

| Platform | Settings and recent projects | Plugin cache |
|---|---|---|
| Linux | `$XDG_CONFIG_HOME/komponist` or `~/.config/komponist` | `$XDG_CACHE_HOME/komponist` or `~/.cache/komponist` |
| macOS | `~/Library/Application Support/komponist` | `~/Library/Caches/komponist` |
| Windows | Roaming AppData `komponist` | Local AppData `komponist` |

Settings and recent lists are separate `settings.json` and `recent.json`; the scan cache is
`plugins.json`. Windows resolves known folders through the shell, with environment fallbacks.
Waveform and tempo caches are user cache data rather than project assets.

## 6. Engine (`komponist-engine`)

### 6.1 Realtime rules

Processing uses preallocated buffers and bounded event/command storage. Current limits include
2048 frames per processing block, 128 tracks, 1024 processor slots, 1024 events per block and
8192 frames per PDC delay line. Larger device callbacks are split into engine blocks.

Debug/test allocator guards detect allocation and freeing in processing. Retired programs,
processors and recording endpoints travel through a garbage queue to be destroyed off the audio
thread. The callback sets FTZ/DAZ; worker jobs copy its floating-point mode.

### 6.2 Processor interfaces

`Processor` supplies non-RT activation and RT processing, latency, parameter updates and reset.
`process_sidechain` supplies an aligned stereo sidechain; `begin` can start a bridge block early.
An optional `EventProcessor` transforms bounded event lists before the instrument instead of
processing audio. `ProcessContext` supplies frame count, sample rate, transport, offline mode and
the realtime deadline. Events have sample offsets and include notes, raw MIDI, parameter changes
and all-notes-off.

Implementations include Hexa, Volt, Surf, Klopfer, Sampler, internal audio/MIDI effects and `ExternalProcessor`.
Test processors in `komponist_engine::testing` exercise the same contracts. The metronome belongs
to the engine rather than a track device.

### 6.3 Programs, commands and telemetry

`compile::build` converts a project into a `Program`: sorted sequencer data, processor slots,
audio plans, routing, gates and latency information. Compilation runs outside the callback,
reuses unchanged shared data and performs no audio-file IO. `EngineSync` installs programs and
maintains slots as the document changes.

Compilation never fails: what the engine cannot play is left out of the program. The app compiles
with `compile::build_with_diagnostics`, which also returns one `Diagnostic` per left-out track or
device (position, id and reason: track, MIDI-effect or slot limit, duplicate id, a device in the
wrong place or on a chain that takes effects only), in project order (#92). `EngineSync` keeps the
diagnostics of the program it sent; the status bar reports new ones, the inspector marks the track
or device "spielt nicht" with the reason, and the export summary lists them.

UI-to-engine commands include transport, program/processor installation, fast parameter and
mixer changes, live notes and recording setup. Engine-to-UI events include captured MIDI,
transport transitions, processor failures and retired objects. Atomic telemetry supplies
playhead/state, CPU/xruns, track/master/input meters, held input keys, streaming health and
leased audio taps. Meter ballistics and visualization transforms run in the UI.

### 6.4 Block processing

1. Drain a bounded number of commands and timestamped input events.
2. Split transport processing at loop/count-in boundaries and generate metronome events.
3. Collect sequenced/live MIDI and capture recording input. Start eligible bridged instruments
   early, once their events and required inputs are available.
4. Run track and bus jobs: source/events → MIDI stage/instrument or audio source → audio effects
   → gain ramp → pan → gates, meters and routing taps. Buses first sum aligned inputs.
5. Sum master inputs in fixed order, run master inserts and fader, then meter/tap and output.
   Add the metronome after the master fader.
6. Publish telemetry and events.

Stop, locate and loop wrap end active notes while effects/release tails continue. A second Stop
performs a short output fade and resets processors. Program swaps keep track state by ID.

#### 6.4.1 Worker pool

A fixed pool is prepared outside the callback. Workers and the audio thread atomically claim
ready routing jobs. The scheduler respects dependencies, keeps each track's state exclusive and
sums inputs in a fixed order, producing deterministic serial/parallel output. Short segments,
small job sets and unavailable workers use the serial path.

The dispatcher claims unstarted jobs itself and waits only for work another thread already owns.
A late owned job cannot be abandoned or processed twice; deadline overruns are counted, and the
dispatcher continues waiting for completion. A job that panics never ends a batch early: on a
worker it is re-raised on the dispatcher afterwards, and a panic on the dispatcher waits for the
rest of the batch before it unwinds, because worker jobs read the segment's context on the
dispatcher's stack. `worker_threads` supports automatic sizing or an
explicit count, including zero. Automatic sizing accounts for available CPU topology and the
macOS audio workgroup's parallelism limit.

Linux uses SCHED_FIFO and futexes; macOS uses time-constraint threads, device audio workgroups
and process-local `os_sync` waits with timer-leeway compensation; Windows uses MMCSS
("Pro Audio") with a thread-priority fallback and `WaitOnAddress`. Scheduling failures are reported.
Offline rendering uses the same job processing path.

#### 6.4.2 MIDI effect stage

MIDI effects transform events in chain order before the instrument. Fixed-capacity buffers and
note guards preserve note identity and releases across transforms, bypass and transport changes.
Recording captures the input before this stage. MIDI effects reset on all-notes-off and have no
audio tail.

### 6.5 Sequencing and MIDI recording

Compilation expands clip loops, swing and ratchets, clipping notes at clip ends. Probability uses
a deterministic per-track RNG seeded at playback start. An active-note table owns note-offs
across program swaps. Start, locate and loop wrap chase notes already sounding at the destination.
MIDI input timestamps are mapped to sample offsets and recording positions account for output
latency. Recording grows a take in one gesture; loop passes become takes in the same clip.

A MIDI track monitors when explicitly enabled or armed. Input filters and per-channel held-note
tracking release live notes when monitoring or routing changes without flushing sequencer notes.
Computer-keyboard notes retain their original target until release.

### 6.6 Latency compensation (PDC)

The compiler sums active processor latency and aligns graph inputs with preallocated delay lines.
Flat routing reduces to the longest track chain minus each track's own latency. Routing and
sidechain alignment use the graph rules in §6.8.5. Reported latency changes trigger compilation;
delay-line identity is retained across compatible program changes.

### 6.7 Offline render and export

The app detaches the engine and its handle from the audio stream, which outputs silence while a
worker renders. This handoff belongs to the audio stream lifecycle rather than an engine command.
The transport stops during export. Plugins switch to offline render mode, and the app waits for
activation/warm-up; unavailable devices are listed so the user can explicitly export without them.

The worker sets the export rate, reactivates processors and recompiles PDC at that rate. It renders
the selected song/loop/selection range plus tail into float memory, discards project latency for
timeline alignment, optionally normalizes to −1 dBFS and writes WAV through `hound`. Integer
16/24-bit output uses TPDF dither; float output does not. Source read-ahead runs synchronously
before offline blocks using the same renderer as playback.

Output is written in an exclusive staging directory and published only when complete. Errors or
cancellation preserve an existing destination. Every outcome restores the program, device rate,
engine attachment and bridge render mode. Track rendering/flattening uses the engine's track
export path and the app's undoable project edits.

### 6.8 Routing: buses, sends and sidechains

#### 6.8.1 Graph and track kinds

Nodes are MIDI tracks, audio tracks, buses and the master. Edges are outputs, sends, sidechains
and bus inputs to audio tracks. Each non-master node has one output to the master or a bus.
MIDI/audio tracks can send to buses; buses have no sends. A group and an effect bus share
`TrackKind::Bus`: `folder` controls presentation. Group membership is defined by output routing,
with members kept adjacent in document order. Deleting a group dissolves it and repairs routing.
New sends default to −6 dB post-fader; new sidechains default to pre-fader.

#### 6.8.2 Signal flow and taps

```text
MIDI: events → MIDI effects → instrument ─┐
Audio: clips + monitored input ───────────┼→ audio effects → Pre → fader → pan → mute → Post → output
Bus: aligned sum of outputs/sends ────────┘
```

Pre is behind the device chain and before the strip; Post is behind fader, pan and mute.
Muted or solo-muted tracks have silent sends, including pre-fader sends. Sidechains are control
signals: pre ignores source fader/mute/solo; post follows fader/mute but ignores solo.
Bus recording/monitoring takes its source's post-fader/pan/mute signal.

#### 6.8.3 Invariants and edits

The complete dependency graph is acyclic, including sidechain and audio-input edges. No node
feeds itself. Output/send targets are buses, each track sends at most once to a target, and the
master has no outgoing edge and is not a sidechain source. Track content and device roles match
the track kind. Routing queries expose valid targets to the UI; edits reject cycles and invalid
kinds, normalize group order and repair references when tracks/buses disappear. Duplicating tracks
remaps references within the copied set. Copying devices retains a sidechain only if it remains valid.

#### 6.8.4 Compilation and scheduling

`compile/graph.rs` builds a topologically ordered job graph with per-node inputs, dependencies,
tap requirements, gates and delay-line identities. `graph.rs` and `workers.rs` execute it.
A completed job publishes newly ready dependents. Independent branches run in parallel; bus
sums remain ordered. The master runs after track/bus jobs on the audio thread. Flat projects
retain the direct-to-master path. Gate tables and send-gain commands support mixer gestures
without recompiling every frame; send gain changes ramp over processing segments.

#### 6.8.5 Graph latency

A node's input latency is the latest arrival of its required inputs; its output latency adds its
active chain latency. Each incoming edge delays earlier sources to that input latency. Sidechains
align with the main signal at their device's chain position and can raise the holder's input
latency. MIDI/audio source tracks use a head delay before audio effects when required. A sidechain
into a MIDI instrument itself arrives without that head alignment. Delay lengths are clamped to
`MAX_PDC_FRAMES`; the master is the final join node.

#### 6.8.6 Mute, solo and meters

`routing::gates` computes output and send audibility along signal paths, including groups and
soloed effect buses. Sidechain control paths use their separate tap semantics. The UI sends the
compiled gate table on mute/solo changes. Input and output metering remain separate; UI meter
ballistics operate on atomic peak readings.

#### 6.8.7 Export

Export uses the same graph, taps and compensation as playback. Project latency is removed from
the rendered file, including master latency. Recompilation after an export-rate change keeps
skip and delay compensation in the same sample-rate domain.

#### 6.8.8 Realtime storage

Node buffers, edge delays, gate tables and job counters are preallocated. A job writes only its
own node state and scratch lane. Stable track/device/send identities retain compatible runtime
state when programs change.

#### 6.8.9 Audio tracks in the graph

An audio track replaces MIDI sequencing/instrument generation with streamed clip audio plus
monitored input. It retains effects, output, sends, sidechain taps, PDC and gates. A bus input adds
a dependency on that bus and uses its post tap without applying the input's graph delay twice.

### 6.9 Audio tracks and clips

#### 6.9.1 Sources, takes and time mapping

An audio clip holds named source takes with one active take. Each source has linked file metadata
and a native-frame offset. Clip gain, fades, time mode and original tempo are shared by takes.
Fixed is the default; Re-Pitch follows tempo through variable-speed playback; optional Warp
follows tempo while preserving pitch. `WarpMap` uses rational source/tick mapping and phase
tracking, keeping repeated splits/trims aligned. Full-source bar counts refer to the entire active
source file rather than the trimmed clip.

#### 6.9.2 Edits and overlap

Audio clips on a track do not overlap: placing or extending a clip cuts covered material in the
same undo step, shortening, splitting or removing underlying clips. Splits and left trims advance
take source windows; moves keep source mapping. Clip fades/gain remain nondestructive. Playback
adds short edge fades and recognizes source-continuous splits so they play seamlessly.

#### 6.9.3 Playback and disk streaming

Compilation produces an `AudioPlan` of non-overlapping song-frame segments without reading files.
One read-ahead thread per engine opens sources, decodes/resamples and renders clip gain/fades into
per-track 32768-frame SPSC rings. Re-Pitch uses cubic interpolation. The callback consumes ring
samples and cached 100 ms segment heads; unavailable samples become silence with underrun telemetry.
It never waits for disk. Epochs distinguish plans/positions after jumps; read-ahead predicts loop
wraps. Stop/locate transitions use short fades. Missing sources are retried and exposed as status.
Offline rendering synchronously fills its own plans/rings before processing, independent of worker
timing. A plan's mutable streaming state belongs to one engine.

#### 6.9.4 Audio input and recording

`process_with_audio` receives borrowed interleaved input, latency and discontinuity state.
Tracks select mono/stereo hardware channels or a bus. Monitoring modes are Off, Auto while armed,
and On; monitored input enters before effects. Recording taps input independently of monitor,
track mute and fader. Hardware recording corrects input/output latency; bus recording corrects
graph latency. Stop drains each source's remaining latency tail.

Non-RT recording preparation creates exclusive WAV files, bounded sample/timestamp queues and
a writer thread. The callback enqueues captured segments; the writer emits PCM24 or Float32,
finalizes loop passes and publishes bounded waveform/progress snapshots. Overflows, input gaps,
source/rate/tempo/loop changes and write errors fail explicitly with partial files retained for
recovery. Checkpoint and relocation operations support saving during a recording.

#### 6.9.5 Import and file browsing

Import reads metadata through the shared pool and applies one undoable edit. Sources stay linked;
import does not copy or resample them. Drops on empty space create audio tracks; drops on audio
tracks insert sequential clips. MIDI/bus destinations refuse audio clips. Relink validates the
replacement header, source window and clip properties. Collection includes inactive takes.

#### 6.9.6 App lifecycle and live views

The app coordinates recording preparation, commit/cancel, checkpoints and asset relocation.
Linear capture produces clips; loop passes become takes with shared geometry. Arrangement and
waveform views display measured progress from the writer. Unsaved-project recordings move into
the project's `Takes/` folder on save; cross-filesystem handling preserves referenced assets.
Recording edits and final takes participate in document history.

### 6.10 Audio files (`komponist-audiofile`)

WAV uses `hound`; AIFF, FLAC, MP3 and OGG/Vorbis use `symphonia`. Seekable chunked decoding and
`rubato` sample-rate conversion run outside the audio callback. The shared `FilePool` keys entries
by file identity, holds them weakly and shares metadata, decoded buffers, derived Sampler data,
waveform peaks and analysis results among consumers.

A background waveform worker builds multiresolution min/max and measured RMS data, with
per-channel queries proportional to view width. Versioned on-disk peak caches are validated
against source identity; damage is a cache miss. A separate tempo worker analyzes up to 60 seconds
of the source, ranking onset periodicity from 40–240 BPM with confidence. Results are cached by
file identity and algorithm version. The user applies the estimate or a half/double variant as
an undoable original-tempo edit.

### 6.11 Stretch and Warp (`komponist-stretch`)

A checked Rust/C++17 wrapper uses vendored Signalsmith Stretch/Linear with the Default algorithm.
It supports mono/stereo, 8–384 kHz and output/input duration ratios from 1/6 to 6. Construction,
processing, seek/reset and destruction all stay on the audio read-ahead or offline render thread.
Source rate conversion precedes stretch; clip gain/fades follow it.

The renderer primes look-ahead and removes algorithmic delay, generates 512-frame chunks and
retains overhang across ring writes and continuous source splits. Locates, changed plans and loop
wraps re-prime history. A fresh seek can differ in spectral phase from continuous playback.
Heads/rings contain ready audio only; the callback never owns a native stretch object. Failed
segments publish unavailable frames, retry independently and leave other tracks running. Export
uses the same renderer with synchronous filling, exact output length and propagated source/stretch
errors. The UI exposes Fixed/Re-Pitch/Warp and shared source-tempo controls in editor/inspector.

### 6.12 Automation (#56)

Arrangement points use song ticks. MIDI and step envelopes use content-relative ticks and repeat
with the clip. Values are normalized to 0–1 and support Linear, Curve and Hold segments. Two
points at one tick encode a jump. Device targets pair the project's stable device ID with the
device's parameter ID. Tracks also expose volume, pan and send levels. The master exposes volume,
pan and insert parameters.

An unmuted clip envelope overrides its arrangement lane while the clip plays. The later-starting
clip wins on overlap, with the clip ID breaking ties. Splitting a clip preserves the original
envelope's precedence across both halves. When no clip overrides the target, playback uses the
arrangement curve or the base value. Missing plugin targets retain their editable curves.
Removing a device removes its arrangement lanes in the same undo step.

Compilation resolves targets and parameter ranges outside the callback. The resulting curve uses
materialized timeline points when the expansion fits the point budget and a compact repetition
index for larger curves. The engine evaluates both representations on the same absolute song
sample grid, at intervals of `round(sample_rate / 750)` frames, and honors explicit point positions.
Play, locate, loop wrap and curve changes chase the current value. Device events carry frame
offsets. Mixer ramps follow the latency at their tap, and the master fader runs after its inserts.
Offline rendering uses the same playback path. MIDI-to-audio flattening prints the device chain
and moves clip overrides for volume, pan and sends into the retained arrangement lanes. The edit
checks its expansion budget before moving those curves.

Modes belong to individual lanes. Off restores the base value. Read follows the effective curve
and permits a temporary manual override. Touch writes while a control is held and returns to the
curve over 250 ms after release. Latch continues writing after release until a transport boundary.
Stop, locate and loop wrap end write segments. Changing a lane's mode ends its current segment
at the playhead. A write into a clip envelope changes the content shared by its repetitions.

The engine timestamps gesture edges and values and publishes applied overrides for control
displays. Capture uses the bounded engine event queue. The app previews raw writes and thins
completed segments. Previewed segments can update playback before the pass finishes. The pass
becomes one undo step, shared with recorded notes when both belong to the same take. Esc discards
the pass and restores the playback curve or base value.

## 7. Audio and MIDI I/O (`komponist-audio`)

| Platform | Audio backends | MIDI |
|---|---|---|
| Linux | PipeWire, JACK, ALSA | midir ALSA sequencer |
| macOS | CoreAudio | midir CoreMIDI |
| Windows | WASAPI shared mode; feature-gated ASIO | midir WinMM |
| All | Null backend | Synthetic test input |

`AudioSystem` enumerates hosts, devices, rates, buffers and inputs. `EngineSlot<P>` owns the
callback processor and returns it on stream teardown; buffer/device changes rebuild the stream
while preserving the engine. A lost output device enters `NoDevice` and disables playback/recording.
An engine whose callback unwound in a panic is poisoned: it outputs silence, is never handed to
another stream, and the app replaces it with a new engine compiled from the document (#210). The
new engine stays stopped at the old playhead; a running take ends there like an interruption and
keeps what was recorded.

Input capture starts only when configured. Separate-device callbacks exchange timestamped frames
through a bounded bridge; rate conversion and a bounded clock follower align input to output.
Latency includes capture/queue delay. Gaps and device loss mark discontinuity for recording,
while output can continue. Reconnect follows exact saved source IDs where available. ASIO opens
input/output on the selected driver together.

With capture or explicit routes, Linux uses a native JACK client for same-period capture/playback.
Full port names define saved routes; aliases are presentation. Automatic routing respects manual
patchbay links and advertised defaults. Graph reconciliation runs outside the callback. Route loss
invalidates affected capture slots without stopping working output or unrelated inputs.

WASAPI's shared engine supplies the effective device period, so its buffer setting is fixed.
ASIO exposes driver-supported sizes behind the `asio` Cargo feature; the vendored host SDK is
built under its GPLv3 option and requires LLVM/libclang on Windows. The feature is off in default
Cargo builds and enabled by Windows packaging. Driver enumeration loads no driver. Hardware
ASIO operation is not covered by the automated fixture tests.

MIDI inputs are individually selectable and carry timestamps, activity and held-key state.
WinMM inputs can be busy in another program; that state remains visible. Callback timing maps
input to frame offsets; display latency combines the effective period and reported stream latency.

## 8. Plugins

### 8.1 Bridge processes

The distributed app is **one executable**. `komponist --bridge --instance` enters bridge mode
before UI/settings/audio initialization; scans use `komponist --bridge --scan <bundle>`.
The utility `komponist-bridge` binary remains for tests/tools. Every external instance has its
own process and RT thread; the plugin manager controls its lifecycle and saved state.

Shared memory (layout version 3) contains request/completion sequence words, transport/status,
latency, bounded event rings, stereo input/output and stereo sidechain input. Each request marks
whether sidechain audio is present; absent sidechains receive silence. The postcard control
protocol (version 14) uses length-prefixed frames for load/activation/state/parameters, GUI and
header actions, render mode, heartbeat and shutdown.

The plugin manager loads an instance, restores saved state if present, then activates it.
`SetState` carries a request ID. `StateApplied` returns that ID with the restored parameter
values, or an error and any parameter changes that occurred during the failed load. The manager
updates its base cache while preserving newer edits. State restoration pauses new audio requests
until the in-flight block finishes. If a request races with the pause and falls back, the bridge
replays its latest parameter values on the next block and resets voices to clear notes whose
note-offs may have been missed.

A checked snapshot (`GetStateChecked`) normally keeps audio running while the plugin serializes
its state and leaves the shared bridge state `Active`. The bridge captures the expected parameter
values at a block boundary (a short pause that waits for the in-flight block; a block that meets
it gets the fallback and resets voices only if it discards a note event), saves, and captures
again at the next boundary. A verified parameter that changed during the save, even if it came
back, or an editor edit that is still unapplied makes the bridge answer `StateDeferred` instead of
a verified state; so does a capture that cannot pause the current block in time. A value that
keeps moving would defer forever, so the exception is a paused fallback: after two such attempts
in a row, or after one when the caller's remaining time (`budget_ms`, the save's shared deadline)
is under twice the last save's duration, the next snapshot holds audio (bridge state `Loaded`)
for its whole save. That fallback is used once; it ends the starvation at the cost of one audible
gap.

The engine-owned external processor is the single consumer of the output event ring. The engine
locates plugin values and gestures on the song timeline and sends them to both automation capture
and the plugin manager. Temporary automation gestures update the plugin manager's live values
while preserving its base cache. Every snapshot request verifies pending edits and temporary
overrides against the expected base values. The bridge accepts a quantized persistent edit only
after the audio block delivered the requested value. Temporary values must return to their
bases before snapshotting. A project save can reuse its last
safe state for a device with no pending persistent edits. With pending edits, saving waits up to
the snapshot deadline for a validated state and leaves the file intact if the device cannot
provide one. Events from the current block carry its frame offsets. Late
edits are anchored to the next engine segment. VST3 controller updates are coalesced on the bridge
main thread. A parameter rescan prepares the delivery and controller entries on the control
thread. The audio thread swaps the table at a block boundary and returns the old table for
destruction. Entries for surviving IDs keep their delivery and controller history. The echo guard
filters controller replies outside an active gesture, and newer control changes supersede stale
updates from earlier blocks.

The engine publishes input/events and wakes the bridge, then collects completion within its
block deadline budget (60% of the block period minus elapsed time). Late effects pass through;
late instruments produce silence. Sequence numbers prevent stale output being reused. Eligible
instrument bridges start early while other tracks run. Offline requests use a longer timeout.

| Platform | Shared memory / block wake | Control and process watching |
|---|---|---|
| Linux | memfd / futex | Unix socket / pidfd |
| macOS | POSIX shared memory / shared address waits | Unix socket / kqueue |
| Windows | File mapping / auto-reset events and high-resolution deadline timer | Duplex named pipe / process handle |

Bridge RT scheduling uses SCHED_FIFO, macOS time constraints with an audio work interval, or
Windows MMCSS. Failure is reported as a realtime warning. The main loop answers heartbeats;
crashed or stalled instances can be reloaded from their last state.

Bridges and scan processes never outlive the app, even when the app dies while a bridge is
stuck in a plugin call and does not see its socket close. On Linux they get
`PR_SET_PDEATHSIG(SIGKILL)` between fork and exec, followed by a parent check. On macOS the bridge
watches the app's pid, which the app passes in the environment, with a kqueue exit filter and then
calls `_exit` (a bridge that cannot watch the app ends before any plugin code runs). On Windows
they are created suspended and join a kill-on-close job object of the app before they run. A scan runs in a process
group of its own (Windows: a nested job), so a timeout or cancel also kills the helper processes
its plugin started. Killed bridges are reaped together with one short wait; a bridge not reaped by
then is handed to a reaper thread instead of being left as a zombie. On macOS, control socket pairs
are created under a lock that every spawn also takes, since they cannot be made close-on-exec
atomically there. The app never waits for a
control message that has not arrived: each channel keeps the partial frame and the fragments of
a large message between reads, and a message that has begun to arrive is read on for at most a
short budget per poll, so a bridge frozen mid-message cannot hold up the UI thread or the
watchdog. Snapshots are collected on
save, GUI close and debounced dirty notifications. Bridge exit suppresses plugin teardown code
that could run after unloading. Scan stdout is isolated from plugin output to preserve JSON.

### 8.2 Hosting core (`komponist-hosting`)

`HostedPlugin` covers activation, audio/events, state, parameters, latency, GUI and render mode.
`HostLoop` provides main-thread callbacks, timers and Unix fd services. Linux uses a poll loop,
macOS integrates with CFRunLoop/AppKit, and Windows pumps messages with a wake relay, including
while a frame or modal loop is active.

CLAP supports audio/note ports, parameters, state, latency, GUI, timers, Unix fd support,
thread checking, logging, render mode and note names. VST3 implements component/controller
lifecycle, events/parameters, host callbacks, component/controller state, GUI resize/scale and
Linux `IRunLoop`. VST3 state stores both component and controller in a `KPV3` envelope.

VST3 render blocks stamp controller updates with the control generation so that `SetParam` and
state restoration can supersede older values. When a block provides both input and processor
output for the same parameter, the output supplies the controller value. During deactivation the
bridge applies any remaining controller values after joining the RT thread.

Main audio and sidechain ports are adapted to plugin channel counts: mono receives the stereo
average, surplus channels receive silence, and the main output is returned as stereo. CLAP uses
the first eligible non-main audio input as sidechain; VST3 uses its auxiliary input bus. Unused
inputs are silenced. Plugin libraries stay mapped for process lifetime to protect plugin-owned
threads and callbacks. GUI embedding uses X11, NSView or HWND according to platform.

### 8.3 Scanning

Background scans walk configured/platform plugin folders and run separate scan processes with
a 10-second timeout; a timed-out scan is killed with its process group (§8.1). Versioned caches use canonical bundle identity and modification times.
Descriptors include format/ID, vendor/name/version, kind, features/categories and sidechain
capability. Scanning initializes instances to inspect ports without activating audio.
Crashes/load failures are cached until the bundle changes; timeouts can be retried. Browser
categories derive from CLAP features and VST3 subcategories. Linux defaults are `~/.clap`,
`/usr/lib/clap`, `/usr/local/lib/clap`, `$CLAP_PATH` and the corresponding `vst3` directories.
macOS and Windows paths are listed in [INSTALL.md](INSTALL.md).

### 8.4 Plugin GUI windows

Linux frames are eframe viewports owned by the app, with a shared 28-point egui header and an
X11 child holding the plugin editor. A unique viewport title locates its native window; the
app passes the child XID to the bridge. macOS and Windows frames belong to the bridge, embedding
NSView/HWND editors below the same shared header. The app sends header state and receives
semantic actions, shortcuts, frame position and lifecycle events over IPC.

Frames negotiate plugin size and scale, respect fixed-size editors, and debounce interactive
resize requests. Position is saved on close. macOS activation and Windows foreground handover
are implemented in platform modules; Windows frames follow per-monitor DPI. Keep-on-top follows
komponist's activation state. Keyboard routing preserves app save/transport/undo shortcuts while
allowing plugin text input. Generic parameter views cover devices without a native editor.

### Native layouts for CLAP devices

After initialization the CLAP format layer queries the [v1 extension](clap-device-layout/README.md)
on the bridge main thread. The bridge validates the bounded document and bindings against the
CLAP parameter inventory. Control protocol 10 carries the descriptor with the loaded metadata and
with subsequent parameter inventories. The app validates each received descriptor before drawing
its existing device card with `ParamId::plugin` bindings. The floating parameter view uses the
same card when available. Plugins still offer their own editor through the existing GUI action.
A layout rejection leaves the generic parameter view available.

### 8.5 Built-in devices

Built-ins run in process and use the normal document/processor/device-card interfaces:

- **Hexa:** six-operator FM via `hexa-core`, sample-accurate events, serialized patches, macros and
  factory presets. The persistent plugin ID remains `pisynth` for project compatibility.
- **Volt:** subtractive synthesis with two oscillators, sub oscillator, filter and modulation.
- **Surf:** wavetable synthesis on Volt's engine: the voice, filter, envelopes, LFOs, matrix and
  voice modes are shared (`Engine<O: Oscillators>`), only the two oscillators differ. Sixteen
  original factory tables are generated at first use from recipes in
  `komponist_model::params::wavetable`, which the 3D graph reads too, into ten band-limited
  levels each. User tables are a later issue (#184).
- **Klopfer:** drum sampler with eight pads on MIDI notes 36–43. The existing built-in ID
  remains `sampler` for project compatibility.
- **Sampler:** general-purpose instrument with chromatic and slice modes, waveform regions,
  fades, tempo sync and modulation. Both instruments share the voice engine, background sample
  loading and linked-source storage.
- **Audio effects:** EQ, Delay, Reverb (`Hall` internally), Presse compressor with sidechain and
  Limiter. EQ spectrum and other device visualizations use leased taps, analyzed outside RT.
- **MIDI effects:** Arpeggiator and Scale (`Tonleiter` internally), run before the instrument.

Device specifications define parameters, layouts, defaults and localized catalog names.
Built-in processing uses `klang-dsp` primitives where shared; sample-derived buffers use the
process-wide audio file pool.

### 8.6 Runtime device state

The plugin manager keeps runtime state keyed by `DeviceId`: ready, loading, failed/crashed or
missing, with document bypass reflected in presentation. Loading also covers rate/render-mode
reactivation and a 250 ms warm-up, so ready means audio processing is available. Missing devices
retain identity/state for recovery; failed instances offer reload. These statuses are not project data.

### 8.7 macOS application bundle

`scripts/bundle-macos.sh` assembles `komponist.app` for Apple Silicon and macOS 14.4+ with the
executable, icons, localized bundle strings and project document type. Fonts/texts are embedded.
The app handles Finder/Dock open requests through the normal unsaved-changes/project-open flow.
The bundle uses hardened runtime with plugin library-validation and audio-input entitlements.
Bridge mode uses the same executable/signature, stays a UI element and suppresses App Nap;
the main app adopts regular Dock/menu-bar behavior. Current distribution uses ad-hoc signing.
The release workflow packages and verifies the bundle and checksum. See [INSTALL.md](INSTALL.md#macos).

### 8.8 Windows package

`scripts/package-windows.sh` builds the ASIO-enabled x86_64 executable and packages it with
license/readme and checksum. Embedded resources provide icon, version and a manifest with
per-monitor v2 DPI, long paths and normal user privileges. Release builds use the Windows GUI
subsystem and attach to a parent console for command output when needed, preserving explicit
pipes. Bridge mode uses the same executable. Current distribution is unsigned.
See [INSTALL.md](INSTALL.md#windows).

## 9. User interface

### 9.1 Shared design system (`komponist-ui`)

`komponist-tokens` generates tokens from `design/tokens.json` and `design/device-tokens.json`.
Colours use oklch-to-sRGB conversion; geometry and type styles come from tokens/shared metrics.
Design points map to egui points and axis-aligned geometry snaps to physical pixels. IBM Plex
Sans and Mono are embedded unmodified, with Mono for values. Icons use a Lucide SVG subset;
transport shapes are drawn geometrically. Shared widgets implement interaction/focus states,
keyboard access and localized accessible names. The gallery and snapshots exercise component states.

### 9.2 Canvases

Arrangement, piano roll, step grid and mixer use custom geometry, visible-range culling and
cached paint data. Each exposes AccessKit nodes for visible objects so interaction and
accessibility cost follows the viewport. Clip/note detail scales with zoom; arrangement meshes
are cached by shared data identity and view parameters. The performance target for the 10000-note
arrangement scenario is an 8 ms UI frame; measured results are in [PERFORMANCE.md](PERFORMANCE.md).

Audio waveforms share multiresolution per-channel min/max/RMS data and continuous ribbon meshes.
The arrangement caches source handles and meshes, rechecks changed/missing sources and requests
background peaks with repaint callbacks. The waveform editor uses one source/song mapping for
rulers, view bounds, playhead, peaks and edit hit tests. Trims use arrangement snap; gain/fade
changes use continuous values. Source recovery/relink and tempo analysis share file actions.

### 9.3 Application shell (`komponist-app`)

`App` coordinates `Document`, `UiState`, `EngineSync`, device hosting, settings and windows.
Views emit actions; central dispatch validates and applies edits, transport changes and UI/plugin
operations. `EngineSync` compiles document changes and sends fast commands for ongoing gestures.
The layout combines transport, browser, arrangement, inspector, resizable dock and status bar.
Keyboard chords map to actions by focus context and platform; macOS uses its platform modifiers.
Headless construction with `NullBackend` supports app acceptance tests.

`application_menu` defines shared semantic commands, localized labels, enabled states and chords.
AppKit projects them into native application/Window menus and the Dock; Linux and Windows use
the egui application menu when no native adapter is installed. Menu selections are revalidated
against modal/closing state and current device IDs before dispatch. Native menu mutation remains
on the owner thread, and AppKit defers replacement while a popup is tracking.

`komponist-i18n` generates typed text accessors from DE/EN Fluent catalogs. UI language follows
the desktop or explicit settings/`--lang`; AccessKit roots carry the selected language. The model
receives words as arguments rather than depending on the catalog. See [I18N.md](I18N.md).

## 10. Shared DSP (`klang-dsp`)

`klang-dsp` is a pinned git dependency. Local overrides and revision updates are described in
[INSTALL.md](INSTALL.md). Hexa and Sampler engines are vendored crates in this workspace.
Shared DSP primitives include unit conversion, smoothing/filtering, oscillators, RNG, buffer
operations, pan/balance and meter math. These blocks are independent of product UI and parameter
mapping. Fader taper/formatting belong to model/UI code; export dither/quantization and runtime
PDC delay storage belong to the engine. Realtime thread setup belongs to audio/RT helpers.

## 11. Verification

| Level | Existing coverage |
|---|---|
| Model | Pure edits, routing/ID invariants, undo/redo, time/formatting, persistence and migrations |
| Engine | Deterministic offline audio, sample positions, tails/loops, PDC, recording, serial/parallel equivalence, allocator guards |
| Audio sources | Decoding/resampling, peak/RMS caches, source analysis, streaming and Warp seek/split/error behavior |
| Hosting/IPC | CLAP/VST3 fixtures, state, sidechains, deadlines, crashes/reload, process lifecycle and platform frames |
| UI | `egui_kittest` interaction, AccessKit and DE/EN snapshots; headless app acceptance |
| Compatibility | `scripts/check-warp-compat.sh` exchanges files with a pinned pre-Warp reader |
| Live GUI | `scripts/check-gui.sh` on Linux/Xvfb; inspection and platform-specific frame tests |
| Performance | Dedicated render, scheduling, streaming and UI probes described in PERFORMANCE.md |

`scripts/check.sh` runs formatting, clippy with warnings denied and workspace tests. It requires
test plugin fixtures (`KOMPONIST_REQUIRE_FIXTURES=1`), built by `scripts/build-test-plugins.sh`.
CI covers Linux, macOS arm64 and Windows; documentation-only changes skip the build workflow.
Linux snapshots are the comparison reference. macOS/Windows render snapshots without replacing
or comparing those reference images. Optional installed-plugin tests are separate from required
fixtures. Audio/MIDI tests use synthetic or explicitly selected virtual input rather than hardware.

### 11.1 Live inspection

With `EGUI_INSPECTION=1`, eframe exposes AccessKit, input and screenshots to `egui_mcp` on the
configured loopback port. `egui_kittest` drives the same accessible UI headlessly. Foreign plugin
editors require platform window/input tooling because their contents are outside egui's tree.

## 12. Current implementation limits

- Linux GUI embedding uses X11/XWayland. macOS and Windows use bridge-owned frames.
- Audio sources and processing are mono/stereo at the engine boundary; extra plugin channels are
  adapted as described in §8.2.
- Track kinds are distinct. Buses have no sends, and audio clips on one track do not overlap.
- Engine queues, processor/event counts and delay lines have explicit fixed capacities (§6.1).
- Automation covers parameter lanes and clip envelopes (§6.12). MIDI learn, MIDI CC lanes and
  track macros are separate work.
- Warp is affine from original/project tempo. Fresh seeks re-prime DSP history; optional Warp data
  is not preserved by older v3 readers (§5.5, §6.11).
- Automated backend coverage does not establish compatibility with every hardware driver or
  third-party plugin. Release signing state is documented in §8.7–§8.8 and INSTALL.md.

## 13. Established interaction decisions

1. **Panel focus:** Tab switches arrangement/mixer in the canvas context and exits the maximized
   mixer. Dialogs and text entry retain normal focus traversal; Shift+Tab/arrows navigate widgets.
2. **Computer keyboard MIDI:** the keymap layer tracks held keys and their original destinations.
   Disabling it, deleting a destination or replacing the project clears held notes.
3. **Step rows:** instrument-provided note names define rows where available; the pad fallback
   uses notes 36–43. Klopfer rows follow its pad layout.
4. **Track colours:** vermilion (45°) is available for manual assignment and excluded from the
   automatic hue order.
5. **Export:** rate, format, range, normalization and tail come from the export settings; processor
   activation and PDC use the selected rate.
6. **Naming:** the product is lowercase **komponist**; **klang-studio** names the project umbrella.
   Built-in display names include Hexa, Volt, Surf, Klopfer, Sampler and Presse.
7. **Mixer:** the normal dock uses lean strips with name, toggles, fader and a small pan control;
   the expanded mixer shows devices, sends and routing. Buses precede the master in mixer view.
8. **Track toggles:** mute/solo retain M/S labels. Record arm uses a record-circle symbol and
   monitoring a speaker, separated from M/S by an extra gap. Arm uses `state.record`; monitoring
   uses green `state.play`. Armed MIDI tracks monitor automatically, and the monitoring control
   exposes that implied state.
9. **Focus and windows:** focused panels show an accent header line. Plugin-frame position is
   persisted on close. Missing-plugin presentation includes the stored module filename.
10. **Value controls:** double-click/Alt-click resets knobs and faders; Enter/context menus open
    text entry. Number fields and dB readouts retain double-click text entry. Wheel bursts form
    one undo gesture; Shift makes changes finer and Ctrl coarser. Controls consume their wheel
    input while hovered.
11. **Live MIDI release:** disabling monitoring or changing input/channel filters releases held
    input notes on the next processing segment without ending sequencer notes.
