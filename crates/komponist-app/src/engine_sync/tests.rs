use super::*;
use crate::app::App;
use komponist_engine::MAX_SLOTS;
use komponist_engine::workers::auto_workers;
use komponist_model::edits::{self, ChannelProp, TrackProp};
use komponist_model::fixtures::demo_project;
use std::time::{Duration, Instant};

fn test_song(host: &dyn DeviceHost) -> Project {
    let mut project = demo_project();
    for (index, plugin) in [
        (0, "org.klang-studio.komponist.sampler"),
        (1, "org.klang-studio.komponist.hexa"),
        (2, "org.klang-studio.komponist.hexa"),
    ] {
        let instrument = host
            .catalog()
            .entries()
            .iter()
            .find(|entry| entry.plugin.id == plugin)
            .unwrap_or_else(|| panic!("build fixture {plugin} with scripts/build-test-plugins.sh"));
        let device = &mut Arc::make_mut(&mut project.tracks[index]).channel.devices[0];
        Arc::make_mut(device).plugin = instrument.plugin.clone();
    }
    project
}

pub(super) fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn the_demo_plays_on_the_null_backend() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    assert!(matches!(sync.audio_state(), AudioState::Running { .. }));
    let doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    let program = sync.program().expect("sent");
    assert_eq!(program.tracks.len(), doc.project().tracks.len());
    sync.send(Command::Play);
    assert!(wait_until(|| {
        sync.poll();
        sync.playhead() > Tick::ZERO
    }));
    assert_eq!(sync.telemetry().transport(), TransportState::Playing);
    sync.send(Command::Stop);
    assert!(wait_until(|| {
        sync.poll();
        sync.telemetry().transport() == TransportState::Stopped
    }));
    sync.shutdown();
}

/// #210: an engine whose `process` unwound is not given back to the stream; a new one is
/// built at the same rate and worker setting, the document is compiled into it from scratch,
/// its transport stays stopped at the old playhead, the old engine's queued events still
/// arrive, and the app is told once (with that playhead).
#[test]
fn a_poisoned_engine_is_rebuilt_from_the_document() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    // Single-threaded: the panic must happen on this thread (a worker runs its jobs inside
    // `assert_no_alloc`, where the panic's allocation aborts in debug builds).
    assert_eq!(sync.set_workers(Some(0)).unwrap().unwrap().threads, 0);
    let doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    let lead = doc.project().tracks[2].channel.devices[0].id;
    let slot = sync.program().unwrap().slots.slot(lead).unwrap();
    let sent = sync.programs_sent();
    sync.hold_engine().unwrap();
    // Play a while, so the crash happens away from 0.
    sync.send(Command::Play);
    assert!(sync.run_held(48_000, &[], 0));
    let at = sync.playhead();
    assert!(at > Tick::ZERO);
    sync.send(Command::InsertProcessor {
        slot,
        processor: Box::new(komponist_engine::testing::TestPanic),
    });
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sync.run_held(256, &[], 0);
    }));
    assert!(caught.is_err(), "the processor ran");
    assert!(sync.io.held_engine().is_poisoned());
    // Back to the stream: refused, rebuilt at the next poll.
    sync.release_engine();
    assert_eq!(sync.take_rebuild_notice(), None);
    let events = sync.poll_engine_events();
    assert_eq!(sync.take_rebuild_notice(), Some(at));
    assert_eq!(sync.take_rebuild_notice(), None, "taken once");
    // The old engine's queued garbage stays behind; nothing else was queued here.
    assert!(events.iter().all(|e| !matches!(e, EngineEvent::Retired(_))));
    assert!(sync.program().is_none(), "the old program is forgotten");
    assert_eq!(
        sync.worker_status().unwrap().threads,
        0,
        "the setting stays"
    );
    sync.sync(&doc);
    assert_eq!(sync.programs_sent(), sent + 1);
    assert!(sync.program().unwrap().slots.slot(lead).is_some());
    // Stopped where the old engine was.
    assert!(wait_until(|| {
        sync.poll();
        sync.playhead() == at
    }));
    assert_eq!(sync.transport(), TransportState::Stopped);
    // The new engine runs in the stream and plays the document from there.
    sync.send(Command::Play);
    assert!(wait_until(|| {
        sync.poll();
        sync.playhead() > at
    }));
    sync.hold_engine().unwrap();
    assert!(!sync.io.held_engine().is_poisoned());
    sync.release_engine();
    sync.shutdown();
}

