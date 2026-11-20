//! Compatibility lookup for earlier factory preset names.

use super::Internal;

/// The present name of the preset that device `device` once called `name`.
/// Returns `name` itself if no alias is recorded.
pub fn current_name<'a>(device: &str, name: &'a str) -> &'a str {
    Internal::from_id(device).map_or(name, |kind| kind.spec().current_preset_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_new_name_is_a_preset_of_its_device_and_no_old_name_is() {
        for kind in Internal::ALL {
            let names: Vec<&str> = kind.presets().iter().map(|p| p.name).collect();
            for (old, new) in kind.spec().preset_aliases {
                assert!(names.contains(new), "{kind:?}: no preset {new}");
                assert!(!names.contains(old), "{kind:?}: {old} is still a preset");
            }
        }
    }

    #[test]
    fn an_old_state_still_loads_its_preset() {
        for kind in Internal::ALL {
            for (old, new) in kind.spec().preset_aliases {
                let index = kind.presets().iter().position(|p| p.name == *new).unwrap();
                let state = kind.preset_state(None, index).unwrap();
                let mut json: serde_json::Value = serde_json::from_slice(&state).unwrap();
                assert_eq!(json["preset"], *new);
                json["preset"] = (*old).into();
                let old_state = serde_json::to_vec(&json).unwrap();
                assert_eq!(kind.loaded_preset(Some(&old_state)), Some((index, false)));
            }
        }
    }

    #[test]
    fn current_names_map_old_to_new_and_keep_the_rest() {
        assert_eq!(current_name("hall", "Kirche"), "Church");
        assert_eq!(current_name("hall", "Church"), "Church");
        assert_eq!(current_name("eq", "Kirche"), "Kirche");
        assert_eq!(current_name("delay", "Slapback"), "Slapback");
    }
}
