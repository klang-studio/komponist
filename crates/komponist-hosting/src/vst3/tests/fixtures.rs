//! Tests against the VST3 builds of the WP-27 fixtures `kp-test-synth` and `kp-test-fx` (plus
//! scans of the others).

use komponist_ipc::shm::TRANSPORT_PLAYING;
use komponist_ipc::{GuiSize, PluginKind, RenderMode, WireEvent, WireEventKind, WireTransport};

use super::*;
use crate::host_loop::notification_bits as bits;
use crate::plugin::{ActivateConfig, HostingError};

const SYNTH_CID: &[u8; 16] = b"kpTestSynth00001";
const FX_CID: &[u8; 16] = b"kpTestFx00000001";

fn synth() -> Option<(PathBuf, String)> {
    fixture("kp-test-synth").map(|b| (b, fixture_id(SYNTH_CID)))
}

fn note_on(offset: u32, channel: u8, key: u8) -> WireEvent {
    WireEvent::new(
        offset,
        WireEventKind::NoteOn {
            channel,
            key,
            velocity: 127,
            note_id: u32::MAX,
        },
    )
}

fn note_off(offset: u32, channel: u8, key: u8) -> WireEvent {
    WireEvent::new(
        offset,
        WireEventKind::NoteOff {
            channel,
            key,
            note_id: u32::MAX,
        },
    )
}

fn param(offset: u32, id: u32, value: f64) -> WireEvent {
    WireEvent::new(offset, WireEventKind::ParamValue { id, value })
}

#[test]
fn scans_the_fixtures() {
    for (name, cid, kind) in [
        ("kp-test-synth", SYNTH_CID, PluginKind::Instrument),
        ("kp-test-fx", FX_CID, PluginKind::Effect),
        ("kp-test-crash", b"kpTestCrash00001", PluginKind::Effect),
        ("kp-test-slow", b"kpTestSlow000001", PluginKind::Effect),
    ] {
        let Some(bundle) = fixture(name) else {
            continue;
        };
        let found = crate::scan(&bundle).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let d = &found[0];
        assert_eq!(d.format, PluginFormat::Vst3);
        assert_eq!(d.id, fixture_id(cid));
        assert_eq!(d.name, name);
        assert_eq!(d.vendor, "klang-studio");
        assert!(!d.version.is_empty());
        assert_eq!(d.kind, kind, "{d:?}");
        assert!(d.bundle.is_absolute());
        assert!(d.features.iter().all(|f| *f == f.to_lowercase()));
        if name == "kp-test-synth" {
            assert_eq!(d.features, ["instrument", "synth"]);
            eprintln!("scan {name}: {d:?}");
        }
    }
}

#[test]
fn load_errors() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let host_loop = PollLoop::new(false).unwrap();
    let err = |r: Result<Box<dyn HostedPlugin>, HostingError>| r.err().unwrap();
    let load = |bundle: &Path, id: &str| {
        err(crate::load(
            PluginFormat::Vst3,
            bundle,
            id,
            host_loop.context(),
        ))
    };
    for bad_id in ["de.klang-studio.kp-test-synth", "", &"0".repeat(32)] {
        assert!(
            matches!(load(&bundle, bad_id), HostingError::Load(_)),
            "{bad_id:?}"
        );
    }
    assert!(matches!(
        load(Path::new("/nonexistent/X.vst3"), &id),
        HostingError::Load(_)
    ));
    // A file that dlopen refuses (this test executable), and the CLAP id of the fixture.
    let exe = std::env::current_exe().unwrap();
    assert!(matches!(load(&exe, &id), HostingError::Load(_)));
    assert!(matches!(
        load(&bundle, "de.klang-studio.kp-test-synth"),
        HostingError::Load(_)
    ));
    // The failed loads left nothing behind; a good load still works.
    assert_eq!(host_loop.registrations(), (0, 0));
    assert!(crate::load(PluginFormat::Vst3, &bundle, &id, host_loop.context()).is_ok());
}