/// #211: an export worker that died with the engine: a new one is built at the stream's rate,
/// the commands that waited are dropped (the queue cannot grow while nothing runs), the app
/// is told once, and the new engine plays the document.
#[test]
fn a_lost_engine_is_rebuilt_and_the_waiting_commands_dropped() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    let rate = sync.sample_rate();
    let detached = sync.detach().unwrap();
    sync.send(Command::Play);
    assert_eq!(sync.pending_commands(), 1);
    // The worker dies: the engine goes with it.
    drop(detached);
    sync.recover(None);
    assert!(!sync.is_detached());
    assert_eq!(sync.pending_commands(), 0);
    assert_eq!(sync.sample_rate(), rate);
    assert!(sync.take_rebuild_notice().is_some());
    assert_eq!(sync.take_rebuild_notice(), None, "taken once");
    sync.sync(&doc);
    sync.send(Command::Play);
    let at = sync.playhead();
    assert!(wait_until(|| {
        sync.poll();
        sync.playhead() > at
    }));
    sync.shutdown();
}

/// #211 review: a replacement engine keeps the worker setting the user chose.
#[test]
fn a_replacement_engine_keeps_the_worker_setting() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    sync.set_workers(Some(0)).unwrap();
    let detached = sync.detach().unwrap();
    drop(detached);
    sync.recover(None);
    assert!(wait_until(|| sync.worker_status().is_some()));
    assert_eq!(sync.worker_status().unwrap().threads, 0);
    // A setting changed during the export wins.
    let detached = sync.detach().unwrap();
    sync.set_workers(Some(2)).unwrap();
    drop(detached);
    sync.recover(None);
    assert!(wait_until(|| sync.worker_status().is_some()));
    assert_eq!(sync.worker_status().unwrap().threads, 2);
    sync.shutdown();
}

/// #211 review: a `Locate` sent after a rebuild beats the position the rebuilt engine returns
/// to (also for #210's rebuild after a stream panic: both set `locate_after_rebuild`).
#[test]
fn a_newer_locate_beats_the_rebuild_position() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    sync.send(Command::Play);
    assert!(wait_until(|| {
        sync.poll();
        sync.playhead() > Tick(2_000)
    }));
    let detached = sync.detach().unwrap();
    drop(detached);
    sync.recover(None);
    sync.send(Command::Stop);
    sync.send(Command::Locate(Tick(100)));
    sync.sync(&doc);
    assert!(wait_until(|| {
        sync.poll();
        // The position is rounded to a frame.
        (Tick(95)..=Tick(100)).contains(&sync.playhead())
    }));
    sync.shutdown();
}

/// #92: the diagnostics of the program last sent, and a notice only when a sync leaves out
/// something new.
#[test]
fn diagnostics_follow_the_sent_program_and_notice_only_new_ones() {
    use komponist_engine::MAX_TRACKS;
    use komponist_engine::compile::{Reason, Subject};
    let add = |project: &Project| {
        let spec = edits::NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        };
        edits::add_track(project, spec).unwrap().0
    };
    let mut project = Project::new("Full");
    for _ in 0..MAX_TRACKS {
        project = add(&project);
    }
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let mut doc = Document::new(project);
    sync.sync(&doc);
    assert_eq!(sync.diagnostics(), []);
    assert!(!sync.take_diagnostics_notice());

    doc.apply_with("Add track", |p| Ok((add(p), ()))).unwrap();
    sync.sync(&doc);
    let last = doc.project().tracks[MAX_TRACKS].id;
    assert_eq!(sync.diagnostics().len(), 1);
    assert_eq!(sync.diagnostics()[0].reason, Reason::TrackLimit);
    assert_eq!(
        sync.diagnostics()[0].subject,
        Subject::Track {
            track: last,
            index: MAX_TRACKS
        }
    );
    assert!(sync.take_diagnostics_notice());
    assert!(!sync.take_diagnostics_notice(), "taken once");

    // A change that leaves out nothing new says nothing.
    doc.apply_with("Rename", |p| {
        Ok((
            edits::set_track_prop(p, &[last], TrackProp::Name("Extra".into()))?,
            (),
        ))
    })
    .unwrap();
    sync.sync(&doc);
    assert_eq!(sync.diagnostics().len(), 1);
    assert!(!sync.take_diagnostics_notice());

    // Removing the extra track clears the diagnostics without a notice.
    doc.apply_with("Remove", |p| Ok((edits::remove_tracks(p, &[last])?, ())))
        .unwrap();
    sync.sync(&doc);
    assert_eq!(sync.diagnostics(), []);
    assert!(!sync.take_diagnostics_notice());

    // Another project (open, new) that leaves out the same track at the same position: its
    // diagnostic is new, though equal to one the previous project had.
    doc.apply_with("Add track", |p| Ok((add(p), ()))).unwrap();
    sync.sync(&doc);
    assert!(sync.take_diagnostics_notice());
    let before = sync.diagnostics().to_vec();
    sync.reset();
    assert_eq!(sync.diagnostics(), []);
    let other = Document::new(doc.project().as_ref().clone());
    sync.sync(&other);
    assert_eq!(sync.diagnostics(), before);
    assert!(sync.take_diagnostics_notice());
    sync.shutdown();
}

