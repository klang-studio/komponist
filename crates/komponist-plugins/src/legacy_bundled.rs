//! Plan and transform a project copy for migration from legacy built-ins to bundled CLAPs.

use std::collections::BTreeMap;
use std::path::Path;

use komponist_builtin::{BuiltinKind, Internal};
use komponist_device_core::pinned_clap_state;
use komponist_hexa::{
    self as hexa,
    device::{Field, WIRE_COUNT},
};
use komponist_hosting::HostedPlugin;
use komponist_i18n::Lang;
use komponist_ipc::control::param_flags;
use komponist_ipc::device_assets::AssetKind;
use komponist_ipc::native_layout::NativeLayout;
use komponist_ipc::{
    ParamInfo as ClapParamInfo, PluginDescriptor, PluginFormat as ClapFormat, PluginKind,
};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale};
use komponist_model::project::{Device, DeviceRole, PluginFormat, PluginRef};
use serde_json::{Map, Value};

use crate::bundled_clap::BundledClap;
use crate::catalog::bundled_module;
use crate::direct::DirectClap;

mod project;
pub use project::{LegacyProjectMigration, migrate_legacy_bundled_project};

/// A verified migration plan for the exact source device snapshot.
#[derive(Debug, Clone)]
pub struct LegacyDevicePlan {
    source: Device,
    replacement: Device,
    legacy_preset: Option<String>,
    params: BTreeMap<ParamId, LegacyParameter>,
}

/// A validated parameter address and value domain in the bundled wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyParameter {
    /// The new project parameter address for this bundle.
    pub id: ParamId,
    steps: u32,
    automation_issue: Option<&'static str>,
}

impl LegacyParameter {
    /// Convert a stored normalized automation position into an authored CLAP
    /// value. Interpolation positions themselves are retained unchanged.
    pub fn clap_plain(&self, normalized: f64) -> f64 {
        let position = normalized.clamp(0.0, 1.0);
        if self.steps == 0 {
            position
        } else {
            (position * f64::from(self.steps)).round()
        }
    }
}

impl LegacyDevicePlan {
    /// The replacement retains the source instance ID, bypass, and sidechain.
    pub fn replacement(&self) -> &Device {
        &self.replacement
    }

    /// The preset label read from the legacy device state, if present.
    pub fn legacy_preset(&self) -> Option<&str> {
        self.legacy_preset.as_deref()
    }

    /// Resolve a legacy automation or macro target within this device.
    pub fn parameter(&self, id: &ParamId) -> Option<&LegacyParameter> {
        self.params.get(id)
    }
}

pub(crate) fn clap_id(kind: BuiltinKind) -> &'static str {
    match kind {
        BuiltinKind::Hexa => "org.klang-studio.komponist.hexa",
        BuiltinKind::Internal(Internal::Sampler) => "org.klang-studio.komponist.sampler",
        BuiltinKind::Internal(Internal::GeneralSampler) => {
            "org.klang-studio.komponist.general-sampler"
        }
        BuiltinKind::Internal(Internal::Eq) => "org.klang-studio.komponist.eq",
        BuiltinKind::Internal(Internal::Delay) => "org.klang-studio.komponist.delay",
        BuiltinKind::Internal(Internal::Hall) => "org.klang-studio.komponist.hall",
        BuiltinKind::Internal(Internal::Limiter) => "org.klang-studio.komponist.limiter",
        BuiltinKind::Internal(Internal::Presse) => "org.klang-studio.komponist.presse",
        BuiltinKind::Internal(Internal::Arpeggiator) => "org.klang-studio.komponist.arpeggiator",
        BuiltinKind::Internal(Internal::Tonleiter) => "org.klang-studio.komponist.scale",
        BuiltinKind::Internal(Internal::Volt) => "org.klang-studio.komponist.volt",
        BuiltinKind::Internal(Internal::Surf) => "org.klang-studio.komponist.surf",
    }
}

/// Look up the canonical bundled CLAP identity of a known legacy reference.
/// This identity lookup establishes no trust in any installed file.
pub fn bundled_clap_id(plugin: &PluginRef) -> Option<&'static str> {
    BuiltinKind::from_plugin(plugin).map(clap_id)
}

