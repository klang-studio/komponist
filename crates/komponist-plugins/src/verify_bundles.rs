//! Release smoke test using the same formats and bridge processes as the application.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

use komponist_engine::processor::EventBuffer;
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, StereoIo, TransportInfo,
};
use komponist_ipc::{PluginDescriptor, PluginFormat, PluginKind, RenderMode};
use komponist_model::{Device, DeviceId, DeviceRole};

use crate::bridge::{Bridge, SpawnSetup, StatePurpose, Timing};
use crate::bundled_clap::{BundledClap, VerifiedClap};
use crate::catalog::{PluginDirs, PluginLocation, SCAN_TIMEOUT};
use crate::direct::DirectClap;
use crate::direct_session::DirectSession;
use crate::manager::{
    DeviceStatus, PluginManager, PluginManagerConfig, PluginManagerEvent, ProcessorSetup,
};
use crate::scan::scan_bundle;

const PRESSE_ID: &str = "org.klang-studio.komponist.presse";
const FRAMES: usize = 128;
const SETUP: ProcessorSetup = ProcessorSetup {
    sample_rate: 48_000.0,
    max_block: FRAMES,
};
const TIMEOUT: Duration = Duration::from_secs(15);

/// Verify every artifact named by the app-bound manifest. The bridge route scans and opens
/// the original files from the archive; the direct route loads the verified private snapshot.
pub fn verify_bundled_clap(force_bridge: bool) -> Result<(), String> {
    let config = PluginManagerConfig::defaults();
    let cache_dir = config
        .cache_file
        .parent()
        .ok_or_else(|| "no plugin cache directory".to_owned())?;
    let bundled = BundledClap::discover(cache_dir)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "this executable has no embedded bundled CLAP manifest hash".to_owned())?;
    for (path, id) in bundled.bundle_entries() {
        let result = || {
            let verified = bundled
                .verify(path, id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "bundle was not found in its own manifest".to_owned())?;
            let descriptor = scan(&config, path, id)?;
            let role = match descriptor.kind {
                PluginKind::Effect => DeviceRole::Effect,
                PluginKind::Instrument => DeviceRole::Instrument,
                PluginKind::MidiEffect => DeviceRole::MidiEffect,
                PluginKind::Other
                    if descriptor
                        .features
                        .iter()
                        .any(|feature| feature == "note-effect") =>
                {
                    DeviceRole::MidiEffect
                }
                PluginKind::Other => return Err("unsupported bundled plugin role".to_owned()),
            };
            if force_bridge {
                smoke_bridge(&config, &descriptor, path, role)
            } else {
                smoke_direct(verified, role)
            }
        };
        result().map_err(|error| format!("{id}: {error}"))?;
        let route = if force_bridge { "bridge" } else { "in-process" };
        println!("verified {id} ({route})");
    }
    if !force_bridge
        && let Some((path, id)) = bundled
            .bundle_entries()
            .find(|(_, id)| *id == "org.klang-studio.komponist.hall")
    {
        smoke_live_manager(config, path, id)?;
        println!("verified {id} (normal direct lifecycle)");
    }
    Ok(())
}

