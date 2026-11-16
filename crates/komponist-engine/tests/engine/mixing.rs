//! Mixer, meters, processor slots, garbage, sample rate and offline render.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use komponist_engine::program::TrackProgram;
use komponist_engine::testing::{Logged, TEST_GAIN_PARAM, TEST_SINE_LEVEL, TestGain, TestSine};
use komponist_engine::{
    Command, EngineConfig, EngineEvent, EngineHandle, Event, ProcessContext, ProcessStatus,
    Processor, ProcessorError, SlotId, StereoIo,
};
use komponist_model::{Tick, TrackId};

use crate::common::*;

/// Track `id` holding one long note on key `key` from tick 0, instrument in slot `id`.
fn droning(id: u64, key: u8) -> TrackProgram {
    track(id, id as u32, vec![note(0, 1000 * BAR, key)])
}

/// A rig playing three droning tracks (slots 1–3).
fn three_tracks() -> (Rig, komponist_engine::Program) {
    let p = program(120.0, vec![droning(1, 60), droning(2, 64), droning(3, 67)]);
    let mut rig = Rig::new(RATE, 1024);
    for slot in 1..=3 {
        rig.insert(slot, TestSine::new());
    }
    rig.set_program(&p);
    rig.send(Command::Play);
    (rig, p)
}

/// Peaks of the three track meters after a settled block.
fn settled_meters(rig: &mut Rig, p: &komponist_engine::Program) -> [f32; 3] {
    // The first block ramps to the new state; the second is steady.
    rig.process(1024, 1024);
    for t in &p.tracks {
        t.meter.take();
    }
    rig.handle.telemetry().master_meter().take();
    rig.process(1024, 1024);
    let mut peaks = [0.0; 3];
    for (peak, t) in peaks.iter_mut().zip(&p.tracks) {
        let (l, r) = t.meter.take();
        assert_eq!(l, r);
        *peak = l;
    }
    peaks
}

#[test]
fn solo_mute_matrix() {
    for via_commands in [true, false] {
        let (mut rig, mut p) = three_tracks();
        for bits in 0..64_u32 {
            let mute = |i: usize| bits & (1 << i) != 0;
            let solo = |i: usize| bits & (1 << (i + 3)) != 0;
            if via_commands {
                for i in 0..3 {
                    let track = TrackId(i as u64 + 1);
                    rig.send(Command::SetTrackMute {
                        track,
                        mute: mute(i),
                    });
                    rig.send(Command::SetTrackSolo {
                        track,
                        solo: solo(i),
                    });
                }
            } else {
                for (i, track) in p.tracks.iter_mut().enumerate().take(3) {
                    let t = Arc::make_mut(track);
                    t.mute = mute(i);
                    t.solo = solo(i);
                }
                rig.set_program(&p);
            }
            let peaks = settled_meters(&mut rig, &p);
            let any_solo = (0..3).any(solo);
            for (i, peak) in peaks.iter().enumerate() {
                let audible = !mute(i) && (!any_solo || solo(i));
                if audible {
                    assert!(
                        *peak > 0.45 && *peak <= TEST_SINE_LEVEL,
                        "{bits:06b} track {i}"
                    );
                } else {
                    assert_eq!(*peak, 0.0, "{bits:06b} track {i}");
                }
            }
            let (master, _) = rig.handle.telemetry().master_meter().take();
            assert_eq!(master > 0.0, peaks.iter().any(|&p| p > 0.0), "{bits:06b}");
        }
    }
}

#[test]
fn fast_path_values_last_until_the_next_program() {
    let (mut rig, p) = three_tracks();
    rig.send(Command::SetTrackMute {
        track: TrackId(1),
        mute: true,
    });
    rig.send(Command::SetTrackGain {
        track: TrackId(2),
        gain: 0.5,
    });
    let peaks = settled_meters(&mut rig, &p);
    assert_eq!(peaks[0], 0.0);
    assert!(peaks[1] > 0.2 && peaks[1] <= 0.25);
    rig.set_program(&p);
    let peaks = settled_meters(&mut rig, &p);
    assert!(peaks.iter().all(|&x| x > 0.45));
}