#[test]
fn synth_plays_notes() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let mut rig = load(&bundle, &id);
    let plugin = rig.plugin.as_mut();
    assert_eq!(plugin.descriptor().kind, PluginKind::Instrument);
    assert_eq!(plugin.descriptor().id, id);
    assert!(plugin.has_gui());
    assert!(!plugin.is_active());

    let mut processor = plugin.activate(CONFIG).unwrap();
    assert!(plugin.is_active());
    assert_eq!(plugin.latency(), 0);
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    blocks.transport = WireTransport {
        flags: TRANSPORT_PLAYING,
        meter_num: 4,
        meter_den: 4,
        tempo_bpm: 120.0,
        ..WireTransport::default()
    };

    // Silence before any note.
    assert_eq!(blocks.run(processor.as_mut(), &[]), ProcessStatus::Continue);
    assert_eq!(blocks.peak(0..BLOCK), 0.0);

    // A note at frame 100: silent before, a sine (velocity 1 × −12 dB ≈ 0.25) after, L = R.
    blocks.run(processor.as_mut(), &[note_on(100, 0, 69)]);
    assert!(!blocks.has_nan());
    assert_eq!(blocks.peak(0..100), 0.0);
    let level = blocks.peak(100..BLOCK);
    assert!((0.2..0.3).contains(&level), "level {level}");
    assert_eq!(blocks.output[0], blocks.output[1]);

    // Note off: the 10 ms release ends within a few blocks.
    blocks.run(processor.as_mut(), &[note_off(0, 0, 69)]);
    run_silent(&mut blocks, processor.as_mut(), 4);
    assert!(blocks.peak(0..BLOCK) < 1e-4, "{}", blocks.peak(0..BLOCK));

    processor.stop_processing();
    plugin.deactivate(processor);
    assert!(!plugin.is_active());
    assert_eq!(plugin.latency(), 0);
}

#[test]
fn midi_messages_and_all_notes_off() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let mut rig = load(&bundle, &id);
    let mut processor = rig.plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    let midi = |data| [WireEvent::new(0, WireEventKind::Midi(data))];

    // A raw MIDI note on (channel 4) sounds; a raw note off ends it.
    blocks.run(processor.as_mut(), &midi([0x93, 60, 100]));
    assert!(blocks.peak(0..BLOCK) > 0.1);
    blocks.run(processor.as_mut(), &midi([0x83, 60, 0]));
    run_silent(&mut blocks, processor.as_mut(), 4);
    assert!(blocks.peak(0..BLOCK) < 1e-4);

    // Note on with velocity 0 is a note off.
    blocks.run(processor.as_mut(), &midi([0x90, 62, 100]));
    assert!(blocks.peak(0..BLOCK) > 0.1);
    blocks.run(processor.as_mut(), &midi([0x90, 62, 0]));
    run_silent(&mut blocks, processor.as_mut(), 4);
    assert!(blocks.peak(0..BLOCK) < 1e-4);

    // Notes on two channels, then all-notes-off: the layer releases each held note.
    blocks.run(
        processor.as_mut(),
        &[note_on(0, 0, 60), note_on(0, 9, 64), note_on(10, 0, 67)],
    );
    assert!(blocks.peak(0..BLOCK) > 0.1);
    blocks.run(
        processor.as_mut(),
        &[WireEvent::new(0, WireEventKind::AllNotesOff)],
    );
    run_silent(&mut blocks, processor.as_mut(), 4);
    assert!(blocks.peak(0..BLOCK) < 1e-4);

    // CC 123 (all notes off) without a MIDI mapping does the same.
    blocks.run(processor.as_mut(), &[note_on(0, 2, 70)]);
    assert!(blocks.peak(0..BLOCK) > 0.1);
    blocks.run(processor.as_mut(), &midi([0xB2, 123, 0]));
    run_silent(&mut blocks, processor.as_mut(), 4);
    assert!(blocks.peak(0..BLOCK) < 1e-4);

    // reset() clears a sounding voice at once (setProcessing off/on resets nice-plug plugins).
    blocks.run(processor.as_mut(), &[note_on(0, 0, 72)]);
    assert!(blocks.peak(0..BLOCK) > 0.1);
    processor.reset();
    blocks.run(processor.as_mut(), &[]);
    assert_eq!(blocks.peak(0..BLOCK), 0.0);

    // Out-of-range channels and keys (the wire format does not check them) are dropped, and so
    // are unmapped controllers, pitch bend and pressure.
    blocks.run(
        processor.as_mut(),
        &[
            note_on(0, 200, 60),
            note_on(0, 0, 200),
            note_off(0, 16, 60),
            WireEvent::new(0, WireEventKind::Midi([0xE0, 0, 64])),
            WireEvent::new(0, WireEventKind::Midi([0xB0, 1, 64])),
            WireEvent::new(0, WireEventKind::Midi([0xD0, 64, 0])),
            WireEvent::new(0, WireEventKind::Midi([0xA0, 60, 64])),
        ],
    );
    assert_eq!(blocks.peak(0..BLOCK), 0.0);

    processor.stop_processing();
    rig.plugin.deactivate(processor);
}

