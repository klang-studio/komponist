//! Golden event lists (PLAN WP-24 "Done when"): demo project, looped clip, step clip with swing
//! 50 %, ratchet 3, probability 0/100, muted clip, empty project; plus takes with transposition
//! and velocity change, and device chains with MIDI routing and latency compensation.

use std::sync::Arc;

use komponist_engine::MidiPort;
use komponist_engine::compile::{self, CompileEnv, NoEnv};
use komponist_engine::program::Program;
use komponist_model::fixtures::{demo_project, empty_project};
use komponist_model::project::VOLUME_DB_MIN;
use komponist_model::{
    ClipContent, DeviceId, DeviceRole, MidiChannelSel, MidiClip, MidiInputSel, Project,
    StepResolution, Tick, TickRange,
};

use crate::support::{
    TestEnv, check_golden, clip, device, dump, midi, on, pattern, project, take, track,
};

/// Compiles `project` from scratch and checks the dump against `tests/golden/<name>.txt`.
fn golden(name: &str, title: &str, project: &Project, env: &dyn CompileEnv) -> Program {
    let program = compile::build(project, None, env);
    check_golden(name, &dump(title, project, &program));
    program
}

#[test]
fn demo_project_golden() {
    let program = golden(
        "demo_project",
        "The demo project \"Nachtfahrt\" (fixtures::demo_project)",
        &demo_project(),
        &NoEnv,
    );
    assert_eq!(program.tracks.len(), 6);
}

#[test]
fn empty_project_golden() {
    let program = golden(
        "empty_project",
        "An empty project (fixtures::empty_project)",
        &empty_project(),
        &NoEnv,
    );
    assert!(program.tracks.is_empty());
    assert!(program.slots.entries().is_empty());
    assert_eq!(program.song_end, Tick::ZERO);
}

/// A clip looped every half bar over 5000 ticks (the last repetition cut at the clip end), a note
/// reaching past the loop length, a note starting after it; and an unlooped clip whose notes are
/// cut at, or start at, the clip end.
#[test]
fn looped_clip_golden() {
    let mut looped = clip(
        10,
        3840,
        5000,
        midi(&[(0, 480, 60, 100), (960, 1440, 64, 80), (1920, 240, 67, 90)]),
    );
    looped.loop_length = Some(Tick(1920));
    let plain = clip(
        20,
        11520,
        1920,
        midi(&[
            (0, 960, 48, 100),
            (1440, 960, 52, 100),
            (1920, 240, 55, 100),
        ]),
    );
    let mut p = project(vec![track(1, "Loop", vec![looped, plain])]);
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick(3840), Tick(7680));
    let program = golden(
        "looped_clip",
        "Looped clip (loop 1920 in 5000 ticks) and notes cut at the clip end",
        &p,
        &NoEnv,
    );
    let starts: Vec<i64> = program.tracks[0].notes.iter().map(|n| n.start.0).collect();
    assert_eq!(starts, [3840, 4800, 5760, 6720, 7680, 8640, 11520, 12960]);
    let lengths: Vec<i64> = program.tracks[0].notes.iter().map(|n| n.length.0).collect();
    assert_eq!(lengths, [480, 960, 480, 960, 480, 200, 960, 480]);
}

/// Swing 50 % at 1/16 (odd steps 60 ticks late) and at 1/8 with a 4-step pattern repeating in the
/// clip (odd pattern indices 120 ticks late).
#[test]
fn step_swing_50_golden() {
    let mut sixteenths = pattern(
        &[("Kick", 36), ("Hat zu", 42)],
        16,
        StepResolution::Sixteenth,
    );
    for step in [0, 4, 8, 12] {
        on(&mut sixteenths, 0, step).velocity = 120;
    }
    for step in 0..16 {
        on(&mut sixteenths, 1, step).velocity = if step % 2 == 0 { 90 } else { 50 };
    }
    sixteenths.swing = 0.5;
    let mut eighths = pattern(&[("Hat zu", 42)], 4, StepResolution::Eighth);
    for step in 0..4 {
        on(&mut eighths, 0, step);
    }
    eighths.swing = 0.5;
    let p = project(vec![track(
        1,
        "Drums",
        vec![
            clip(10, 0, 3840, ClipContent::Step(sixteenths)),
            clip(20, 3840, 3840, ClipContent::Step(eighths)),
        ],
    )]);
    let program = golden(
        "step_swing_50",
        "Step clips with swing 50 % (1/16, and 1/8 with a 4-step pattern)",
        &p,
        &NoEnv,
    );
    let hats: Vec<i64> = program.tracks[0]
        .notes
        .iter()
        .filter(|n| n.key == 42 && n.start.0 < 960)
        .map(|n| n.start.0)
        .collect();
    assert_eq!(hats, [0, 300, 480, 780]);
}

