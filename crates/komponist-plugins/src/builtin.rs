//! The built-in entries of the catalog (ARCHITECTURE §8.5): hexa and the internal devices of
//! `komponist-builtin`, all in process, shown as "INTERN".

use komponist_i18n::Lang;
use komponist_ipc::PluginKind;
use komponist_ipc::native_layout::LocalizedPlugin;
use komponist_model::DeviceRole;

use crate::catalog::{BuiltinKind, CatalogEntry, EffectCategory, PluginLocation};

/// Adapts the device registry's entries to the plugin catalog.
pub(crate) fn entries() -> Vec<CatalogEntry> {
    BuiltinKind::all()
        .map(|builtin| {
            let spec = builtin.catalog();
            let features: Vec<String> = spec
                .features
                .iter()
                .map(|feature| (*feature).into())
                .collect();
            CatalogEntry {
                localized: Some(LocalizedPlugin {
                    default_locale: Lang::De.code().into(),
                    names: Lang::ALL
                        .into_iter()
                        .map(|lang| (lang.code().into(), (spec.name_in)(lang).into()))
                        .collect(),
                    descriptions: Default::default(),
                }),
                clap_presets: crate::presets::legacy_inventory(builtin),
                plugin: spec.plugin_ref(),
                kind: match spec.role {
                    DeviceRole::MidiEffect => PluginKind::MidiEffect,
                    DeviceRole::Instrument => PluginKind::Instrument,
                    DeviceRole::Effect => PluginKind::Effect,
                },
                category: (spec.role == DeviceRole::Effect)
                    .then(|| EffectCategory::from_features(&features)),
                features,
                location: PluginLocation::InProcess(builtin),
                sidechain: spec.sidechain,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use komponist_builtin::Internal;
    use komponist_model::PluginFormat;

    use super::*;

    #[test]
    fn the_builtin_entries_are_hexa_then_the_internal_devices() {
        let names: Vec<String> = entries().into_iter().map(|e| e.plugin.name).collect();
        let mut expected = vec!["Hexa".to_owned()];
        expected.extend(Internal::ALL.iter().map(|k| k.name().to_owned()));
        assert_eq!(names, expected);
        assert!(
            entries()
                .iter()
                .all(|e| e.plugin.format == PluginFormat::Builtin)
        );
    }

    #[test]
    fn every_builtin_project_id_round_trips_and_resolves() {
        use komponist_model::edits::{NewDevice, NewTrack, add_track, insert_device};
        use komponist_model::{ChannelRef, DeviceRole, Project, persist};

        let catalog = crate::Catalog::builtin();
        let ids = [
            "hexa",
            "sampler",
            "eq",
            "delay",
            "hall",
            "limiter",
            "presse",
            "arpeggiator",
            "tonleiter",
            "volt",
            "surf",
            "general-sampler",
        ];
        assert!(
            catalog
                .entries()
                .iter()
                .map(|entry| entry.plugin.id.as_str())
                .eq(ids)
        );
        let mut project = Project::new("Built-in devices");
        let mut saved_devices = Vec::new();
        for (entry, id) in catalog.entries().iter().zip(ids) {
            let role = match entry.kind {
                PluginKind::Instrument => DeviceRole::Instrument,
                PluginKind::MidiEffect => DeviceRole::MidiEffect,
                PluginKind::Effect => DeviceRole::Effect,
                PluginKind::Other => panic!("unexpected built-in device: {id}"),
            };
            let (next, track) = add_track(
                &project,
                NewTrack {
                    name: Some(id.to_owned()),
                    hue: None,
                    instrument: None,
                    index: None,
                    arm_exclusive: false,
                },
            )
            .unwrap();
            let (next, device) = insert_device(
                &next,
                ChannelRef::Track(track),
                0,
                NewDevice {
                    role,
                    plugin: entry.plugin.clone(),
                    state: None,
                    sidechain: None,
                },
            )
            .unwrap();
            project = next;
            saved_devices.push((id, device, entry));
        }

        let saved = persist::to_json(&project).unwrap();
        let reopened = persist::from_json(&saved).unwrap();
        assert_eq!(reopened, project);
        for (id, device, entry) in saved_devices {
            let (_, _, device) = reopened.find_device(device).unwrap();
            assert_eq!(device.plugin.id, id);
            assert_eq!(catalog.resolve(&device.plugin), Some(entry));
        }
    }

    #[test]
    fn the_samplers_and_synths_are_instruments_the_effects_are_not() {
        for e in entries() {
            let instrument = e.plugin.name == "Hexa"
                || matches!(
                    Internal::from_plugin(&e.plugin),
                    Some(
                        Internal::Sampler
                            | Internal::GeneralSampler
                            | Internal::Volt
                            | Internal::Surf
                    )
                );
            assert_eq!(
                e.kind == PluginKind::Instrument,
                instrument,
                "{}",
                e.plugin.name
            );
            assert_eq!(
                e.category.is_none(),
                e.kind != PluginKind::Effect,
                "{}",
                e.plugin.name
            );
        }
    }
}