fn smoke_live_manager(config: PluginManagerConfig, path: &Path, id: &str) -> Result<(), String> {
    let mut manager = PluginManager::new_with_direct(config, true);
    manager.start_scan(&PluginDirs {
        clap: Vec::new(),
        vst3: Vec::new(),
    });
    let deadline = Instant::now() + TIMEOUT;
    while manager.is_scanning() {
        manager.poll();
        if Instant::now() >= deadline {
            return Err("normal bundled catalog scan timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let entry = manager.catalog().entries().iter().find(|entry| {
        entry.plugin.id == id
            && matches!(&entry.location, PluginLocation::Bundle { path: scanned, .. } if scanned == path)
    }).ok_or_else(|| "Hall is absent from the normal bundled catalog".to_owned())?;
    let device = Device {
        id: DeviceId(1),
        role: DeviceRole::Effect,
        plugin: entry.plugin.clone(),
        bypass: false,
        state: None,
        sidechain: None,
    };
    let device_id = device.id;
    let mut audio = manager
        .create_processor(&device, SETUP)
        .map_err(|error| error.to_string())?;
    if manager.bridge_pid(device_id).is_some()
        || manager.status(device_id) != DeviceStatus::Ok
        || manager.native_layout(device_id).is_none()
    {
        return Err(
            "normal Hall start did not use the verified direct path with native layout".into(),
        );
    }
    let param = manager
        .params(device_id)
        .and_then(|params| params.iter().find(|info| info.step_count == 0))
        .ok_or_else(|| "normal Hall start has no continuous parameters".to_owned())?;
    let param_id = param.id;
    let low = param.min;
    let high = param.max;
    let baseline = manager
        .param_base(device_id, param_id)
        .ok_or_else(|| "normal Hall parameter base is missing".to_owned())?;
    let edit = if baseline - low < high - baseline {
        high
    } else {
        low
    };
    let snapshot = |manager: &mut PluginManager| {
        manager
            .snapshot_for_save(Duration::ZERO)
            .captured
            .into_iter()
            .find_map(|(id, state)| (id == device_id).then_some(state))
    };
    let initial =
        snapshot(&mut manager).ok_or_else(|| "could not save Hall before editing".to_owned())?;
    manager.set_param(device_id, param_id, edit);
    manager.poll();
    drive_control(&mut *audio, &[]);
    let edited = snapshot(&mut manager)
        .ok_or_else(|| "normal Hall edit was not acknowledged by DSP".to_owned())?;
    if initial == edited {
        return Err("normal Hall edit did not change the saved CLAP state".into());
    }

    drive_control(
        &mut *audio,
        &[Event {
            offset: 0,
            kind: EventKind::Param {
                id: param_id,
                value: baseline,
            },
        }],
    );
    if snapshot(&mut manager).is_some() {
        return Err("normal Hall captured a temporary automation value".into());
    }
    if !manager.set_state(device_id, &initial) {
        return Err("normal Hall did not accept an undo state".into());
    }
    if !manager.poll().iter().any(|event| {
        matches!(event,
        PluginManagerEvent::StateApplied { device } if *device == device_id)
    }) || manager.param_base(device_id, param_id) != Some(baseline)
    {
        return Err("normal Hall undo did not restore its parameter base".into());
    }

    manager.release(device_id);
    if !manager.has_retiring_direct() {
        return Err("normal Hall dropped its control half before the audio half".into());
    }
    drop(audio);
    manager.poll();
    if manager.has_retiring_direct() {
        return Err("normal Hall audio half was not deactivated on retirement".into());
    }
    Ok(())
}

fn drive_control(audio: &mut dyn Processor, events: &[Event]) {
    let context = smoke_context();
    let input = [0.0; FRAMES];
    let mut left = [0.0; FRAMES];
    let mut right = [0.0; FRAMES];
    audio.process(
        &context,
        events,
        &mut StereoIo {
            input: [&input, &input],
            output: [&mut left, &mut right],
        },
    );
}

fn scan(config: &PluginManagerConfig, path: &Path, id: &str) -> Result<PluginDescriptor, String> {
    let scanned = scan_bundle(
        &config.bridge_exe,
        &config.bridge_args,
        path,
        SCAN_TIMEOUT,
        &AtomicBool::new(false),
    )
    .ok_or_else(|| "bundle scan cancelled".to_owned())?;
    if let Some(error) = scanned.error {
        return Err(error);
    }
    let [descriptor] = scanned
        .descriptors
        .try_into()
        .map_err(|descriptors: Vec<_>| {
            format!("expected one descriptor, got {}", descriptors.len())
        })?;
    if descriptor.id != id || descriptor.format != PluginFormat::Clap {
        return Err(format!("unexpected scanned descriptor: {}", descriptor.id));
    }
    Ok(descriptor)
}

fn smoke_direct(bundle: VerifiedClap, role: DeviceRole) -> Result<(), String> {
    let presse = bundle.plugin_id() == PRESSE_ID;
    let mut host = DirectClap::load(bundle).map_err(|error| error.to_string())?;
    let can_render =
        host.try_control(|plugin| plugin.native_layout().is_some() && !plugin.params().is_empty());
    if can_render != Some(true) {
        return Err("native layout or parameters unavailable".to_owned());
    }
    let (mut session, mut audio) =
        DirectSession::start(host, SETUP, role).map_err(|error| error.to_string())?;
    let result = (|| {
        drive_direct(&mut audio, role, presse)?;
        audio
            .activate(SETUP.sample_rate, SETUP.max_block * 2)
            .map_err(|error| error.to_string())?;
        session.set_render_mode(RenderMode::Offline);
        // Let the worker return its current audio half at the next realtime boundary.
        drive_boundary(&mut audio);
        session.poll().map_err(|error| error.to_string())?;
        drive_direct(&mut audio, role, presse)
    })();
    let mut host = session.finish(audio);
    result?;
    host.poll();
    host.try_control(|plugin| {
        let state = plugin.save_state().map_err(|error| error.to_string())?;
        if state.is_empty() {
            return Err("plugin saved an empty state".to_owned());
        }
        plugin.load_state(&state).map_err(|error| error.to_string())
    })
    .ok_or_else(|| "direct audio worker did not release the instance".to_owned())?
}

fn drive_direct(audio: &mut dyn Processor, role: DeviceRole, presse: bool) -> Result<(), String> {
    thread::scope(|scope| {
        scope
            .spawn(|| {
                process_audio(audio, role)?;
                if presse {
                    check_presse_sidechain(audio)?;
                }
                Ok(())
            })
            .join()
            .map_err(|_| "direct audio worker panicked".to_owned())?
    })
}

fn drive_boundary(audio: &mut dyn Processor) {
    let mut context = smoke_context();
    context.offline = false;
    let input = [0.0; FRAMES];
    let mut left = [0.0; FRAMES];
    let mut right = [0.0; FRAMES];
    audio.process(
        &context,
        &[],
        &mut StereoIo {
            input: [&input, &input],
            output: [&mut left, &mut right],
        },
    );
}

fn smoke_bridge(
    config: &PluginManagerConfig,
    descriptor: &PluginDescriptor,
    path: &Path,
    role: DeviceRole,
) -> Result<(), String> {
    let id = DeviceId(1);
    let started = Bridge::spawn(SpawnSetup {
        exe: &config.bridge_exe,
        args: &config.bridge_args,
        device: id,
        role,
        format: PluginFormat::Clap,
        bundle: path,
        plugin_id: &descriptor.id,
        state: None,
        setup: SETUP,
        timing: Timing::default(),
        sidechain: descriptor.sidechain,
    })
    .map_err(|error| error.to_string())?;
    let mut bridge = started.bridge;
    let mut audio = started.processor;
    let result = (|| {
        wait_for_bridge(&mut bridge, |bridge, _, _| {
            Ok((bridge.status() == DeviceStatus::Ok).then_some(()))
        })?;
        if bridge.native_layout.is_none() || bridge.params.is_empty() {
            return Err("bridge did not report native layout and parameters".to_owned());
        }
        process_audio(&mut audio, role)?;
        if descriptor.id == PRESSE_ID {
            check_presse_sidechain(&mut audio)?;
        }
        let request = bridge
            .request_state(StatePurpose::Collect)
            .ok_or_else(|| "bridge refused the state snapshot".to_owned())?;
        let state = wait_for_bridge(&mut bridge, |_, _, states| {
            Ok(states
                .iter()
                .find(|(seq, _)| *seq == request)
                .map(|(_, state)| state.clone()))
        })?;
        if state.is_empty() || !bridge.set_state(&state) {
            return Err("bridge could not restore the plugin state".to_owned());
        }
        wait_for_bridge(&mut bridge, |bridge, events, _| {
            if let Some(error) = bridge.state_warning.as_ref() {
                return Err(error.clone());
            }
            Ok(events.iter().any(|event| {
                matches!(event, PluginManagerEvent::StateApplied { device } if *device == id)
            }).then_some(()))
        })
    })();
    drop(audio);
    bridge.stop(Duration::from_secs(1));
    if !bridge.wait_exit(Duration::from_secs(1)) {
        bridge.kill();
        let _ = bridge.wait_exit(Duration::from_secs(1));
    }
    result
}

fn wait_for_bridge<T>(
    bridge: &mut Bridge,
    mut until: impl FnMut(
        &Bridge,
        &[PluginManagerEvent],
        &[crate::bridge::Collected],
    ) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let deadline = Instant::now() + TIMEOUT;
    let mut events = Vec::new();
    let mut states = Vec::new();
    loop {
        bridge.poll(&mut events, &mut states);
        if let DeviceStatus::Crashed(error) = bridge.status() {
            return Err(format!("bridge failed: {error:?}"));
        }
        if let Some(value) = until(bridge, &events, &states)? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for bridge".to_owned());
        }
        events.clear();
        states.clear();
        thread::sleep(Duration::from_millis(2));
    }
}

