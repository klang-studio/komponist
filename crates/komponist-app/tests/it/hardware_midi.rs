//! Hardware MIDI input through the real hub (midir, ALSA sequencer) into the app's engine: a
//! virtual output port stands in for the keyboard. Skipped where the ALSA sequencer is missing.
//!
//! The hub of these tests connects to the test's own virtual ports only
//! (`MidiInputHub::only_connect_to`), never to the real devices of the machine. The stream is the
//! null backend (silent). It gets its time base like the device backends: `Instant::now()` at the
//! start of the callback.

use std::time::{Duration, Instant};

use egui::Key;
use egui_kittest::Harness;
use komponist_app::device_host::BuiltinHost;
use komponist_app::engine_sync::EngineSync;
use komponist_app::{Action, App, EditAction};
use komponist_audio::{AudioError, HostKind, MidiInputHub, StreamConfig};
use komponist_model::edits::TrackProp;
use komponist_model::{MidiInputSel, TrackId};
use midir::MidiOutput;
use midir::os::unix::VirtualOutput;

use crate::acceptance_support::{ctrl, harness, key};

/// ALSA client name of the test's virtual ports.
const CLIENT: &str = "komponist-it";

fn null_config() -> StreamConfig {
    StreamConfig {
        host: HostKind::Null,
        ..StreamConfig::default()
    }
}

/// A virtual output port named `port` in client [`CLIENT`], or `None` without ALSA sequencer.
fn virtual_output(port: &str) -> Option<midir::MidiOutputConnection> {
    MidiInputHub::new().ok()?;
    Some(
        MidiOutput::new(CLIENT)
            .unwrap()
            .create_virtual(port)
            .unwrap(),
    )
}

/// The settings name of the virtual port `port` (see `MidiInputHub::ports`).
fn port_name(port: &str) -> String {
    format!("{CLIENT}:{port}")
}

/// A unique port name per test and process.
fn unique(test: &str) -> String {
    format!("{test}-{}", std::process::id())
}

fn sync_on(config: &StreamConfig, port: &str) -> (EngineSync, Result<(), AudioError>) {
    let (mut hub, receiver) = MidiInputHub::new().expect("ALSA sequencer");
    hub.only_connect_to(&port_name(port));
    let _ = hub.refresh();
    EngineSync::with_receiver(config, Some(hub), receiver, Box::new(BuiltinHost::new()))
}

/// The whole app (UI loop included) on a hub that listens to the virtual port `port` only.
fn app_on(config: &StreamConfig, port: &str) -> App {
    let (sync, started) = sync_on(config, port);
    started.unwrap();
    let settings = komponist_app::settings::Settings {
        audio: config.clone(),
        ..Default::default()
    };
    App::new(
        komponist_model::Document::new(komponist_model::fixtures::empty_project()),
        sync,
        settings,
        None,
    )
}

fn step_for(h: &mut Harness<'_, App>, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
}

/// Steps the app until `track`'s meter shows signal; whether it did within `secs`.
fn signal_within(h: &mut Harness<'_, App>, track: TrackId, secs: u64) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        step_for(h, 20);
        if let Some(meter) = h.state().engine().track_meter(track) {
            let (l, r) = meter.take();
            if l.max(r) > 0.0 {
                return true;
            }
        }
    }
    false
}

/// A project with one monitoring pisynth track, played from `config`'s stream through `out`.
fn plays_after(config: &StreamConfig, restarts: usize) {
    let port = unique("plays");
    let Some(mut out) = virtual_output(&port) else {
        eprintln!("ALSA sequencer unavailable: skipped");
        return;
    };
    let mut app = app_on(config, &port);
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let track = app.project().tracks[0].id;
    for _ in 0..restarts {
        app.engine_mut().set_audio(config).unwrap();
    }
    let mut h = harness(app);
    step_for(&mut h, 300);
    h.state().engine().track_meter(track).map(|m| m.take());
    out.send(&[0x90, 60, 100]).unwrap();
    assert!(
        signal_within(&mut h, track, 3),
        "no signal from the virtual port"
    );
    out.send(&[0x80, 60, 0]).unwrap();
}

#[test]
fn a_virtual_port_plays_a_monitored_track() {
    plays_after(&null_config(), 0);
}

#[test]
fn midi_survives_a_stream_restart() {
    plays_after(&null_config(), 2);
}

/// Manual: the PipeWire host on a silent null sink. Create one first
/// (`pactl load-module module-null-sink sink_name=kp-test-null`), remove it afterwards.
#[test]
#[ignore = "needs PipeWire and a null sink named kp-test-null"]
fn pipewire_virtual_port_plays() {
    let config = StreamConfig {
        host: HostKind::PipeWire,
        device: Some("kp-test-null".to_owned()),
        ..StreamConfig::default()
    };
    plays_after(&config, 0);
    plays_after(&config, 2);
}

