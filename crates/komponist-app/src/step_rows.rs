//! Step rows per instrument (WP-54; ARCHITECTURE §8.5, §13.3; S03).
//!
//! **The instrument decides the rows when a step clip is created** ([`rows_for`]), and they are
//! stored in the model then (`NewClipContent::Step { rows }`):
//!
//! - an instrument that reports note names (CLAP note-name): its first [`ROWS`] named keys, in key
//!   order, with their names;
//! - klopfer without note names (nice-plug has no note-name yet): pads 1–8 = notes 36–43, named
//!   like the rows of S03 (Kick, Snare, … Perc);
//! - every other instrument (and a track without one): the prototype's General MIDI rows
//!   ([`DEFAULT_STEP_ROWS`]).
//!
//! **Labels follow the track's current instrument** ([`row_names`], [`row_label`]): a row shows the
//! instrument's note name for its key where it has one, else its stored name; the small label
//! right of the name is the klopfer pad ("P1", as S03 shows) on a klopfer track and the note name
//! ("C1") elsewhere. Changing the instrument later does not remap the rows of existing clips: a
//! pattern keeps the notes it was written for.

use komponist_ipc::NoteName;
use komponist_model::edits::StepRowSpec;
use komponist_model::format::note_name;
use komponist_model::project::DEFAULT_STEP_ROWS;
use komponist_model::{Device, DeviceRole, PluginRef, StepClip, Track};
use komponist_plugins::builtin::{KLOPFER_FIRST_NOTE, is_klopfer, klopfer_pad};

/// Rows of a new step clip (S03: 8 pads).
pub const ROWS: usize = 8;

/// The instrument of `track`: the first device, if it is one.
pub fn instrument(track: &Track) -> Option<&Device> {
    track
        .channel
        .devices
        .first()
        .filter(|d| d.role == DeviceRole::Instrument)
        .map(|d| &**d)
}

/// Whether `track` plays klopfer.
pub fn is_klopfer_track(track: &Track) -> bool {
    instrument(track).is_some_and(|d| is_klopfer(&d.plugin))
}

/// The rows of a new step clip for `instrument` with the `note_names` it reports now (see the
/// module docs).
pub fn rows_for(instrument: Option<&PluginRef>, note_names: &[NoteName]) -> Vec<StepRowSpec> {
    let mut named: Vec<&NoteName> = note_names
        .iter()
        .filter(|n| n.key <= 127 && !n.name.trim().is_empty())
        .collect();
    named.sort_by_key(|n| n.key);
    named.dedup_by_key(|n| n.key);
    if !named.is_empty() {
        return named
            .into_iter()
            .take(ROWS)
            .map(|n| spec(n.name.trim(), n.key))
            .collect();
    }
    if instrument.is_some_and(is_klopfer) {
        return DEFAULT_STEP_ROWS
            .iter()
            .zip(KLOPFER_FIRST_NOTE..)
            .map(|(&(name, _), note)| spec(name, note))
            .collect();
    }
    DEFAULT_STEP_ROWS
        .iter()
        .map(|&(name, note)| spec(name, note))
        .collect()
}

fn spec(name: &str, note: u8) -> StepRowSpec {
    StepRowSpec {
        name: name.to_owned(),
        note,
    }
}

/// The names `pattern`'s rows show: the instrument's note name for the row's key where it has
/// one, else the stored name.
pub fn row_names(pattern: &StepClip, note_names: &[NoteName]) -> Vec<String> {
    pattern
        .rows
        .iter()
        .map(|r| {
            note_names
                .iter()
                .find(|n| n.key == r.note && !n.name.trim().is_empty())
                .map_or_else(|| r.name.clone(), |n| n.name.trim().to_owned())
        })
        .collect()
}

