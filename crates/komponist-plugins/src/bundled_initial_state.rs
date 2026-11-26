//! Prepare project state for verified built-in CLAPs before creating audio processors.

use std::collections::BTreeMap;
use std::sync::Arc;

use komponist_ipc::clap_presets::ClapPresetLocation;
use komponist_ipc::{PluginKind, WireEvent, WireEventKind};
use komponist_model::params::ParamId;
use komponist_model::{Device, DeviceId, DeviceRole, PluginFormat, PluginRef};

use crate::bundled_clap::BundledClap;
use crate::catalog::bundled_module;
use crate::direct::DirectClap;

/// An initialized device and the authored controls requested for project automation.
pub struct InitialDevice {
    pub plugin: PluginRef,
    pub role: DeviceRole,
    pub state: Option<Arc<[u8]>>,
    pub targets: BTreeMap<String, (ParamId, f32)>,
}

impl InitialDevice {
    pub fn into_device(self, id: DeviceId) -> Device {
        Device {
            id,
            role: self.role,
            plugin: self.plugin,
            bypass: false,
            state: self.state,
            asset_links: Default::default(),
            parameter_bases: Default::default(),
            sidechain: None,
        }
    }
}

pub(crate) fn prepare(
    inventory: &BundledClap,
    plugin_id: &str,
    preset: Option<&str>,
    edits: &[(&str, f64)],
    targets: &[&str],
) -> Result<InitialDevice, String> {
    let (path, _) = inventory
        .bundle_entries()
        .find(|(_, id)| *id == plugin_id)
        .ok_or("bundle unavailable in this package")?;
    let proof = inventory
        .verify(path, plugin_id)
        .map_err(|error| error.to_string())?
        .ok_or("bundle unavailable in this package")?;
    let module = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid bundle filename")?;
    let preset = preset
        .map(|name| {
            komponist_hosting::discover_clap_presets(proof.bundle_path())
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|preset| {
                    preset.plugin_id == plugin_id
                        && preset.factory
                        && preset.source.location == ClapPresetLocation::Plugin
                        && preset.name == name
                })
                .ok_or_else(|| format!("factory preset {name} is unavailable"))
        })
        .transpose()?;
    let mut instance = DirectClap::load(proof).map_err(|error| error.to_string())?;
    instance
        .try_control(|plugin| {
            let descriptor = plugin.descriptor();
            let role = match descriptor.kind {
                PluginKind::Instrument => DeviceRole::Instrument,
                PluginKind::Effect => DeviceRole::Effect,
                PluginKind::MidiEffect => DeviceRole::MidiEffect,
                PluginKind::Other
                    if descriptor
                        .features
                        .iter()
                        .any(|feature| feature == "note-effect") =>
                {
                    DeviceRole::MidiEffect
                }
                PluginKind::Other => return Err("unsupported bundled device role".into()),
            };
            let reference = PluginRef {
                format: PluginFormat::Clap,
                id: descriptor.id.clone(),
                name: descriptor.name.clone(),
                vendor: descriptor.vendor.clone(),
                version: descriptor.version.clone(),
                module: bundled_module(module),
            };
            if let Some(preset) = &preset {
                plugin
                    .load_clap_preset(preset)
                    .map_err(|error| error.to_string())?;
            }
            let inventory = plugin.params();
            let mapper = |author: &str| {
                let (_, layout) = plugin.native_layout()?;
                let id = layout.author_ids()?.get(author)?;
                inventory.iter().find(|param| param.id == *id)
            };
            let input = edits
                .iter()
                .map(|&(author, value)| {
                    let info = mapper(author).ok_or_else(|| format!("unknown control {author}"))?;
                    if !(info.min..=info.max).contains(&value) {
                        return Err(format!("value {value} is outside the range for {author}"));
                    }
                    Ok(WireEvent::new(
                        0,
                        WireEventKind::ParamValue { id: info.id, value },
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let bindings = targets
                .iter()
                .map(|&author| {
                    let info = mapper(author)
                        .filter(|info| crate::params::is_automatable(info))
                        .ok_or_else(|| format!("control {author} is not automatable"))?;
                    Ok((author, info))
                })
                .collect::<Result<Vec<_>, String>>()?;
            if !input.is_empty() {
                plugin.flush_params(&input, &mut Vec::new());
            }
            let targets = bindings
                .into_iter()
                .map(|(author, info)| {
                    let value = plugin
                        .param_value(info.id)
                        .filter(|value| (info.min..=info.max).contains(value))
                        .ok_or_else(|| format!("control {author} has no current value"))?;
                    let normalized = ((value - info.min) / (info.max - info.min)) as f32;
                    Ok((author.to_owned(), (ParamId::plugin(info.id), normalized)))
                })
                .collect::<Result<_, String>>()?;
            let state = plugin.save_state().map_err(|error| error.to_string())?;
            Ok(InitialDevice {
                plugin: reference,
                role,
                state: Some(state.into()),
                targets,
            })
        })
        .ok_or_else(|| "bundled CLAP control is busy".to_owned())?
}