/// Verify the actual shipped artifact, read its live inventory, and produce a
/// replacement proposal. This never modifies project state or chooses a broader
/// trust policy after an error.
pub fn plan_legacy_bundled_device(
    inventory: &BundledClap,
    old: &Device,
    project_dir: Option<&Path>,
) -> Result<LegacyDevicePlan, String> {
    let kind = BuiltinKind::from_plugin(&old.plugin)
        .ok_or_else(|| "device does not refer to a legacy built-in".to_owned())?;
    let id = clap_id(kind);
    let (path, _) = inventory
        .bundle_entries()
        .find(|(_, candidate)| *candidate == id)
        .ok_or_else(|| format!("bundled device {id} is unavailable"))?;
    let bundle = inventory
        .verify(path, id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("bundled device {id} is unavailable"))?;
    let module = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid bundle file name")?;
    let mut host = DirectClap::load(bundle).map_err(|error| error.to_string())?;
    host.try_control(|plugin| {
        let (_, layout) = plugin
            .native_layout()
            .ok_or("missing native bundled layout")?;
        let mut plan = prepare(
            old,
            kind,
            plugin.descriptor(),
            layout,
            &plugin.params(),
            module,
            project_dir,
        )?;
        if let Some(replacement) = crate::bundled_sampler::move_embedded_links(&plan.replacement)
            && supports_sample_links(plugin, &replacement)
        {
            plan.replacement = replacement;
        }
        Ok(plan)
    })
    .ok_or_else(|| "bundled device inventory is busy".to_owned())?
}

/// Update packed sampler state only when its verified bundle declares the matching audio slots.
pub(crate) fn migrate_packaged_sample_assets(
    inventory: &BundledClap,
    old: &Device,
) -> Option<Device> {
    let replacement = crate::bundled_sampler::move_embedded_links(old)?;
    let (path, id) = inventory
        .bundle_entries()
        .find(|(_, id)| *id == old.plugin.id)?;
    let proof = inventory.verify(path, id).ok()??;
    let mut host = DirectClap::load(proof).ok()?;
    host.try_control(|plugin| supports_sample_links(plugin, &replacement).then_some(replacement))?
}

fn supports_sample_links(plugin: &dyn HostedPlugin, replacement: &Device) -> bool {
    plugin.asset_slots().is_some_and(|slots| {
        replacement.asset_links.keys().all(|id| {
            slots
                .slots
                .iter()
                .any(|slot| slot.id == *id && slot.kind == AssetKind::AudioSample)
        })
    })
}

fn legacy_infos(kind: BuiltinKind) -> Vec<ParamInfo> {
    match kind {
        BuiltinKind::Hexa => (0..WIRE_COUNT)
            .map(|wire| {
                Field::from_wire(wire as u32)
                    .expect("valid Hexa wire ID")
                    .info_in(Lang::En)
            })
            .collect(),
        BuiltinKind::Internal(device) => device.params_in(Lang::En).to_vec(),
    }
}

fn prepare(
    old: &Device,
    kind: BuiltinKind,
    descriptor: &PluginDescriptor,
    layout: &NativeLayout,
    inventory: &[ClapParamInfo],
    module: &str,
    project_dir: Option<&Path>,
) -> Result<LegacyDevicePlan, String> {
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
    if descriptor.format != ClapFormat::Clap
        || descriptor.id != clap_id(kind)
        || role != old.role
        || role != kind.catalog().role
    {
        return Err("bundled device identity or role differs from the legacy device".into());
    }

    layout
        .validate_params(inventory)
        .map_err(|error| error.to_string())?;
    let aliases = layout
        .author_ids()
        .ok_or("bundled device has no authoritative author ID map")?;
    let numeric: BTreeMap<_, _> = inventory.iter().map(|param| (param.id, param)).collect();
    let infos = legacy_infos(kind);
    let mut params = BTreeMap::new();
    for info in &infos {
        let id = aliases
            .get(info.id.as_str())
            .ok_or_else(|| format!("unmapped parameter {}", info.id))?;
        let binding = numeric[id];
        let steps = info.steps();
        if binding.min != 0.0
            || binding.max != f64::from(steps.max(1))
            || binding.step_count != steps
            || binding.flags & (param_flags::READ_ONLY | param_flags::HIDDEN | param_flags::BYPASS)
                != 0
        {
            return Err(format!("incompatible bundled range for {}", info.id));
        }
        params.insert(
            info.id.clone(),
            LegacyParameter {
                id: ParamId::plugin(*id),
                steps,
                automation_issue: automation_issue(info, binding),
            },
        );
    }
    let (payload, legacy_preset) = convert_state(
        kind,
        old.state.as_deref(),
        &infos,
        &descriptor.version,
        project_dir,
    )?;
    let mut replacement = old.clone();
    replacement.plugin = PluginRef {
        format: PluginFormat::Clap,
        id: descriptor.id.clone(),
        name: descriptor.name.clone(),
        vendor: descriptor.vendor.clone(),
        version: descriptor.version.clone(),
        module: bundled_module(module),
    };
    replacement.state = payload.map(Into::into);
    Ok(LegacyDevicePlan {
        source: old.clone(),
        replacement,
        legacy_preset,
        params,
    })
}

