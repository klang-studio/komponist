//! Tests of the MIDI effects (WP-88): the scale mapping table, the arpeggiator's patterns, timing at
//! sample accuracy across block boundaries and tempo changes, hold, jumps of the transport and the
//! RT rules. The engine side (guard, bypass, workers) is tested in `komponist-engine`'s
//! `tests/engine/midi_fx.rs`.

use assert_no_alloc::assert_no_alloc;
use komponist_engine::{
    Event, EventBuffer, EventKind, EventProcessor, NOTE_ID_NONE, ProcessContext, Processor,
    TransportInfo,
};
use komponist_model::{Meter, Tempo, TempoMap, Tick, TickRange};

use super::arpeggiator::{self, MODES, RATES};
use super::tonleiter::{self, SCALES, Snap, map_key};
use super::*;

const RATE: f32 = 48_000.0;

fn on(key: u8, offset: u32) -> Event {
    Event {
        offset,
        kind: EventKind::NoteOn {
            key,
            velocity: 100,
            channel: 0,
            note_id: NOTE_ID_NONE,
        },
    }
}

fn off(key: u8, offset: u32) -> Event {
    Event {
        offset,
        kind: EventKind::NoteOff {
            key,
            channel: 0,
            note_id: NOTE_ID_NONE,
        },
    }
}

fn ctx(frames: u32, bpm: f64, playing: bool, position_ticks: f64) -> ProcessContext {
    ProcessContext {
        frames,
        sample_rate: RATE,
        transport: TransportInfo {
            playing,
            recording: false,
            looping: false,
            tempo_bpm: bpm,
            meter: Meter::FOUR_FOUR,
            position_ticks,
            loop_range: TickRange::new(Tick::ZERO, Tick(3840)),
        },
        offline: false,
        deadline: None,
    }
}

// ------------------------------------------------------------------------------------------------
// Tonleiter
// ------------------------------------------------------------------------------------------------

fn scale_index(name: &str) -> usize {
    SCALES.iter().position(|s| s.0 == name).unwrap()
}

/// The mapping table of C major: every pitch class of the octave from C4 (60) for each snap.
#[test]
fn scale_mapping_table_c_major() {
    let major = scale_index("Dur");
    // (key offset above C4, nearest, up, down); ties go down.
    let table: [(u8, u8, u8, u8); 12] = [
        (0, 0, 0, 0),     // C
        (1, 0, 2, 0),     // C#: between C and D, a tie
        (2, 2, 2, 2),     // D
        (3, 2, 4, 2),     // D#: between D and E, a tie
        (4, 4, 4, 4),     // E
        (5, 5, 5, 5),     // F
        (6, 5, 7, 5),     // F#: between F and G, a tie
        (7, 7, 7, 7),     // G
        (8, 7, 9, 7),     // G#: between G and A, a tie
        (9, 9, 9, 9),     // A
        (10, 9, 11, 9),   // A#: between A and B, a tie
        (11, 11, 11, 11), // B
    ];
    for (k, nearest, up, down) in table {
        let key = 60 + k;
        for (snap, want) in [(Snap::Nearest, nearest), (Snap::Up, up), (Snap::Down, down)] {
            assert_eq!(
                map_key(key, 0, major, snap),
                Some(60 + want),
                "key {key} {snap:?}"
            );
        }
        let in_scale = nearest == k;
        assert_eq!(
            map_key(key, 0, major, Snap::Filter),
            in_scale.then_some(key),
            "key {key} filter"
        );
    }
}

