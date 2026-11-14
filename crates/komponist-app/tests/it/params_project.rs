//! Parameter values in project files (WP-81): the parameter model changed nothing in the file.
//! Values live in the devices' opaque state; `fixtures/pisynth-macros.komponist` was saved before
//! the model existed (Algorithmus 5, Ratio 2 = 3.5, Index 0.6, Sustain 0.25).

use komponist_app::views::devices::model::pisynth_params;
use komponist_builtin::Macro;
use komponist_model::persist;

const OLD_FILE: &str = include_str!("../fixtures/pisynth-macros.komponist");

fn old_project() -> komponist_model::Project {
    persist::from_json(OLD_FILE).expect("the old file loads")
}

#[test]
fn an_old_file_loads_and_its_macros_read_the_saved_values() {
    let project = old_project();
    let pisynth = project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .find(|d| komponist_app::device_host::is_pisynth(d))
        .expect("a pisynth device");
    let params = pisynth_params(pisynth);
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
