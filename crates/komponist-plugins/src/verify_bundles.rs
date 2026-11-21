//! Release smoke test using the same formats and bridge processes as the application.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, StereoIo, TransportInfo,
};
use komponist_ipc::{PluginDescriptor, PluginFormat, PluginKind, ProcessExit};
use komponist_model::{DeviceId, DeviceRole};

use crate::bridge::{Bridge, SpawnSetup, StatePurpose, Timing};
use crate::bundled_clap::{BundledClap, VerifiedClap};
use crate::catalog::SCAN_TIMEOUT;
use crate::direct::DirectClap;
use crate::manager::{DeviceStatus, PluginManagerConfig, PluginManagerEvent, ProcessorSetup};
use crate::scan::scan_bundle;

const HALL_ID: &str = "org.klang-studio.komponist.hall";
const FRAMES: usize = 128;
const SETUP: ProcessorSetup = ProcessorSetup {
    sample_rate: 48_000.0,
    max_block: FRAMES,
};
const TIMEOUT: Duration = Duration::from_secs(15);

/// Verify every artifact named by the app-bound manifest. Both routes use private snapshots of
/// the unpacked artifacts so the scan and instance open the same authenticated bytes.
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
            let descriptor = scan(&config, verified.bundle_path(), id)?;
            let role = match descriptor.kind {
                PluginKind::Effect => DeviceRole::Effect,
                PluginKind::Instrument => DeviceRole::Instrument,
                PluginKind::MidiEffect | PluginKind::Other => {
                    return Err("MIDI-only bundle verification is not yet supported".to_owned());
                }
            };
            if force_bridge {
                smoke_bridge(&config, &descriptor, &verified, role)
            } else {
                smoke_direct(verified, role)
            }
        };
        result().map_err(|error| format!("{id}: {error}"))?;
        let route = if force_bridge { "bridge" } else { "in-process" };
        println!("verified {id} ({route})");
    }
    Ok(())
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
    let hall = bundle.plugin_id() == HALL_ID;
    let mut host = DirectClap::load(bundle).map_err(|error| error.to_string())?;
    if host.plugin().native_layout().is_none() || host.plugin().params().is_empty() {
        return Err("native layout or parameters unavailable".to_owned());
    }
    let mut audio = host
        .activate(SETUP, role)
        .map_err(|error| error.to_string())?;
    let result = process_audio(&mut audio, role, hall);
    host.deactivate(audio);
    result?;
    host.poll();
    let state = host
        .plugin_mut()
        .save_state()
        .map_err(|error| error.to_string())?;
    if state.is_empty() {
        return Err("plugin saved an empty state".to_owned());
    }
    host.plugin_mut()
        .load_state(&state)
        .map_err(|error| error.to_string())
}

fn smoke_bridge(
    config: &PluginManagerConfig,
    descriptor: &PluginDescriptor,
    bundle: &VerifiedClap,
    role: DeviceRole,
) -> Result<(), String> {
    let id = DeviceId(1);
    let started = Bridge::spawn(SpawnSetup {
        exe: &config.bridge_exe,
        args: &config.bridge_args,
        device: id,
        role,
        format: PluginFormat::Clap,
        bundle: bundle.bundle_path(),
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
        process_audio(&mut audio, role, bundle.plugin_id() == HALL_ID)?;
        let request = bridge
            .request_state(StatePurpose::Collect)
            .ok_or_else(|| "bridge refused the state snapshot".to_owned())?;
        let state = wait_for_bridge(&mut bridge, |_, _, states| {
            Ok(states
                .iter()
                .find(|(seq, _)| *seq == request)
                .map(|(_, state)| state.clone()))
        })?;
        if state.is_empty() {
            return Err("bridge saved an empty plugin state".to_owned());
        }
        let restore = bridge
            .request_restore(&state)
            .ok_or_else(|| "bridge refused the state restore".to_owned())?;
        wait_for_bridge(&mut bridge, |bridge, _, _| {
            bridge.restore_result(restore).transpose()
        })
    })();
    drop(audio);
    let grace = Duration::from_secs(1);
    bridge.stop(grace);
    let shutdown = bridge.wait_exit_status(grace);
    if !matches!(shutdown, Ok(Some(_))) {
        bridge.kill();
        let _ = bridge.wait_exit(grace);
    }
    result?;
    match shutdown {
        Ok(Some(ProcessExit {
            code: Some(0),
            signal: None,
        })) => Ok(()),
        Ok(Some(exit)) => Err(format!(
            "bridge exited abnormally during shutdown: {exit:?}"
        )),
        Ok(None) => Err("bridge timed out during shutdown".to_owned()),
        Err(error) => Err(format!("could not wait for bridge shutdown: {error}")),
    }
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

fn process_audio(
    processor: &mut dyn Processor,
    role: DeviceRole,
    hall: bool,
) -> Result<(), String> {
    let context = ProcessContext {
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
    };
    let note = [Event {
        offset: 0,
        kind: EventKind::NoteOn {
            key: 60,
            velocity: 102,
            channel: 0,
            note_id: 1,
        },
    }];
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