#[test]
fn scale_mapping_with_other_roots_and_scales() {
    let minor = scale_index("Moll");
    // A minor (root 9) has the white keys of C major: A4 = 69.
    for key in 48..=84u8 {
        let c_major = map_key(key, 0, scale_index("Dur"), Snap::Up);
        assert_eq!(map_key(key, 9, minor, Snap::Up), c_major, "key {key}");
    }
    // D major pentatonic (root 2): D E F# A B. G (67) → nearest F# (66) vs A (69): F#.
    let penta = scale_index("Pentatonik Dur");
    assert_eq!(map_key(67, 2, penta, Snap::Nearest), Some(66));
    assert_eq!(map_key(67, 2, penta, Snap::Up), Some(69));
    assert_eq!(map_key(68, 2, penta, Snap::Nearest), Some(69));
    // Blues on C: C Eb F F# G Bb. D (62) is nearest to Eb (63, 1 away) before C (2 away).
    assert_eq!(
        map_key(62, 0, scale_index("Blues"), Snap::Nearest),
        Some(63)
    );
    // Chromatic never changes anything; every scale has its root.
    for key in 0..=127u8 {
        assert_eq!(
            map_key(key, 5, scale_index("Chromatisch"), Snap::Down),
            Some(key)
        );
    }
    for (i, _) in SCALES.iter().enumerate() {
        for root in 0..12u8 {
            assert_eq!(map_key(60 + root, root, i, Snap::Filter), Some(60 + root));
        }
    }
}

#[test]
fn scale_mapping_at_the_ends_of_the_key_range() {
    let major = scale_index("Dur");
    // C# at 1 and 127 (G major's F#... use C major: 127 is G, in scale; 126 is F#, below is F).
    assert_eq!(map_key(126, 0, major, Snap::Up), Some(127));
    assert_eq!(map_key(126, 0, major, Snap::Down), Some(125));
    // Nothing above 127 in a scale whose next key lies beyond: F# major pentatonic root 6 has
    // no key above 127 (G#=128): 127 is G.
    let penta = scale_index("Pentatonik Dur");
    assert_eq!(map_key(127, 6, penta, Snap::Up), None);
    assert_eq!(map_key(127, 6, penta, Snap::Nearest), Some(126));
    assert_eq!(map_key(0, 1, penta, Snap::Down), None);
}

fn stage_values(kind: Internal, values: &[(&str, f64)]) -> InternalProcessor {
    let mut p = kind.processor_from_state(None);
    p.activate(RATE, 256).unwrap();
    for (key, value) in values {
        let wire = kind.wire(&ParamId::new(*key)).unwrap();
        p.event_processor().unwrap().set_param(wire, *value);
    }
    p
}

/// One call of a stage inside `assert_no_alloc`; returns the output.
fn call(stage: &mut dyn EventProcessor, ctx: &ProcessContext, input: &[Event]) -> Vec<Event> {
    let mut buffer = EventBuffer::new(1024);
    assert_no_alloc(|| {
        let mut sink = buffer.sink(ctx.frames);
        stage.process_events(ctx, input, &mut sink);
    });
    buffer.events().to_vec()
}

