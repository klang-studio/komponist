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
//! **Labels follow the track's current instrument** ([`row_names`], [`device_row_label`]): a row shows the
//! instrument's note name for its key where it has one, else its stored name; the small label
//! right of the name is the pad position ("P1", as S03 shows) on a pad-bank instrument and the note name
//! ("C1") elsewhere. Changing the instrument later does not remap the rows of existing clips: a
//! pattern keeps the notes it was written for.

use std::borrow::Cow;

use komponist_i18n::Lang;
use komponist_ipc::NoteName;
use komponist_model::edits::StepRowSpec;
use komponist_model::format::note_name;
use komponist_model::project::DEFAULT_STEP_ROWS;
use komponist_model::{Device, StepClip, Track};
use komponist_plugin_api::native_layout::PadBank;

use crate::device_host::DevicePresentationHost;

/// Rows of a new step clip (S03: 8 pads).
pub const ROWS: usize = 8;

/// The instrument of `track`: the device right behind the MIDI effects, if it is one.
pub fn instrument(track: &Track) -> Option<&Device> {
    track.channel.instrument().map(|(_, d)| &**d)
}

/// A native pad bank derives note names from its project-linked audio samples.
pub(crate) fn device_note_names<'a>(
    device: Option<&Device>,
    host: &'a dyn DevicePresentationHost,
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
    host: &'a dyn DevicePresentationHost,
) -> Option<&'a PadBank> {
    host.native_layout(device?.id)?.pad_bank.as_ref()
}

/// The rows of a new step clip for a plugin-declared pad bank.
pub(crate) fn pad_rows(t: Lang, bank: &PadBank, names: &[NoteName]) -> Vec<StepRowSpec> {
    rows_for_pads(t, bank.pads.iter().map(|pad| pad.note), names)
}

/// The pad position or key name that appears next to this row's name.
pub(crate) fn device_row_label(bank: Option<&PadBank>, note: u8) -> String {
    match bank {
        Some(bank) => bank
            .pads
            .iter()
            .position(|pad| pad.note == note)
            .map_or_else(|| note_name(note), |pad| format!("P{}", pad + 1)),
        None => note_name(note),
    }
}

/// Rows for pads on `notes`, named after `names` or the default row names of `t`.
pub(crate) fn rows_for_pads(
    t: Lang,
    notes: impl Iterator<Item = u8>,
    names: &[NoteName],
) -> Vec<StepRowSpec> {
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

/// [`DEFAULT_STEP_ROWS`] with the names of `t`: the same notes, "Hat zu" and "Hat offen" are
/// "Closed Hat" and "Open Hat" in English. The names are content once a clip has them.
pub fn default_rows(t: Lang) -> [(&'static str, u8); 8] {
    let mut rows = DEFAULT_STEP_ROWS;
    rows[3].0 = t.default_name_hat_closed();
    rows[4].0 = t.default_name_hat_open();
    rows
}

/// The rows of a new step clip in German; see [`rows_for_in`].
pub fn rows_for(note_names: &[NoteName]) -> Vec<StepRowSpec> {
    rows_for_in(Lang::De, note_names)
}

/// The rows of a new step clip for `instrument` with the `note_names` it reports now (see the
/// module docs); the default names are those of `t`.
pub fn rows_for_in(t: Lang, note_names: &[NoteName]) -> Vec<StepRowSpec> {
    let defaults = default_rows(t);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        use komponist_plugin_api::native_layout::Pad;

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
        assert_eq!(device_row_label(Some(&bank), 61), "P1");
        assert_eq!(device_row_label(Some(&bank), 23), "P2");
        assert_eq!(device_row_label(Some(&bank), 48), note_name(48));
    }

    #[test]
    fn the_default_rows_are_named_in_the_language() {
        assert_eq!(default_rows(Lang::De), DEFAULT_STEP_ROWS);
        let gm = default_rows(Lang::En).to_vec();
        assert_eq!(pairs(&rows_for_in(Lang::En, &[])), gm);
        assert_eq!(gm[3], ("Closed Hat", 42));
        // A name the instrument reports wins in every language.
        let named = [name(36, "808 Kick")];
        assert_eq!(rows_for_in(Lang::En, &named)[0].name, "808 Kick");
    }

    #[test]
    fn note_names_decide_the_rows_in_key_order() {
        // Unordered, a duplicate key, an empty name, more than eight names.
        let mut names = vec![name(40, "Hat"), name(36, " Kick "), name(37, "Snare")];
        names.push(name(36, "Kick again"));
        names.push(name(38, "  "));
        names.extend((50..60).map(|k| name(k, &format!("Key {k}"))));
        let rows = rows_for(&names);
        assert_eq!(rows.len(), ROWS);
        assert_eq!(
            pairs(&rows[..4]),
            [("Kick", 36), ("Snare", 37), ("Hat", 40), ("Key 50", 50)]
        );
        assert_eq!(pairs(&rows[7..]), [("Key 54", 54)]);
    }
}