#[test]
fn params_are_normalized_and_change_the_sound() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let mut rig = load(&bundle, &id);
    let plugin = rig.plugin.as_mut();
    let params = plugin.params();
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Gain", "Decay"]);
    let gain = param_id(plugin, "Gain");
    let info = params.iter().find(|p| p.id == gain).unwrap();
    assert_eq!((info.min, info.max, info.step_count), (0.0, 1.0, 0));
    assert!((info.default - 0.8).abs() < 1e-6, "{info:?}");
    assert!(info.flags & komponist_ipc::control::param_flags::AUTOMATABLE != 0);
    assert!((plugin.param_value(gain).unwrap() - 0.8).abs() < 1e-6);
    assert_eq!(plugin.param_value(12345), None);
    assert_eq!(plugin.param_text(12345, 0.5), None);
    let text = plugin.param_text(gain, 0.0).unwrap();
    assert!(text.contains("-60") && text.contains("dB"), "{text}");
    assert_eq!(text.matches("dB").count(), 1, "unit doubled: {text}");

    let mut processor = plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 69)]);
    let loud = blocks.peak(0..BLOCK);
    // Gain to the bottom of its range (−60 dB) from this block on, as the bridge sends it.
    plugin.set_param(gain, 0.0);
    blocks.run(processor.as_mut(), &[param(0, gain, 0.0)]);
    let quiet = blocks.peak(0..BLOCK);
    assert!(quiet < loud * 0.01, "loud {loud}, quiet {quiet}");
    assert!(plugin.param_value(gain).unwrap() < 1e-6);
    // A change in the middle of a block applies from the next one (no sample-accurate
    // automation in the fixture), values outside 0..1 are clamped, unknown ids are ignored.
    blocks.run(
        processor.as_mut(),
        &[
            param(128, gain, 7.0),
            param(0, 777, 1.0),
            param(0, u32::MAX, 1.0),
        ],
    );
    blocks.run(processor.as_mut(), &[]);
    assert!(
        blocks.peak(0..BLOCK) > loud * 0.9,
        "{}",
        blocks.peak(0..BLOCK)
    );
    processor.stop_processing();
    plugin.deactivate(processor);
}

#[test]
fn values_flushed_while_inactive_reach_the_processor() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let mut rig = load(&bundle, &id);
    let plugin = rig.plugin.as_mut();
    let gain = param_id(plugin, "Gain");
    let mut out = Vec::new();
    plugin.flush_params(&[param(0, gain, 0.0)], &mut out);
    assert!(out.is_empty(), "VST3 has no flush output");
    assert!(plugin.param_value(gain).unwrap() < 1e-6);

    let mut processor = plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 69)]);
    let level = blocks.peak(0..BLOCK);
    assert!(level < 0.01, "level {level}");
    processor.stop_processing();
    plugin.deactivate(processor);
}