#[test]
fn the_scale_stage_maps_notes_and_ends_each_one_with_its_own_key() {
    let mut p = stage_values(
        Internal::Tonleiter,
        &[("scale", 0.0), ("mode", 1.0)], // C major, snap up
    );
    let stage = p.event_processor().unwrap();
    let c = ctx(256, 120.0, true, 0.0);
    let out = call(stage, &c, &[on(61, 0), on(64, 10), off(61, 100)]);
    let keys: Vec<_> = out
        .iter()
        .map(|e| match e.kind {
            EventKind::NoteOn { key, .. } => ("on", key),
            EventKind::NoteOff { key, .. } => ("off", key),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(keys, [("on", 62), ("on", 64), ("off", 62)]);
    // A parameter change while key 64 is held: its off still ends key 64.
    stage.set_param(tonleiter::ROOT, 2.0);
    stage.set_param(tonleiter::MODE, 2.0);
    let out = call(stage, &c, &[off(64, 5), on(64, 6)]);
    assert!(matches!(out[0].kind, EventKind::NoteOff { key: 64, .. }));
    // D major snapping down: E (64 = F#? no: root D, 64 is E, in D major) stays.
    assert!(matches!(out[1].kind, EventKind::NoteOn { key: 64, .. }));
    // An off for a key that never went down is dropped; other MIDI passes.
    let cc = Event {
        offset: 3,
        kind: EventKind::Midi([0xb0, 1, 64]),
    };
    let out = call(stage, &c, &[off(99, 0), cc]);
    assert_eq!(out, [cc]);
}

#[test]
fn two_keys_that_land_on_one_note_share_it() {
    let mut p = stage_values(Internal::Tonleiter, &[("mode", 1.0)]); // C major, up
    let stage = p.event_processor().unwrap();
    let c = ctx(256, 120.0, true, 0.0);
    // C# and D both become D: one note-on, the off with the last release.
    let out = call(stage, &c, &[on(61, 0), on(62, 1), off(61, 10), off(62, 20)]);
    let ev: Vec<_> = out.iter().map(|e| (e.offset, e.kind)).collect();
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert!(matches!(ev[0].1, EventKind::NoteOn { key: 62, .. }) && ev[0].0 == 0);
    assert!(matches!(ev[1].1, EventKind::NoteOff { key: 62, .. }) && ev[1].0 == 20);
}

#[test]
fn filter_mode_drops_notes_outside_the_scale_and_their_offs() {
    let mut p = stage_values(Internal::Tonleiter, &[("mode", 3.0)]);
    let stage = p.event_processor().unwrap();
    let c = ctx(256, 120.0, true, 0.0);
    let out = call(stage, &c, &[on(61, 0), on(60, 1), off(61, 5), off(60, 6)]);
    assert_eq!(out.len(), 2);
    assert!(matches!(out[0].kind, EventKind::NoteOn { key: 60, .. }));
    assert!(matches!(out[1].kind, EventKind::NoteOff { key: 60, .. }));
}

// ------------------------------------------------------------------------------------------------
// Arpeggiator
// ------------------------------------------------------------------------------------------------

fn arp(values: &[(&str, f64)]) -> InternalProcessor {
    stage_values(Internal::Arpeggiator, values)
}

/// `(absolute sample, "on"/"off", key)` of the output of calls of `blocks` frames each (cycled)
/// over `total` frames, at a constant tempo while playing from tick 0. `first` is fed into the first
/// call; `later` into the call that starts at sample `at`.
fn run(
    p: &mut InternalProcessor,
    bpm: f64,
    blocks: &[u32],
    total: u32,
    first: &[Event],
) -> Vec<(u64, &'static str, u8)> {
    let map = TempoMap::constant(Tempo::new(bpm), Meter::FOUR_FOUR);
    let stage = p.event_processor().unwrap();
    let mut out = Vec::new();
    let (mut done, mut i) = (0u32, 0usize);
    while done < total {
        let n = blocks[i % blocks.len()].min(total - done);
        let ticks = map.samples_to_ticks(f64::from(done), f64::from(RATE));
        let c = ctx(n, bpm, true, ticks);
        let input = if done == 0 { first } else { &[] };
        for e in call(stage, &c, input) {
            let at = u64::from(done) + u64::from(e.offset);
            match e.kind {
                EventKind::NoteOn { key, .. } => out.push((at, "on", key)),
                EventKind::NoteOff { key, .. } => out.push((at, "off", key)),
                _ => {}
            }
        }
        done += n;
        i += 1;
    }
    out
}

/// The chord C E G held from the start.
fn chord() -> Vec<Event> {
    vec![on(60, 0), on(64, 0), on(67, 0)]
}

#[test]
fn patterns_walk_the_held_notes() {
    let keys = |mode: &str, octaves: f64, input: &[Event]| -> Vec<u8> {
        let m = MODES.iter().position(|m| *m == mode).unwrap() as f64;
        let mut p = arp(&[("mode", m), ("octaves", octaves), ("rate", 4.0)]);
        run(&mut p, 120.0, &[512], 12 * 6000, input)
            .into_iter()
            .filter(|e| e.1 == "on")
            .map(|e| e.2)
            .collect()
    };
    // Pressed out of order: G C E.
    let played = [on(67, 0), on(60, 0), on(64, 0)];
    assert_eq!(keys("Auf", 1.0, &played)[..6], [60, 64, 67, 60, 64, 67]);
    assert_eq!(keys("Ab", 1.0, &played)[..6], [67, 64, 60, 67, 64, 60]);
    assert_eq!(
        keys("Auf/Ab", 1.0, &played)[..8],
        [60, 64, 67, 64, 60, 64, 67, 64]
    );
    assert_eq!(
        keys("Gespielt", 1.0, &played)[..6],
        [67, 60, 64, 67, 60, 64]
    );
    assert_eq!(
        keys("Auf", 2.0, &chord())[..8],
        [60, 64, 67, 72, 76, 79, 60, 64]
    );
    assert_eq!(keys("Ab", 2.0, &chord())[..4], [79, 76, 72, 67]);
    // Up/down over two octaves turns at both ends without repeating them.
    assert_eq!(
        keys("Auf/Ab", 2.0, &chord())[..11],
        [60, 64, 67, 72, 76, 79, 76, 72, 67, 64, 60]
    );
    // A single note repeats in every mode; a note whose octave leaves the keyboard is skipped.
    for mode in MODES {
        assert!(
            keys(mode, 1.0, &[on(60, 0)]).iter().all(|k| *k == 60),
            "{mode}"
        );
    }
    let high = keys("Auf", 3.0, &[on(120, 0)]);
    assert_eq!(high[..2], [120, 120]);
    // Random: the same sequence every time (seeded), only held keys and octaves.
    let a = keys("Zufall", 2.0, &chord());
    let b = keys("Zufall", 2.0, &chord());
    assert_eq!(a, b);
    assert!(a.iter().all(|k| [60, 64, 67, 72, 76, 79].contains(k)));
    assert!(a.iter().collect::<std::collections::BTreeSet<_>>().len() > 3);
}

#[test]
fn steps_land_on_the_sequencers_samples_whatever_the_blocks_are() {
    for (bpm, rate_index) in [
        (120.0, 4usize),
        (127.0, 4),
        (93.7, 1),
        (141.0, 7),
        (240.0, 2),
    ] {
        let step = RATES[rate_index].1 as i64;
        let map = TempoMap::constant(Tempo::new(bpm), Meter::FOUR_FOUR);
        let total = 80_000u32;
        let steps = (i64::from(total) as f64 / map.samples_per_tick(f64::from(RATE)) / step as f64)
            .ceil() as i64;
        // What the sequencer would do for notes on the grid.
        let expect: Vec<u64> = (0..steps)
            .map(|n| map.tick_to_sample(Tick(n * step), f64::from(RATE)) as u64)
            .filter(|s| *s < u64::from(total))
            .collect();
        let splits: [&[u32]; 5] = [
            &[total],
            &[512],
            &[1, 2, 3, 255, 1024],
            &[37],
            &[2048, 7, 333],
        ];
        let mut first: Option<Vec<(u64, &str, u8)>> = None;
        for blocks in splits {
            let mut p = arp(&[("rate", rate_index as f64), ("gate", 0.5)]);
            let got = run(&mut p, bpm, blocks, total, &[on(60, 0)]);
            let ons: Vec<u64> = got.iter().filter(|e| e.1 == "on").map(|e| e.0).collect();
            assert_eq!(
                ons, expect,
                "{bpm} bpm, rate {rate_index}, blocks {blocks:?}"
            );
            match &first {
                None => first = Some(got),
                Some(f) => assert_eq!(&got, f, "{bpm} bpm, blocks {blocks:?}: offs too"),
            }
        }
    }
}

#[test]
fn gate_sets_the_note_length_and_the_off_always_follows_the_on() {
    // 1/16 at 120 bpm: a step is 6000 samples.
    let length = |gate: f64| -> u64 {
        let mut p = arp(&[("gate", gate)]);
        let got = run(&mut p, 120.0, &[300], 7000, &[on(60, 0)]);
        let on = got.iter().find(|e| e.1 == "on").unwrap().0;
        let off = got.iter().find(|e| e.1 == "off").unwrap().0;
        off - on
    };
    assert_eq!(length(0.5), 3000);
    assert_eq!(length(0.25), 1500);
    // Gate 100 %: legato, the off lands on the next step's sample, before its note-on.
    let mut p = arp(&[("gate", 1.0)]);
    let got = run(&mut p, 120.0, &[300], 13_000, &[on(60, 0)]);
    assert_eq!(
        got[..4],
        [
            (0, "on", 60),
            (6000, "off", 60),
            (6000, "on", 60),
            (12_000, "off", 60)
        ]
    );
    // The shortest gate still gives the note at least one sample, also at a block's last frame.
    for blocks in [&[1][..], &[3000], &[7]] {
        let mut q = arp(&[("gate", 0.05), ("rate", 7.0)]);
        let got = run(&mut q, 300.0, blocks, 20_000, &[on(60, 0)]);
        for pair in got.chunks(2) {
            if let [(a, "on", _), (b, "off", _)] = pair {
                assert!(b > a, "{pair:?}");
            }
        }
        assert!(got.len() > 10);
    }
}

#[test]
fn tempo_changes_keep_the_steps_on_the_tick_grid() {
    // 120 bpm for 40 000 samples, then 90 bpm: the grid is in ticks, so the next steps follow it.
    let mut p = arp(&[("rate", 4.0), ("gate", 0.5)]);
    let stage = p.event_processor().unwrap();
    let (mut ticks, mut all) = (0.0f64, Vec::new());
    let plan: [(f64, u32); 6] = [
        (120.0, 5000),
        (120.0, 7777),
        (90.0, 4096),
        (90.0, 10_000),
        (200.0, 3333),
        (200.0, 9000),
    ];
    for (i, (bpm, frames)) in plan.into_iter().enumerate() {
        let spt = f64::from(RATE) * 60.0 / (bpm * 960.0);
        let c = ctx(frames, bpm, true, ticks);
        let input = if i == 0 { vec![on(60, 0)] } else { Vec::new() };
        for e in call(stage, &c, &input) {
            if let EventKind::NoteOn { .. } = e.kind {
                all.push((ticks + f64::from(e.offset) / spt, spt));
            }
        }
        ticks += f64::from(frames) / spt;
    }
    assert!(all.len() >= 8, "{all:?}");
    for (n, (tick, spt)) in all.iter().enumerate() {
        let grid = (n as f64) * 240.0;
        // Within the half sample the rounding allows.
        assert!(
            (tick - grid).abs() <= 0.5 / spt + 1e-6,
            "step {n} at tick {tick}, grid {grid}"
        );
    }
}

#[test]
fn hold_keeps_released_keys_until_a_new_chord_starts() {
    let mut p = arp(&[("hold", 1.0), ("rate", 4.0)]);
    let stage = p.event_processor().unwrap();
    // One call per step (240 ticks = 6000 samples at 120 bpm); the keys it started.
    let mut pos = 0.0;
    let mut step = |stage: &mut dyn EventProcessor, input: &[Event]| -> Vec<u8> {
        let c = ctx(6000, 120.0, true, pos);
        pos += 240.0;
        call(stage, &c, input)
            .into_iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } => Some(key),
                _ => None,
            })
            .collect()
    };
    assert_eq!(step(stage, &[on(60, 0), on(64, 0)]), [60]);
    assert_eq!(step(stage, &[off(60, 0), off(64, 0)]), [64]);
    assert_eq!(step(stage, &[]), [60], "released keys keep playing");
    assert_eq!(step(stage, &[]), [64]);
    // A new key while none is held starts a new chord.
    assert_eq!(step(stage, &[on(67, 0)]), [67]);
    assert_eq!(step(stage, &[]), [67]);
    // Hold off: released keys drop out at once.
    stage.set_param(arpeggiator::HOLD, 0.0);
    assert_eq!(step(stage, &[]), [67], "still held down");
    assert_eq!(step(stage, &[off(67, 0)]), Vec::<u8>::new());
}