/// The small label right of a row's name: the klopfer pad ("P1") of `note` on a klopfer track,
/// else the note name ("C1"; also for keys outside klopfer's pads).
pub fn row_label(instrument: Option<&PluginRef>, note: u8) -> String {
    instrument
        .filter(|p| is_klopfer(p))
        .and_then(|_| klopfer_pad(note))
        .map_or_else(|| note_name(note), |pad| format!("P{pad}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::project::MAX_STEPS;
    use komponist_model::project::{BUILTIN_KLOPFER, BUILTIN_PISYNTH};
    use komponist_model::{PluginFormat, Step, StepResolution, StepRow};

    fn plugin(format: PluginFormat, id: &str) -> PluginRef {
        PluginRef {
            format,
            id: id.into(),
            name: id.into(),
            vendor: String::new(),
            version: String::new(),
            module: Box::default(),
        }
    }

    fn klopfer() -> PluginRef {
        plugin(PluginFormat::Builtin, BUILTIN_KLOPFER)
    }

    fn pisynth() -> PluginRef {
        plugin(PluginFormat::Builtin, BUILTIN_PISYNTH)
    }

    fn name(key: u8, name: &str) -> NoteName {
        NoteName {
            key,
            channel: None,
            name: name.into(),
        }
    }

    fn pairs(rows: &[StepRowSpec]) -> Vec<(&str, u8)> {
        rows.iter().map(|r| (r.name.as_str(), r.note)).collect()
    }

    #[test]
    fn klopfer_without_note_names_gets_pads_1_to_8() {
        let rows = rows_for(Some(&klopfer()), &[]);
        assert_eq!(
            pairs(&rows),
            [
                ("Kick", 36),
                ("Snare", 37),
                ("Clap", 38),
                ("Hat zu", 39),
                ("Hat offen", 40),
                ("Tom", 41),
                ("Rim", 42),
                ("Perc", 43),
            ]
        );
        // The same plugin referenced as CLAP.
        let clap = plugin(
            PluginFormat::Clap,
            komponist_plugins::builtin::KLOPFER_CLAP_ID,
        );
        assert_eq!(rows_for(Some(&clap), &[]), rows);
    }

    #[test]
    fn other_instruments_get_the_general_midi_rows() {
        let gm: Vec<(&str, u8)> = DEFAULT_STEP_ROWS.to_vec();
        assert_eq!(pairs(&rows_for(Some(&pisynth()), &[])), gm);
        assert_eq!(pairs(&rows_for(None, &[])), gm);
        let surge = plugin(PluginFormat::Clap, "org.surge-synth-team.surge-xt");
        assert_eq!(pairs(&rows_for(Some(&surge), &[])), gm);
    }

    #[test]
    fn note_names_decide_the_rows_in_key_order() {
        // Unordered, a duplicate key, an empty name, more than eight names.
        let mut names = vec![name(40, "Hat"), name(36, " Kick "), name(37, "Snare")];
        names.push(name(36, "Kick again"));
        names.push(name(38, "  "));
        names.extend((50..60).map(|k| name(k, &format!("Key {k}"))));
        let rows = rows_for(Some(&pisynth()), &names);
        assert_eq!(rows.len(), ROWS);
        assert_eq!(
            pairs(&rows[..4]),
            [("Kick", 36), ("Snare", 37), ("Hat", 40), ("Key 50", 50)]
        );
        assert_eq!(pairs(&rows[7..]), [("Key 54", 54)]);
        // For klopfer too, once it reports them.
        assert_eq!(rows_for(Some(&klopfer()), &names), rows);
    }

    #[test]
    fn labels_follow_the_current_instrument() {
        assert_eq!(row_label(Some(&klopfer()), 36), "P1");
        assert_eq!(row_label(Some(&klopfer()), 43), "P8");
        assert_eq!(row_label(Some(&klopfer()), 67), "P32");
        // Keys outside klopfer's pads, and other instruments, show the note name.
        assert_eq!(row_label(Some(&klopfer()), 35), "B1");
        assert_eq!(row_label(Some(&klopfer()), 68), "G#4");
        assert_eq!(row_label(Some(&pisynth()), 36), "C2");
        assert_eq!(row_label(None, 42), "F#2");
    }

    #[test]
    fn row_names_prefer_the_instruments_note_names() {
        let row = |name: &str, note: u8| StepRow {
            name: name.into(),
            note,
            mute: false,
            solo: false,
            steps: [Step::default(); MAX_STEPS],
        };
        let pattern = StepClip {
            rows: vec![row("Kick", 36), row("Snare", 37), row("Clap", 38)],
            length_steps: 16,
            resolution: StepResolution::Sixteenth,
            swing: 0.0,
        };
        assert_eq!(row_names(&pattern, &[]), ["Kick", "Snare", "Clap"]);
        let names = [name(37, "Rim"), name(38, "")];
        assert_eq!(row_names(&pattern, &names), ["Kick", "Rim", "Clap"]);
    }
}