#[test]
fn state_round_trip() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let mut rig = load(&bundle, &id);
    let plugin = rig.plugin.as_mut();
    let gain = param_id(plugin, "Gain");
    let decay = param_id(plugin, "Decay");

    let mut out = Vec::new();
    plugin.flush_params(&[param(0, gain, 0.5), param(0, decay, 0.25)], &mut out);
    assert!((plugin.param_value(gain).unwrap() - 0.5).abs() < 1e-6);
    assert!((plugin.param_value(decay).unwrap() - 0.25).abs() < 1e-6);

    let blob = plugin.save_state().unwrap();
    assert_eq!(&blob[..4], b"KPV3");
    plugin.flush_params(&[param(0, gain, 1.0)], &mut out);
    assert!((plugin.param_value(gain).unwrap() - 1.0).abs() < 1e-6);
    plugin.load_state(&blob).unwrap();
    assert!((plugin.param_value(gain).unwrap() - 0.5).abs() < 1e-6);

    // Into a fresh instance, as on project load or after a crash.
    let mut other = load(&bundle, &id);
    other.plugin.load_state(&blob).unwrap();
    assert!((other.plugin.param_value(gain).unwrap() - 0.5).abs() < 1e-6);
    assert!((other.plugin.param_value(decay).unwrap() - 0.25).abs() < 1e-6);
    assert!(matches!(
        other.plugin.load_state(b"not a state"),
        Err(HostingError::State(_))
    ));
    // Another plugin's state is refused before the plugin sees it.
    if let Some(fx) = fixture("kp-test-fx") {
        let fx_state = load(&fx, &fixture_id(FX_CID)).plugin.save_state().unwrap();
        let err = other.plugin.load_state(&fx_state).unwrap_err();
        assert!(
            matches!(&err, HostingError::State(m) if m.contains("belongs")),
            "{err}"
        );
    }

    // State loads while active too, and takes effect in the next block.
    let mut processor = other.plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    other.plugin.load_state(&blob).unwrap();
    let mut blocks = Blocks::new();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 69)]);
    // −30 dB at gain 0.5 of −60…0 dB.
    let level = blocks.peak(0..BLOCK);
    assert!((0.025..0.04).contains(&level), "level {level}");
    // A state saved while active round-trips too.
    let active_blob = other.plugin.save_state().unwrap();
    assert_eq!(active_blob, blob);
    processor.stop_processing();
    other.plugin.deactivate(processor);
}

#[test]
fn fx_reports_and_applies_latency() {
    let Some(bundle) = fixture("kp-test-fx") else {
        return;
    };
    let mut rig = load(&bundle, &fixture_id(FX_CID));
    let plugin = rig.plugin.as_mut();
    assert_eq!(plugin.descriptor().kind, PluginKind::Effect);
    assert!(!plugin.has_gui());
    assert!(matches!(plugin.gui_open(1, 1.0), Err(HostingError::Gui(_))));
    assert_eq!(plugin.latency(), 0, "inactive");

    let mut processor = plugin.activate(CONFIG).unwrap();
    assert_eq!(plugin.latency(), 64);
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    blocks.input[0][10] = 1.0;
    blocks.input[1][20] = -0.5;
    blocks.run(processor.as_mut(), &[]);
    let (l, r) = (&blocks.output[0], &blocks.output[1]);
    assert_eq!(l[74], 1.0);
    assert_eq!(r[84], -0.5);
    assert_eq!(l.iter().filter(|s| **s != 0.0).count(), 1);
    assert_eq!(r.iter().filter(|s| **s != 0.0).count(), 1);
    processor.stop_processing();
    plugin.deactivate(processor);

    rig.host_loop
        .run_once(rig.plugin.as_mut(), std::time::Duration::ZERO);
    let observed = rig.host_loop.take_observed();
    assert!(!observed.has(bits::GUI_CLOSED), "{observed:?}");
    eprintln!(
        "kp-test-fx: notifications after activation {:#x} (latency changed: {})",
        observed.notifications,
        observed.has(bits::LATENCY_CHANGED)
    );
}