#[test]
fn a_jump_of_the_position_ends_the_note_and_realigns_the_grid() {
    let mut p = arp(&[("rate", 4.0), ("gate", 1.0)]);
    let stage = p.event_processor().unwrap();
    // 120 bpm: 6000 samples per step. Play 1.5 steps, then jump back (a loop wrap).
    let first = call(stage, &ctx(9000, 120.0, true, 0.0), &[on(60, 0)]);
    assert_eq!(first.len(), 3); // on at 0, off + on at 6000
    let jumped = call(stage, &ctx(100, 120.0, true, 0.0), &[]);
    // The sounding note ends at offset 0, the new lap's first step starts there too.
    let kinds: Vec<_> = jumped
        .iter()
        .map(|e| (e.offset, matches!(e.kind, EventKind::NoteOn { .. })))
        .collect();
    assert_eq!(kinds, [(0, false), (0, true)]);
    // Stopping the transport (live playing) switches to the stage's own clock: no jump of notes
    // beyond the release, and steps keep coming on the grid.
    let live = call(stage, &ctx(6000, 120.0, false, 960.0), &[]);
    assert!(
        live.iter()
            .any(|e| matches!(e.kind, EventKind::NoteOn { .. }))
    );
}

#[test]
fn live_playing_runs_on_its_own_clock_while_the_transport_stands() {
    let mut p = arp(&[("rate", 4.0)]);
    let stage = p.event_processor().unwrap();
    let mut ons = Vec::new();
    for i in 0..5u64 {
        // The transport does not move at tick 480 (on the grid); the stage's clock does.
        let c = ctx(3000, 120.0, false, 480.0);
        let input = if i == 0 { vec![on(60, 0)] } else { Vec::new() };
        for e in call(stage, &c, &input) {
            if let EventKind::NoteOn { .. } = e.kind {
                ons.push(i * 3000 + u64::from(e.offset));
            }
        }
    }
    assert_eq!(ons, [0, 6000, 12_000]);
}

