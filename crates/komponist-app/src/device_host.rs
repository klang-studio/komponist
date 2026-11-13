//! Where the processors of devices come from: the seam between `EngineSync` and plugin hosting.
//!
//! [`EngineSync`](crate::engine_sync::EngineSync) asks a [`DeviceHost`] for a processor whenever a
//! device gets a slot, tells it when a device leaves, reads latencies for the compiler and status
//! for the UI, and forwards its events every frame.
//!
//! Until the plugin manager (WP-45) lands, [`BuiltinHost`] is the only implementation: pisynth
//! runs in process ([`PisynthProcessor`]); every other device (klopfer, CLAP, VST3) gets no
//! processor, so the engine passes an effect's input through and an instrument stays silent, and
//! its status is [`DeviceStatus::Missing`]. WP-45 implements [`DeviceHost`] for a type that wraps
//! `komponist_plugins::PluginManager` (bridges for external devices, `Loading` / `Crashed`
//! statuses, state snapshots) and keeps [`BuiltinHost`] for pisynth, then swaps it in with
//! [`crate::app::App::with_device_host`].
//!
//! Thread: UI thread.

use std::collections::HashMap;

use komponist_builtin::PisynthProcessor;
use komponist_engine::Processor;
use komponist_model::project::{BUILTIN_KLOPFER, BUILTIN_PISYNTH};
use komponist_model::{Device, DeviceId, DeviceRole, PluginFormat, PluginRef};
use komponist_plugins::{DeviceStatus, FailReason, PluginManagerEvent, ProcessorSetup};

/// Creates processors for devices and reports their runtime status.
pub trait DeviceHost {
    /// Creates the processor for `device`, already activated for `setup`, for
    /// `Command::InsertProcessor`. `None`: the device plays without a processor (passthrough for
    /// an effect, silence for an instrument); its [`DeviceHost::status`] says why.
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>>;

    /// `device` left the program; its processor (if any) is being removed from the engine.
    fn release(&mut self, device: DeviceId);

    /// Runtime status of `device` (device cards, track headers, insert slots); `Ok` for devices
    /// the host has not seen.
    fn status(&self, device: DeviceId) -> DeviceStatus;

    /// Latency of `device`'s processor in frames (`compile::CompileEnv::device_latency`).
    fn latency(&self, device: DeviceId) -> u32;

    /// Called every UI frame; returns what happened since the last call (status changes, state
    /// snapshots, latency changes, …). Never blocks.
    fn poll(&mut self) -> Vec<PluginManagerEvent>;

    /// Instrument or effect, for a plugin from the browser; `None` if unknown (not in the
    /// catalog).
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole>;
}

/// Whether `plugin` is one of the built-in instruments (pisynth, klopfer).
pub fn is_builtin_instrument(plugin: &PluginRef) -> bool {
    plugin.format == PluginFormat::Builtin
        && (plugin.id == BUILTIN_PISYNTH || plugin.id == BUILTIN_KLOPFER)
}

/// Whether `device` is pisynth, the in-process built-in.
pub fn is_pisynth(device: &Device) -> bool {
    device.plugin.format == PluginFormat::Builtin && device.plugin.id == BUILTIN_PISYNTH
}

/// The host before WP-45: pisynth in process, everything else without processor (`Missing`).
#[derive(Debug, Default)]
pub struct BuiltinHost {
    devices: HashMap<DeviceId, Known>,
    events: Vec<PluginManagerEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Known {
    status: DeviceStatus,
    latency: u32,
}

impl BuiltinHost {
    /// A host that has seen no device.
    pub fn new() -> BuiltinHost {
        BuiltinHost::default()
    }

    fn set(&mut self, device: DeviceId, status: DeviceStatus, latency: u32) {
        let changed = self.devices.get(&device).map(|k| &k.status) != Some(&status);
        if changed {
            self.events.push(PluginManagerEvent::StatusChanged {
                device,
                status: status.clone(),
            });
        }
        self.devices.insert(device, Known { status, latency });
    }
}

impl DeviceHost for BuiltinHost {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        if !is_pisynth(device) {
            self.set(device.id, DeviceStatus::Missing, 0);
            return None;
        }
        // A state that is not a pisynth patch plays "Init" rather than nothing; the state stays
        // in the document untouched.
        let mut processor = PisynthProcessor::from_state(device.state.as_deref())
            .unwrap_or_else(|_| PisynthProcessor::new(komponist_builtin::default_patch()));
        match processor.activate(setup.sample_rate, setup.max_block) {
            Ok(()) => {
                self.set(device.id, DeviceStatus::Ok, processor.latency());
                Some(Box::new(processor))
            }
            Err(e) => {
                self.set(
                    device.id,
                    DeviceStatus::Crashed(FailReason::LoadFailed(e.to_string())),
                    0,
                );
                None
            }
        }
    }

    fn release(&mut self, device: DeviceId) {
        self.devices.remove(&device);
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        self.devices
            .get(&device)
            .map_or(DeviceStatus::Ok, |k| k.status.clone())
    }

    fn latency(&self, device: DeviceId) -> u32 {
        self.devices.get(&device).map_or(0, |k| k.latency)
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        std::mem::take(&mut self.events)
    }

    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        // The built-ins are instruments; everything else needs the catalog (WP-45).
        is_builtin_instrument(plugin).then_some(DeviceRole::Instrument)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    const SETUP: ProcessorSetup = ProcessorSetup {
        sample_rate: 48_000.0,
        max_block: komponist_engine::MAX_BLOCK,
    };

    #[test]
    fn pisynth_plays_and_everything_else_is_missing() {
        let project = demo_project();
        let mut host = BuiltinHost::new();
        let mut made = Vec::new();
        for track in &project.tracks {
            for device in &track.channel.devices {
                let p = host.create_processor(device, SETUP);
                made.push((
                    device.plugin.name.clone(),
                    p.is_some(),
                    host.status(device.id),
                ));
            }
        }
        for (name, has_processor, status) in &made {
            if name == "pisynth" {
                assert!(has_processor);
                assert_eq!(*status, DeviceStatus::Ok);
            } else {
                assert!(!has_processor, "{name}");
                assert_eq!(*status, DeviceStatus::Missing, "{name}");
            }
        }
        let events = host.poll();
        assert_eq!(events.len(), made.len(), "one status event per device");
        assert!(host.poll().is_empty());
        // Unknown devices are Ok, released devices forgotten.
        let lead = &project.tracks[2].channel.devices[0];
        host.release(lead.id);
        assert_eq!(host.status(lead.id), DeviceStatus::Ok);
        assert_eq!(host.latency(lead.id), 0);
    }
}