/// Ratchet 3 at 1/16 (80-tick hits, later hits at 75 % velocity and following the first hit's
/// roll), with probability 50 on one ratcheted step, and ratchet 3 in a 1/16-triplet step.
#[test]
fn step_ratchet_3_golden() {
    let mut sixteenths = pattern(
        &[("Kick", 36), ("Snare", 38)],
        16,
        StepResolution::Sixteenth,
    );
    on(&mut sixteenths, 0, 0);
    let s = on(&mut sixteenths, 1, 4);
    s.ratchet = 3;
    let s = on(&mut sixteenths, 1, 12);
    s.ratchet = 3;
    s.velocity = 90;
    s.probability = 50;
    let mut triplets = pattern(&[("Hat zu", 42)], 6, StepResolution::SixteenthTriplet);
    on(&mut triplets, 0, 0).ratchet = 3;
    let p = project(vec![track(
        1,
        "Drums",
        vec![
            clip(10, 0, 3840, ClipContent::Step(sixteenths)),
            clip(20, 3840, 960, ClipContent::Step(triplets)),
        ],
    )]);
    golden(
        "step_ratchet_3",
        "Step clips with ratchet 3 (1/16 and 1/16 triplets)",
        &p,
        &NoEnv,
    );
}

/// Probability 100 and 0 kept per note (0 never plays, but stays in the program so the engine's
/// "follow" rule sees the roll); a ratchet on a probability-0 step follows that roll.
#[test]
fn step_probability_golden() {
    let mut steps = pattern(
        &[("Kick", 36), ("Hat zu", 42)],
        16,
        StepResolution::Sixteenth,
    );
    for (step, probability) in [(0, 100), (4, 0), (8, 100), (12, 0)] {
        on(&mut steps, 0, step).probability = probability;
    }
    let s = on(&mut steps, 1, 2);
    s.probability = 0;
    s.ratchet = 2;
    on(&mut steps, 1, 6).probability = 100;
    let p = project(vec![track(
        1,
        "Drums",
        vec![clip(10, 0, 3840, ClipContent::Step(steps))],
    )]);
    golden(
        "step_probability",
        "Step clip with probability 0 and 100",
        &p,
        &NoEnv,
    );
}

/// A muted MIDI clip and a muted step clip are left out; a muted track keeps its notes (mute is
/// applied by the engine, so it can change without recompiling).
#[test]
fn muted_clip_golden() {
    let mut steps = pattern(&[("Kick", 36)], 16, StepResolution::Sixteenth);
    on(&mut steps, 0, 0);
    let mut muted_midi = clip(20, 1920, 1920, midi(&[(0, 240, 72, 100)]));
    muted_midi.mute = true;
    let mut muted_steps = clip(30, 3840, 3840, ClipContent::Step(steps));
    muted_steps.mute = true;
    let mut muted_track = track(
        2,
        "Stumm",
        vec![clip(40, 0, 3840, midi(&[(0, 960, 50, 90)]))],
    );
    muted_track.channel.mute = true;
    let p = project(vec![
        track(
            1,
            "Spur",
            vec![
                clip(10, 0, 1920, midi(&[(0, 240, 60, 100), (960, 240, 62, 100)])),
                muted_midi,
                muted_steps,
            ],
        ),
        muted_track,
    ]);
    let program = golden(
        "muted_clip",
        "Muted MIDI and step clips are skipped; a muted track keeps its notes",
        &p,
        &NoEnv,
    );
    assert_eq!(program.tracks[0].notes.len(), 2);
    assert!(program.tracks[1].mute);
    assert_eq!(program.tracks[1].notes.len(), 1);
}