#[test]
fn reset_forgets_everything_and_emits_nothing() {
    let mut p = arp(&[]);
    let stage = p.event_processor().unwrap();
    let out = call(stage, &ctx(100, 120.0, true, 0.0), &[on(60, 0)]);
    assert_eq!(out.len(), 1);
    stage.reset();
    let out = call(stage, &ctx(8000, 120.0, true, 0.0), &[]);
    assert!(out.is_empty(), "{out:?}");
    // The processor's own reset does the same.
    let mut p = arp(&[]);
    let _ = call(
        p.event_processor().unwrap(),
        &ctx(100, 120.0, true, 0.0),
        &[on(60, 0)],
    );
    p.reset();
    let out = call(
        p.event_processor().unwrap(),
        &ctx(8000, 120.0, true, 0.0),
        &[],
    );
    assert!(out.is_empty());
}

#[test]
fn midi_effects_do_not_allocate_and_other_midi_passes() {
    let mut p = arp(&[("mode", 3.0), ("octaves", 4.0)]);
    let stage = p.event_processor().unwrap();
    let bend = Event {
        offset: 2,
        kind: EventKind::Midi([0xe0, 0, 64]),
    };
    let mut input: Vec<Event> = (0..40).map(|i| on(30 + i, 0)).collect();
    input.push(bend);
    let out = call(stage, &ctx(2048, 300.0, true, 0.0), &input);
    assert!(out.contains(&bend));
    for _ in 0..50 {
        call(stage, &ctx(333, 300.0, true, 5000.0), &[]);
    }
    stage.reset();
}

