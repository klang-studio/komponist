//! Sample-exact timing: note onsets, loop wraps, count-in, metronome.

use komponist_engine::testing::{Logged, TEST_SINE_LEVEL, TestSine};
use komponist_engine::{Command, EngineEvent, TransportState};
use komponist_model::Tick;

use crate::common::*;

#[test]
fn notes_start_and_end_on_their_samples() {
    // Ticks that land between samples at these tempos and rates.
    let ticks = [0_i64, 50, 959, BAR + 123, 2 * BAR + 7, 3 * BAR - 1];
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for bpm in [124.0, 97.5, 240.0] {
            let seq: Vec<_> = ticks
                .iter()
                .enumerate()
                .map(|(i, &t)| note(t, 13 + i as i64, 60 + i as u8))
                .collect();
            let p = program(bpm, vec![track(1, 0, seq)]);
            for (max_block, block) in [(64, 173), (2048, 4096), (1, 7)] {
                let mut rig = Rig::new(rate, max_block);
                let (sine, mut log) = TestSine::logged(1 << 16);
                rig.insert(0, sine);
                rig.set_program(&p);
                rig.send(Command::Play);
                let total = sample(&p, 3 * BAR + 200, rate) as usize;
                // Drained in chunks: with a block of 1 every frame is a logged call.
                let mut entries = Vec::new();
                let mut done = 0;
                while done < total {
                    let n = (total - done).min(30_030);
                    rig.process(n, block);
                    entries.extend(drain(&mut log));
                    done += n;
                }
                let got = notes(&entries);
                let mut expected = Vec::new();
                for (i, &t) in ticks.iter().enumerate() {
                    let key = 60 + i as u8;
                    expected.push((sample(&p, t, rate) as u64, "on", key));
                    expected.push((sample(&p, t + 13 + i as i64, rate) as u64, "off", key));
                }
                expected.sort();
                let mut got: Vec<_> = got.iter().map(|&(f, k, key, _)| (f, k, key)).collect();
                got.sort();
                assert_eq!(
                    got, expected,
                    "{rate} Hz, {bpm} bpm, block {block}/{max_block}"
                );
                // The audio starts on the same sample, at the note's peak, and stops dead.
                for (i, &t) in ticks.iter().enumerate() {
                    let on = sample(&p, t, rate) as usize;
                    let off = sample(&p, t + 13 + i as i64, rate) as usize;
                    assert_eq!(rig.l[on], TEST_SINE_LEVEL, "onset of tick {t}");
                    assert!(on == 0 || rig.l[on - 1] == 0.0, "silence before tick {t}");
                    assert_eq!(rig.l[off], 0.0, "silence after tick {t}");
                }
            }
        }
    }
}

#[test]
fn loop_wraps_mid_block() {
    // 120 bpm at 48 kHz: one bar is 96 000 samples, 93.75 blocks of 1024.
    let mut p = program(
        120.0,
        vec![track(1, 0, vec![note(0, 960, 60), note(3600, 480, 64)])],
    );
    p.loop_on = true;
    p.loop_range.end = Tick(BAR);
    let mut rig = Rig::new(RATE, 2048);
    let (sine, mut log) = TestSine::logged(1 << 16);
    rig.insert(0, sine);
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.process(3 * 96_000 + 500, 1024);
    let log = drain(&mut log);
    let got: Vec<_> = notes(&log)
        .iter()
        .map(|&(f, k, key, _)| (f, k, key))
        .collect();
    let mut expected = Vec::new();
    for lap in 0..3 {
        let o = lap * 96_000;
        expected.extend([
            (o, "on", 60),
            (o + 24_000, "off", 60),
            (o + 90_000, "on", 64),
            // Cut at the wrap, before the next lap's first note.
            (o + 96_000, "off", 64),
        ]);
    }
    expected.push((3 * 96_000, "on", 60));
    assert_eq!(got, expected);
    // The processor call ends at the loop end and the next starts at the loop start.
    let calls: Vec<(u64, u32, f64)> = log
        .iter()
        .filter_map(|e| match *e {
            Logged::Call {
                frame,
                frames,
                transport,
                ..
            } => Some((frame, frames, transport.position_ticks)),
            _ => None,
        })
        .collect();
    let wrap = calls
        .iter()
        .position(|c| c.0 + u64::from(c.1) == 96_000)
        .unwrap();
    assert_eq!(calls[wrap].0, 93 * 1024);
    assert_eq!(calls[wrap + 1].0, 96_000);
    assert_eq!(calls[wrap + 1].2, 0.0);
    // Mid-block: the block of the wrap was cut into 768 + 256 frames.
    assert_eq!((calls[wrap].1, calls[wrap + 1].1), (768, 256));
    let wraps = rig
        .take_events()
        .into_iter()
        .filter(|e| matches!(e, EngineEvent::LoopWrapped { to: Tick(0) }))
        .count();
    assert_eq!(wraps, 3);
    assert_eq!(rig.handle.telemetry().playhead(), Tick(500 / 25));
}