#[test]
fn meters_show_the_known_signal_after_fader_and_balance() {
    // TestSine starts at its peak 0.5. Track: gain 0.5, balance 0.5 right → L 0.125, R 0.25.
    // Master: gain 0.5 → L 0.0625, R 0.125.
    let mut t = track(1, 0, vec![note(0, BAR, 69)]);
    t.gain = 0.5;
    t.pan = 0.5;
    let mut p = program(120.0, vec![t]);
    p.master.gain = 0.5;
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(0, TestSine::new());
    rig.set_program(&p);
    rig.send(Command::Play);
    let range = rig.process(1000, 1000);
    assert_eq!(p.tracks[0].meter.take(), (0.125, 0.25));
    assert_eq!(
        rig.handle.telemetry().master_meter().take(),
        (0.0625, 0.125)
    );
    assert_eq!((rig.l[range.start], rig.r[range.start]), (0.0625, 0.125));
    // Taken: back to 0 until the next block.
    assert_eq!(p.tracks[0].meter.take(), (0.0, 0.0));
    // The master fader moves on the fast path, ramped over the next block.
    rig.send(Command::SetMasterGain(1.0));
    rig.process(1000, 1000);
    rig.handle.telemetry().master_meter().take();
    rig.process(1000, 1000);
    let (l, r) = rig.handle.telemetry().master_meter().take();
    assert!(l > 0.12 && l <= 0.125 && r > 0.24 && r <= 0.25, "{l} {r}");
}

/// The transport bar's master tap (#34): the mono sum of the master output after the fader, only
/// while the UI holds the lease, never in an offline render; the rate follows the engine.
#[test]
fn the_master_tap_records_the_mono_output_only_while_wanted() {
    let mut t = track(1, 0, vec![note(0, 1000 * BAR, 69)]);
    t.pan = 0.5;
    let mut p = program(120.0, vec![t]);
    p.master.gain = 0.5;
    let mut rig = Rig::new(RATE, 256);
    rig.insert(0, TestSine::new());
    rig.set_program(&p);
    rig.send(Command::Play);
    let tap = Arc::clone(rig.handle.telemetry().master_tap());
    assert_eq!(tap.rate(), RATE);
    // Nobody looks: the audio thread copies nothing (`Rig::process` runs in `assert_no_alloc`).
    rig.process(1024, 256);
    assert_eq!(tap.written(), 0);
    // The UI looks: every frame goes in, as (L + R) / 2 of what the engine output.
    tap.want();
    let range = rig.process(1024, 256);
    assert_eq!(tap.written(), 1024);
    let mut window = [0.0_f32; 512];
    assert_eq!(tap.read_latest(&mut window), Some(1024));
    let out = range.end - 512..range.end;
    for (i, (s, (l, r))) in window
        .iter()
        .zip(rig.l[out.clone()].iter().zip(&rig.r[out]))
        .enumerate()
    {
        assert_eq!(*s, 0.5 * (l + r), "{i}");
    }
    assert!(window.iter().any(|s| s.abs() > 0.05), "the sine is in it");
    // An offline render (export) does not feed the display.
    rig.render(512, 256);
    assert_eq!(tap.written(), 1024);
    // The lease runs out after half a second of audio; then the copying stops.
    rig.process(RATE as usize, 256);
    assert!(!tap.is_wanted());
    let stopped = tap.written();
    assert!(stopped <= 1024 + (RATE / 2.0) as u64 + 256, "{stopped}");
    rig.process(1024, 256);
    assert_eq!(tap.written(), stopped);
    // A new sample rate reaches the tap.
    rig.engine.set_sample_rate(96_000.0).unwrap();
    assert_eq!(tap.rate(), 96_000.0);
}