#[test]
fn attach_flushes_program_and_transport_commands_in_order() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let doc = Document::new(Project::new("Queued program"));
    sync.hold_engine().unwrap();
    let detached = sync.detach().unwrap();
    sync.sync(&doc);
    sync.send(Command::Locate(Tick(1920)));
    sync.send(Command::Play);
    sync.send(Command::Stop);
    assert_eq!(sync.pending_commands(), 4);
    assert_eq!(sync.playhead(), Tick::ZERO);

    sync.attach(detached);
    assert_eq!(sync.pending_commands(), 0);
    assert!(sync.run_held(256, &[], 0));
    assert!((Tick(1919)..=Tick(1920)).contains(&sync.playhead()));
    assert_eq!(sync.transport(), TransportState::Stopped);

    // The attached engine's rate may differ from the one used while detached.
    sync.sync(&doc);
    assert_eq!(sync.programs_sent(), 2);
}

#[test]
fn reattaching_at_a_new_rate_recompiles_the_same_document_revision() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    let sent = sync.programs_sent();
    let detached = sync.detach().unwrap();
    sync.set_audio(&StreamConfig {
        host: HostKind::Null,
        sample_rate: 44_100,
        ..StreamConfig::default()
    })
    .unwrap();
    sync.attach(detached);
    assert_eq!(sync.sample_rate(), 44_100);
    sync.sync(&doc);
    assert_eq!(sync.programs_sent(), sent + 1);
}

/// WP-65: the stream starts the automatic worker pool; the setting changes it (with the
/// engine held, during an export and in the stream) and playback goes on.
#[test]
fn worker_threads_follow_the_setting() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let realtime = Some(komponist_engine::workers::DEFAULT_WORKER_PRIORITY);
    let status = sync.worker_status().expect("prepared at stream start");
    // The automatic count depends on the machine: none on a small one (a 3-core CI VM before
    // #79 counted performance cores), and no pool has no priority (#94).
    assert_eq!(status.threads, auto_workers());
    let expected = if status.threads > 0 { realtime } else { None };
    assert_eq!(status.priority, expected, "{status:?}");
    let status = sync.set_workers(Some(2)).unwrap().unwrap();
    assert_eq!(status.threads, 2);
    assert_eq!(status.priority, realtime);
    let doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    sync.send(Command::Play);
    assert!(wait_until(|| {
        sync.poll();
        sync.worker_stats().worker_jobs() > 0
    }));
    assert_eq!(sync.set_workers(Some(0)).unwrap().unwrap().threads, 0);
    // Held engine: applied at once.
    sync.hold_engine().unwrap();
    assert_eq!(sync.set_workers(Some(1)).unwrap().unwrap().threads, 1);
    sync.release_engine();
    // During an export: applied when the engine comes back.
    let detached = sync.detach().unwrap();
    assert_eq!(sync.set_workers(Some(3)).unwrap(), None);
    sync.attach(detached);
    assert!(wait_until(|| sync
        .worker_status()
        .is_some_and(|s| s.threads == 3)));
    let before = sync.playhead();
    assert!(wait_until(|| {
        sync.poll();
        sync.playhead() > before
    }));
    sync.shutdown();
}