/// Monitoring while stopped, then recording a take, both from the virtual port.
#[test]
fn the_app_monitors_and_records_a_virtual_port() {
    use crate::acceptance_support::click;
    use egui::accesskit::Role;
    use komponist_app::panels::transport::names as tp;
    use komponist_engine::TransportState;
    use komponist_model::ClipContent;

    let port = unique("record");
    let Some(mut out) = virtual_output(&port) else {
        eprintln!("ALSA sequencer unavailable: skipped");
        return;
    };
    let mut h = harness(app_on(&null_config(), &port));
    ctrl(&mut h, Key::T);
    let track = h.state().project().tracks[0].id;
    assert!(h.state().project().tracks[0].arm);

    step_for(&mut h, 200);
    h.state().engine().track_meter(track).map(|m| m.take());
    out.send(&[0x90, 60, 100]).unwrap();
    assert!(signal_within(&mut h, track, 3), "monitoring: no signal");
    out.send(&[0x80, 60, 0]).unwrap();

    click(&mut h, Role::Button, tp::COUNT_IN);
    key(&mut h, Key::R);
    let until = Instant::now() + Duration::from_secs(10);
    while h.state().transport() != TransportState::Recording {
        assert!(Instant::now() < until, "not recording");
        step_for(&mut h, 10);
    }
    step_for(&mut h, 100);
    out.send(&[0x90, 64, 90]).unwrap();
    step_for(&mut h, 250);
    out.send(&[0x80, 64, 0]).unwrap();
    step_for(&mut h, 100);
    key(&mut h, Key::Space);
    let until = Instant::now() + Duration::from_secs(10);
    while h.state().transport() != TransportState::Stopped {
        assert!(Instant::now() < until, "not stopped");
        step_for(&mut h, 10);
    }
    let p = h.state().project();
    let ClipContent::Midi(m) = &p.tracks[0].clips[0].content else {
        panic!("no MIDI clip")
    };
    let pitches: Vec<u8> = m.active().notes.iter().map(|n| n.pitch).collect();
    assert_eq!(pitches, [64]);
}

/// Hot-plug: the keyboard appears after the app started, and the track names it as its input.
/// The track was compiled while the port was absent (it received nothing); the periodic scan
/// connects the port, and the project is compiled again so the name resolves.
#[test]
fn a_port_plugged_in_later_reaches_a_track_that_names_it() {
    let port = unique("hotplug");
    if MidiInputHub::new().is_err() {
        eprintln!("ALSA sequencer unavailable: skipped");
        return;
    }
    let mut h = harness(app_on(&null_config(), &port));
    ctrl(&mut h, Key::T);
    let track = h.state().project().tracks[0].id;
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![track],
            prop: TrackProp::Input(MidiInputSel::Device(port_name(&port))),
        }));
    step_for(&mut h, 200);
    assert!(
        h.state()
            .engine()
            .midi()
            .unwrap()
            .port_id(&port_name(&port))
            .is_none()
    );

    let mut out = virtual_output(&port).unwrap();
    // The scan runs every MIDI_SCAN_INTERVAL (2 s) from `poll`.
    let until = Instant::now() + Duration::from_secs(6);
    while h
        .state()
        .engine()
        .midi()
        .unwrap()
        .port_id(&port_name(&port))
        .is_none()
    {
        assert!(Instant::now() < until, "the port was never noticed");
        step_for(&mut h, 50);
    }
    step_for(&mut h, 200);
    h.state().engine().track_meter(track).map(|m| m.take());
    out.send(&[0x90, 60, 100]).unwrap();
    assert!(signal_within(&mut h, track, 3), "no signal after hot-plug");
    out.send(&[0x80, 60, 0]).unwrap();
}

/// Scans the MIDI ports and steps the app until the status bar shows text containing `needle`;
/// the text, or `None` after `secs`.
fn status_with(h: &mut Harness<'_, App>, needle: &str, secs: u64) -> Option<String> {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        h.state_mut().engine_mut().scan_midi();
        step_for(h, 50);
        if let Some(status) = h.state().ui().status.as_ref()
            && status.text.contains(needle)
        {
            return Some(status.text.clone());
        }
    }
    None
}

/// The status bar names a device that is plugged in or out, once per change (WP midi-notice).
#[test]
fn the_status_bar_tells_when_a_midi_device_appears_and_vanishes() {
    use komponist_app::ui_state::StatusKind;

    let port = unique("notice");
    if MidiInputHub::new().is_err() {
        eprintln!("ALSA sequencer unavailable: skipped");
        return;
    }
    let mut h = harness(app_on(&null_config(), &port));
    // The scan at start-up and a scan without a change say nothing.
    step_for(&mut h, 100);
    assert!(!h.state_mut().engine_mut().scan_midi());
    assert!(h.state_mut().engine_mut().take_midi_changes().is_empty());

    let out = virtual_output(&port).unwrap();
    let text = status_with(&mut h, "verbunden", 6).expect("no notice for the new port");
    assert_eq!(text, format!("{} verbunden", port_name(&port)));
    assert_eq!(
        h.state().ui().status.as_ref().unwrap().kind,
        StatusKind::Info
    );
    // Nothing changed since: no further change is reported.
    assert!(!h.state_mut().engine_mut().scan_midi());
    assert!(h.state_mut().engine_mut().take_midi_changes().is_empty());

    drop(out);
    let text = status_with(&mut h, "getrennt", 6).expect("no notice for the vanished port");
    assert_eq!(text, format!("{} getrennt", port_name(&port)));
}
