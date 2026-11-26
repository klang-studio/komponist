//! Step rows per instrument (WP-54; ARCHITECTURE §8.5, §13.3; S03).
//!
//! **The instrument decides the rows when a step clip is created** ([`rows_for`]), and they are
//! stored in the model then (`NewClipContent::Step { rows }`):
//!
//! - an instrument that declares a pad bank: its first [`ROWS`] pads in their declared order,
//!   with the names of their linked samples and the S03 row names for empty pads;
//! - an instrument that reports note names: its first [`ROWS`] named keys in key order;
//! - every other instrument (and a track without one): the prototype's General MIDI rows
//!   ([`DEFAULT_STEP_ROWS`]).
//!
//! **Labels follow the track's current instrument** ([`row_names`], [`row_label`]): a row shows the
//! instrument's note name for its key where it has one, else its stored name; the small label
//! right of the name is the pad position ("P1", as S03 shows) on a pad-bank instrument and the note name
//! ("C1") elsewhere. Changing the instrument later does not remap the rows of existing clips: a
//! pattern keeps the notes it was written for.

use std::borrow::Cow;

use komponist_i18n::Lang;
use komponist_ipc::{NoteName, native_layout::PadBank};
use komponist_model::PluginFormat;
use komponist_model::edits::StepRowSpec;
use komponist_model::format::note_name;
use komponist_model::project::BUILTIN_SAMPLER;
use komponist_model::project::DEFAULT_STEP_ROWS;
use komponist_model::{Device, PluginRef, StepClip, Track};
use komponist_sampler as sampler;

use crate::device_host::DeviceHost;

/// Rows of a new step clip (S03: 8 pads).
pub const ROWS: usize = 8;

/// The instrument of `track`: the device right behind the MIDI effects, if it is one.
pub fn instrument(track: &Track) -> Option<&Device> {
    track.channel.instrument().map(|(_, d)| &**d)
}

/// A native pad bank derives note names from its project-linked audio samples.
pub(crate) fn device_note_names<'a>(
    device: Option<&Device>,
    host: &'a dyn DeviceHost,
) -> Cow<'a, [NoteName]> {
    let Some(device) = device else {
        return Cow::Borrowed(&[]);
    };
    let Some(bank) = pad_bank(Some(device), host) else {
        return Cow::Borrowed(host.note_names(device.id));
    };
    Cow::Owned(
        bank.pads
            .iter()
            .filter_map(|pad| {
                let name = device
                    .asset_links
                    .get(&pad.asset_slot)?
                    .name()?
                    .into_owned();
                Some(NoteName {
                    key: pad.note,
                    channel: None,
                    name,
                })
            })
            .collect(),
    )
}

pub(crate) fn pad_bank<'a>(
    device: Option<&Device>,
    host: &'a dyn DeviceHost,
) -> Option<&'a PadBank> {
    host.native_layout(device?.id)?.pad_bank.as_ref()
}

/// The rows of a new step clip for a plugin-declared pad bank.
pub(crate) fn pad_rows(t: Lang, bank: &PadBank, names: &[NoteName]) -> Vec<StepRowSpec> {
    rows_for_pads(t, bank.pads.iter().map(|pad| pad.note), names)
}

/// The pad position or key name that appears next to this row's name.
pub(crate) fn device_row_label(
    bank: Option<&PadBank>,
    instrument: Option<&PluginRef>,
    note: u8,
) -> String {
    match bank {
        Some(bank) => bank
            .pads
            .iter()
            .position(|pad| pad.note == note)
            .map_or_else(|| note_name(note), |pad| format!("P{}", pad + 1)),
        None => row_label(instrument, note),
    }
}

fn rows_for_pads(t: Lang, notes: impl Iterator<Item = u8>, names: &[NoteName]) -> Vec<StepRowSpec> {
    default_rows(t)
        .iter()
        .zip(notes)
        .map(|(&(default, _), note)| {
            let name = names
                .iter()
                .find(|n| n.key == note && !n.name.trim().is_empty())
                .map_or(default, |n| n.name.trim());
            spec(name, note)
        })
        .collect()
}