fn smoke_context() -> ProcessContext {
    ProcessContext {
        frames: FRAMES as u32,
        sample_rate: SETUP.sample_rate,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Default::default(),
            position_ticks: 0.0,
            loop_range: Default::default(),
        },
        offline: true,
        deadline: None,
    }
}

fn process_audio(processor: &mut dyn Processor, role: DeviceRole) -> Result<(), String> {
    let context = smoke_context();
    let note = [Event {
        offset: 0,
        kind: EventKind::NoteOn {
            key: 60,
            velocity: 102,
            channel: 0,
            note_id: 1,
        },
    }];
    if role == DeviceRole::MidiEffect {
        let mut buffer = EventBuffer::new(128);
        let mut current = context;
        // Hold the note across one beat so a sequenced MIDI processor can reach a grid boundary.
        let frames_per_beat = f64::from(current.sample_rate) * 60.0 / current.transport.tempo_bpm;
        let blocks = (frames_per_beat / f64::from(current.frames)).ceil() as u32 + 1;
        let baseline_failures = processor.offline_failures();
        let stage = processor
            .event_processor()
            .ok_or_else(|| "missing MIDI stage adapter".to_owned())?;
        let mut heard_note = false;
        for block in 0..blocks {
            current.transport.position_ticks =
                f64::from(block * current.frames) * komponist_model::PPQ as f64 / frames_per_beat;
            let events = if block == 0 { &note[..] } else { &[] };
            stage.process_events(&current, events, &mut buffer.sink(current.frames));
            if buffer
                .events()
                .iter()
                .any(|event| matches!(event.kind, EventKind::NoteOn { .. }))
            {
                heard_note = true;
                break;
            }
        }
        if !heard_note || processor.offline_failures() != baseline_failures {
            return Err("MIDI effect did not produce a valid note".to_owned());
        }
        return Ok(());
    }
    let mut input = [0.0; FRAMES];
    input[0] = 0.125;
    let mut left = [f32::NAN; FRAMES];
    let mut right = [f32::NAN; FRAMES];
    let events = if role == DeviceRole::Instrument {
        &note[..]
    } else {
        &[]
    };
    let status = processor.process(
        &context,
        events,
        &mut StereoIo {
            input: [&input, &input],
            output: [&mut left, &mut right],
        },
    );
    if status != ProcessStatus::Continue
        || processor.offline_failures() != 0
        || left.iter().chain(&right).any(|x| !x.is_finite())
    {
        return Err("plugin did not render a valid block".to_owned());
    }
    Ok(())
}

