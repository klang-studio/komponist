//! Scale mapping, MIDI note lifetimes and realtime processing.

use assert_no_alloc::assert_no_alloc;
use komponist_device_core::InternalProcessor;
use komponist_engine::{
    Event, EventBuffer, EventKind, EventProcessor, NOTE_ID_NONE, ProcessContext, Processor,
    TransportInfo,
};
use komponist_model::params::ParamId;
use komponist_model::{Meter, Tick, TickRange};

use super::{MODE, ROOT, SCALES, SPEC, Snap, map_key};

// Each device's test binary checks allocations and frees in debug builds.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

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

fn with_note_id(mut event: Event, id: u32) -> Event {
    match &mut event.kind {
        EventKind::NoteOn { note_id, .. } | EventKind::NoteOff { note_id, .. } => *note_id = id,
        _ => unreachable!(),
    }
    event
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

fn processor(values: &[(&str, f64)]) -> InternalProcessor {
    let mut processor = InternalProcessor::new(&SPEC, &[]);
    processor.activate(RATE, 256).unwrap();
    for (key, value) in values {
        let wire = SPEC.wire(&ParamId::new(*key)).unwrap();
        processor.event_processor().unwrap().set_param(wire, *value);
    }
    processor
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

#[test]
fn the_scale_stage_maps_notes_and_ends_each_one_with_its_own_key() {
    let mut p = processor(
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
    stage.set_param(ROOT, 2.0);
    stage.set_param(MODE, 2.0);
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
    let mut p = processor(&[("mode", 1.0)]); // C major, up
    let stage = p.event_processor().unwrap();
    let c = ctx(256, 120.0, true, 0.0);
    // C# and D both become D: one note-on, the off with the last release.
    let out = call(
        stage,
        &c,
        &[
            with_note_id(on(61, 0), 15),
            with_note_id(on(62, 1), 39),
            with_note_id(off(61, 10), 15),
            with_note_id(off(62, 20), 39),
        ],
    );
    let ev: Vec<_> = out.iter().map(|e| (e.offset, e.kind)).collect();
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert!(
        matches!(
            ev[0].1,
            EventKind::NoteOn {
                key: 62,
                note_id: 15,
                ..
            }
        ) && ev[0].0 == 0
    );
    assert!(
        matches!(
            ev[1].1,
            EventKind::NoteOff {
                key: 62,
                note_id: 15,
                ..
            }
        ) && ev[1].0 == 20
    );
}

#[test]
fn a_stale_note_off_does_not_release_a_retriggered_scale_note() {
    let mut p = processor(&[("mode", 1.0)]); // C major, up
    let stage = p.event_processor().unwrap();
    let c = ctx(256, 120.0, true, 0.0);
    for release_id in [39, NOTE_ID_NONE] {
        let events = [
            with_note_id(on(61, 0), 15),
            with_note_id(on(61, 1), 39),
            with_note_id(off(61, 10), 15),
            with_note_id(off(61, 20), release_id),
        ];
        assert_eq!(
            call(stage, &c, &events),
            [
                with_note_id(on(62, 0), 15),
                with_note_id(off(62, 1), 15),
                with_note_id(on(62, 1), 39),
                with_note_id(off(62, 20), 39),
            ]
        );
    }
}

#[test]
fn filter_mode_drops_notes_outside_the_scale_and_their_offs() {
    let mut p = processor(&[("mode", 3.0)]);
    let stage = p.event_processor().unwrap();
    let c = ctx(256, 120.0, true, 0.0);
    let out = call(stage, &c, &[on(61, 0), on(60, 1), off(61, 5), off(60, 6)]);
    assert_eq!(out.len(), 2);
    assert!(matches!(out[0].kind, EventKind::NoteOn { key: 60, .. }));
    assert!(matches!(out[1].kind, EventKind::NoteOff { key: 60, .. }));
}

#[test]
fn the_audio_path_passes_through() {
    let mut p = processor(&[]);
    let input = [vec![0.25f32; 64], vec![-0.5f32; 64]];
    let mut out = [vec![0.0f32; 64], vec![0.0f32; 64]];
    let [ol, or] = &mut out;
    let mut io = komponist_engine::StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    assert_no_alloc(|| p.process(&ctx(64, 120.0, true, 0.0), &[], &mut io));
    assert_eq!(out, input);
}
