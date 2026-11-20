# komponist – Performance and RT safety (WP-61, WP-65, WP-102, #11)

Measured 2026-11-14 on the branch `wp/WP-61`; parallel track processing on `wp/WP-65`
([§6](#6-parallel-track-processing-wp-65)). Every number below comes from a test or bench in the
repository; [§5](#5-how-to-re-run) lists the commands.

## 1. Machine and conditions

| | |
|---|---|
| CPU | AMD Ryzen 5 5600, 6 cores / 12 threads, governor `powersave` (3.6–4.0 GHz seen) |
| RAM | 15 GiB |
| OS | Arch Linux, kernel 7.2.14-arch1-1, PipeWire session; user in the `realtime` group (RLIMIT_RTPRIO 98) |
| Toolchain | rustc 1.99.0, release profile of the workspace |
| Audio | null backend (timer thread, 48 kHz, 256 frames = 5.33 ms) — never the speakers |

**Caveat:** other agents built the workspace in parallel during most runs (load average between 5
and 51 on 12 threads). Means and medians are robust to that; worst cases, peaks and overruns are
upper bounds. Re-run on an idle machine before reading the tails as properties of komponist.

## 2. Targets and results

| Area | Target | Measured | Verdict |
|---|---|---|---|
| UI frame, 16 tracks / 200 clips / 10 000 notes, arrangement only | ≤ 8 ms (ARCHITECTURE §9.2) | median 1.3 ms (1×) / 1.3 ms (2×), worst 2.0 / 1.7 ms; all selected: median 1.8 / 1.9 ms, worst 2.5 / 2.5 ms | met |
| UI frame, same project **playing** (16 Hexa tracks on the audio thread), dock open with the piano roll | ≤ 8 ms | median 1.3 / 1.3 ms, worst 1.5 / 1.4 ms | met |
| First frame after load (clip previews built) | — | 10–17 ms | one-off, acceptable |
| Piano roll, one clip with 10 000 notes, whole app | ≤ 8 ms | median 2.5 ms | met |
| Timeline paint, 10 000 notes (WP-22 bench) | < 4 ms | piano roll 3.4 ms (1×) / 4.0 ms (2×, median 3.97); arrangement 0.6 ms | met, **2× at the limit** |
| Engine's own cost, 16 tracks (sequencer, chains, mixer, meters, PDC) | — | 8 µs mean per 256 frames (0.15 %) | negligible |
| Engine + 16 Hexa tracks, stress material, one thread | no overrun | mean 3.3 ms = 62 % of the period, worst 6–11 ms | **missed under load** (Hexa DSP, see §4.1) |
| Same in the real callback path (app, null backend) | no overrun | mean 67–69 %, peak 110–120 %, overruns 49–63 of 1 880 callbacks (2.6–3.4 %), xruns 0 | **missed under load** |
| + 8 kp-test-fx bridges (one per track on 8 tracks) | no late block | mean +2 points (70.7 %), late blocks 12 (SCHED_FIFO) / 95 (SCHED_OTHER) of 15 000 | **missed under load** (follows from the above) |
| **WP-65:** 16 Hexa, callback path, 4 worker threads (§6) | < 50 % of the period, no overrun in 5 min | mean 17.5–17.7 %, peak 28.5 %, 0 overruns in 56 265 callbacks; + 8 bridges 21.1 %, 0 late blocks | **met** |
| IPC round trip, back to back, SCHED_FIFO 80 | p99 ≤ 7.6 µs (WP-15 spike) | p50 5.6 µs, **p99 7.0 µs**, max 23 µs | met |
| IPC round trip, back to back, SCHED_OTHER | — | p50 7.6 µs, p99 10.9 µs, max 1.5 ms | as the spike |
| IPC round trip, paced like a 256-frame callback, SCHED_FIFO 80 | p99 ≤ 26 µs (spike, idle machine) | p50 13 µs, p99 202 µs, max 448 µs (load average 51) | tail missed under load, see §4.4 |
| Soak, random edits during playback, debug (allocation check) | no allocation, no stall | 6 s and 5 min: no allocation, no stall, no command left | met |
| Soak, release, 4–5 min runs (timing) | no overrun | 0 overruns in 56 000 callbacks (5 min, SCHED_FIFO); 1 in 45 000 (4 min) | see §3.3 |
| Soak, release, 30 min, SCHED_FIFO 70 | no overrun | 10 overruns in 337 532 callbacks (0.003 %), 1 xrun, no stall, 207 exports | **missed, system memory pressure** (§3.3) |
| S07 opens / driver change | no frame stall | before: 7–10 ms (driver probe) and 170–200 ms (ALSA devices) on the UI thread; now 0 (worker thread) | fixed |
| klopfer ready after `Activated` | export never misses the first hits | first hit 0.2–1.1 ms (realtime), 9.0–9.5 ms (offline, 48 kHz with a 44.1 kHz kit); `WARM_UP` = 250 ms | fixed (§4.3) |

## 3. RT safety

### 3.1 `assert_no_alloc` sweep

`assert_no_alloc` aborts on any allocation **or free** inside its scope in debug builds of binaries
that install `AllocDisabler` (every test binary that touches the engine does). Paths and where they
are checked:

| Path | Checked by |
|---|---|
| `Engine::process` / `render`: commands, transport, count-in, loop wrap, sequencer, **note chasing**, probability, live MIDI, `LiveNote`, recording capture, fast paths, program swap, processor insert/remove (garbage), panic, PDC, mixer, meters, metronome | `komponist-engine` `tests/engine` (every call via `Rig`), incl. 1 000 random command sequences (`random.rs`) and `chase.rs` (10 000-note scan) |
| Held MIDI-input keys (telemetry) | `live.rs` `held_input_keys_…` |
| Hexa `PisynthProcessor::process` / `reset` / parameter events | `komponist-builtin` `tests/hexa` |
| `ExternalProcessor::process` (bridge client: wire events, stash, futex wait, fallbacks) | `komponist-plugins` `tests/it/engine.rs` (`call` and `Rig`) |
| `ExternalProcessor::process_sidechain` (#6: the sidechain through the shared memory) | `komponist-plugins` `tests/it/sidechain.rs` (direct calls and `Rig`) |
| IPC `begin_block` / `wait_done` / output events | `komponist-ipc` `tests/bridge_process.rs` (10 000 checked blocks), sidechain blocks in `shm.rs` unit tests |
| CLAP / VST3 `process` with a sidechain port | `komponist-hosting` fixture tests (`Blocks::run`, `sidechain_reaches_the_port`) |
| Bridge RT loop | `komponist-bridge` `rt.rs` runs its loop inside `assert_no_alloc` in debug builds |
| Audio callback: backend → `EngineSlot` (attach/detach mailbox) → `EngineProcess` (MIDI → frames) → engine | `komponist-audio` `StreamCallback::run` wraps every callback; every app test on the null backend runs it (acceptance flows, soak) |
| Audio clips: `Voice::prepare` / `render` (ring read, heads, de-click, tails, epoch starts), compile of plans, program swaps with audio tracks, loop wraps, locates, stops | `komponist-engine` `tests/engine/audio.rs` (every call via `Rig`; `playing_clips_never_allocate_on_the_audio_thread`), the streamer thread is outside the rule and allocates freely |
| **Export render** | new: `komponist_engine::export` wraps every `Engine::render` call (WP-61); checked by the engine export tests, `export_bridges`, `klopfer`, acceptance flow 5 and the soak |

Findings: no allocation, free or lock was found on these paths. The only RT-path change was the
export render, which ran unchecked before. Locks seen near the RT paths are all off it:
`ExternalProcessor::activate` (non-RT by contract), `StreamShared::mark_lost` (`try_lock`, error
callback), `MidiSender` (MIDI threads only; the audio side pops an rtrb queue).

### 3.2 xruns caused by the app

`StreamStats` now separates the two kinds:

- `overruns()`: callbacks whose own work took longer than their period — an xrun the app caused,
  whatever the scheduler did.
- `xruns()`: as before, backend reports plus late wake-ups of the audio thread (scheduling).
- `take_peak_load()`: the highest single-callback load since the last read.

### 3.3 Soak

`crates/komponist-app/tests/soak.rs`: the demo plays on the null backend in real time while a seeded
random generator, every 16.7 ms UI frame (the whole UI drawn and tessellated), does 0–2 of: locate,
loop on/off and range, metronome, record / punch-out / discard take, play/stop, MIDI input notes
(through `TestMidiSource`, the real input path), live notes, add/remove/move/resize notes,
add/remove/move/copy/loop clips, fader/pan/mute/solo, tempo, new Hexa track, remove track,
Hexa preset, bypass, undo, redo, open clip, mixer, and at least one WAV export of the loop
(detach, offline render, re-attach). Checks: no allocation in the callback (debug), the audio thread
kept running (≥ 90 % of the expected callbacks), no command left in the queue, no overrun (release).

Runs (all release unless noted; the test binary under `chrt -f 70`, so the null audio thread has RT
priority like a PipeWire callback):

| Run | Callbacks | Mean / peak load | Overruns | xruns | UI frame worst | Exports |
|---|---|---|---|---|---|---|
| 6 s, `cargo test` (debug) | 1 100 of ~1 128 | — / 260 % | 118 (unoptimised Hexa; not asserted) | 6 | 156 ms | 1 |
| 6 s, release, SCHED_OTHER | 1 127 | — / 35 % | 0 | 0 | 6.9 ms | 2 |
| 5 min, debug (SCHED_OTHER) | 52 527 of ~56 253 | — / 443 % | 9 907 (unoptimised Hexa; not asserted) | 478 | 137 ms | 38 |
| 4 min, release | 45 000 | 3–5 % / 96 % | 1 (at 30.7 s) | 0 | 249 ms | 32 |
| 5 min, release | 56 252 | 2–12 % / 65 % | 0 | 0 | 8.7 ms | 43 |
| 30 min, release | 337 532 of ~337 521 | 3–9 % / 430 % | 10 (at 110, 371, 371, 443, 1336, 1508, 1509, 1647, 1684, 1754 s) | 1 | **2.0 s** | 207 |

What the overruns are: rare single callbacks (≈ 1 in 40 000) at a mean load of 2–12 %, not tied to
an action (the test prints the actions before each overrun; the one at 30.7 s followed ordinary
edits, transport moves and a live note, no export) and not reproducible with the same kind of run
5 minutes later. During these runs the machine swapped hard: other agents' builds had pushed
7.6 of 8 GiB into swap (8.18 of 8.19 GiB at the end of the 30-minute run, whose worst UI frame took
2 s) and the I/O pressure stall (`/proc/pressure/io`, avg300) was 3.3 %. The same
run once saw a 249 ms UI frame. A page fault of the audio thread into a page the kernel reclaimed
(code or data) blocks on I/O for milliseconds, which fits both. The first calls of a freshly
created Hexa are not slower than later ones (`first_calls_of_a_new_processor`, ±10 µs), so new
processors after undo/redo are not the cause.

Recommendation (owner decision, not done in WP-61): lock the app's memory at start
(`mlockall(MCL_CURRENT | MCL_FUTURE)`, as JACK clients and Ardour do; this user's `RLIMIT_MEMLOCK`
is unlimited) or at least pre-fault and lock the engine's memory and the audio thread's stack. A
GPU app locks its driver mappings too with `MCL_FUTURE`, so this needs a measurement of the
resident size first.

## 4. Decisions

### 4.1 Engine CPU: Hexa dominates

The engine's own work for 16 tracks is 8 µs per 256-frame block (0.15 %); Hexa's DSP is the
rest. The stress material (per track a four-note chord per half bar, held, plus a 1/16 line, with
the presets E-Piano, Strings, Supersaw and Init; 8 or more voices per track, release tails
overlapping) costs 62 % of one core at 48 kHz in a single thread, matching the per-voice numbers of
the WP-25 bench (8 voices: Init 1.0 %, E-Piano 2.6 %, Strings 5.6 %, Supersaw 5.0 %). The demo
(gate G3, 10 Hexa instances) used 11–12 %.

At that load the single audio thread has no headroom on a busy machine: 2.6–3.4 % of the callbacks
overran, and bridges placed after many tracks miss their 60 % deadline (they fall back to
passthrough, as designed). Not fixed in WP-61, because both remedies are bigger than this package:

- **Parallel track processing** (ARCHITECTURE §8.1 "later optimisation"): tracks are independent
  until the master bus; a worker pool (or bridge fan-out) would spread 16 tracks over the cores.
  Recommended as a work package after v0.1.
- **Hexa DSP cost** (`crates/hexa-core`, WP-34's klang-dsp migration is the place).
- Meanwhile a 512-frame buffer halves the per-callback scheduling risk.

### 4.2 Note chasing: yes, like Ableton

Implemented in the engine (`sequencer.rs`). After a playback start (`Play`, `Record` from stop), a
`Locate` while playing and a loop wrap, the first call starts every note that began before the
position and still sounds there, at offset 0 with its remaining length; probability rolls for it
like for any note. Reasons: a pad or bass note under the playhead is part of what the user hears at
that spot (S01/S02 workflows start playback anywhere), loop playback of a section that a long note
enters from before would otherwise lose it on every lap, and a loop or selection **export** must
sound like playback. Program swaps do not chase (an edit under the playhead should not fire a
note in the middle). Drum hits that straddle the position are chased too (Ableton does the same);
they are short, so this is rarely audible. Cost: one tick comparison per earlier note of the track,
only in the first call after the event (10 000 notes scan inside `assert_no_alloc` in the tests).

### 4.3 Bridge re-activation, render mode and warm-up → `DeviceStatus::Loading`

- A bridge asked to re-activate (`Processor::activate` with another rate, from `set_sample_rate`)
  reads `Loading` **at once** (the shared `reactivating` flag) and gets a `StatusChanged(Loading)`
  in the next poll; `Ok` follows when the bridge confirms (sync `Pong`).
- The render-mode switch (`SetRenderMode`) deactivates and re-activates the plugin in the bridge,
  so it is treated the same way. Before WP-61 the export's second run could render silence when it
  started right after the first one (found by the updated `export_bridges` test).
- `komponist_plugins::WARM_UP` (250 ms): after every activation the device stays `Loading` while
  audio already flows. It answers klopfer's cold start (WP-54 saw
  ~70 ms of silence; WP-61 measured 0.2–1.1 ms realtime and 9 ms offline, so 250 ms leaves margin
  for a loaded machine and a cold disk). `PluginManager::set_warm_up` changes it (tests).
- The export (app) no longer blocks the UI on a `snapshot_states` barrier (up to 2 s): it holds the
  worker back frame by frame until no device of the project is `Loading`, at most
  `READY_TIMEOUT` (5 s). The `export` docs (engine and app) say so.
- After an export at another rate, playback started at once is still briefly silent while the
  bridges re-activate back, but the devices now read „Lädt“ meanwhile.

### 4.4 Optional futex spin (WP-16): not implemented

Measured with the IPC report's new `KOMPONIST_IPC_SPIN_US` (the engine side spins on `is_idle`
before the futex wait):

| SCHED_FIFO 80 | p50 | p90 | p99 | p99.9 | max |
|---|---|---|---|---|---|
| back to back, no spin | 5.6 µs | 6.7 | 7.0 | 9.9 | 23 µs |
| back to back, spin 50 µs | 3.0 µs | 3.1 | 4.1 | 7.0 | 29 µs |
| paced 5.33 ms, no spin | 13.1 µs | 24.5 | 202 | 313 | 448 µs |
| paced 5.33 ms, spin 50 µs | 7.5 µs | 18.5 | 36.5 | 211 | 462 µs |

The spin helps only when the plugin answers within the spin window, which the trivial test bridge
does; a real plugin's `process` takes longer than 50 µs, so the spin would burn up to 50 µs per
bridge per callback (8 bridges: 400 µs = 7.5 % of the period) and then sleep anyway. The paced tail
(p99 202 µs) was measured at load average 51 and is a wake-up latency of the bridge side, which the
engine-side spin does not remove. Decision: no spin in `BridgeClient`; the knob stays in the
measurement test. Revisit together with parallel track processing (§4.1).

### 4.5 Audio device enumeration off the UI thread

`AudioDevices::is_slow` (true for `SystemDevices`): S07 runs `available_hosts` and `devices` on a
worker thread („komponist-audio-devices“), shows the lists it has meanwhile and repaints every
16 ms until the answer arrives. Test fixtures (`FixedDevices`) stay synchronous. Measured before:
`available_hosts` 7–10 ms (the JACK probe opens a client), PipeWire devices 1.5 ms, JACK 4 ms,
**ALSA 170–200 ms**.

### 4.6 Held MIDI-input notes

Contract addition (small, RT-safe): `Telemetry::held_input_keys() -> u128`, written by the engine
from the MIDI events of each call (note-on sets, note-off / velocity 0 clears, CC 120/123 clear all;
all ports and channels merge, whether or not a track listens). The shell merges it into
`ViewCtx.held_notes`. `MidiInputHub::set_waker` lets a MIDI message repaint the idle UI.

### 4.7 Follow-ups (not in WP-61)

- Parallel track processing or bridge fan-out (§4.1): the only way to more than ~60 % of one core
  of instruments at 256 frames. **Done in WP-65 (§6).**
- Memory locking (`mlockall` or pre-faulted, locked engine memory) against page-fault stalls under
  memory pressure (§3.3).
- Timeline paint at 2× is at its 4 ms budget (piano roll, 10 000 notes visible); the whole-app frame
  is far below 8 ms, so no change now.
- `export` could apply its restore commands (Stop, program, Locate) itself instead of the caller's
  `render(1)` (optional in the WP-53b note; unchanged).
- Re-run every timing here on an idle machine without swap pressure.

## 5. How to re-run

```bash
# UI frame (16 tracks, 200 clips, 10 000 notes; idle, all selected, playing)
cargo test --release -p komponist-app --test arrangement_perf -- --ignored --nocapture
cargo test --release -p komponist-app --test it piano_roll::ten_thousand -- --nocapture
cargo bench -p komponist-ui --bench timeline

# Engine CPU: engine alone and + 16 Hexa, and the real callback path (+ 8 bridges; needs
# scripts/build-test-plugins.sh), each with 0 and 4 worker threads (WP-65; KOMPONIST_BENCH_WORKERS /
# KOMPONIST_PERF_WORKERS take another list, KOMPONIST_PERF_SECS another length). For RT priority
# run the test binary under chrt -f 70.
cargo test --release -p komponist-engine --test engine bench -- --ignored --nocapture
# Audio clips (#11): N streamed stereo clips, audio thread and read-ahead thread (KOMPONIST_BENCH_AUDIO_TRACKS=16,64)
cargo test --release -p komponist-engine --test engine audio::bench -- --ignored --nocapture
# Routing (WP-102): 16 tracks flat vs. + 4 buses, sends and a sidechain, 0 and 4 workers
cargo test --release -p komponist-engine --test engine routing::bench -- --ignored --nocapture
cargo test --release -p komponist-app --test engine_perf -- --ignored --nocapture --test-threads 1
KOMPONIST_PERF_WORKERS=4 KOMPONIST_PERF_SECS=300 <engine_perf binary> engine_cpu_16_hexa_tracks \
    --exact --ignored --nocapture                                  # the 5-minute stress run
cargo test --release -p komponist-builtin --test hexa cpu_per_block -- --ignored --nocapture

# IPC round trip (back to back / paced / spin), RT via chrt on the test binary:
cargo test --release -p komponist-ipc --test bridge_process --no-run   # prints the binary
chrt -f 80 env KOMPONIST_IPC_BLOCKS=100000 <binary> latency_report --exact --ignored --nocapture
chrt -f 80 env KOMPONIST_IPC_BLOCKS=10000 KOMPONIST_IPC_PACED_US=5333 KOMPONIST_IPC_SPIN_US=50 \
    <binary> latency_report --exact --ignored --nocapture

# Soak: a few seconds in cargo test (debug: allocation check); 30 minutes opt-in (release: timing)
cargo test -p komponist-app --test soak -- --nocapture
KOMPONIST_SOAK_SECS=1800 cargo test --release -p komponist-app --test soak -- --nocapture
KOMPONIST_SOAK_SEED=<seed> …   # replays a run
KOMPONIST_SOAK_WORKERS=0 …     # single-threaded engine (default: the app's automatic count)
KOMPONIST_SOAK_MIN_CALLBACKS=50 … # callback minimum in % (default 75 debug, 90 release; macOS CI VM)

# klopfer cold start (needs scripts/build-klopfer.sh and klopfer's test kit)
cargo test --release -p komponist-plugins --test it installed::klopfer_cold_start -- --ignored --nocapture

# Audio device enumeration (talks to the audio servers, opens no stream)
cargo test --release -p komponist-audio device_enumeration_time -- --ignored --nocapture
```

## 6. Parallel track processing (WP-65)

Measured 2026-11-14 on the branch `wp/WP-65`, same machine as §1, **idle**: no builds running,
load average 1.5–3.3 (the tail of this branch's own release build at the first runs), 2.0–2.8 during
the 5-minute runs. Swap held 5.0 of 8 GiB (7.8 GiB at the baseline run) from earlier sessions but
was not active: `/proc/pressure/io` avg10 0.00 during the runs. All callback-path runs on the null
backend at 48 kHz / 256 frames (5.33 ms) under `chrt -f 70`; workers at `SCHED_FIFO` 80. Design:
ARCHITECTURE §6.4.1.

| Run | Before (main, single-threaded) | After, 0 workers | After, 4 workers |
|---|---|---|---|
| Engine alone, 16 idle instruments, one thread (`bench`) | 8.4 µs (0.16 %), worst 143 µs | 8.8 µs (0.16 %), worst 142 µs | 10.8 µs (0.20 %), worst 214 µs |
| Engine + 16 Hexa, stress material, one thread (`bench`) | 3.1 ms (58.6 %), worst 5.1 ms (96 %) | 3.1 ms (58.7 %), worst 5.2 ms (98 %) | **0.89 ms (16.7 %), worst 2.2 ms (42 %)** |
| 16 Hexa, real callback path, 10 s (`engine_perf`) | mean 63.5 %, peak 94.6 %, 0 overruns | mean 63.4 %, peak 79.7 %, 0 overruns | **mean 17.5 %, peak 27.7 %, 0 overruns** |
| + 8 kp-test-fx bridges (after Hexa on 8 tracks), 10 s | — | mean 65.0 %, peak 88.6 %, 0 late blocks | **mean 21.1 %, peak 27.9 %, 0 late blocks** |
| 16 Hexa, callback path, **5 minutes** | — | — | **mean 17.7 %, peak 28.5 %, 0 overruns, 0 xruns in 56 265 callbacks**, 0 late waits |
| Soak, release, **5 minutes** (demo, random edits, 42 exports) | — | — | **0 overruns, 0 xruns in 56 250 callbacks**, peak 63 %, worst UI frame 7.3 ms; 59 635 batches, 390 397 jobs on workers, 0 late waits |
| Soak, release, 30 minutes | — | — | **open** (see below); stopped after 6 of 30 minutes by owner decision with 0 overruns, 0 xruns in 67 503 callbacks so far |

**Done-when met:** 16 Hexa stress tracks at 17.5–17.7 % of the period (target < 50 %) with 4
workers, no overrun in the 5-minute stress run and the 5-minute soak. The 3.5× speed-up on 5
threads (callback + 4 workers) is close to the ideal for 16 jobs of uneven cost; the remaining time
is the longest job chain on one thread plus the wake-ups (~10–30 µs per batch). The dispatch costs
2 µs per block with 16 trivial tracks (idle-instrument bench). The worst single callback of the
WP-61 baseline (95–98 %) shrinks to 28 % with workers: one slow track no longer adds to 15 others.

Notes:
- "Before" and "after, 0 workers" agree, so the three-phase segment (events, jobs, ordered sum)
  costs nothing single-threaded. The bit-exact tests prove they compute the same output.
- The bridge case puts the kp-test-fx *after* Hexa, so those bridges cannot start early; their
  waits now overlap on the workers. Instrument bridges at the head of a chain start before any
  chain runs (`Processor::begin`), checked by `komponist-plugins`
  `bridged_instruments_with_workers_match_single_threaded` (bit-exact offline, all realtime blocks
  in time).
- The soak's peak load (63 % in one callback) is the attach after an export (the soak runs 42 of
  them); its mean stays at 1–5 %.
- `WorkerStats::late` (waits for a running job beyond one period) stayed 0 in every run.

**Open: the 30-minute soak with workers.** The done-when of WP-65 is the 5-minute soak; the
30-minute release soak (with the default 4 workers) is still to be run once, overnight or on an
otherwise idle machine (no builds), and its result added to the table above. v0.2 was released
without it (owner decision, 2026-11-15); run it before v0.3:

```bash
cargo test --release -p komponist-app --test soak --no-run   # prints the binary
cd crates/komponist-app && KOMPONIST_SOAK_SECS=1800 chrt -f 70 <soak binary> --nocapture
```

## 7. Effect DSP (WP-90)

Measured 2026-11-15 on `wp/WP-90` (load average 3–4 on 12 threads), release profile, 48 kHz, calls of
256 frames (5.33 ms period) on 10 s of white noise, one thread; "16×" runs 16 instances one after
the other in the same call, as one worker of the engine would.

| Effect (settings) | 1 instance, mean | worst | 16 instances, mean | worst |
|---|---|---|---|---|
| EQ, 6 bands active (every type, +6 dB) | 6.5 µs (0.12 %) | 79 µs | 103 µs (1.9 %) | 140 µs |
| Delay, ping-pong, feedback 0.6, mix 50 % | 6.2 µs (0.12 %) | 56 µs | 103 µs (1.9 %) | 302 µs |
| Hall, defaults (Halle, modulation 25 %) | 16.6 µs (0.31 %) | 26 µs | 270 µs (5.1 %) | 401 µs |
| Hall, Kirche, modulation 100 % | 16.7 µs (0.31 %) | 32 µs | 272 µs (5.1 %) | 402 µs |
| Limiter, lookahead 2 ms, +12 dB input | 5.7 µs (0.11 %) | 12 µs | 62 µs (1.2 %) | 105 µs |

All effects together on one track cost about 0.7 % of the period; the Hall is the largest; a Hall per
track (16 tracks: 5 % of the period) fits the budget without sends. The worst cases are scheduler noise on a loaded machine (no effect allocates, locks or loops
data-dependently); every effect runs inside `assert_no_alloc` in the unit tests, and the engine test
`internal_effects_are_bit_exact_with_workers` (8 tracks × 4 effects, 0/2/4 workers, realtime and
offline) is bit-exact.

**Presse, the compressor (WP-93)**, measured 2026-11-15 on `wp/WP-93` the same way (release, 48 kHz,
256 frames, white noise, threshold −30 dB so it compresses all the time). The proposal estimated
14 µs (0.26 %); it costs less because the level conversion uses a 4-term series for `log2` and a
5-term polynomial for `exp2` instead of `ln` and `powf` per sample.

| Presse (settings) | 1 instance, mean | worst | 16 instances, mean | worst |
|---|---|---|---|---|
| RMS, link 100 % (one gain computer), no lookahead, auto-release | 9.9 µs (0.19 %) | 19 µs | 155 µs (2.9 %) | 203 µs |
| Peak, link 0 % (two gain computers), lookahead, SC-HP 120 Hz, auto-release | 11.5 µs (0.22 %) | 521 µs (noise) | 176 µs (3.3 %) | 462 µs |
| RMS, link 100 %, mix 50 % | 9.9 µs (0.19 %) | 15 µs | 162 µs (3.0 %) | 571 µs |

The tap that feeds the level point and the reduction history copies one pair of floats per 32 frames,
and only while the card is on screen; it is not in these numbers (a few atomic stores per block). The
engine test `internal_effects_are_bit_exact_with_workers` now runs 8 tracks × 5 effects with a
lookahead `Presse` on every track.

```bash
cargo test --release -p komponist-builtin --lib cpu_ -- --ignored --nocapture
```

## 8. Volt (WP-95)

Measured 2026-11-15 on `wp/WP-95` (load average 6–11 on 12 threads, other agents building), release
profile, 48 kHz, calls of 256 frames (5.33 ms period), keys held (envelopes in sustain), one thread.
"Per key" is the cost of a held key (all its oscillators, sub, noise, high-pass, filter, three
envelopes, two LFOs, matrix, drift), measured as (call with N keys − call without) / N; the share
is of **one core** at 48 kHz. The proposal estimated 0.18 % (unison 1) and 0.42 % (unison 5 +
spread) per key; the numbers below are better, on a machine that was busy.

| Case | ns per key and sample | share of a core per key | 8 keys, 256 frames |
|---|---|---|---|
| Unison 1, LP 24 (default) | 30 | 0.14 % | 59 µs = 1.1 % |
| Unison 1, LP 12 | 21 | 0.10 % | 45 µs = 0.8 % |
| Unison 3 + spread | 53 | 0.25 % | 110 µs = 2.1 % |
| Unison 5 + spread | 64 | 0.31 % | 132 µs = 2.5 % |
| Unison 7 + spread | 76 | 0.36 % | 6 keys: 117 µs = 2.2 % |
| Unison 1, four matrix slots, both LFOs per voice, Analog 100 % | 29 | 0.14 % | – |
| Unison 7 + spread + matrix + Analog 100 % | 75 | 0.36 % | – |

- The matrix, the LFOs and the drift run at the control rate (every 32 samples) and are below the
  noise of the measurement; the unison oscillators (two PolyBLEP/PolyBLAMP oscillators per pair,
  about 8 ns each) and the second filter channel (spread) are what costs.
- **Cap:** `voices × unison` is limited to 48 oscillator groups (`volt::MAX_GROUPS`): unison 7
  leaves 6 keys, 5 leaves 9, 3 or less all 16. At the cap a track costs about 2.2–2.5 % of the
  period, so 16 tracks at the cap are about 40 % of one core, 10 % per worker with four workers
  (WP-65). 16 tracks with the default 8 keys and unison 1: 18 %, 4–5 % per worker.
- One parameter event per call (which splits the render): below 0.3 µs.
- No allocation: every `process` and `reset` of the unit tests runs inside `assert_no_alloc`
  (`internal::volt::tests`, the generic `processing_and_reset_do_not_allocate_in_any_device`) and
  the engine tests (`komponist-engine` `tests/engine/volt.rs`: 12 tracks with presets, live notes,
  pitch bend, mod wheel and pressure, bit-exact output with 0, 2 and 4 workers).

```bash
cargo test --release -p komponist-builtin --lib volt::bench -- --ignored --nocapture
```

## 9. Routing graph (WP-102)

Measured 2026-11-15 on the branch `wp/WP-102`, same machine as §1, **not idle**: other agents built and tested in
parallel (load average 6–10 on 12 threads during the runs), so worst cases are upper bounds; two runs back to back.
Bench `tests/engine/routing.rs` `bench_routing_16_tracks_4_buses`, release, 48 kHz / 256 frames (5.33 ms), wall
time of `Engine::process` on the calling thread over 20 s, workers at `SCHED_FIFO` 80. Design: ARCHITECTURE §6.8.4.

Material: 16 MIDI tracks, each instrument → `TestGain` (latencies 0–75 frames). **Flat**: every track into the
master. **Routed**: two groups of six tracks inside a third group (nested), two effect buses (a `TestGain` each, one
with 64 frames latency) fed by a post send from all 16 tracks and a pre send from every other track (24 sends), and
a sidechain from track 1 into an effect on track 13 (delayed to that device): 20 jobs, 4 of them joins, every
path delay-compensated. "Constant" instruments write a constant (the engine's own cost); "busy" ones cost about
120 µs per block each (a synth-like load, comparable to Hexa's 190 µs in §6).

| Run | Flat, 0 workers | Routed, 0 workers | Flat, 4 workers | Routed, 4 workers |
|---|---|---|---|---|
| Constant instruments, mean (run 1 / run 2) | 27.2 / 26.1 µs (0.5 %) | 45.6 / 43.9 µs (0.8 %) | 18.3 / 15.4 µs (0.3 %) | 35.7 / 32.0 µs (0.6–0.7 %) |
| Constant instruments, worst | 162 / 176 µs | 194 / 175 µs | 253 / 152 µs | 218 / 161 µs |
| Busy instruments, mean | 2.0 / 1.8 ms (34–37 %) | 1.8 / 1.9 ms (35 %) | 471 / 530 µs (8.8–10 %) | 478 / 746 µs (9.0–14 %) |
| Busy instruments, worst | 2.9 / 2.6 ms | 2.8 / 2.5 ms | 1.9 / 3.3 ms | 1.0 / 4.3 ms |

- **The graph costs about 18 µs per block** (0.33 % of the period) single-threaded for 4 buses, 24 sends (a copy,
  a delay line and a gain ramp each), the taps and a sidechain, and the same with workers. Against a synth load it
  disappears in the noise (34–37 % either way single-threaded).
- **Parallel scaling holds with buses:** 4 workers take the busy material from 35 % to 9–14 % of the period, routed
  or flat. The joins wait for their sources; the thread that completes a group's last member usually runs the group
  itself, so the dependency chain (sources → groups → outer group → master) adds no wake-ups in the common case.
  The spread of the routed 4-worker means (478 / 746 µs) and of the worst cases follows the machine's load between
  the two runs, not the routing; re-run on an idle machine.
- Projects without buses compile to the flat routing and render bit-identically to the engine before WP-102
  (golden hash of a render of the base commit, 0 and 3 workers); their cost is unchanged (flat row vs. §6 and the
  `bench` above).
- RT: every `process` / `render` of the routing tests runs inside `assert_no_alloc`, the workers run their jobs
  inside it, and program swaps with buses, sends and sidechains (also with changed latencies and routing) allocate
  nothing on the audio thread; a recompiled program is bit-identical to not swapping (delay lines and send gains are
  kept by edge).

## 10. Audio clips and disk streaming (#11)

Measured 2026-11-17 on `wp/11-audio-playback`, **Apple M1 Pro** (10 cores, 16 GiB, macOS 27.2, rustc 1.99.0, release),
not the machine of §1: the test engine has no realtime scheduling (no workgroup, normal priority), so single blocks
that a busy desktop delays say nothing about the callback path; the **mean** and the **read-ahead load** are the
figures. `audio::bench_audio_streams` plays N audio tracks, each with one 8 s clip from one of four 20 s stereo files
(two at 48 kHz, two at 44.1 kHz: half of the voices convert to the engine rate), looped over those 8 s so the stream
wraps every 8 s, in blocks of 256 frames paced like a device (5.33 ms) for 20 s. Files come from the OS page cache
(they were just written); a cold disk is the streamer's problem, which the ring (0.68 s) and the heads absorb, see
the stall tests.

| Streamed stereo clips | Audio thread, mean per 256 frames | Worst block | Blocks longer than the period | Read-ahead thread busy | Underruns |
|---|---|---|---|---|---|
| 16 | 65–75 µs (1.2–1.4 %) | 5.6–16.7 ms (scheduler, not at a loop wrap) | 1 of 3 750 | 8.3–9.0 % of one core | 0 |
| 64 | 165–174 µs (3.1–3.3 %) | 7.6–17.5 ms (scheduler) | 5 of 3 750 | 24–27 % of one core | 0 |

- **The audio thread's share is the ring copy:** about 4 µs per track and block (two planar atomics reads per frame
  and the fader), 0.08 % of the period per track including the engine's own overhead; a loop wrap costs nothing there
  (the streamer predicts it) and the long blocks fall at varying block numbers from run to run (two runs of the 64-track case: 96 and
  other blocks), which points at the scheduler, not at a pattern in the engine.
- **The read-ahead thread** decodes, converts (FFT resampler for the 44.1 kHz half), applies the fades and writes:
  ≈ 0.4–0.6 % of a core per voice at 48 kHz stereo, and it kept up in real time (≈ 48 000 frames per second per voice
  written, 0 underruns in both runs). It runs at normal priority and polls every 2 ms; if it had to wait for a slow
  disk, the ring covers 0.68 s, and `audio_underruns` counts the blocks where it did not.
- **Seeks:** a locate or a loop change makes the streamer seek and, for converted files, decode an 8192-frame pre-roll
  before the target; the first chunk of a new epoch arrives within a few milliseconds (2 ms poll plus the chunk).
  Heads (100 ms of every segment) cover jumps to segment starts in that time; the head memory is
  `100 ms × 8 bytes × rate` ≈ 38 KB per clip at 48 kHz (1 000 clips: 38 MB), built in the background.
- RT: no allocation or free on the audio thread with audio clips playing, looping, locating, stopping and swapping
  programs (`playing_clips_never_allocate_on_the_audio_thread`, debug, odd block sizes).

## 11. Warp worker (#14 WP-D)

Measured 2026-11-18 on AMD Ryzen 5 5600 (6 cores/12 threads), Linux x86_64, release,
48 kHz, actual 256-frame callbacks paced at the sample clock. Generated stereo sources,
warm heads/rings, one second per case, Signalsmith Default portable FFT. Run separately
under the shared build lock to avoid competing builds:

```bash
cargo test --release -p komponist-engine --test engine warp::worker_load -- --ignored --nocapture --test-threads=1
```

| Stereo tracks | Duration ratio | Worker µs / 256 | Equivalent share of one core | Underruns |
| --- | --- | --- | --- | --- |
| 8 | 1/6 | 978.1 | 18.34% | 0 |
| 8 | 1.5 | 650.9 | 12.20% | 0 |
| 8 | 6 | 655.9 | 12.30% | 0 |
| 16 | 1/6 | 1914.7 | 35.90% | 0 |
| 16 | 1.5 | 1206.4 | 22.62% | 0 |
| 16 | 6 | 1249.2 | 23.42% | 0 |

The worker time includes file decoding, stretching and ring writes, normalized to produced
frames per track. It is separate from callback processing; observed wall-time CPU share
agreed within 0.1 percentage points. This warmed single-worker probe does not characterize
cold priming peaks, long-session load, high-rate capacity or every target machine. Native
macOS/MSVC validation of WP-D remains pending; the earlier WP-B platform gates covered the
wrapper's original domain. Architecture §6.11 describes buffering, errors and seek behavior;
the WP-D internal notes retain logs and the native sanitizer probe.

## 12. Automation (#56)

| Scenario | Acceptance budget |
|---|---|
| Engine, 16 tracks with 16 ramping targets each, 256 frames at 48 kHz | At most 2% of the audio period (about 107 µs), with no realtime allocations |
| Recompile one track with 32 dense lanes and 50 looping clips | At most 2 ms |
| Arrangement playback, 16 tracks with 4 visible lanes each and 2,000 points per lane | Steady frames at most 8 ms with a median of at most 2 ms |

The engine and arrangement benchmarks use the same test binaries as the existing performance suites.
The engine tests also compare live and offline event positions across buffer sizes and exercise event
overflow with a bounded queue. The arrangement clips painting to the visible timeline.

On the Linux development host, the ramp benchmark averaged 7.1 µs without automation and 86.4 µs
with automation across 1,000 callbacks. The added 79.3 µs is 1.49% of a 256-frame period at 48 kHz.

The dense-lane benchmark measured steady medians of 1.45 ms at 1× and 0.90 ms at 2×, with
worst steady frames of 2.06 ms and 1.31 ms. The first frames, including layout and mesh
construction, took 10.46 ms and 10.85 ms.

## Track macros (#74)

| Case | Budget |
|---|---|
| Engine, 16 tracks × 8 macros × 4 mappings, 256 frames at 48 kHz | At most 2% of the block period (about 107 µs) added on the calling thread, with no realtime allocations |

Run `cargo test --release -p komponist-app --test engine_perf engine_cpu_16_tracks_with_track_macros -- --ignored --nocapture`.
The test swaps between two precompiled programs whose macro base values differ. It compares the
mean callback time with and without 512 bent parameter mappings, changing every macro on each
block. Program preparation and retirement run outside the measurement. This measures the
block-rate fan-out. Timeline ramps may emit several values per block and are covered by #76.
