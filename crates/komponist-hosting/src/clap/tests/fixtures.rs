//! Tests against the WP-27 fixtures `kp-test-synth` and `kp-test-fx` (plus scans of the others).

use komponist_ipc::{GuiSize, PluginKind, RenderMode, WireEventKind};

use super::*;
use crate::clap::events::{NO_NOTE_ID, encode};
use crate::host_loop::notification_bits as bits;
use crate::plugin::HostingError;

const SYNTH: &str = "de.klang-studio.kp-test-synth";
const FX: &str = "de.klang-studio.kp-test-fx";

fn note_on(offset: u32, channel: u8, key: u8) -> WireEvent {
    encode(
        offset,
        WireEventKind::NoteOn {
            channel,
            key,
            velocity: 127,
            note_id: NO_NOTE_ID,
        },
    )
}

fn note_off(offset: u32, channel: u8, key: u8) -> WireEvent {
    encode(
        offset,
        WireEventKind::NoteOff {
            channel,
            key,
            note_id: NO_NOTE_ID,
        },
    )
}

fn param(offset: u32, id: u32, value: f64) -> WireEvent {
    encode(offset, WireEventKind::ParamValue { id, value })
}

#[test]
fn scans_the_fixtures() {
    for (name, kind) in [
        ("kp-test-synth", Some(PluginKind::Instrument)),
        ("kp-test-fx", Some(PluginKind::Effect)),
        ("kp-test-crash", None),
        ("kp-test-slow", None),
    ] {
        let Some(bundle) = fixture(name) else {
            continue;
        };
        let found = crate::scan(&bundle).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let d = &found[0];
        assert_eq!(d.format, PluginFormat::Clap);
        assert_eq!(d.id, format!("de.klang-studio.{name}"));
        assert_eq!(d.name, name);
        assert_eq!(d.vendor, "klang-studio");
        assert!(!d.version.is_empty());
        assert!(d.bundle.is_absolute());
        assert!(d.features.iter().all(|f| *f == f.to_lowercase()));
        if let Some(kind) = kind {
            assert_eq!(d.kind, kind, "{d:?}");
        }
    }
    let Some(synth) = fixture("kp-test-synth") else {
        return;
    };
    let features = &crate::scan(&synth).unwrap()[0].features;
    assert!(features.contains(&"instrument".to_string()), "{features:?}");
}

#[test]
fn load_errors() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let host_loop = PollLoop::new(false).unwrap();
    let err = |r: Result<Box<dyn HostedPlugin>, HostingError>| r.err().unwrap();
    assert!(matches!(
        err(crate::load(
            PluginFormat::Clap,
            &bundle,
            "no.such.plugin",
            host_loop.context()
        )),
        HostingError::Load(_)
    ));
    assert!(matches!(
        err(crate::load(
            PluginFormat::Clap,
            Path::new("/nonexistent/X.clap"),
            SYNTH,
            host_loop.context()
        )),
        HostingError::Load(_)
    ));
    assert!(matches!(
        err(crate::load(
            PluginFormat::Vst3,
            &bundle,
            SYNTH,
            host_loop.context()
        )),
        HostingError::Unsupported(_)
    ));
}

#[test]
fn synth_plays_notes() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut rig = load(&bundle, SYNTH);
    let plugin = rig.plugin.as_mut();
    assert_eq!(plugin.descriptor().kind, PluginKind::Instrument);
    assert!(plugin.has_gui());
    assert!(!plugin.is_active());

    let mut processor = plugin.activate(CONFIG).unwrap();
    assert!(plugin.is_active());
    assert_eq!(plugin.latency(), 0);
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();

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
    for _ in 0..4 {
        blocks.run(processor.as_mut(), &[]);
    }
    assert!(blocks.peak(0..BLOCK) < 1e-4, "{}", blocks.peak(0..BLOCK));

    processor.stop_processing();
    plugin.deactivate(processor);
    assert!(!plugin.is_active());
    assert_eq!(plugin.latency(), 0);
}

#[test]
fn midi_messages_and_all_notes_off() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut rig = load(&bundle, SYNTH);
    let mut processor = rig.plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();

    // A raw MIDI note on (channel 4) sounds; a raw note off ends it.
    blocks.run(
        processor.as_mut(),
        &[encode(0, WireEventKind::Midi([0x93, 60, 100]))],
    );
    assert!(blocks.peak(0..BLOCK) > 0.1);
    blocks.run(
        processor.as_mut(),
        &[encode(0, WireEventKind::Midi([0x83, 60, 0]))],
    );
    for _ in 0..4 {
        blocks.run(processor.as_mut(), &[]);
    }
    assert!(blocks.peak(0..BLOCK) < 1e-4);

    // Notes on two channels, then all-notes-off: the layer releases each held note.
    blocks.run(
        processor.as_mut(),
        &[note_on(0, 0, 60), note_on(0, 9, 64), note_on(10, 0, 67)],
    );
    assert!(blocks.peak(0..BLOCK) > 0.1);
    blocks.run(processor.as_mut(), &[encode(0, WireEventKind::AllNotesOff)]);
    for _ in 0..4 {
        blocks.run(processor.as_mut(), &[]);
    }
    assert!(blocks.peak(0..BLOCK) < 1e-4);

    // reset() clears a sounding voice at once.
    blocks.run(processor.as_mut(), &[note_on(0, 0, 72)]);
    assert!(blocks.peak(0..BLOCK) > 0.1);
    processor.reset();
    blocks.run(processor.as_mut(), &[]);
    assert_eq!(blocks.peak(0..BLOCK), 0.0);

    processor.stop_processing();
    rig.plugin.deactivate(processor);
}