/// Mute and solo on the fast path carry the gate table of the routing graph: soloing a group
/// member keeps its group open (WP-102).
#[test]
fn mute_and_solo_gestures_send_the_routing_gates() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let mut doc = Document::new(test_song(sync.host()));
    assert!(sync.gates(doc.project()).is_none());
    let (drums, bass) = (doc.project().tracks[0].id, doc.project().tracks[1].id);
    doc.apply("Gruppe", |p| {
        edits::add_bus(p, edits::NewBus::group(vec![drums, bass])).map(|(p, _)| p)
    })
    .unwrap();
    sync.sync(&doc);
    let program = sync.program().unwrap().clone();
    assert!(!program.routing.is_flat());
    doc.apply("Solo", |p| {
        edits::set_track_prop(p, &[bass], TrackProp::Channel(ChannelProp::Solo(true)))
    })
    .unwrap();
    let Some(Command::SetGates(gates)) = sync.gates(doc.project()) else {
        panic!("no gate table");
    };
    let index = |id| program.tracks.iter().position(|t| t.id == id).unwrap();
    let group = doc.project().tracks[0].id;
    assert!(doc.project().tracks[0].is_group());
    assert!(gates.output_open(index(bass)) && gates.output_open(index(group)));
    assert!(!gates.output_open(index(drums)));
    assert_eq!(gates, compile::gates(doc.project(), &program));
}

#[test]
fn unchanged_documents_are_not_recompiled() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    let first = sync.program().unwrap().tracks[0].clone();
    sync.sync(&doc);
    assert!(Arc::ptr_eq(&first, &sync.program().unwrap().tracks[0]));
}

#[test]
fn removed_devices_leave_the_host_and_new_ones_get_processors() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let mut doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    let lead_synth = doc.project().tracks[2].channel.devices[0].id;
    assert!(wait_until(|| {
        sync.poll();
        sync.device_status(lead_synth) == DeviceStatus::Ok
    }));
    let slot = sync.program().unwrap().slots.slot(lead_synth);
    assert!(slot.is_some());
    let lead = doc.project().tracks[2].id;
    doc.apply("Spur löschen", |p| edits::remove_tracks(p, &[lead]))
        .unwrap();
    sync.sync(&doc);
    assert_eq!(sync.program().unwrap().slots.slot(lead_synth), None);
    // Released: the host forgot it (unknown devices report Ok).
    assert_eq!(sync.device_status(lead_synth), DeviceStatus::Ok);
    let surge = doc.project().tracks[2].channel.devices[0].id;
    assert_eq!(sync.device_status(surge), DeviceStatus::Missing);
}

#[test]
fn sync_reuses_full_processor_slots_after_retiring_the_old_device() {
    use edits::NewDevice;
    use komponist_model::automation::{AutomationOwner, AutomationTarget};
    use komponist_model::{ChannelRef, DeviceRole};

    const LIMITER: &str = "org.klang-studio.komponist.limiter";
    let host = App::headless_plugin_host();
    let prepare = |plugin: &str, edits: &[(&str, f64)], targets: &[&str]| {
        host.manager()
            .prepare_bundled_device(plugin, None, edits, targets)
            .unwrap_or_else(|error| panic!("{error}; build the bundles for the tests"))
    };
    let new_device =
        |prepared: komponist_plugins::bundled_initial_state::InitialDevice| NewDevice {
            role: DeviceRole::Effect,
            plugin: prepared.plugin,
            state: prepared.state,
            sidechain: None,
            preset: None,
        };
    let eq = new_device(prepare("org.klang-studio.komponist.eq", &[], &[]));
    let limiter = new_device(prepare(LIMITER, &[], &[]));
    let owner = AutomationOwner::Master;
    let gain = prepare(LIMITER, &[], &["gain"]).targets["gain"].0.clone();
    let parameter = |device| AutomationTarget::Param {
        device,
        param: gain.clone(),
    };
    let zero_latency_limiter = new_device(prepare(LIMITER, &[("lookahead", 0.0)], &[]));
    // With one initial spare, the first new limiter causes a latency recompile while
    // the second limiter still needs to wait for the removed device's slot.
    for spare in [0, 1] {
        let (mut project, removed) = edits::insert_device(
            &Project::new("Full processor table"),
            ChannelRef::Master,
            0,
            zero_latency_limiter.clone(),
        )
        .unwrap();
        for index in 1..MAX_SLOTS - spare {
            (project, _) =
                edits::insert_device(&project, ChannelRef::Master, index, eq.clone()).unwrap();
        }
        (project, _) =
            edits::automation::add_lane(&project, owner, parameter(removed), "Gain".into())
                .unwrap();
        let mut doc = Document::new(project);
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        sync.hold_engine().unwrap();
        sync.sync(&doc);
        let original = sync.program().unwrap();
        let old_slot = original.slots.slot(removed).unwrap();
        let old_target = original
            .automation_target(owner, &parameter(removed))
            .unwrap();
        // Finish all initial insertions, including any program install waiting for room
        // in the command queue.
        sync.io.held_engine_mut().settle();
        sync.poll();
        sync.io.held_engine_mut().settle();
        let initial_programs = sync.programs_sent();

        let replacements = doc
            .apply_with("Replace devices", |project| {
                let mut project = edits::remove_device(project, removed)?;
                let mut replacements = Vec::new();
                for index in 0..=spare {
                    let id;
                    (project, id) =
                        edits::insert_device(&project, ChannelRef::Master, index, limiter.clone())?;
                    replacements.push(id);
                }
                let (project, _) = edits::automation::add_lane(
                    &project,
                    owner,
                    parameter(replacements[spare]),
                    "Gain".into(),
                )?;
                Ok((project, replacements))
            })
            .unwrap();
        sync.sync(&doc);
        sync.sync(&doc);
        assert_eq!(sync.programs_sent(), initial_programs + 2);
        sync.io.held_engine_mut().settle();
        let program = sync.program().unwrap();
        assert_eq!(program.slots.entries().len(), MAX_SLOTS);
        assert_eq!(program.slots.slot(removed), None);
        assert_eq!(program.slots.slot(replacements[spare]), Some(old_slot));
        assert!(!sync.plugin_parameter_slot_matches(old_slot, removed));
        assert!(sync.plugin_parameter_slot_matches(old_slot, replacements[spare]));
        let replacement_target = parameter(replacements[spare]);
        let new_target = program
            .automation_target(owner, &replacement_target)
            .unwrap();
        assert_eq!(program.automation_model_target(owner, old_target), None);
        assert_eq!(
            program.automation_model_target(owner, new_target),
            Some(&replacement_target)
        );
        let mut total_latency = 0;
        for replacement in replacements {
            let slot = program.slots.slot(replacement).unwrap();
            let latency = sync.host().latency(replacement);
            assert!(latency > 0);
            assert_eq!(sync.io.held_engine().processor_latency(slot), latency);
            total_latency += latency;
        }
        assert_eq!(program.master.chain_latency, total_latency);
    }
}