#[test]
fn devices_are_cataloged_with_roles_german_names_and_valid_layouts() {
    assert_eq!(
        Internal::Arpeggiator.role(),
        komponist_model::DeviceRole::MidiEffect
    );
    assert_eq!(
        Internal::Tonleiter.role(),
        komponist_model::DeviceRole::MidiEffect
    );
    assert!(!Internal::Eq.is_midi_effect());
    let names =
        |kind: Internal| -> Vec<String> { kind.params().iter().map(|p| p.name.clone()).collect() };
    assert_eq!(
        names(Internal::Arpeggiator),
        ["Muster", "Rate", "Gate", "Oktaven", "Halten"]
    );
    assert_eq!(
        names(Internal::Tonleiter),
        ["Grundton", "Tonleiter", "Modus"]
    );
    // The processors are MIDI effects: they have a stage, the audio effects have none.
    assert!(
        Internal::Arpeggiator
            .processor(&[])
            .event_processor()
            .is_some()
    );
    assert!(Internal::Eq.processor(&[]).event_processor().is_none());
    // Their audio path passes through (the engine never runs it).
    let mut p = activated_midi(Internal::Tonleiter);
    let input = [vec![0.25f32; 64], vec![-0.5f32; 64]];
    let mut out = [vec![0.0f32; 64], vec![0.0f32; 64]];
    let [ol, or] = &mut out;
    let mut io = komponist_engine::StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    p.process(&ctx(64, 120.0, true, 0.0), &[], &mut io);
    assert_eq!(out, input);
}

fn activated_midi(kind: Internal) -> InternalProcessor {
    let mut p = kind.processor(&[]);
    p.activate(RATE, 256).unwrap();
    p
}
