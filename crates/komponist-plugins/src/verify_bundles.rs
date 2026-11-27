//! Release smoke test: loads every bundled device in-process (D11) and checks that an external copy with a bundled ID is bridged without trust.

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

use crate::bundled_clap::{BundledClap, VerifiedClap, snapshot_cache};
use crate::bundled_effects::Meter;
use crate::catalog::{PluginDirs, PluginLocation, SCAN_CACHE_FILE, SCAN_TIMEOUT};
use crate::direct::DirectClap;
use crate::direct_session::DirectSession;
use crate::manager::{
    DeviceStatus, PluginManager, PluginManagerConfig, PluginManagerEvent, ProcessorSetup,
};
use crate::scan::{bundle_key, scan_bundle};

const HALL_ID: &str = "org.klang-studio.komponist.hall";
const PRESSE_ID: &str = "org.klang-studio.komponist.presse";
const FRAMES: usize = 128;
const SETUP: ProcessorSetup = ProcessorSetup {
    sample_rate: 48_000.0,
    max_block: FRAMES,
};
const TIMEOUT: Duration = Duration::from_secs(15);

/// Verify every artifact named by the app-bound manifest. Both routes use private snapshots of
/// the unpacked artifacts so the scan and instance open the same authenticated bytes.
///
/// `cache_dir` is the app's cache folder (the scan cache and the private snapshots live below it).
pub fn verify_bundled_clap(cache_dir: &Path) -> Result<(), String> {
    let mut config = PluginManagerConfig::defaults(cache_dir);
    let cache_root = snapshot_cache(cache_dir).map_err(|error| error.to_string())?;
    let scratch = tempfile::Builder::new()
        .prefix("bundled-clap-smoke-")
        .tempdir_in(cache_root)
        .map_err(|error| format!("could not create a private smoke cache: {error}"))?;
    config.cache_file = scratch.path().join(SCAN_CACHE_FILE);
    let bundled = BundledClap::discover(scratch.path())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "this executable has no embedded bundled CLAP manifest hash".to_owned())?;
    for (path, id) in bundled.bundle_entries() {
        let result = || {
            let verified = bundled
                .verify(path, id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "bundle was not found in its own manifest".to_owned())?;
            let descriptor = scan(&config, verified.bundle_path(), id)?;
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
            smoke_direct(verified, &descriptor, role)
        };
        result().map_err(|error| format!("{id}: {error}"))?;
        println!("verified {id} (in-process)");
    }
    if let Some((path, id)) = bundled.bundle_entries().find(|(_, id)| *id == HALL_ID) {
        // The same loadable bytes and CLAP ID outside the app inventory are an external plugin.
        let copy = bundled
            .verify(path, id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Hall is absent from its own manifest".to_owned())?;
        smoke_live_manager(config, path, &bundle_key(copy.bundle_path())?, id)?;
        println!("verified {id} (normal direct lifecycle)");
    }
    if let Some(digest) = option_env!("KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256") {
        println!("manifest sha256 {digest}");
    }
    Ok(())
}

