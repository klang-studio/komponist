//! App actions and device events keep live CLAP parameters and asset links in sync through
//! undo and redo. Audio probes and snapshots check the running processors.

use crate::common::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use komponist_app::device_host::PluginCatalogHost;
use komponist_app::{Action, App, EditAction, PluginAction};
use komponist_audiofile::fixtures as testkit;
use komponist_engine::Command;
use komponist_ipc::control::RenderMode;
use komponist_model::edits::{self, NewDevice};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::project::SavedPluginState;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, TrackId};
use komponist_plugin_api::device_assets::AssetStatus;
use komponist_plugins::{DeviceStatus, PluginManagerEvent};

fn undo(app: &mut App) {
    assert!(app.doc().undo_label().is_some());
    app.dispatch(Action::Edit(EditAction::Undo));
}

fn redo(app: &mut App) {
    assert!(app.doc().redo_label().is_some());
    app.dispatch(Action::Edit(EditAction::Redo));
}

fn saved_state(app: &App, device: DeviceId) -> Option<SavedPluginState> {
    app.project().find_device(device)?.2.saved_state()
}

/// Commit one parameter gesture through the dispatcher and wait for the app to store its state.
fn edit_parameters(
    app: &mut App,
    device: DeviceId,
    first: (u32, f64),
    rest: &[(u32, f64)],
) -> SavedPluginState {
    let previous_step = app.doc().top_step();
    let (id, _) = first;
    app.dispatch(Action::Plugin(PluginAction::BeginParamGesture {
        device,
        id,
    }));
    for (id, value) in std::iter::once(first).chain(rest.iter().copied()) {
        app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
    }
    app.dispatch(Action::Edit(EditAction::EndGesture));
    step_until(
        app,
        "the app to store the released gesture's snapshot",
        |app| {
            let Some(stored) = saved_state(app, device) else {
                return false;
            };
            let host = app.engine().host();
            // The host's live bases and a new undo step can precede the snapshot response.
            // Wait for a handled snapshot that includes all pending edits and is stored in the project.
            app.doc()
                .top_step()
                .is_some_and(|step| Some(step) != previous_step)
                && app.device_events().iter().any(|event| {
                    matches!(event, PluginManagerEvent::StateSnapshot { device: id, state }
                    if *id == device && *state == stored
                        && host.is_current_param_snapshot(device, &state.bytes))
                })
                && std::iter::once(first)
                    .chain(rest.iter().copied())
                    .all(|(id, value)| {
                        host.param_base(device, id)
                            .is_some_and(|actual| (actual - value).abs() < 1e-6)
                    })
        },
    );
    saved_state(app, device).expect("the app stored the current snapshot")
}