#[test]
fn loop_shorter_than_a_block_wraps_every_time() {
    // 240 bpm: 12.5 samples per tick; a 1/32 loop (120 ticks) is 1500 samples.
    let mut p = program(240.0, vec![track(1, 0, vec![note(BAR, 60, 60)])]);
    p.loop_on = true;
    p.loop_range = komponist_model::TickRange::new(Tick(BAR), Tick(BAR + 120));
    let mut rig = Rig::new(RATE, 2048);
    let (sine, mut log) = TestSine::logged(1 << 16);
    rig.insert(0, sine);
    rig.set_program(&p);
    rig.send(Command::Locate(Tick(BAR)));
    rig.send(Command::Play);
    rig.process(15_000, 2048);
    let got: Vec<_> = notes(&drain(&mut log))
        .iter()
        .map(|&(f, k, _, _)| (f, k))
        .collect();
    let expected: Vec<_> = (0..10_u64)
        .flat_map(|lap| [(lap * 1500, "on"), (lap * 1500 + 750, "off")])
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn count_in_lasts_its_bars_and_clicks() {
    for (bars, from) in [(1_u8, 2 * BAR), (2, 0), (0, BAR)] {
        let mut p = program(120.0, Vec::new());
        p.count_in_bars = bars;
        let mut rig = Rig::new(RATE, 512);
        rig.set_program(&p);
        rig.send(Command::Record { from: Tick(from) });
        let length = usize::from(bars) * 96_000;
        rig.process(length, 1000);
        let t = rig.handle.telemetry().clone();
        if bars > 0 {
            // At the capture start the count-in has not ended yet …
            assert_eq!(t.transport(), TransportState::CountIn, "{bars} bars");
            assert!(rig.take_events().is_empty());
        }
        // … it ends with the first frame there.
        rig.process(1, 1);
        assert_eq!(t.transport(), TransportState::Recording);
        assert_eq!(t.playhead(), Tick(from));
        let events = rig.take_events();
        assert!(
            matches!(events[..], [EngineEvent::CountInDone { at }] if at == Tick(from)),
            "{events:?}"
        );
        // One click per beat, accent on beat 1, metronome off afterwards.
        rig.process(96_000, 1000);
        let clicks = onsets(&rig.l);
        let expected: Vec<usize> = (0..4 * usize::from(bars)).map(|b| b * 24_000).collect();
        assert_eq!(clicks, expected, "{bars} bars");
        assert_eq!(rig.l, rig.r);
        for (beat, &at) in clicks.iter().enumerate() {
            let accent = rig.l[at] == rig.l[clicks[0]];
            assert_eq!(accent, beat % 4 == 0, "beat {beat}");
        }
    }
}

#[test]
fn count_in_starts_at_negative_ticks() {
    let mut p = program(120.0, Vec::new());
    p.count_in_bars = 2;
    let mut rig = Rig::new(RATE, 2048);
    rig.set_program(&p);
    rig.send(Command::Record { from: Tick(0) });
    rig.process(1, 1);
    let t = rig.handle.telemetry().clone();
    assert_eq!(t.transport(), TransportState::CountIn);
    assert_eq!(t.playhead(), Tick(-2 * BAR));
    // Recording while playing: capture starts at once.
    let mut rig = Rig::new(RATE, 2048);
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.process(48_000, 1000);
    rig.send(Command::Record { from: Tick(99) });
    rig.process(1, 1);
    let events = rig.take_events();
    assert!(
        matches!(events[..], [EngineEvent::CountInDone { at: Tick(1920) }]),
        "{events:?}"
    );
    assert_eq!(
        rig.handle.telemetry().transport(),
        TransportState::Recording
    );
}

#[test]
fn metronome_clicks_on_every_beat() {
    for (rate, bpm) in [(48_000.0, 120.0), (44_100.0, 124.0)] {
        let mut p = program(bpm, Vec::new());
        p.metronome = true;
        let mut rig = Rig::new(rate, 300);
        rig.set_program(&p);
        rig.send(Command::Play);
        let total = sample(&p, 2 * BAR + 10, rate) as usize;
        rig.process(total, 512);
        let expected: Vec<usize> = (0..9).map(|b| sample(&p, b * 960, rate) as usize).collect();
        let clicks = onsets(&rig.l);
        assert_eq!(clicks, expected, "{rate} Hz {bpm} bpm");
        // Beat 1 is accented with a higher pitch: its first sample is larger.
        let first = rig.l[clicks[0]];
        let accents: Vec<bool> = clicks.iter().map(|&i| rig.l[i] == first).collect();
        assert_eq!(
            accents,
            [true, false, false, false, true, false, false, false, true]
        );
        assert!(rig.l[clicks[1]] > 0.0 && rig.l[clicks[1]] < first);
        // The click is not metered.
        assert_eq!(rig.handle.telemetry().master_meter().take(), (0.0, 0.0));
    }
}

#[test]
fn metronome_follows_the_loop_and_stays_off_without_it() {
    // Loop over beats 2–3 of bar 1: clicks at the loop start after every wrap.
    let mut p = program(120.0, Vec::new());
    p.metronome = true;
    p.loop_on = true;
    p.loop_range = komponist_model::TickRange::new(Tick(960), Tick(2880));
    let mut rig = Rig::new(RATE, 2048);
    rig.set_program(&p);
    rig.send(Command::Locate(Tick(960)));
    rig.send(Command::Play);
    rig.process(48_000 * 2 + 1, 1000);
    assert_eq!(onsets(&rig.l), [0, 24_000, 48_000, 72_000, 96_000]);
    // Metronome off: silence while playing.
    p.metronome = false;
    let mut rig = Rig::new(RATE, 2048);
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.process(100_000, 1000);
    assert!(rig.l.iter().all(|&x| x == 0.0));
}
