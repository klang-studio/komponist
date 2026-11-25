//! Parameter values in project files (WP-81): the parameter model changed nothing in the file.
//! Values live in the devices' opaque state; `fixtures/hexa-macros.komponist` was saved before
//! the model existed (Algorithmus 5, Ratio 2 = 3.5, Index 0.6, Sustain 0.25).

use komponist_app::views::devices::model::legacy_frame_params;
use komponist_hexa::Macro;
use komponist_model::persist;

const OLD_FILE: &str = include_str!("../fixtures/hexa-macros.komponist");

fn old_project() -> komponist_model::Project {
    persist::from_json(OLD_FILE).expect("the old file loads")
}

#[test]
fn an_old_file_loads_and_its_macros_read_the_saved_values() {
    let project = old_project();
    let hexa = project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .find(|d| komponist_app::device_host::is_hexa(d))
        .expect("a hexa device");
    let params = legacy_frame_params(komponist_i18n::Lang::De, hexa);
    let value = |m: Macro| params.iter().find(|p| p.wire == m.id()).unwrap().value;
    assert_eq!(value(Macro::Algorithm), 5.0);
    assert_eq!(value(Macro::Ratio2), 3.5);
    assert!((value(Macro::Index) - 0.6).abs() < 1e-6);
    assert!((value(Macro::Sustain) - 0.25).abs() < 1e-6);
    // The model ids are the stable keys and the texts the macros' own.
    let ids: Vec<&str> = params.iter().map(|p| p.info.id.as_str()).collect();
    assert_eq!(ids[..3], ["algorithm", "ratio2", "index"]);
    assert_eq!(params[0].text, "5");
    assert_eq!(params[1].text, "3.50");
}

#[test]
fn an_old_file_that_calls_the_synth_pisynth_shows_it_as_hexa() {
    // The fixture stores plugin id and name "pisynth". The id stays, the name is shown as hexa.
    assert!(OLD_FILE.contains("\"name\": \"pisynth\""));
    let project = old_project();
    let device = project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .find(|d| komponist_app::device_host::is_hexa(d))
        .expect("a hexa device");
    assert_eq!(device.plugin.id, "pisynth");
    assert_eq!(device.plugin.name, "Hexa");
    // Saving writes the new name and keeps the id, so older files and new files load alike.
    let saved = persist::to_json(&project).expect("saves");
    let v: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let plugin = &v["project"]["tracks"][0]["channel"]["devices"][0]["plugin"];
    assert_eq!(
        (plugin["id"].as_str(), plugin["name"].as_str()),
        (Some("pisynth"), Some("Hexa"))
    );
}

#[test]
fn an_old_file_shows_on_the_new_card_too() {
    // WP-86: the card has its own parameters; the macro-era state reads through them.
    let project = old_project();
    let hexa = project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .find(|d| komponist_app::device_host::is_hexa(d))
        .expect("a hexa device");
    let params = komponist_app::views::devices::model::hexa_params(komponist_i18n::Lang::De, hexa);
    let value = |key: &str| {
        params
            .iter()
            .find(|p| p.info.id.as_str() == key)
            .unwrap_or_else(|| panic!("{key}"))
            .value
    };
    assert_eq!(value("algorithm"), 5.0);
    assert_eq!(value("op2_ratio"), 3.5);
    assert!((value("op2_level") - 0.6).abs() < 1e-6);
    let patch = komponist_hexa::decode_state(hexa.state.as_deref()).unwrap();
    let first = patch.operators.iter().position(|o| o.output).unwrap_or(0);
    assert!((value(&format!("op{}_sustain", first + 1)) - 0.25).abs() < 1e-6);
    // The first card parameters are the macros that stayed: same keys, same wire ids.
    assert_eq!(
        (params[0].info.id.as_str(), params[0].wire),
        ("algorithm", Macro::Algorithm.id())
    );
}

#[test]
fn saving_an_old_file_keeps_every_device_state_byte() {
    let project = old_project();
    let saved = persist::to_json(&project).unwrap();
    let again = persist::from_json(&saved).unwrap();
    assert_eq!(again, project);
    let states = |p: &komponist_model::Project| -> Vec<Option<Vec<u8>>> {
        p.tracks
            .iter()
            .flat_map(|t| &t.channel.devices)
            .map(|d| d.state.as_deref().map(<[u8]>::to_vec))
            .collect()
    };
    assert_eq!(states(&again), states(&project));
    assert!(states(&project).iter().any(Option::is_some));
    // No parameter id leaks into the file: the schema is the one the old file has.
    assert!(!saved.contains("\"p1\""));
}

/// Lowercases the stored name of every built-in device, as files saved before 2026-11-16 have it.
fn lowercase_builtin_names(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("format").and_then(|f| f.as_str()) == Some("builtin")
                && let Some(name) = map.get("name").and_then(|n| n.as_str())
            {
                let lower = name.to_lowercase();
                map.insert("name".into(), lower.into());
            }
            map.values_mut().for_each(lowercase_builtin_names);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(lowercase_builtin_names),
        _ => {}
    }
}

#[test]
fn a_file_with_lowercase_device_names_shows_hexa_volt_and_presse() {
    use komponist_i18n::Lang;
    let saved = persist::to_json(&komponist_app::showcase::showcase_project()).expect("saves");
    let mut v: serde_json::Value = serde_json::from_str(&saved).unwrap();
    lowercase_builtin_names(&mut v["project"]);
    let old = serde_json::to_string(&v).unwrap();
    assert!(old.contains("\"name\":\"hexa\"") && old.contains("\"name\":\"volt\""));
    assert!(old.contains("\"name\":\"presse\""));

    let project = persist::from_json(&old).expect("the old file loads");
    let mut shown = std::collections::BTreeSet::new();
    let catalog = komponist_plugins::Catalog::builtin();
    for device in project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .chain(&project.master.devices)
    {
        for lang in [Lang::De, Lang::En] {
            shown.insert((
                device.plugin.id.clone(),
                komponist_app::views::catalog_plugin_name(lang, &catalog, &device.plugin),
            ));
        }
    }
    for (id, name) in [("pisynth", "Hexa"), ("knurr", "Volt"), ("presse", "Presse")] {
        assert!(
            shown.contains(&(id.to_owned(), name.to_owned())),
            "{id} is shown as {name}: {shown:?}"
        );
    }
    // The ids and the shown names never go through the lowercase spelling.
    assert!(
        !shown
            .iter()
            .any(|(_, n)| ["hexa", "volt", "presse"].contains(&n.as_str()))
    );
}
