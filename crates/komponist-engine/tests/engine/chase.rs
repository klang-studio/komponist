//! Note chasing (WP-61): playback start, locate while playing and loop wrap start the notes that
//! sound across the new position; program swaps do not.

use komponist_engine::export::{BitDepth, ExportJob};
use komponist_engine::program::SeqNote;
use komponist_engine::testing::{EventLog, TestSine};
use komponist_engine::{Command, Program};
use komponist_model::{Tick, TickRange};

use crate::common::*;

/// 120 bpm at 48 kHz: 25 samples per tick, one bar = 96 000 samples.
const BAR_SAMPLES: u64 = 96_000;

/// A rig with a logged `TestSine` in slot 0, `p` installed, the playhead at `at`, then `Play`.
fn play_from(p: &Program, at: i64) -> (Rig, EventLog) {
    let mut rig = Rig::new(RATE, 512);
    let (sine, log) = TestSine::logged(1 << 16);
    rig.insert(0, sine);
    rig.set_program(p);
    rig.send(Command::Locate(Tick(at)));
    rig.send(Command::Play);
    (rig, log)
}

fn keys(log: &mut EventLog) -> Vec<(u64, &'static str, u8)> {
    notes(&drain(log))
        .into_iter()
        .map(|n| (n.0, n.1, n.2))
        .collect()
}

#[test]
fn play_inside_a_long_note_starts_it_with_its_remaining_length() {
    let p = program(
        120.0,
        vec![track(
            1,
            0,
            vec![
                note(0, 2 * BAR, 60),
                // Ended before the position: not chased.
                note(BAR / 2, 100, 62),
                // Ends exactly at the position: not chased.
                note(BAR / 2, BAR / 2, 64),
            ],
        )],
    );
    let (mut rig, mut log) = play_from(&p, BAR);
    rig.process(2 * BAR_SAMPLES as usize, 1000);
    assert_eq!(keys(&mut log), [(0, "on", 60), (BAR_SAMPLES, "off", 60)]);
    // The chased note sounds from the first frame.
    assert!(rig.l[0] != 0.0 || rig.l[1] != 0.0);
}

#[test]
fn locate_while_playing_chases_at_the_new_position() {
    let p = program(
        120.0,
        vec![track(
            1,
            0,
            vec![note(0, BAR / 4, 60), note(BAR, 2 * BAR, 67)],
        )],
    );
    let (mut rig, mut log) = play_from(&p, 0);
    rig.process(10_000, 1000);
    rig.send(Command::Locate(Tick(2 * BAR)));
    rig.process(10_000, 1000);
    // The note sounding at the locate ends there; the pad under the new position starts.
    assert_eq!(
        keys(&mut log),
        [(0, "on", 60), (10_000, "off", 60), (10_000, "on", 67)]
    );
}

#[test]
fn loop_wrap_chases_notes_that_started_before_the_loop_start() {
    let mut p = program(
        120.0,
        vec![track(
            1,
            0,
            vec![note(0, 3 * BAR, 60), note(BAR + BAR / 2, BAR / 4, 64)],
        )],
    );
    p.loop_range = TickRange::new(Tick(BAR), Tick(2 * BAR));
    p.loop_on = true;
    let (mut rig, mut log) = play_from(&p, BAR);
    // Two laps.
    rig.process(2 * BAR_SAMPLES as usize, 1000);
    let half = BAR_SAMPLES / 2;
    let quarter = BAR_SAMPLES / 4;
    assert_eq!(
        keys(&mut log),
        [
            (0, "on", 60),
            (half, "on", 64),
            (half + quarter, "off", 64),
            // Wrap: sequenced notes end at the loop end, the pad starts again at the loop start.
            (BAR_SAMPLES, "off", 60),
            (BAR_SAMPLES, "on", 60),
            (BAR_SAMPLES + half, "on", 64),
            (BAR_SAMPLES + half + quarter, "off", 64),
        ]
    );
}

#[test]
fn program_swap_does_not_chase() {
    let empty = program(120.0, vec![track(1, 0, vec![])]);
    let (mut rig, mut log) = play_from(&empty, 0);
    rig.process(1000, 500);
    // A note under the playhead appears (an edit while playing): it is not started mid-way.
    let edited = program(120.0, vec![track(1, 0, vec![note(0, BAR, 60)])]);
    rig.set_program(&edited);
    rig.process(1000, 500);
    assert_eq!(keys(&mut log), []);
}

#[test]
fn probability_applies_to_chased_notes() {
    let never = SeqNote {
        probability: 0,
        ..note(0, 2 * BAR, 60)
    };
    let p = program(120.0, vec![track(1, 0, vec![never, note(0, 2 * BAR, 62)])]);
    let (mut rig, mut log) = play_from(&p, BAR);
    rig.process(1000, 500);
    assert_eq!(keys(&mut log), [(0, "on", 62)]);
}

#[test]
fn chasing_scans_thousands_of_notes_without_allocating() {
    // 10 000 short notes before a long one: the scan runs inside `assert_no_alloc` (Rig).
    let mut notes: Vec<SeqNote> = (0..10_000).map(|i| note(i, 1, 40)).collect();
    notes.push(note(10_000, 100 * BAR, 72));
    let p = program(120.0, vec![track(1, 0, notes)]);
    let (mut rig, mut log) = play_from(&p, 20_000);
    rig.process(512, 512);
    assert_eq!(keys(&mut log), [(0, "on", 72)]);
}

#[test]
fn an_export_starting_inside_a_pad_renders_the_pad() {
    let p = program(120.0, vec![track(1, 0, vec![note(0, 4 * BAR, 60)])]);
    let mut rig = Rig::new(RATE, 512);
    rig.insert(0, TestSine::new());
    rig.set_program(&p);
    let dir = crate::export::dir("chase");
    let range = ExportJob::frames_of(
        &p.tempo_map,
        TickRange::new(Tick(BAR), Tick(2 * BAR)),
        RATE as u32,
    );
    let job = ExportJob {
        path: dir.join("pad.wav"),
        sample_rate: RATE as u32,
        range,
        tail: 0,
        bit_depth: BitDepth::Float32,
        normalize: None,
        dither: false,
    };
    let (result, _) = crate::export::run(&mut rig, &p, &job);
    result.expect("export");
    let wav = crate::export::read(&job.path);
    // The pad sounds from the first frame of the file to its end.
    let first = &wav.samples[..2 * 64];
    assert!(first.iter().any(|&x| x != 0.0), "silent start");
    let silent_frames = wav
        .samples
        .chunks(2)
        .collect::<Vec<_>>()
        .windows(64)
        .filter(|w| w.iter().all(|f| f[0] == 0.0 && f[1] == 0.0))
        .count();
    assert_eq!(silent_frames, 0);
}
