//! Renamed factory presets. A device state stores the name of the preset it loaded
//! (`"preset"`); the factory names became English in every language (owner, 2026-11-15, #4), so a
//! state saved earlier still says the German name. [`current_name`] maps such an old name to
//! the present one, and [`Internal::loaded_preset`](super::Internal::loaded_preset) uses it,
//! so an old project shows its preset (and the „ *“ mark) as before. hexa's device state is its
//! patch and stores no preset name, so hexa has no entry here.

/// `(device id, name in earlier versions, name now)`.
const RENAMED: &[(&str, &str, &str)] = &[
    ("eq", "Flach", "Flat"),
    ("eq", "Gesang Präsenz", "Vocal Presence"),
    ("eq", "Bass Wärme", "Warm Bass"),
    ("eq", "Höhen Glanz", "Treble Sheen"),
    ("eq", "Telefon", "Telephone"),
    ("delay", "Achtel Ping-Pong", "Eighth Ping-Pong"),
    ("delay", "Punktierte Achtel", "Dotted Eighths"),
    ("delay", "Dub-Echo", "Dub Echo"),
    ("delay", "Weiches Band", "Soft Tape"),
    ("hall", "Kleiner Raum", "Small Room"),
    ("hall", "Studio-Platte", "Studio Plate"),
    ("hall", "Große Halle", "Large Hall"),
    ("hall", "Kirche", "Church"),
    ("limiter", "Master sanft", "Gentle Master"),
    ("limiter", "Master laut", "Loud Master"),
    ("limiter", "Spitzenschutz", "Peak Guard"),
    ("limiter", "Drums hart", "Hard Drums"),
    ("presse", "Gesang weich", "Soft Vocal"),
    ("presse", "Gesang vorn", "Forward Vocal"),
    ("presse", "Drums fest", "Tight Drums"),
    ("presse", "Drums parallel", "Parallel Drums"),
    ("presse", "Bass stabil", "Steady Bass"),
    ("presse", "Gitarre/Keys sanft", "Gentle Guitar & Keys"),
    ("presse", "Limitieren (hart)", "Hard Limit"),
    ("presse", "Pumpen", "Pumping"),
    ("arpeggiator", "Achtel auf", "Eighths Up"),
    ("arpeggiator", "Sechzehntel Auf/Ab", "Sixteenths Up/Down"),
    ("arpeggiator", "Triolen ab", "Triplets Down"),
    ("arpeggiator", "Zufall", "Random"),
    ("arpeggiator", "Gespielt weich", "Soft As Played"),
    ("tonleiter", "C-Dur", "C Major"),
    ("tonleiter", "A-Moll", "A Minor"),
    ("tonleiter", "D-Dorisch", "D Dorian"),
    ("tonleiter", "A-Moll-Pentatonik", "A Minor Pentatonic"),
    ("tonleiter", "C-Blues", "C Blues"),
    ("tonleiter", "C-Dur filtern", "C Major Filtered"),
    ("knurr", "Fette Säge", "Fat Saw"),
    ("knurr", "Sub-Brumm", "Sub Rumble"),
    ("knurr", "Acid-Zupf", "Acid Pluck"),
    ("knurr", "Mono-Lead", "Mono Lead"),
    ("knurr", "Breiter Pad", "Wide Pad"),
    ("knurr", "Blechstab", "Brass Stab"),
    ("knurr", "Zufalls-Zupf", "Random Pluck"),
    ("knurr", "Kick-Bass", "Kick Bass"),
    ("knurr", "Rauschwind", "Noise Wind"),
];

/// The present name of the preset that device `device` (catalog id) once called `name`; `name`
/// itself when it was never renamed.
pub fn current_name<'a>(device: &str, name: &'a str) -> &'a str {
    RENAMED
        .iter()
        .find(|(d, old, _)| *d == device && *old == name)
        .map_or(name, |(_, _, new)| new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::Internal;

    #[test]
    fn every_new_name_is_a_preset_of_its_device_and_no_old_name_is() {
        for (device, old, new) in RENAMED {
            let kind = Internal::from_id(device).unwrap_or_else(|| panic!("{device}"));
            let names: Vec<&str> = kind.presets().iter().map(|p| p.name).collect();
            assert!(names.contains(new), "{device}: no preset {new}");
            assert!(!names.contains(old), "{device}: {old} is still a preset");
        }
    }

    #[test]
    fn an_old_state_still_loads_its_preset() {
        for (device, old, new) in RENAMED {
            let kind = Internal::from_id(device).unwrap();
            let index = kind.presets().iter().position(|p| p.name == *new).unwrap();
            // A state as the German version saved it: the preset's values and its old name.
            let state = kind.preset_state(None, index).unwrap();
            let mut json: serde_json::Value = serde_json::from_slice(&state).unwrap();
            assert_eq!(json["preset"], *new);
            json["preset"] = (*old).into();
            let old_state = serde_json::to_vec(&json).unwrap();
            assert_eq!(
                kind.loaded_preset(Some(&old_state)),
                Some((index, false)),
                "{device}: {old}"
            );
        }
    }

    #[test]
    fn current_names_map_old_to_new_and_keep_the_rest() {
        assert_eq!(current_name("hall", "Kirche"), "Church");
        assert_eq!(current_name("hall", "Church"), "Church");
        // The same old name on another device is not renamed.
        assert_eq!(current_name("eq", "Kirche"), "Kirche");
        assert_eq!(current_name("delay", "Slapback"), "Slapback");
    }
}