/// Whether `plugin` is the built-in sampler.
pub fn is_sampler(plugin: &PluginRef) -> bool {
    plugin.format == PluginFormat::Builtin && plugin.id == BUILTIN_SAMPLER
}

/// Whether `track` plays the sampler.
pub fn is_sampler_track(track: &Track) -> bool {
    instrument(track).is_some_and(|d| is_sampler(&d.plugin))
}

/// [`DEFAULT_STEP_ROWS`] with the names of `t`: the same notes, "Hat zu" and "Hat offen" are
/// "Closed Hat" and "Open Hat" in English. The names are content once a clip has them.
pub fn default_rows(t: Lang) -> [(&'static str, u8); 8] {
    let mut rows = DEFAULT_STEP_ROWS;
    rows[3].0 = t.default_name_hat_closed();
    rows[4].0 = t.default_name_hat_open();
    rows
}

/// The rows of a new step clip in German; see [`rows_for_in`].
pub fn rows_for(instrument: Option<&PluginRef>, note_names: &[NoteName]) -> Vec<StepRowSpec> {
    rows_for_in(Lang::De, instrument, note_names)
}

/// The rows of a new step clip for `instrument` with the `note_names` it reports now (see the
/// module docs); the default names are those of `t`.
pub fn rows_for_in(
    t: Lang,
    instrument: Option<&PluginRef>,
    note_names: &[NoteName],
) -> Vec<StepRowSpec> {
    let defaults = default_rows(t);
    if instrument.is_some_and(is_sampler) {
        return rows_for_pads(t, sampler::FIRST_NOTE.., note_names);
    }
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
    defaults
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

/// The small label right of a row's name: the sampler pad ("P1") of `note` on a sampler track,
/// else the note name ("C1"; also for keys outside the sampler's pads).
pub fn row_label(instrument: Option<&PluginRef>, note: u8) -> String {
    instrument
        .filter(|p| is_sampler(p))
        .and_then(|_| sampler::pad_of_note(note))
        .map_or_else(|| note_name(note), |pad| format!("P{}", pad + 1))
}

/// The row of `pattern` that plays pad `pad` (0-based) of a sampler, if it has one.
pub fn row_of_pad(pattern: &StepClip, pad: usize) -> Option<usize> {
    let note = sampler::note_of_pad(pad);
    pattern.rows.iter().position(|r| r.note == note)
}

/// The sampler pad (0-based) the pattern's row `row` plays, if it plays one.
pub fn pad_of_row(pattern: &StepClip, row: usize) -> Option<usize> {
    pattern
        .rows
        .get(row)
        .and_then(|r| sampler::pad_of_note(r.note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::project::BUILTIN_HEXA;
    use komponist_model::project::MAX_STEPS;
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

    fn sampler() -> PluginRef {
        plugin(PluginFormat::Builtin, BUILTIN_SAMPLER)
    }

    fn hexa() -> PluginRef {
        plugin(PluginFormat::Builtin, BUILTIN_HEXA)
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
    fn a_native_pad_bank_preserves_declared_order_and_labels() {
        use komponist_ipc::native_layout::Pad;

        let bank = PadBank {
            pads: vec![
                Pad {
                    note: 61,
                    asset_slot: 9,
                },
                Pad {
                    note: 23,
                    asset_slot: 4,
                },
            ],
            activity_tap: None,
            follow_param: None,
        };
        let rows = pad_rows(Lang::En, &bank, &[name(23, " Snare sample ")]);
        assert_eq!(pairs(&rows), [("Kick", 61), ("Snare sample", 23)]);
        assert_eq!(device_row_label(Some(&bank), None, 61), "P1");
        assert_eq!(device_row_label(Some(&bank), None, 23), "P2");
        assert_eq!(device_row_label(Some(&bank), None, 48), note_name(48));
    }

    #[test]
    fn the_sampler_without_note_names_gets_pads_1_to_8() {
        let rows = rows_for(Some(&sampler()), &[]);
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
        // Klopfer, an ordinary CLAP plugin now, gets the General MIDI rows like any other.
        let clap = plugin(PluginFormat::Clap, "com.klopfer.drum-sampler");
        let gm: Vec<(&str, u8)> = DEFAULT_STEP_ROWS.to_vec();
        assert_eq!(pairs(&rows_for(Some(&clap), &[])), gm);
    }

    #[test]
    fn the_default_rows_are_named_in_the_language() {
        assert_eq!(default_rows(Lang::De), DEFAULT_STEP_ROWS);
        let en = rows_for_in(Lang::En, Some(&sampler()), &[]);
        assert_eq!(
            pairs(&en),
            [
                ("Kick", 36),
                ("Snare", 37),
                ("Clap", 38),
                ("Closed Hat", 39),
                ("Open Hat", 40),
                ("Tom", 41),
                ("Rim", 42),
                ("Perc", 43),
            ]
        );
        let gm = default_rows(Lang::En).to_vec();
        assert_eq!(pairs(&rows_for_in(Lang::En, None, &[])), gm);
        assert_eq!(gm[3], ("Closed Hat", 42));
        // A name the instrument reports wins in every language.
        let named = [name(36, "808 Kick")];
        assert_eq!(
            rows_for_in(Lang::En, Some(&sampler()), &named)[0].name,
            "808 Kick"
        );
    }

    #[test]
    fn other_instruments_get_the_general_midi_rows() {
        let gm: Vec<(&str, u8)> = DEFAULT_STEP_ROWS.to_vec();
        assert_eq!(pairs(&rows_for(Some(&hexa()), &[])), gm);
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
        let rows = rows_for(Some(&hexa()), &names);
        assert_eq!(rows.len(), ROWS);
        assert_eq!(
            pairs(&rows[..4]),
            [("Kick", 36), ("Snare", 37), ("Hat", 40), ("Key 50", 50)]
        );
        assert_eq!(pairs(&rows[7..]), [("Key 54", 54)]);
        // The sampler always has its eight pads, named by the pads that hold a sample.
        let sampler_rows = rows_for(Some(&sampler()), &names);
        assert_eq!(sampler_rows.len(), ROWS);
        assert_eq!(
            pairs(&sampler_rows[2..5]),
            [("Clap", 38), ("Hat zu", 39), ("Hat", 40)]
        );
    }

    #[test]
    fn labels_follow_the_current_instrument() {
        assert_eq!(row_label(Some(&sampler()), 36), "P1");
        assert_eq!(row_label(Some(&sampler()), 43), "P8");
        // Keys outside the sampler's pads, and other instruments, show the note name.
        assert_eq!(row_label(Some(&sampler()), 35), "B1");
        assert_eq!(row_label(Some(&sampler()), 44), "G#2");
        assert_eq!(row_label(Some(&hexa()), 36), "C2");
        assert_eq!(row_label(None, 42), "F#2");
    }

    #[test]
    fn the_samplers_pad_names_make_the_rows_and_pads_map_to_rows() {
        // Pads 1, 2 and 4 hold samples, as the device host reports them; pad 3 is empty.
        let names = [name(36, "808 Kick"), name(37, "Rimshot"), name(39, "Hat")];
        let rows = rows_for(Some(&sampler()), &names);
        assert_eq!(
            pairs(&rows[..5]),
            [
                ("808 Kick", 36),
                ("Rimshot", 37),
                ("Clap", 38),
                ("Hat", 39),
                ("Hat offen", 40)
            ]
        );
        let row = |name: &str, note: u8| StepRow {
            name: name.into(),
            note,
            mute: false,
            solo: false,
            steps: [Step::default(); MAX_STEPS],
        };
        let pattern = StepClip {
            rows: vec![row("Rim", 42), row("Kick", 36), row("Tom", 50)],
            length_steps: 16,
            resolution: StepResolution::Sixteenth,
            swing: 0.0,
        };
        assert_eq!(row_of_pad(&pattern, 0), Some(1));
        assert_eq!(row_of_pad(&pattern, 6), Some(0));
        assert_eq!(row_of_pad(&pattern, 1), None);
        assert_eq!(pad_of_row(&pattern, 0), Some(6));
        assert_eq!(pad_of_row(&pattern, 1), Some(0));
        // A row outside the pads (note 50) and a row that is not there.
        assert_eq!(pad_of_row(&pattern, 2), None);
        assert_eq!(pad_of_row(&pattern, 9), None);
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