fn automation_issue(info: &ParamInfo, binding: &ClapParamInfo) -> Option<&'static str> {
    if !info.automatable || binding.flags & param_flags::AUTOMATABLE == 0 {
        return Some("parameter is not automatable");
    }
    // Continuous CLAP positions still use the legacy scale in DSP. Stepped CLAP
    // positions quantize linearly, so a nonlinear legacy scale would move step
    // boundaries inside curves and macro ranges even if their endpoints matched.
    if matches!(
        info.kind,
        ParamKind::Float {
            scale: Scale::Log | Scale::Db,
            steps: 1..,
            ..
        }
    ) {
        return Some("nonlinear stepped range cannot preserve normalized interpolation");
    }
    None
}

fn convert_state(
    kind: BuiltinKind,
    old: Option<&[u8]>,
    infos: &[ParamInfo],
    version: &str,
    project_dir: Option<&Path>,
) -> Result<(Option<Vec<u8>>, Option<String>), String> {
    let Some(old) = old else {
        return Ok((None, None));
    };
    let BuiltinKind::Internal(device) = kind else {
        let patch = hexa::decode_state(Some(old)).map_err(|error| error.to_string())?;
        let state = hexa::state::encode_clap_patch(patch, version);
        return Ok((Some(state), None));
    };

    let values = device
        .decode_state(Some(old))
        .map_err(|error| error.to_string())?;
    let sampler = matches!(device, Internal::Sampler | Internal::GeneralSampler);
    // A CLAP host gives the sampler no project directory in its state callback.
    // Refresh the absolute fallback when the current project-relative source exists.
    let rebased = if sampler {
        project_dir.and_then(|dir| komponist_sampler::links::rebase(Some(old), Some(dir), dir))
    } else {
        None
    };
    let mut raw: Map<String, Value> = serde_json::from_slice(rebased.as_deref().unwrap_or(old))
        .map_err(|error| error.to_string())?;
    raw.remove("params");
    let preset = match raw.remove("preset") {
        Some(Value::String(name)) => Some(name),
        Some(_) => return Err("invalid legacy preset name".into()),
        None => None,
    };

    let mut fields = Map::new();
    if let Some((index, _)) = device.loaded_preset(Some(old)) {
        let name = device.presets()[index].name;
        fields.insert(
            pinned_clap_state::FACTORY_PRESET_FIELD.into(),
            Value::String(Value::String(name.into()).to_string()),
        );
    }
    if !raw.is_empty() {
        for (key, value) in &raw {
            let supported = (sampler && key == "samples")
                || (device == Internal::GeneralSampler && key == "sample_edit");
            if !supported || !value.is_object() {
                return Err(format!("unsupported legacy device state field {key}"));
            }
        }
        let assets = Value::Object(raw).to_string();
        if assets.len() > pinned_clap_state::MAX_SAMPLE_ASSET_BYTES {
            return Err("legacy sampler asset state exceeds the bundled limit".into());
        }
        fields.insert(
            pinned_clap_state::SAMPLE_ASSETS_FIELD.into(),
            Value::from(assets),
        );
    }

    let params = infos
        .iter()
        .zip(values)
        .map(|(info, plain)| {
            (
                info.id.as_str().to_owned(),
                pinned_clap_state::parameter(info, plain).0,
            )
        })
        .collect();
    Ok((
        Some(pinned_clap_state::encode(version, params, fields)),
        preset,
    ))
}

#[cfg(test)]
mod tests;
