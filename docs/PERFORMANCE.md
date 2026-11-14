# komponist – Performance and RT safety (WP-61, WP-65)

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
| UI frame, same project **playing** (16 pisynth tracks on the audio thread), dock open with the piano roll | ≤ 8 ms | median 1.3 / 1.3 ms, worst 1.5 / 1.4 ms | met |
| First frame after load (clip previews built) | — | 10–17 ms | one-off, acceptable |
| Piano roll, one clip with 10 000 notes, whole app | ≤ 8 ms | median 2.5 ms | met |
| Timeline paint, 10 000 notes (WP-22 bench) | < 4 ms | piano roll 3.4 ms (1×) / 4.0 ms (2×, median 3.97); arrangement 0.6 ms | met, **2× at the limit** |
| Engine's own cost, 16 tracks (sequencer, chains, mixer, meters, PDC) | — | 8 µs mean per 256 frames (0.15 %) | negligible |
| Engine + 16 pisynth tracks, stress material, one thread | no overrun | mean 3.3 ms = 62 % of the period, worst 6–11 ms | **missed under load** (pisynth DSP, see §4.1) |
| Same in the real callback path (app, null backend) | no overrun | mean 67–69 %, peak 110–120 %, overruns 49–63 of 1 880 callbacks (2.6–3.4 %), xruns 0 | **missed under load** |
| + 8 kp-test-fx bridges (one per track on 8 tracks) | no late block | mean +2 points (70.7 %), late blocks 12 (SCHED_FIFO) / 95 (SCHED_OTHER) of 15 000 | **missed under load** (follows from the above) |
| **WP-65:** 16 pisynth, callback path, 4 worker threads (§6) | < 50 % of the period, no overrun in 5 min | mean 17.5–17.7 %, peak 28.5 %, 0 overruns in 56 265 callbacks; + 8 bridges 21.1 %, 0 late blocks | **met** |
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
| pisynth `PisynthProcessor::process` / `reset` / parameter events | `komponist-builtin` `tests/pisynth` |
| `ExternalProcessor::process` (bridge client: wire events, stash, futex wait, fallbacks) | `komponist-plugins` `tests/engine.rs` (`call` and `Rig`) |
| IPC `begin_block` / `wait_done` / output events | `komponist-ipc` `tests/bridge_process.rs` (10 000 checked blocks) |
| Bridge RT loop | `komponist-bridge` `rt.rs` runs its loop inside `assert_no_alloc` in debug builds |
| Audio callback: backend → `EngineSlot` (attach/detach mailbox) → `EngineProcess` (MIDI → frames) → engine | `komponist-audio` `StreamCallback::run` wraps every callback; every app test on the null backend runs it (acceptance flows, soak) |
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
add/remove/move/copy/loop clips, fader/pan/mute/solo, tempo, new pisynth track, remove track,
pisynth preset, bypass, undo, redo, open clip, mixer, and at least one WAV export of the loop
(detach, offline render, re-attach). Checks: no allocation in the callback (debug), the audio thread
kept running (≥ 90 % of the expected callbacks), no command left in the queue, no overrun (release).

Runs (all release unless noted; the test binary under `chrt -f 70`, so the null audio thread has RT
priority like a PipeWire callback):

| Run | Callbacks | Mean / peak load | Overruns | xruns | UI frame worst | Exports |
|---|---|---|---|---|---|---|
| 6 s, `cargo test` (debug) | 1 100 of ~1 128 | — / 260 % | 118 (unoptimised pisynth; not asserted) | 6 | 156 ms | 1 |
| 6 s, release, SCHED_OTHER | 1 127 | — / 35 % | 0 | 0 | 6.9 ms | 2 |
| 5 min, debug (SCHED_OTHER) | 52 527 of ~56 253 | — / 443 % | 9 907 (unoptimised pisynth; not asserted) | 478 | 137 ms | 38 |
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
created pisynth are not slower than later ones (`first_calls_of_a_new_processor`, ±10 µs), so new
processors after undo/redo are not the cause.

Recommendation (owner decision, not done in WP-61): lock the app's memory at start
(`mlockall(MCL_CURRENT | MCL_FUTURE)`, as JACK clients and Ardour do; this user's `RLIMIT_MEMLOCK`
is unlimited) or at least pre-fault and lock the engine's memory and the audio thread's stack. A
GPU app locks its driver mappings too with `MCL_FUTURE`, so this needs a measurement of the
resident size first.

## 4. Decisions

### 4.1 Engine CPU: pisynth dominates

The engine's own work for 16 tracks is 8 µs per 256-frame block (0.15 %); pisynth's DSP is the
rest. The stress material (per track a four-note chord per half bar, held, plus a 1/16 line, with
the presets E-Piano, Strings, Supersaw and Init; 8 or more voices per track, release tails
overlapping) costs 62 % of one core at 48 kHz in a single thread, matching the per-voice numbers of
the WP-25 bench (8 voices: Init 1.0 %, E-Piano 2.6 %, Strings 5.6 %, Supersaw 5.0 %). The demo
(gate G3, 10 pisynth instances) used 11–12 %.