/// The active take plays (not take 0); transposition and velocity change apply to MIDI and step
/// clips; notes moved outside 0 ..= 127 are dropped; velocities clamp to 1 ..= 127.
#[test]
fn takes_transpose_velocity_golden() {
    let mut takes = clip(10, 0, 3840, midi(&[]));
    takes.content = ClipContent::Midi(MidiClip {
        takes: vec![
            take("Take 1", 100, &[(0, 480, 40, 100)]),
            take(
                "Take 2",
                200,
                &[(0, 480, 60, 100), (480, 480, 64, 80), (960, 480, 120, 90)],
            ),
        ],
        active_take: 1,
    });
    takes.transpose = 12;
    takes.velocity_pct = 50;
    let mut down = clip(
        20,
        3840,
        1920,
        midi(&[(0, 240, 10, 100), (480, 240, 60, 33)]),
    );
    down.transpose = -24;
    down.velocity_pct = -100;
    let mut steps = pattern(&[("Kick", 36)], 16, StepResolution::Sixteenth);
    on(&mut steps, 0, 0).ratchet = 2;
    let mut step_clip = clip(30, 7680, 960, ClipContent::Step(steps));
    step_clip.transpose = 1;
    step_clip.velocity_pct = -50;
    let p = project(vec![track(1, "Keys", vec![takes, down, step_clip])]);
    let program = golden(
        "takes_transpose_velocity",
        "Active take, clip transposition and velocity change",
        &p,
        &NoEnv,
    );
    let keys: Vec<(u8, u8)> = program.tracks[0]
        .notes
        .iter()
        .map(|n| (n.key, n.velocity))
        .collect();
    assert_eq!(keys, [(72, 127), (76, 120), (36, 1), (37, 50), (37, 38)]);
}

/// Device chains → slots (bypass, an effect-only track, invalid instrument and duplicate device
/// skipped, master inserts), fader gain incl. −∞, MIDI input routing through the environment,
/// latency compensation from the chain latencies.
#[test]
fn chains_and_routing_golden() {
    use DeviceRole::{Effect, Instrument};

    let mut synth = track(1, "Synth", vec![]);
    synth.channel.devices = vec![device(11, Instrument, "hexa"), device(12, Effect, "EQ")];
    synth.channel.volume_db = 6.0;
    synth.input = MidiInputSel::Device("Keystep 37".to_owned());
    synth.midi_channel = MidiChannelSel::Channel(2);
    synth.arm = true;
    synth.monitor = true;

    let mut pad = track(2, "Pad", vec![]);
    let mut chorus = device(22, Effect, "Chorus");
    Arc::make_mut(&mut chorus).bypass = true;
    pad.channel.devices = vec![
        device(21, Instrument, "Surge XT"),
        chorus,
        device(23, Effect, "Reverb"),
    ];
    pad.channel.volume_db = -9.0;
    pad.channel.pan = -0.1;
    pad.channel.solo = true;
    pad.input = MidiInputSel::Device("Gone".to_owned());

    let mut silent = track(3, "Leer", vec![]);
    silent.channel.volume_db = VOLUME_DB_MIN;

    let mut broken = track(4, "Kaputt", vec![]);
    broken.channel.devices = vec![
        device(41, Effect, "Delay"),
        device(42, Instrument, "Dexed"),
        device(12, Effect, "EQ"),
    ];

    let mut p = project(vec![synth, pad, silent, broken]);
    let master = Arc::make_mut(&mut p.master);
    master.devices = vec![
        device(51, Effect, "Limiter"),
        device(52, Instrument, "Vital"),
    ];
    master.volume_db = -3.0;
    master.pan = 0.25;

    let env = TestEnv {
        ports: vec![("Keystep 37".to_owned(), MidiPort(3))],
        latencies: vec![
            (DeviceId(12), 64),
            (DeviceId(21), 128),
            (DeviceId(22), 1000),
            (DeviceId(23), 256),
            (DeviceId(51), 32),
        ],
    };
    let program = golden(
        "chains_and_routing",
        "Device chains, slots, fader gain, MIDI input routing and latency compensation",
        &p,
        &env,
    );
    let pdc: Vec<u32> = program.tracks.iter().map(|t| t.pdc_delay).collect();
    assert_eq!(pdc, [320, 0, 384, 384]);
    assert_eq!(program.tracks[2].gain, 0.0);
}