#[test]
fn param_events_change_the_sound() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut rig = load(&bundle, SYNTH);
    let plugin = rig.plugin.as_mut();
    let params = plugin.params();
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Gain", "Decay"]);
    let gain = param_id(plugin, "Gain");
    let info = params.iter().find(|p| p.id == gain).unwrap();
    // nice-plug exposes continuous parameters normalized.
    assert_eq!((info.min, info.max), (0.0, 1.0));
    assert!((info.default - 0.8).abs() < 1e-6, "{info:?}");
    assert!((plugin.param_value(gain).unwrap() - 0.8).abs() < 1e-6);
    assert_eq!(plugin.param_value(12345), None);
    let text = plugin.param_text(gain, 0.0).unwrap();
    assert!(text.contains("-60") && text.contains("dB"), "{text}");

    let mut processor = plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 69)]);
    let loud = blocks.peak(0..BLOCK);
    // Gain to the bottom of its range (−60 dB) from this block on.
    plugin.set_param(gain, 0.0);
    blocks.run(processor.as_mut(), &[param(0, gain, 0.0)]);
    let quiet = blocks.peak(0..BLOCK);
    assert!(quiet < loud * 0.01, "loud {loud}, quiet {quiet}");
    assert!(plugin.param_value(gain).unwrap() < 1e-6);
    // Unknown and invalid parameter ids are ignored.
    blocks.run(
        processor.as_mut(),
        &[param(0, 777, 1.0), param(0, u32::MAX, 1.0)],
    );
    processor.stop_processing();
    plugin.deactivate(processor);
}

#[test]
fn flush_and_state_round_trip() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut rig = load(&bundle, SYNTH);
    let plugin = rig.plugin.as_mut();
    let gain = param_id(plugin, "Gain");
    let decay = param_id(plugin, "Decay");

    let mut out = Vec::new();
    plugin.flush_params(&[param(0, gain, 0.5), param(0, decay, 0.25)], &mut out);
    assert!((plugin.param_value(gain).unwrap() - 0.5).abs() < 1e-6);
    assert!((plugin.param_value(decay).unwrap() - 0.25).abs() < 1e-6);

    let blob = plugin.save_state().unwrap();
    assert!(!blob.is_empty());
    plugin.flush_params(&[param(0, gain, 1.0)], &mut out);
    assert!((plugin.param_value(gain).unwrap() - 1.0).abs() < 1e-6);
    plugin.load_state(&blob).unwrap();
    assert!((plugin.param_value(gain).unwrap() - 0.5).abs() < 1e-6);

    // Into a fresh instance, as on project load or after a crash.
    let mut other = load(&bundle, SYNTH);
    other.plugin.load_state(&blob).unwrap();
    assert!((other.plugin.param_value(gain).unwrap() - 0.5).abs() < 1e-6);
    assert!((other.plugin.param_value(decay).unwrap() - 0.25).abs() < 1e-6);
    assert!(matches!(
        other.plugin.load_state(b"not a state"),
        Err(HostingError::State(_))
    ));

    // State loads while active too, and takes effect in the next block.
    let mut processor = other.plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    other.plugin.load_state(&blob).unwrap();
    let mut blocks = Blocks::new();
    blocks.run(processor.as_mut(), &[note_on(0, 0, 69)]);
    // −30 dB at gain 0.5 of −60…0 dB.
    let level = blocks.peak(0..BLOCK);
    assert!((0.025..0.04).contains(&level), "level {level}");
    processor.stop_processing();
    other.plugin.deactivate(processor);
}

#[test]
fn fx_reports_and_applies_latency() {
    let Some(bundle) = fixture("kp-test-fx") else {
        return;
    };
    let mut rig = load(&bundle, FX);
    let plugin = rig.plugin.as_mut();
    assert_eq!(plugin.descriptor().kind, PluginKind::Effect);
    assert!(!plugin.has_gui());
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

    // A rescan request or latency notification may have arrived; the loop records it.
    rig.host_loop
        .run_once(rig.plugin.as_mut(), std::time::Duration::ZERO);
    let observed = rig.host_loop.take_observed();
    assert!(!observed.has(bits::GUI_CLOSED), "{observed:?}");
}

#[test]
fn processor_runs_on_its_own_thread() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut rig = load(&bundle, SYNTH);
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
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut rig = load(&bundle, SYNTH);
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
    // A processor dropped without stop/deactivate is stopped on drop; deactivate still works.
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
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let rig = load(&bundle, SYNTH);
    let host_loop = rig.host_loop.clone();
    let mut plugin = rig.plugin;
    let processor = plugin.activate(CONFIG).unwrap();
    drop(processor);
    drop(plugin);
    assert_eq!(host_loop.registrations(), (0, 0));
}