At that load the single audio thread has no headroom on a busy machine: 2.6–3.4 % of the callbacks
overran, and bridges placed after many tracks miss their 60 % deadline (they fall back to
passthrough, as designed). Not fixed in WP-61, because both remedies are bigger than this package:

- **Parallel track processing** (ARCHITECTURE §8.1 "later optimisation"): tracks are independent
  until the master bus; a worker pool (or bridge fan-out) would spread 16 tracks over the cores.
  Recommended as a work package after v0.1.
- **pisynth DSP cost** (`../pisynth`, WP-34's klang-dsp migration is the place).
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
  audio already flows. It answers klopfer's cold start without touching `../klopfer` (WP-54 saw
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
cargo test --release -p komponist-app --test piano_roll ten_thousand -- --nocapture
cargo bench -p komponist-ui --bench timeline

# Engine CPU: engine alone and + 16 pisynth, and the real callback path (+ 8 bridges; needs
# scripts/build-test-plugins.sh), each with 0 and 4 worker threads (WP-65; KOMPONIST_BENCH_WORKERS /
# KOMPONIST_PERF_WORKERS take another list, KOMPONIST_PERF_SECS another length). For RT priority
# run the test binary under chrt -f 70.
cargo test --release -p komponist-engine --test engine bench -- --ignored --nocapture
cargo test --release -p komponist-app --test engine_perf -- --ignored --nocapture --test-threads 1
KOMPONIST_PERF_WORKERS=4 KOMPONIST_PERF_SECS=300 <engine_perf binary> engine_cpu_16_pisynth_tracks \
    --exact --ignored --nocapture                                  # the 5-minute stress run
cargo test --release -p komponist-builtin --test pisynth cpu_per_block -- --ignored --nocapture

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

# klopfer cold start (needs scripts/build-klopfer.sh and klopfer's test kit)
cargo test --release -p komponist-plugins --test installed klopfer_cold_start -- --ignored --nocapture

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
| Engine + 16 pisynth, stress material, one thread (`bench`) | 3.1 ms (58.6 %), worst 5.1 ms (96 %) | 3.1 ms (58.7 %), worst 5.2 ms (98 %) | **0.89 ms (16.7 %), worst 2.2 ms (42 %)** |
| 16 pisynth, real callback path, 10 s (`engine_perf`) | mean 63.5 %, peak 94.6 %, 0 overruns | mean 63.4 %, peak 79.7 %, 0 overruns | **mean 17.5 %, peak 27.7 %, 0 overruns** |
| + 8 kp-test-fx bridges (after pisynth on 8 tracks), 10 s | — | mean 65.0 %, peak 88.6 %, 0 late blocks | **mean 21.1 %, peak 27.9 %, 0 late blocks** |
| 16 pisynth, callback path, **5 minutes** | — | — | **mean 17.7 %, peak 28.5 %, 0 overruns, 0 xruns in 56 265 callbacks**, 0 late waits |
| Soak, release, **5 minutes** (demo, random edits, 42 exports) | — | — | **0 overruns, 0 xruns in 56 250 callbacks**, peak 63 %, worst UI frame 7.3 ms; 59 635 batches, 390 397 jobs on workers, 0 late waits |
| Soak, release, 30 minutes | — | — | **open** (see below); stopped after 6 of 30 minutes by owner decision with 0 overruns, 0 xruns in 67 503 callbacks so far |

**Done-when met:** 16 pisynth stress tracks at 17.5–17.7 % of the period (target < 50 %) with 4
workers, no overrun in the 5-minute stress run and the 5-minute soak. The 3.5× speed-up on 5
threads (callback + 4 workers) is close to the ideal for 16 jobs of uneven cost; the remaining time
is the longest job chain on one thread plus the wake-ups (~10–30 µs per batch). The dispatch costs
2 µs per block with 16 trivial tracks (idle-instrument bench). The worst single callback of the
WP-61 baseline (95–98 %) shrinks to 28 % with workers: one slow track no longer adds to 15 others.

Notes:
- "Before" and "after, 0 workers" agree, so the three-phase segment (events, jobs, ordered sum)
  costs nothing single-threaded. The bit-exact tests prove they compute the same output.
- The bridge case puts the kp-test-fx *after* pisynth, so those bridges cannot start early; their
  waits now overlap on the workers. Instrument bridges at the head of a chain start before any
  chain runs (`Processor::begin`), checked by `komponist-plugins`
  `bridged_instruments_with_workers_match_single_threaded` (bit-exact offline, all realtime blocks
  in time).
- The soak's peak load (63 % in one callback) is the attach after an export (the soak runs 42 of
  them); its mean stays at 1–5 %.
- `WorkerStats::late` (waits for a running job beyond one period) stayed 0 in every run.

**Open: the 30-minute soak with workers.** The done-when of WP-65 is the 5-minute soak; the
30-minute release soak (with the default 4 workers) is still to be run once before the next
release, overnight or on an otherwise idle machine (no builds), and its result added to the table
above:

```bash
cargo test --release -p komponist-app --test soak --no-run   # prints the binary
cd crates/komponist-app && KOMPONIST_SOAK_SECS=1800 chrt -f 70 <soak binary> --nocapture
```
