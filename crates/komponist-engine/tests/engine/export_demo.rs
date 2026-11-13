//! The demo project "Nachtfahrt" exported through `PisynthProcessor` (every instrument is a
//! pisynth with its stored state or "Init"; effects have no processor and pass audio through).

use std::sync::atomic::AtomicBool;

use komponist_builtin::PisynthProcessor;
use komponist_engine::compile::{self, NoEnv};
use komponist_engine::export::{BitDepth, ExportJob, NORMALIZE_TARGET_DB, export};
use komponist_engine::{Command, MAX_BLOCK, Processor, SlotId, TransportState};
use komponist_model::fixtures::demo_project;
use komponist_model::{DeviceRole, Tick, TickRange};

use crate::common::*;
use crate::export::{dir, files, peak_db, read};

#[test]
fn the_demo_project_exports_through_pisynth() {
    let project = demo_project();
    let program = compile::build(&project, None, &NoEnv);
    let mut rig = Rig::new(RATE, MAX_BLOCK);
    for (device, slot) in compile::slot_changes(None, &program).added {
        let model = project
            .tracks
            .iter()
            .flat_map(|t| &t.channel.devices)
            .find(|d| d.id == device)
            .unwrap();
        if model.role == DeviceRole::Instrument {
            let mut synth =
                PisynthProcessor::from_state(model.state.as_deref()).unwrap_or_default();
            synth.activate(RATE, MAX_BLOCK).unwrap();
            rig.send(Command::InsertProcessor {
                slot: SlotId(slot.0),
                processor: Box::new(synth),
            });
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
    let path = dir.join("Nachtfahrt.wav");
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
    assert_eq!(files(&dir), ["Nachtfahrt.wav"]);
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
    // Music in the bar, decaying releases in the tail.
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