/// Render a live note and drain its release before probing again.
fn note_peak_with_tail(app: &mut App, track: TrackId, key: u8, tail_frames: usize) -> f32 {
    let engine = app.engine_mut();
    engine.send(Command::LiveNote {
        track,
        key,
        velocity: 100,
    });
    let (l, r) = engine.render_held(9600).expect("held engine");
    engine.send(Command::LiveNote {
        track,
        key,
        velocity: 0,
    });
    engine.render_held(tail_frames);
    l.iter().chain(&r).fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn note_peak(app: &mut App, track: TrackId) -> f32 {
    note_peak_with_tail(app, track, 60, 96_000)
}

/// Capture the device's bytes and host-owned parameter bases after the app handles the event.
fn snapshot(app: &mut App, device: DeviceId) -> SavedPluginState {
    app.engine_mut().host_mut().request_state(device);
    let mut snapshot = None;
    step_until(app, "the device state snapshot", |app| {
        snapshot = app
            .device_events()
            .iter()
            .rev()
            .find_map(|event| match event {
                PluginManagerEvent::StateSnapshot { device: id, state } if *id == device => {
                    Some(state.clone())
                }
                _ => None,
            });
        snapshot.is_some()
    });
    snapshot.unwrap()
}

fn wait_for_state(app: &mut App, device: DeviceId) {
    step_until(app, "the device applied the state", |app| {
        app.device_events().iter().any(|event| {
            matches!(event, PluginManagerEvent::StateApplied { device: applied } if *applied == device)
        })
    });
}

/// Keep the live processors in offline mode, like an export. The probes render the held engine
/// offline (`render_held`), so they wait for every bridge block instead of a deadline (#347).
fn hold_offline(app: &mut App, devices: &[DeviceId]) {
    app.engine_mut().hold_engine().expect("engine held");
    app.engine_mut()
        .host_mut()
        .set_render_mode(RenderMode::Offline);
    step_until(app, "the processors' offline render mode", |app| {
        devices
            .iter()
            .all(|&device| app.engine().device_status(device) == DeviceStatus::Ok)
    });
}

#[test]
fn undo_and_redo_of_clap_effect_state_reach_the_running_processor() {
    const LIMITER_ID: &str = "org.klang-studio.komponist.limiter";
    let Some(mut dirs) = clap_fixture_dirs("state-sync-clap", "limiter.clap") else {
        return;
    };
    let Some(synth_dirs) = fixture_dirs() else {
        return;
    };
    dirs.clap.extend(synth_dirs.clap);

    let dir = scratch("undo-limiter");
    let host = scan_fixture_host(&dir, &dirs);
    let (project, track, synth) = instrument_project("State sync", clap_plugin(SYNTH));
    let plugin = host
        .catalog()
        .entries()
        .iter()
        .find(|entry| entry.plugin.id == LIMITER_ID)
        .expect("the CLAP limiter")
        .plugin
        .clone();
    let (project, limiter) = edits::insert_device(
        &project,
        ChannelRef::Master,
        0,
        NewDevice {
            role: DeviceRole::Effect,
            plugin,
            state: None,
            sidechain: None,
            preset: None,
        },
    )
    .unwrap();
    let mut app = app_with(project, host);
    step_until(&mut app, "the synth and limiter", |app| {
        [synth, limiter]
            .into_iter()
            .all(|device| app.engine().device_status(device) == DeviceStatus::Ok)
    });
    wait_for_layout(&mut app, limiter);
    let gain = author_parameter(&app, limiter, "gain");
    let ceiling = author_parameter(&app, limiter, "ceiling");
    // The normalized gain maximum is +24 dB. Ceiling endpoints are -12 dB and 0 dB.
    let loud = edit_parameters(&mut app, limiter, (gain, 1.0), &[(ceiling, 1.0)]);
    let quiet = edit_parameters(&mut app, limiter, (ceiling, 0.0), &[]);
    assert_ne!(loud, quiet);
    hold_offline(&mut app, &[synth, limiter]);

    let quiet_peak = note_peak(&mut app, track);
    assert!(
        (0.20..=0.26).contains(&quiet_peak),
        "−12 dB ceiling: {quiet_peak}"
    );

    let reloads = app.engine().device_reloads();
    undo(&mut app);
    wait_for_state(&mut app, limiter);
    assert_eq!(saved_state(&app, limiter), Some(loud));
    let loud_peak = note_peak(&mut app, track);
    assert!(
        loud_peak > quiet_peak * 1.5,
        "undo: loud {loud_peak}, quiet {quiet_peak}"
    );

    redo(&mut app);
    wait_for_state(&mut app, limiter);
    assert_eq!(saved_state(&app, limiter), Some(quiet));
    let peak = note_peak(&mut app, track);
    assert!(
        (0.20..=0.26).contains(&peak),
        "redo: the new ceiling ({peak})"
    );
    assert_eq!(app.engine().device_reloads(), reloads);
}

#[test]
fn undo_and_redo_of_asset_links_reach_the_clap_sampler() {
    const SLOTS: u32 = 8;
    let dir = scratch("undo-sampler");
    let Some((host, plugin)) =
        native_fixture(&dir, "klopfer.clap", "org.klang-studio.komponist.sampler")
    else {
        return;
    };
    let (project, track, drums) = instrument_project("State sync", plugin);
    // The two files have the same short waveform at different levels. The running
    // processor's peak distinguishes their identities independently of the project's links.
    let write_sample = |name: &str, amplitude: f32| {
        let path = dir.join(name);
        let samples: Vec<_> = (0..2048)
            .map(|frame| (std::f32::consts::TAU * frame as f32 / 64.0).sin() * amplitude)
            .collect();
        testkit::write_wav(&path, &samples, testkit::RATE).unwrap();
        AssetLink::new(&path, None)
    };
    let original = write_sample("Original.wav", 0.125);
    let replacement = write_sample("Replacement.wav", 0.5);
    let links: BTreeMap<_, _> = (0..SLOTS).map(|slot| (slot, original.clone())).collect();
    let project = edits::set_device_asset_links(&project, drums, links).unwrap();
    let mut app = app_with(project, host);
    step_until(&mut app, "the sampler pad layout", |app| {
        app.engine().device_status(drums) == DeviceStatus::Ok
            && app
                .engine()
                .host()
                .native_layout(drums)
                .is_some_and(|layout| layout.pad_bank.is_some())
    });
    let pads = app
        .engine()
        .host()
        .native_layout(drums)
        .unwrap()
        .pad_bank
        .as_ref()
        .unwrap()
        .pads
        .clone();
    assert_eq!(pads.len(), SLOTS as usize);
    hold_offline(&mut app, &[drums]);

    let expect_slots = |app: &mut App, target: u32, link: Option<&AssetLink>| {
        let expected = if link.is_some() {
            AssetStatus::Ready
        } else {
            AssetStatus::Empty
        };
        step_until(
            app,
            &format!("slot {target} to be {expected:?} and the rest ready"),
            |app| {
                let device = app.project().find_device(drums).unwrap().2;
                device.asset_links.get(&target).map(|link| &link.abs) == link.map(|link| &link.abs)
                    && app
                        .engine()
                        .host()
                        .asset_slots(drums)
                        .is_some_and(|snapshot| {
                            snapshot.slots.len() == SLOTS as usize
                                && snapshot
                                    .slots
                                    .iter()
                                    .any(|slot| slot.id == target && slot.status == expected)
                                && snapshot.slots.iter().all(|slot| {
                                    slot.id == target || slot.status == AssetStatus::Ready
                                })
                        })
            },
        );
    };
    let set_slot = |app: &mut App, slot, link: Option<&AssetLink>| {
        app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device: drums,
            slot,
            path: link.map(|link| link.abs.clone()),
        }));
    };
    let peak = |app: &mut App, key| note_peak_with_tail(app, track, key, 256);

    for pad in pads {
        let slot = pad.asset_slot;
        expect_slots(&mut app, slot, Some(&original));
        let reference = peak(&mut app, pad.note);
        assert!(
            reference > 1e-3,
            "original asset in slot {slot}: {reference}"
        );

        set_slot(&mut app, slot, None);
        expect_slots(&mut app, slot, None);
        undo(&mut app);
        expect_slots(&mut app, slot, Some(&original));
        let restored = peak(&mut app, pad.note);
        assert!(
            (restored / reference - 1.0).abs() < 0.03,
            "undo must restore the original asset in slot {slot}: {restored} vs {reference}"
        );
        redo(&mut app);
        expect_slots(&mut app, slot, None);

        set_slot(&mut app, slot, Some(&replacement));
        expect_slots(&mut app, slot, Some(&replacement));
        let relinked = peak(&mut app, pad.note);
        assert!(
            (relinked / reference - 4.0).abs() < 0.12,
            "relink must load the replacement in slot {slot}: {relinked} vs {reference}"
        );
        undo(&mut app);
        expect_slots(&mut app, slot, None);
        redo(&mut app);
        expect_slots(&mut app, slot, Some(&replacement));
        let redone = peak(&mut app, pad.note);
        assert!(
            (redone / reference - 4.0).abs() < 0.12,
            "redo must load the replacement in slot {slot}: {redone} vs {reference}"
        );

        set_slot(&mut app, slot, Some(&original));
        expect_slots(&mut app, slot, Some(&original));
    }
}

