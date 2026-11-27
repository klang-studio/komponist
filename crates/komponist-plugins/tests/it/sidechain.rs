//! #6: bridged plugins with a sidechain port get the device's sidechain. `kp-test-sc` outputs its
//! input plus its sidechain, so the tests see exactly what reached the port: through the engine
//! the same samples, at the same frames, as an in-process device gets on the same path (the one
//! internal devices such as presse use, `Processor::process_sidechain`), and silence without a
//! source. Every engine and processor call runs inside `assert_no_alloc`.

use crate::common;

use std::time::{Duration, Instant};

use assert_no_alloc::assert_no_alloc;
use common::*;
use komponist_engine::testing::{TestSidechain, TestSidechainMode, TestSine};
use komponist_engine::{
    Command as EngineCommand, ProcessContext, ProcessStatus, StereoIo, TransportInfo,
};
use komponist_model::{
    DeviceId, DeviceRole, Meter, PluginFormat, PluginRef, Sidechain, Tick, TickRange, TrackId,
};
use komponist_plugins::DeviceStatus;

pub const SC: &str = "de.klang-studio.kp-test-sc";

/// The reference of an in-process device whose processor the test inserts itself.
fn in_process() -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: "fixture.probe".into(),
        name: "probe".into(),
        vendor: String::new(),
        version: String::new(),
        module: Box::default(),
    }
}

/// What the second track holds.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Holder {
    /// `kp-test-sc` in a bridge, its sidechain from track 1 (pre); track 1 muted.
    Bridged,
    /// The same, with an in-process `TestSidechain` (outputs its sidechain) in the device's slot.
    InProcess,
    /// No second track; track 1 plays itself.
    Nothing,
}

/// Track 1: a `TestSine` (device 10) and the bridged `kp-test-fx` (device 11, 64 frames of
/// latency, so the sidechain path has delay compensation to get right). Track 2 per `holder`:
/// device 20 on its own, so its main input is silent. Renders a note offline; the output of the
/// rendered range, or `None` without fixtures.
fn play(holder: Holder) -> Option<(Vec<f32>, Vec<f32>)> {
    let dir = scratch(&format!("sidechain-{holder:?}"));
    let manager = scanned_manager(&dir)?;
    let mut source = track(
        1,
        vec![
            device(10, DeviceRole::Instrument, in_process(), None),
            device(11, DeviceRole::Effect, clap_plugin(FX), None),
        ],
    );
    let mut tracks = Vec::new();
    if holder != Holder::Nothing {
        // A pre tap ignores the source's mute: the master hears only the holder.
        source.channel.mute = true;
        let mut sc = device(20, DeviceRole::Effect, clap_plugin(SC), None);
        sc.sidechain = Some(Sidechain::new(TrackId(1)));
        tracks.push(source);
        tracks.push(track(2, vec![sc]));
    } else {
        tracks.push(source);
    }
    let mut rig = Rig::new(manager, project(tracks));
    let slot = rig.slots[&DeviceId(10)];
    rig.send(EngineCommand::InsertProcessor {
        slot,
        processor: Box::new(TestSine::new()),
    });
    if holder == Holder::InProcess {
        let slot = rig.slots[&DeviceId(20)];
        rig.send(EngineCommand::InsertProcessor {
            slot,
            processor: Box::new(TestSidechain::new(TestSidechainMode::Replace)),
        });
    }
    rig.wait_status(11, DeviceStatus::Ok);
    if holder == Holder::Bridged {
        rig.wait_status(20, DeviceStatus::Ok);
        assert!(rig.manager.has_sidechain(DeviceId(20)));
    }
    rig.recompile();
    rig.render(4 * BLOCK);
    rig.note(1, 69, 127);
    let range = rig.render(12 * BLOCK);
    rig.note(1, 69, 0);
    rig.render(4 * BLOCK);
    assert!(rig.engine_events.is_empty(), "{:?}", rig.engine_events);
    Some((rig.l[range.clone()].to_vec(), rig.r[range].to_vec()))
}