#[test]
fn reset_starts_the_next_project_from_scratch() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let first = Document::new(test_song(sync.host()));
    sync.sync(&first);
    let lead = first.project().tracks[2].channel.devices[0].id;
    assert!(sync.program().unwrap().slots.slot(lead).is_some());
    sync.reset();
    assert!(sync.program().is_none());
    // Released: the host forgot every device (unknown devices report Ok).
    let drums = first.project().tracks[0].channel.devices[0].id;
    assert_eq!(sync.device_status(drums), DeviceStatus::Ok);
    // A new document starts at revision 0 like the old one; it is compiled anyway, and its
    // devices (ids from 1 again) get fresh processors.
    let second = Document::new(test_song(sync.host()));
    assert_eq!(second.revision(), first.revision());
    sync.sync(&second);
    // The verified Sampler reloads in the new document.
    assert!(wait_until(|| {
        sync.poll();
        sync.device_status(drums) == DeviceStatus::Ok
    }));
    // The compressor behind it is not installed: it stays missing.
    let compressor = second.project().tracks[0].channel.devices[1].id;
    assert_eq!(sync.device_status(compressor), DeviceStatus::Missing);
    // The same document again without reset: nothing is recompiled.
    let program = sync.program().expect("compiled again");
    assert_eq!(program.tracks.len(), second.project().tracks.len());
    let tracks = Arc::clone(&program.tracks[0]);
    sync.sync(&second);
    assert!(Arc::ptr_eq(&tracks, &sync.program().unwrap().tracks[0]));
}

#[test]
fn fast_path_defers_the_recompile_to_the_gesture_end() {
    let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
    let mut doc = Document::new(test_song(sync.host()));
    sync.sync(&doc);
    let track = doc.project().tracks[1].id;
    let before_program = sync.program().unwrap().tracks[1].clone();
    doc.begin_gesture("Lautstärke");
    let before = doc.revision();
    doc.apply_in_gesture(|p| {
        edits::set_track_prop(
            p,
            &[track],
            TrackProp::Channel(ChannelProp::VolumeDb(-12.0)),
        )
    })
    .unwrap();
    sync.fast_path(
        Command::SetTrackGain { track, gain: 0.25 },
        before,
        doc.revision(),
    );
    sync.sync(&doc);
    assert!(
        Arc::ptr_eq(&before_program, &sync.program().unwrap().tracks[1]),
        "no recompile during the gesture"
    );
    doc.end_gesture();
    sync.sync(&doc);
    assert!(!Arc::ptr_eq(
        &before_program,
        &sync.program().unwrap().tracks[1]
    ));
}