/// With factory defaults, Presse compresses a loud main input only when the detector sees it.
/// An attached silent source keeps the same main input at unity.
fn check_presse_sidechain(processor: &mut dyn Processor) -> Result<(), String> {
    let context = smoke_context();
    let input = std::array::from_fn::<_, FRAMES, _>(|i| if i < FRAMES / 2 { 0.75 } else { -0.75 });
    let silent = [0.0; FRAMES];
    let mut render = |sidechain| {
        processor.reset();
        let mut left = [0.0; FRAMES];
        let mut right = [0.0; FRAMES];
        for _ in 0..32 {
            let mut io = StereoIo {
                input: [&input, &input],
                output: [&mut left, &mut right],
            };
            let status = match sidechain {
                Some(sidechain) => processor.process_sidechain(&context, &[], &mut io, sidechain),
                None => processor.process(&context, &[], &mut io),
            };
            if status != ProcessStatus::Continue
                || processor.offline_failures() != 0
                || left.iter().chain(&right).any(|x| !x.is_finite())
            {
                return Err("Presse sidechain check did not render a valid block".to_owned());
            }
        }
        Ok(left
            .into_iter()
            .chain(right)
            .map(f32::abs)
            .fold(0.0, f32::max))
    };
    let disconnected = render(None)?;
    let connected_silent = render(Some([silent.as_slice(); 2]))?;
    if connected_silent < 0.70 || disconnected > connected_silent * 0.75 {
        return Err(format!(
            "Presse sidechain did not distinguish disconnection from silence (peaks {disconnected:.3}, {connected_silent:.3})"
        ));
    }
    Ok(())
}