fn smoke_live_manager(
    config: PluginManagerConfig,
    path: &Path,
    external: &Path,
    id: &str,
) -> Result<(), String> {
    let mut manager = PluginManager::new_with_bundled_clap(config, true);
    manager.start_scan(&PluginDirs {
        clap: vec![
            external
                .parent()
                .expect("external bundle has a parent directory")
                .into(),
        ],
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
    let resolve_device = |path: &Path, device_id| {
        let entry = manager.catalog().entries().iter().find(|entry| {
            entry.plugin.id == id
                && matches!(&entry.location, PluginLocation::Bundle { path: scanned, .. } if scanned == path)
        }).ok_or_else(|| format!("Hall is absent from the catalog at {}", path.display()))?;
        Ok::<_, String>(Device {
            id: device_id,
            role: DeviceRole::Effect,
            plugin: entry.plugin.clone(),
            bypass: false,
            parameter_bases: Default::default(),
            asset_links: Default::default(),
            state: None,
            sidechain: None,
            preset: None,
        })
    };
    let device = resolve_device(path, DeviceId(1))?;
    let duplicate = resolve_device(external, DeviceId(2))?;
    let external_audio = manager
        .create_processor(&duplicate, SETUP)
        .map_err(|error| format!("external Hall with bundled CLAP ID did not load: {error}"))?;
    wait_for_manager_bridge(
        &mut manager,
        duplicate.id,
        "external Hall with bundled CLAP ID",
    )?;
    if manager.verified_bundle(duplicate.id).is_some() {
        return Err("external Hall with bundled CLAP ID was granted bundle trust".into());
    }
    drop(external_audio);
    manager.release(duplicate.id);

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
    if !manager.restore_saved_state(device_id, &initial) {
        return Err("normal Hall did not accept an undo state".into());
    }
    if snapshot(&mut manager).as_ref() != Some(&initial) {
        return Err("normal Hall save did not service the pending undo".into());
    }
    if !manager.poll().iter().any(|event| {
        matches!(event,
        PluginManagerEvent::StateApplied { device } if *device == device_id)
    }) || manager.param_base(device_id, param_id) != Some(baseline)
    {
        return Err("normal Hall undo did not restore its parameter base".into());
    }

    if !manager.restore_saved_state(device_id, &edited) {
        return Err("normal Hall did not queue a restore before reload".into());
    }
    let replacement = manager
        .reload(&device, SETUP)
        .map_err(|error| error.to_string())?;
    if manager.param_base(device_id, param_id) != Some(edit) {
        return Err("normal Hall reload ignored the pending restore".into());
    }
    drop(audio);
    manager.poll();
    let audio = replacement;
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

fn wait_for_manager_bridge(
    manager: &mut PluginManager,
    device: DeviceId,
    label: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        manager.poll();
        match manager.status(device) {
            DeviceStatus::Ok if manager.bridge_pid(device).is_some() => return Ok(()),
            DeviceStatus::Loading if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            status => return Err(format!("{label} did not start in the bridge: {status:?}")),
        }
    }
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

fn expected_signal_taps(plugin_id: &str) -> &'static [&'static str] {
    match plugin_id {
        "org.klang-studio.komponist.eq" => &["spectrum"],
        PRESSE_ID => &["detector", "reduction"],
        "org.klang-studio.komponist.limiter" => &["reduction"],
        _ => &[],
    }
}

fn smoke_direct(
    bundle: VerifiedClap,
    descriptor: &PluginDescriptor,
    role: DeviceRole,
) -> Result<(), String> {
    let meter = Meter::new(&descriptor.features, SETUP.sample_rate);
    let hall = bundle.plugin_id() == HALL_ID;
    let presse = bundle.plugin_id() == PRESSE_ID;
    let expected_taps = expected_signal_taps(bundle.plugin_id());
    let mut host = DirectClap::load(bundle).map_err(|error| error.to_string())?;
    let can_render =
        host.try_control(|plugin| plugin.native_layout().is_some() && !plugin.params().is_empty());
    if can_render != Some(true) {
        return Err("native layout or parameters unavailable".to_owned());
    }
    let (mut session, mut audio) =
        DirectSession::start(host, SETUP, role).map_err(|error| error.to_string())?;
    if let Some(meter) = &meter {
        audio.set_effect_meter(meter.clone());
    }
    let result = (|| {
        drive_direct(&mut audio, role, hall, presse)?;
        if let Some(meter) = &meter {
            thread::scope(|scope| scope.spawn(|| check_effect_meter(&mut audio, meter)).join())
                .map_err(|_| "direct meter audio worker panicked".to_owned())??;
        }
        for &name in expected_taps {
            let tap = session
                .signal_tap(name)
                .ok_or_else(|| format!("direct session did not report signal tap {name}"))?;
            thread::scope(|scope| scope.spawn(|| check_named_tap(&mut audio, tap)).join())
                .map_err(|_| "direct signal tap worker panicked".to_owned())??;
        }
        audio
            .activate(SETUP.sample_rate, SETUP.max_block * 2)
            .map_err(|error| error.to_string())?;
        session.set_render_mode(RenderMode::Offline);
        // The detached engine must return the activation even before its first rendered block.
        audio.prepare_offline();
        session.poll().map_err(|error| error.to_string())?;
        drive_direct(&mut audio, role, hall, presse)
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

fn drive_direct(
    audio: &mut dyn Processor,
    role: DeviceRole,
    hall: bool,
    presse: bool,
) -> Result<(), String> {
    thread::scope(|scope| {
        scope
            .spawn(|| {
                process_audio(audio, role, hall)?;
                if presse {
                    check_presse_sidechain(audio)?;
                }
                Ok(())
            })
            .join()
            .map_err(|_| "direct audio worker panicked".to_owned())?
    })
}

fn check_named_tap(
    processor: &mut dyn Processor,
    tap: &crate::signal_taps::SignalTap,
) -> Result<(), String> {
    use komponist_ipc::signal_taps::SPECTRUM_SOURCE;
    let input = [2.0; FRAMES];
    let mut left = [0.0; FRAMES];
    let mut right = [0.0; FRAMES];
    let mut process = || {
        let context = ProcessContext {
            offline: false,
            deadline: Some(Instant::now() + Duration::from_secs(1)),
            ..smoke_context()
        };
        processor.process(
            &context,
            &[],
            &mut StereoIo {
                input: [&input, &input],
                output: [&mut left, &mut right],
            },
        )
    };
    let before = tap.samples().written();
    process();
    if tap.samples().written() != before {
        return Err("an unleased signal tap recorded samples".into());
    }
    tap.want();
    for _ in 0..16 {
        if process() == ProcessStatus::Failed {
            return Err("effect processor failed while feeding a named signal tap".into());
        }
    }
    let mut latest = [0.0; 4];
    if tap.samples().read_latest(&mut latest).is_none()
        || latest.iter().any(|sample| !sample.is_finite())
        || tap.descriptor().kind == SPECTRUM_SOURCE && latest.iter().all(|s| s.abs() <= 1e-4)
    {
        return Err(format!(
            "no usable signal from tap {:?}",
            tap.descriptor().key()
        ));
    }
    Ok(())
}

fn check_effect_meter(processor: &mut dyn Processor, meter: &Meter) -> Result<(), String> {
    let tap = meter.view.tap();
    if let Some(tap) = tap {
        tap.want();
    }
    let input = [2.0; FRAMES];
    let mut left = [0.0; FRAMES];
    let mut right = [0.0; FRAMES];
    let context = smoke_context();
    let baseline_failures = processor.offline_failures();
    for _ in 0..16 {
        let status = processor.process(
            &context,
            &[],
            &mut StereoIo {
                input: [&input, &input],
                output: [&mut left, &mut right],
            },
        );
        if status == ProcessStatus::Failed {
            return Err("effect processor failed while feeding its meter".to_owned());
        }
    }
    if processor.offline_failures() != baseline_failures {
        return Err("effect processor used offline fallback while feeding its meter".to_owned());
    }
    if meter.requests_reduction() {
        let (input, reduction, output) = meter.view.take();
        if input != 2.0 || reduction <= 0.0 || output <= 0.0 {
            return Err(format!(
                "effect did not report input, gain reduction and output: {input}, {reduction}, {output}",
            ));
        }
    }
    if let Some(tap) = tap {
        let mut samples = [0.0; 16];
        if tap.read_latest(&mut samples).is_none() {
            return Err("effect did not expose recent signal for its graph".to_owned());
        }
        let has_signal = if meter.requests_reduction() {
            // Presse reports pairs of detector level in dBFS and reduction in dB.
            samples.as_chunks::<2>().0.iter().any(|pair| pair[1] > 0.0)
        } else {
            samples.iter().any(|sample| sample.abs() > 1e-4)
        };
        if !has_signal {
            return Err("effect graph signal was silent during a non-silent block".to_owned());
        }
    }
    Ok(())
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

fn process_audio(
    processor: &mut dyn Processor,
    role: DeviceRole,
    hall: bool,
) -> Result<(), String> {
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
    let blocks = if hall { 128 } else { 1 };
    let mut has_tail = false;
    for block in 0..blocks {
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
        if block > 0 {
            has_tail |= left.iter().chain(&right).any(|sample| sample.abs() > 1e-7);
        }
        input[0] = 0.0;
    }
    if hall && !has_tail {
        return Err("Hall produced no reverb tail after the input impulse".to_owned());
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
    if connected_silent < 0.70 || disconnected < 1e-6 || disconnected > connected_silent * 0.75 {
        return Err(format!(
            "Presse sidechain did not distinguish disconnection from silence (peaks {disconnected:.3}, {connected_silent:.3})"
        ));
    }
    Ok(())
}