/// The bridged plugin gets the source's signal sample-accurately: bit for bit what an in-process
/// device gets on the same sidechain path, and what the source itself sends to the master.
#[test]
fn a_bridged_plugin_gets_the_sidechain_aligned_like_an_internal_device() {
    let Some(bridged) = play(Holder::Bridged) else {
        return;
    };
    let in_process = play(Holder::InProcess).expect("fixtures found once");
    let source = play(Holder::Nothing).expect("fixtures found once");
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    let peak = bridged.0.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.1, "silent: {peak}");
    // The note starts after the effect's 64 frames, in all three.
    let onset = |v: &[f32]| v.iter().position(|s| *s != 0.0);
    assert_eq!(onset(&bridged.0), Some(64));
    assert_eq!(onset(&in_process.0), Some(64));
    assert_eq!(bits(&bridged.0), bits(&in_process.0), "left");
    assert_eq!(bits(&bridged.1), bits(&in_process.1), "right");
    assert_eq!(bits(&bridged.0), bits(&source.0), "left vs the source");
    assert_eq!(bits(&bridged.1), bits(&source.1), "right vs the source");
}

/// A context for direct processor calls.
fn context(frames: usize) -> ProcessContext {
    ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick(0), Tick(3840)),
        },
        offline: true,
        deadline: None,
    }
}

/// Without a source the plugin's port gets silence (also right after blocks that had one), with
/// one exactly the sidechain; `has_sidechain` says which devices have the port, from the catalog
/// before the bridge loaded and from the plugin after.
#[test]
fn the_port_is_silent_without_a_source_and_only_sc_plugins_have_one() {
    let dir = scratch("sidechain-direct");
    let Some(mut manager) = scanned_manager(&dir) else {
        return;
    };
    let catalog = manager.catalog();
    assert!(catalog.resolve(&clap_plugin(SC)).unwrap().sidechain);
    for plain in [SYNTH, FX, CRASH, SLOW] {
        assert!(!catalog.resolve(&clap_plugin(plain)).unwrap().sidechain);
    }
    let sc = device(30, DeviceRole::Effect, clap_plugin(SC), None);
    let fx = device(31, DeviceRole::Effect, clap_plugin(FX), None);
    let mut processor = manager.create_processor(&sc, SETUP).unwrap();
    let _fx = manager.create_processor(&fx, SETUP).unwrap();
    // Before loading: from the catalog.
    assert!(manager.has_sidechain(DeviceId(30)));
    assert!(!manager.has_sidechain(DeviceId(31)));
    assert!(!manager.has_sidechain(DeviceId(99)));
    let deadline = Instant::now() + PATIENCE;
    while manager.status(DeviceId(30)) != DeviceStatus::Ok
        || manager.status(DeviceId(31)) != DeviceStatus::Ok
    {
        assert!(Instant::now() < deadline, "not loaded");
        manager.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(manager.has_sidechain(DeviceId(30)));
    assert!(!manager.has_sidechain(DeviceId(31)));

    let wave = |step: f32, block: usize| -> Vec<f32> {
        (0..BLOCK)
            .map(|i| ((block * BLOCK + i) as f32 * step).sin() * 0.5)
            .collect()
    };
    let ctx = context(BLOCK);
    let mut output = [vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]];
    for (block, with) in [false, true, true, false, true, false]
        .into_iter()
        .enumerate()
    {
        let input = [wave(0.01, block), wave(0.02, block)];
        let side = [wave(0.03, block), wave(-0.05, block)];
        let [l, r] = &mut output;
        let mut io = StereoIo {
            input: [&input[0], &input[1]],
            output: [&mut l[..], &mut r[..]],
        };
        let status = assert_no_alloc(|| {
            if with {
                processor.process_sidechain(&ctx, &[], &mut io, [&side[0], &side[1]])
            } else {
                processor.process(&ctx, &[], &mut io)
            }
        });
        assert_eq!(status, ProcessStatus::Continue);
        for ch in 0..2 {
            let expected: Vec<f32> = if with {
                input[ch]
                    .iter()
                    .zip(&side[ch])
                    .map(|(a, b)| a + b)
                    .collect()
            } else {
                input[ch].clone()
            };
            assert_eq!(output[ch], expected, "block {block}, channel {ch}");
        }
    }
    assert_eq!(manager.late_blocks(DeviceId(30)), 0);
    manager.shutdown();
}