#[test]
fn delay_compensation_lines_tracks_up() {
    // Track 1: instrument → effect with 100 frames latency. Track 2: instrument, delayed by 100.
    let mut a = track(1, 0, vec![note(0, BAR, 69)]);
    a.chain.push(effect(1));
    let mut b = track(2, 2, vec![note(0, BAR, 69)]);
    b.pdc_delay = 100;
    let p = program(120.0, vec![a, b]);
    let mut rig = Rig::new(RATE, 64);
    rig.insert(0, TestSine::new());
    rig.insert(1, TestGain::with_latency(1.0, 100));
    rig.insert(2, TestSine::new());
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.process(1000, 64);
    assert_eq!(onsets(&rig.l), [100]);
    assert_eq!(rig.l[100], 2.0 * TEST_SINE_LEVEL);
    // Both in phase: the sum is twice track 2 alone.
    let mut alone = Rig::new(RATE, 64);
    alone.insert(2, TestSine::new());
    alone.set_program(&program(120.0, vec![p.tracks[1].as_ref().clone()]));
    alone.send(Command::Play);
    alone.process(1000, 64);
    for i in 0..1000 {
        assert_eq!(rig.l[i], 2.0 * alone.l[i], "{i}");
    }
}

#[test]
fn bypass_skips_and_resets_once() {
    let mut t = track(1, 0, vec![note(0, 100 * BAR, 69)]);
    t.chain.push(effect(1));
    let mut p = program(120.0, vec![t]);
    let mut rig = Rig::new(RATE, 2048);
    let (sine, mut log) = TestSine::logged(1 << 14);
    rig.insert(0, sine);
    rig.insert(1, TestGain::new(0.0));
    rig.set_program(&p);
    rig.send(Command::Play);
    // The effect silences everything.
    let range = rig.process(1000, 1000);
    assert!(rig.l[range].iter().all(|&x| x == 0.0));
    // Effect bypassed: passthrough.
    Arc::make_mut(&mut p.tracks[0]).chain[1].bypass = true;
    rig.set_program(&p);
    let range = rig.process(1000, 1000);
    assert!(rig.l[range].iter().any(|&x| x != 0.0));
    // Instrument bypassed (in two programs): silence, one reset, no calls.
    Arc::make_mut(&mut p.tracks[0]).chain[0].bypass = true;
    rig.set_program(&p);
    rig.set_program(&p);
    drain(&mut log);
    let range = rig.process(1000, 1000);
    assert!(rig.l[range].iter().all(|&x| x == 0.0));
    assert_eq!(drain(&mut log), [Logged::Reset { frame: 2000 }]);
    // Bypass off again: the processor runs.
    Arc::make_mut(&mut p.tracks[0]).chain[0].bypass = false;
    rig.set_program(&p);
    rig.process(1000, 1000);
    let entries = drain(&mut log);
    assert!(matches!(entries[0], Logged::Call { .. }), "{entries:?}");
}

#[test]
fn bypass_turning_on_resets_exactly_once() {
    let mut p = program(120.0, vec![track(1, 0, vec![])]);
    let mut rig = Rig::new(RATE, 2048);
    let (sine, mut log) = TestSine::logged(1 << 12);
    rig.insert(0, sine);
    rig.set_program(&p);
    rig.process(100, 100);
    Arc::make_mut(&mut p.tracks[0]).chain[0].bypass = true;
    for _ in 0..3 {
        rig.set_program(&p);
        rig.process(100, 100);
    }
    let resets = drain(&mut log)
        .iter()
        .filter(|e| matches!(e, Logged::Reset { .. }))
        .count();
    assert_eq!(resets, 1);
}

/// A processor that fails after `ok` calls and counts its calls.
struct Failing {
    ok: usize,
    calls: Arc<AtomicUsize>,
}

impl Processor for Failing {
    fn activate(&mut self, sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        if sample_rate == 44_100.0 {
            Err(ProcessorError::UnsupportedSampleRate(sample_rate))
        } else {
            Ok(())
        }
    }

