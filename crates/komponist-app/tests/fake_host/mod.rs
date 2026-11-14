//! A device host for the browser and device-chain tests: pisynth in process (through
//! `BuiltinHost`), every other device with a status, parameters, values, a GUI flag and a state
//! warning the test chooses, and a catalog with the plugins of S01/S05. Statuses can change while
//! the app runs ([`FakeHost::statuses`]); parameter changes are recorded ([`FakeHost::set`]).

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use komponist_app::device_host::{BuiltinHost, DeviceHost, is_pisynth};
use komponist_engine::Processor;
use komponist_ipc::control::param_flags;
use komponist_ipc::{
    ParamInfo, ParamValue, PluginDescriptor, PluginFormat as BundleFormat, PluginKind,
};
use komponist_model::{Device, DeviceId, DeviceRole, PluginFormat, PluginRef};
use komponist_plugins::{Catalog, DeviceStatus, PluginManagerEvent, ProcessorSetup};

/// Statuses the test can change while the app runs.
pub type Statuses = Arc<Mutex<HashMap<DeviceId, DeviceStatus>>>;

/// Recorded `set_param` calls.
pub type SetLog = Arc<Mutex<Vec<(DeviceId, u32, f64)>>>;

#[derive(Default)]
pub struct FakeHost {
    builtin: BuiltinHost,
    catalog: Catalog,
    pub statuses: Statuses,
    params: HashMap<DeviceId, Vec<ParamInfo>>,
    values: HashMap<(DeviceId, u32), ParamValue>,
    warnings: HashMap<DeviceId, String>,
    gui: HashSet<DeviceId>,
    pub set: SetLog,
}

/// A scanned CLAP plugin (format CLAP, id = `id`).
pub fn descriptor(id: &str, name: &str, kind: PluginKind, features: &[&str]) -> PluginDescriptor {
    PluginDescriptor {
        format: BundleFormat::Clap,
        id: id.into(),
        name: name.into(),
        vendor: "Test".into(),
        version: "1.0".into(),
        kind,
        features: features.iter().map(|f| (*f).into()).collect(),
        bundle: format!("/plugins/{id}.clap").into(),
    }
}

/// pisynth, klopfer and the plugins of S01/S05.
pub fn screens_catalog() -> Catalog {
    let mut c = Catalog::builtin(Some("/plugins/Klopfer.clap".into()));
    c.set_scanned(&[
        descriptor(
            "surge",
            "Surge XT",
            PluginKind::Instrument,
            &["synthesizer"],
        ),
        descriptor("vital", "Vital", PluginKind::Instrument, &["synthesizer"]),
        descriptor("dexed", "Dexed", PluginKind::Instrument, &["synthesizer"]),
        descriptor("obxd", "OB-Xd", PluginKind::Instrument, &["synthesizer"]),
        descriptor("eq", "LSP Para EQ x8", PluginKind::Effect, &["equalizer"]),
        descriptor(
            "comp",
            "LSP Compressor",
            PluginKind::Effect,
            &["compressor"],
        ),
        descriptor(
            "hall",
            "Dragonfly Hall Reverb",
            PluginKind::Effect,
            &["reverb"],
        ),
        descriptor(
            "plate",
            "Dragonfly Plate Reverb",
            PluginKind::Effect,
            &["reverb"],
        ),
        descriptor("room", "LSP Room Builder", PluginKind::Effect, &["reverb"]),
        descriptor(
            "early",
            "Dragonfly Early Reflections",
            PluginKind::Effect,
            &["reverb"],
        ),
        descriptor("delay", "LSP Delay", PluginKind::Effect, &["delay"]),
        descriptor("chorus", "TAL-Chorus-LX", PluginKind::Effect, &["chorus"]),
        descriptor(
            "airw",
            "Airwindows Consolidated",
            PluginKind::Effect,
            &["distortion"],
        ),
    ]);
    c
}

/// The reference of catalog entry `id` in `catalog`.
pub fn plugin(catalog: &Catalog, id: &str) -> PluginRef {
    catalog
        .entries()
        .iter()
        .find(|e| e.plugin.id == id)
        .unwrap_or_else(|| panic!("{id} in the catalog"))
        .plugin
        .clone()
}

/// A CLAP reference that is not in any catalog.
pub fn unknown(id: &str, name: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: id.into(),
        name: name.into(),
        vendor: "Test".into(),
        version: "1.0".into(),
    }
}

/// A normalised automatable parameter.
pub fn param(id: u32, name: &str, default: f64) -> ParamInfo {
    ParamInfo {
        id,
        name: name.into(),
        module: String::new(),
        min: 0.0,
        max: 1.0,
        default,
        step_count: 0,
        flags: param_flags::AUTOMATABLE,
    }
}

impl FakeHost {
    /// A host with [`screens_catalog`].
    pub fn new() -> FakeHost {
        FakeHost {
            catalog: screens_catalog(),
            ..FakeHost::default()
        }
    }

    /// `device` reports `status`.
    pub fn status(self, device: DeviceId, status: DeviceStatus) -> FakeHost {
        self.statuses.lock().unwrap().insert(device, status);
        self
    }

    /// `device` has `params`, with the values and texts in `values` (`(value, text)` per param).
    pub fn params(
        mut self,
        device: DeviceId,
        params: Vec<ParamInfo>,
        values: &[(f64, &str)],
    ) -> FakeHost {
        for (p, (value, text)) in params.iter().zip(values) {
            self.values.insert(
                (device, p.id),
                ParamValue {
                    id: p.id,
                    value: *value,
                    text: (*text).into(),
                },
            );
        }
        self.params.insert(device, params);
        self
    }

    /// `device`'s stored state did not load.
    pub fn warning(mut self, device: DeviceId, why: &str) -> FakeHost {
        self.warnings.insert(device, why.into());
        self
    }

    /// `device` has its own GUI.
    pub fn gui(mut self, device: DeviceId) -> FakeHost {
        self.gui.insert(device);
        self
    }
}

impl DeviceHost for FakeHost {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        if is_pisynth(device) {
            return self.builtin.create_processor(device, setup);
        }
        self.statuses
            .lock()
            .unwrap()
            .entry(device.id)
            .or_insert(DeviceStatus::Ok);
        None
    }

    fn release(&mut self, device: DeviceId) {
        self.builtin.release(device);
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        match self.statuses.lock().unwrap().get(&device) {
            Some(s) => s.clone(),
            None => self.builtin.status(device),
        }
    }

    fn latency(&self, _device: DeviceId) -> u32 {
        0
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.builtin.poll()
    }

    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        match self.catalog.resolve(plugin)?.kind {
            PluginKind::Instrument => Some(DeviceRole::Instrument),
            PluginKind::Effect => Some(DeviceRole::Effect),
            PluginKind::Other => None,
        }
    }

    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        self.params.get(&device).map(Vec::as_slice)
    }

    fn param_value(&self, device: DeviceId, id: u32) -> Option<&ParamValue> {
        self.values.get(&(device, id))
    }

    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        if !self.params.contains_key(&device) {
            return false;
        }
        self.set.lock().unwrap().push((device, id, value));
        true
    }

    fn state_warning(&self, device: DeviceId) -> Option<&str> {
        self.warnings.get(&device).map(String::as_str)
    }

    fn has_gui(&self, device: DeviceId) -> bool {
        self.gui.contains(&device)
    }
}
