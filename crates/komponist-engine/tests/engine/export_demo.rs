//! The compiled "Night Drive" project exported with test instruments and feedback delays.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use komponist_engine::compile::{self, NoEnv};
use komponist_engine::export::{BitDepth, ExportJob, NORMALIZE_TARGET_DB, export};
use komponist_engine::testing::TestSine;
use komponist_engine::{Command, MAX_BLOCK, TransportState};
use komponist_model::fixtures::demo_project;
use komponist_model::{DeviceRole, Tick, TickRange};

use crate::common::*;
use crate::export::{dir, files, peak_db, read};
use crate::feedback_delay::FeedbackDelay;

#[test]
fn the_demo_project_exports_and_can_be_played_again() {
    let project = demo_project();
    let mut program = compile::build(&project, None, &NoEnv);
    let mut rig = Rig::new(RATE, MAX_BLOCK);
    let mut next_slot = compile::slot_changes(None, &program)
        .added
        .iter()
        .map(|(_, slot)| slot.0)
        .max()
        .unwrap()
        + 1;
    for track in &mut program.tracks {
        if let Some(instrument) = track
            .chain
            .iter()
            .find(|slot| slot.role == DeviceRole::Instrument)
        {
            rig.insert(instrument.slot.0, TestSine::new());
            rig.insert(next_slot, FeedbackDelay::new(2_048, 0.7));
            let track = Arc::make_mut(track);
            track.chain.push(effect(next_slot));
            track.gain *= 0.35;
            next_slot += 1;
        }
    }
    rig.set_program(&program);

    // "Loop" shortened to its first bar (bar 9: drums, bass, lead, pad, arp muted) to keep the
    // debug build fast, plus one second of tail; 24 bit, normalised, dithered.
    let ticks = TickRange::new(
        program.loop_range.start,
        Tick(program.loop_range.start.0 + BAR),
    );
    let range = ExportJob::frames_of(&program.tempo_map, ticks, 48_000);
    let tail = ExportJob::tail_frames(1.0, 48_000);
    let dir = dir("demo");
    let path = dir.join("Night Drive.wav");
    let job = ExportJob {
        path: path.clone(),
        sample_rate: 48_000,
        range: range.clone(),
        tail,
        bit_depth: BitDepth::Int24,
        normalize: Some(NORMALIZE_TARGET_DB),
        dither: true,
    };
    let cancel = AtomicBool::new(false);
    let mut fractions = Vec::new();
    let report = export(
        &mut rig.engine,
        &mut rig.handle,
        &program,
        &job,
        &cancel,
        |p| fractions.push(p.fraction),
    )
    .unwrap();
    assert_eq!(files(&dir), ["Night Drive.wav"]);
    assert_eq!(fractions.last(), Some(&1.0));

    let wav = read(&path);
    let bar = range.end - range.start;
    assert_eq!(u64::from(wav.frames), bar + tail);
    assert_eq!(report.frames, bar + tail);
    let peak = peak_db(&wav.samples);
    assert!(
        (peak - NORMALIZE_TARGET_DB).abs() < 0.01,
        "peak {peak} dBFS"
    );
    assert!(report.peak_db < 0.0 && report.peak_db > -30.0, "{report:?}");
    // Music in the bar, decaying echoes in the tail.
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let body = rms(&wav.samples[..2 * bar as usize]);
    let end = rms(&wav.samples[wav.samples.len() - 9_600..]);
    assert!(body > 0.05, "rms {body}");
    assert!(end < body / 10.0, "tail rms {end} vs {body}");

    // Re-attached, the engine plays the demo again.
    rig.process(1024, 1024);
    assert_eq!(rig.handle.telemetry().transport(), TransportState::Stopped);
    rig.send(Command::Locate(Tick::ZERO));
    rig.send(Command::Play);
    let played = rig.process(48_000, 1024);
    assert!(rms(&rig.l[played]) > 0.01);
    assert_eq!(rig.garbage, 2);
}