#[test]
fn processor_runs_on_its_own_thread() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let mut rig = load(&bundle, &id);
    let processor = rig.plugin.activate(CONFIG).unwrap();
    // The bridge moves the processor to its RT thread and hands it back for deactivation.
    let (processor, level) = std::thread::spawn(move || {
        let mut processor = processor;
        processor.start_processing().unwrap();
        let mut blocks = Blocks::new();
        blocks.run(processor.as_mut(), &[note_on(0, 0, 60)]);
        let level = blocks.peak(0..BLOCK);
        processor.stop_processing();
        (processor, level)
    })
    .join()
    .unwrap();
    assert!(level > 0.1);
    rig.plugin.deactivate(processor);
    assert!(!rig.plugin.is_active());
}

#[test]
fn lifecycle_rules() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let mut rig = load(&bundle, &id);
    let plugin = rig.plugin.as_mut();
    for bad in [
        ActivateConfig {
            min_frames: 0,
            ..CONFIG
        },
        ActivateConfig {
            min_frames: 300,
            ..CONFIG
        },
        ActivateConfig {
            max_frames: 100_000,
            ..CONFIG
        },
        ActivateConfig {
            sample_rate: 0.0,
            ..CONFIG
        },
    ] {
        assert!(matches!(
            plugin.activate(bad),
            Err(HostingError::Activate(_))
        ));
    }
    assert!(plugin.set_render_mode(RenderMode::Offline).is_ok());
    let mut processor = plugin.activate(CONFIG).unwrap();
    assert!(matches!(
        plugin.activate(CONFIG),
        Err(HostingError::WrongState(_))
    ));
    assert!(matches!(
        plugin.set_render_mode(RenderMode::Realtime),
        Err(HostingError::WrongState(_))
    ));
    // Offline rendering produces the same sound.
    processor.start_processing().unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 60)]);
    assert!(blocks.peak(0..BLOCK) > 0.1);
    processor.stop_processing();
    // Stopped: process refuses instead of calling the plugin.
    assert_eq!(blocks.run(processor.as_mut(), &[]), ProcessStatus::Error);
    plugin.deactivate(processor);

    // Re-activation at another rate works.
    plugin.set_render_mode(RenderMode::Realtime).unwrap();
    let mut processor = plugin
        .activate(ActivateConfig {
            sample_rate: 44_100.0,
            ..CONFIG
        })
        .unwrap();
    processor.start_processing().unwrap();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 60)]);
    assert!(blocks.peak(0..BLOCK) > 0.1);
    // A processor handed back while still started is stopped on drop; deactivate still works.
    plugin.deactivate(processor);
    assert!(!plugin.is_active());

    // GUI calls without an open GUI.
    assert!(matches!(
        plugin.gui_set_size(GuiSize {
            width: 10,
            height: 10
        }),
        Err(HostingError::WrongState(_))
    ));
    plugin.gui_close();
    assert!(plugin.note_names().is_empty());
}

#[test]
fn dropping_an_active_plugin_cleans_up() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    let rig = load(&bundle, &id);
    let host_loop = rig.host_loop.clone();
    let mut plugin = rig.plugin;
    let processor = plugin.activate(CONFIG).unwrap();
    drop(processor);
    drop(plugin);
    assert_eq!(host_loop.registrations(), (0, 0));
}

#[test]
fn instances_share_the_module_and_reload_after_unload() {
    let Some((bundle, id)) = synth() else {
        return;
    };
    // Two instances of one bundle; the second keeps playing after the first is gone.
    let first = load(&bundle, &id);
    let mut second = load(&bundle, &id);
    drop(first);
    let mut processor = second.plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 60)]);
    assert!(blocks.peak(0..BLOCK) > 0.1);
    processor.stop_processing();
    second.plugin.deactivate(processor);
    drop(second);
    // Unloaded (ModuleExit, dlclose) and loaded again (dlopen, ModuleEntry).
    for _ in 0..3 {
        let rig = load(&bundle, &id);
        assert_eq!(rig.plugin.params().len(), 2);
    }
}