#[test]
fn undo_and_redo_of_a_bridged_plugins_state_reach_the_plugin() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("undo-bridge");
    let host = scan_fixture_host(&dir, &dirs);
    let (project, _, synth) = instrument_project("State sync", clap_plugin(SYNTH));
    let mut app = app_with(project, host);
    step_until(&mut app, "the plugin", |app| {
        app.engine().device_status(synth) == DeviceStatus::Ok
    });
    assert!(app.engine().host().bridge_pid(synth).is_some());
    let gain = app
        .engine()
        .host()
        .params(synth)
        .unwrap()
        .iter()
        .find(|p| p.name == "Gain")
        .expect("a gain parameter")
        .id;

    let loud = edit_parameters(&mut app, synth, (gain, 0.9), &[]);
    let quiet = edit_parameters(&mut app, synth, (gain, 0.3), &[]);
    assert_ne!(loud, quiet);

    undo(&mut app);
    wait_for_state(&mut app, synth);
    assert_eq!(snapshot(&mut app, synth), loud, "undo: the old state");
    redo(&mut app);
    wait_for_state(&mut app, synth);
    assert_eq!(snapshot(&mut app, synth), quiet, "redo: the new state");
}

/// A restore failure must preserve the original bytes while the user leaves the controls alone.
/// A deliberate edit may recover the device and record freshly edited state.
#[test]
fn invalid_clap_state_survives_until_a_deliberate_edit() {
    use komponist_app::actions::DropTarget;

    let corrupted: &[u8] = b"{\"params\": {\"gain\": ";
    let mut app = App::headless(Project::new("Unreadable"));
    let shipped = |app: &App, id| crate::common::shipped_plugin(app.engine().host(), id).unwrap();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "org.klang-studio.komponist.hexa"),
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "org.klang-studio.komponist.limiter"),
        target: DropTarget::Track(track),
    }));
    let devices: Vec<DeviceId> = app.project().tracks[0]
        .channel
        .devices
        .iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(devices.len(), 2);
    for &device in &devices {
        crate::common::wait_for_layout(&mut app, device);
    }
    let hexa_volume = crate::common::author_parameter(&app, devices[0], "volume");
    let limiter_gain = crate::common::author_parameter(&app, devices[1], "gain");
    let knobs = [(devices[0], hexa_volume), (devices[1], limiter_gain)];
    let state = |app: &App, device: DeviceId| {
        let (_, _, d) = app.project().find_device(device).unwrap();
        d.state.as_deref().map(<[u8]>::to_vec)
    };
    // The same actions do reach the controls of readable instances.
    for (device, id) in knobs {
        let initial = app.engine().host().param_base(device, id).unwrap();
        let value = if initial < 0.5 { 0.75 } else { 0.25 };
        app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
        crate::common::step_until(&mut app, "the live edit on readable state", |app| {
            app.engine()
                .host()
                .param_value(device, id)
                .is_some_and(|param| (param.value - value).abs() < 1e-6)
        });
    }
    let mut project = Arc::clone(app.project());
    for &device in &devices {
        project = Arc::new(
            edits::set_device_state(&project, device, Some(corrupted.to_vec().into())).unwrap(),
        );
    }
    let mut app = App::headless(Arc::unwrap_or_clone(project));
    crate::common::step_until(&mut app, "both CLAP restore warnings", |app| {
        devices
            .iter()
            .all(|&device| app.engine().host().state_warning(device).is_some())
    });
    // Outlast the one-second persistence debounce before checking the unreadable bytes.
    crate::common::step_for(&mut app, Duration::from_secs(2));
    for &device in &devices {
        assert_eq!(
            state(&app, device).as_deref(),
            Some(corrupted),
            "{device:?}"
        );
    }
    assert_eq!(app.doc().undo_label(), None);
    for (device, id) in knobs {
        let initial = app.engine().host().param_base(device, id).unwrap();
        let value = if initial < 0.5 { 0.75 } else { 0.25 };
        app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
        crate::common::step_until(&mut app, "the deliberate recovery edit", |app| {
            app.engine().host().state_warning(device).is_none()
                && state(app, device).is_some_and(|bytes| bytes.as_slice() != corrupted)
        });
    }
}