    fn process(&mut self, _: &ProcessContext, _: &[Event], io: &mut StereoIo<'_>) -> ProcessStatus {
        let calls = self.calls.fetch_add(1, Ordering::Relaxed);
        io.silence();
        if calls >= self.ok {
            ProcessStatus::Failed
        } else {
            io.output[0].fill(0.25);
            io.output[1].fill(0.25);
            ProcessStatus::Continue
        }
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

#[test]
fn failed_processor_is_reported_once_and_skipped_until_replaced() {
    let calls = Arc::new(AtomicUsize::new(0));
    let p = program(120.0, vec![track(1, 7, vec![])]);
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(
        7,
        Failing {
            ok: 3,
            calls: calls.clone(),
        },
    );
    rig.set_program(&p);
    rig.process(10_000, 1000);
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    let failed: Vec<_> = rig
        .take_events()
        .into_iter()
        .filter(|e| matches!(e, EngineEvent::ProcessorFailed { slot: SlotId(7) }))
        .collect();
    assert_eq!(failed.len(), 1);
    assert!(rig.l[3000..].iter().all(|&x| x == 0.0));
    assert!(rig.l[..3000].iter().all(|&x| x == 0.25));
    // A replacement runs again.
    rig.insert(7, TestSine::new());
    rig.send(Command::LiveNote {
        track: TrackId(1),
        key: 69,
        velocity: 127,
    });
    let range = rig.process(100, 100);
    assert_eq!(rig.l[range.start], TEST_SINE_LEVEL);
    assert_eq!(rig.garbage, 1);
}

#[test]
fn set_sample_rate_reactivates_and_reports_failures() {
    let p = program(120.0, vec![track(1, 0, vec![]), track(2, 1, vec![])]);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(0, TestSine::new());
    rig.insert(
        1,
        Failing {
            ok: usize::MAX,
            calls: calls.clone(),
        },
    );
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.process(48_000, 1000);
    assert_eq!(rig.handle.telemetry().playhead(), Tick(1920));
    let errors = rig.engine.set_sample_rate(44_100.0).unwrap_err();
    assert_eq!(
        errors,
        [(SlotId(1), ProcessorError::UnsupportedSampleRate(44_100.0))]
    );
    assert_eq!(rig.engine.sample_rate(), 44_100.0);
    let before = calls.load(Ordering::Relaxed);
    rig.process(441, 441);
    // The musical position stays; the failed processor is skipped and not reported again.
    // 441 frames at 44.1 kHz and 120 bpm are 19.2 ticks.
    assert_eq!(rig.handle.telemetry().playhead(), Tick(1920 + 19));
    assert_eq!(calls.load(Ordering::Relaxed), before);
    assert!(
        rig.events
            .iter()
            .all(|e| !matches!(e, EngineEvent::ProcessorFailed { .. }))
    );
    assert_eq!(rig.engine.set_sample_rate(48_000.0), Ok(()));
}

/// A processor that counts its drops.
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl Processor for Counted {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(&mut self, _: &ProcessContext, _: &[Event], io: &mut StereoIo<'_>) -> ProcessStatus {
        io.passthrough();
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

/// Sends `InsertProcessor`s for slot 5 until `sent` reaches `total` or the queue is full. A
/// command the queue refused is kept in `pending` (dropping it here would count as a drop).
fn fill(
    handle: &mut EngineHandle,
    pending: &mut Option<Command>,
    sent: &mut usize,
    total: usize,
    drops: &Arc<AtomicUsize>,
) -> bool {
    let mut progressed = false;
    while *sent < total {
        let command = pending.take().unwrap_or_else(|| Command::InsertProcessor {
            slot: SlotId(5),
            processor: Box::new(Counted(drops.clone())),
        });
        match handle.send(command) {
            Ok(()) => {
                *sent += 1;
                progressed = true;
            }
            Err(command) => {
                *pending = Some(command);
                break;
            }
        }
    }
    progressed
}

#[test]
fn garbage_is_never_dropped_by_the_engine_even_when_the_ui_stops_polling() {
    // More replaced processors than the event queue, the holding list and the command queue
    // hold together. The engine runs in `assert_no_alloc`, so a drop on its side would abort.
    let total = komponist_engine::EVENT_CAPACITY + komponist_engine::COMMAND_CAPACITY + 1000;
    let drops = Arc::new(AtomicUsize::new(0));
    let (mut engine, mut handle) = komponist_engine::Engine::new(EngineConfig::default());
    let (mut sent, mut stalled, mut pending) = (0, 0, None);
    let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
    let input = komponist_engine::LiveInput {
        midi: &[],
        output_latency: 0,
    };
    while sent < total && stalled < 3 {
        let progressed = fill(&mut handle, &mut pending, &mut sent, total, &drops);
        assert_no_alloc::assert_no_alloc(|| engine.process(&mut l, &mut r, &input));
        stalled = if progressed { 0 } else { stalled + 1 };
    }
    // The engine stopped taking commands that retire something; nothing was dropped.
    assert!(sent < total, "{sent}");
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(handle.telemetry().dropped_events(), 0);
    // The UI polls again: everything arrives and the remaining commands go through.
    let mut garbage = 0;
    while sent < total || garbage < total - 1 {
        while let Some(event) = handle.poll() {
            assert!(matches!(event, EngineEvent::Retired(_)));
            garbage += 1;
        }
        fill(&mut handle, &mut pending, &mut sent, total, &drops);
        assert_no_alloc::assert_no_alloc(|| engine.process(&mut l, &mut r, &input));
    }
    assert_eq!(garbage, total - 1);
    assert_eq!(drops.load(Ordering::Relaxed), total - 1);
}

#[test]
fn parameters_reach_effects_and_master_inserts() {
    let mut t = track(1, 0, vec![note(0, BAR, 69)]);
    t.chain.push(effect(1));
    let mut p = program(120.0, vec![t]);
    p.master.chain.push(effect(2));
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(0, TestSine::new());
    rig.insert(1, TestGain::new(1.0));
    rig.insert(2, TestGain::new(1.0));
    rig.set_program(&p);
    rig.send(Command::Play);
    let range = rig.process(10, 10);
    assert_eq!(rig.l[range.start], TEST_SINE_LEVEL);
    for (slot, value) in [(1, 0.5), (2, 0.25)] {
        rig.send(Command::SetParam {
            slot: SlotId(slot),
            id: TEST_GAIN_PARAM,
            value,
        });
    }
    rig.send(Command::Stop);
    rig.send(Command::Locate(Tick(0)));
    rig.send(Command::Play);
    let range = rig.process(10, 10);
    assert_eq!(rig.l[range.start], TEST_SINE_LEVEL * 0.5 * 0.25);
}

#[test]
fn render_is_offline_and_matches_process() {
    let mut p = program(
        133.0,
        vec![track(1, 0, vec![note(0, 500, 60), note(700, 1500, 67)])],
    );
    p.metronome = true;
    let run = |offline: bool| {
        let mut rig = Rig::new(44_100.0, 512);
        let (sine, mut log) = TestSine::logged(1 << 12);
        rig.insert(0, sine);
        rig.set_program(&p);
        rig.send(Command::Play);
        if offline {
            rig.render(50_000, 2048);
        } else {
            rig.process(50_000, 2048);
        }
        let flags: Vec<bool> = drain(&mut log)
            .iter()
            .filter_map(|e| match e {
                Logged::Call { offline, .. } => Some(*offline),
                _ => None,
            })
            .collect();
        (rig.l, rig.r, flags)
    };
    let (rl, rr, offline) = run(true);
    let (pl, pr, online) = run(false);
    assert_eq!(rl, pl);
    assert_eq!(rr, pr);
    assert!(offline.iter().all(|&o| o) && online.iter().all(|&o| !o));
}

#[test]
#[should_panic(expected = "render")]
fn render_rejects_oversized_blocks() {
    let (mut engine, _handle) = komponist_engine::Engine::new(EngineConfig::default());
    engine.render(komponist_engine::MAX_BLOCK + 1);
}

#[test]
fn instruments_ignore_events_while_their_slot_is_empty() {
    // The program refers to a slot nobody filled: silence, and nothing breaks.
    let p = program(120.0, vec![track(1, 9, vec![note(0, 100, 60)])]);
    let mut rig = Rig::new(RATE, 2048);
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.send(Command::SetParam {
        slot: SlotId(9),
        id: 1,
        value: 1.0,
    });
    rig.send(Command::SetParam {
        slot: SlotId(100_000),
        id: 1,
        value: 1.0,
    });
    rig.send(Command::RemoveProcessor {
        slot: SlotId(100_000),
    });
    rig.insert(100_000, TestSine::new());
    rig.process(10_000, 1000);
    assert!(rig.l.iter().all(|&x| x == 0.0));
    // The processor for the invalid slot came back.
    assert_eq!(rig.garbage, 1);
}
