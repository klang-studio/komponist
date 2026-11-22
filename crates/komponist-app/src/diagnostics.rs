//! The compile diagnostics as the user sees them (#92): what the engine leaves out of the project
//! (`komponist_engine::compile::Diagnostic`) and why, as catalog texts.
//!
//! - The status bar says so when a sync leaves out something it did not before
//!   (`EngineSync::take_diagnostics_notice`, `App::report_compile_diagnostics`): [`summary`].
//! - The inspector marks the track section and the device rows "spielt nicht", with the reason in
//!   the tooltip: [`for_track`], [`for_device`], [`detail`].
//! - The export's finished message appends the [`summary`].

use komponist_engine::compile::{Chain, Diagnostic, Reason, Subject};
use komponist_engine::{MAX_SLOTS, MAX_STAGES, MAX_TRACKS};
use komponist_i18n::Lang;
use komponist_model::DeviceRole;

/// Why `diagnostic`'s object is left out: "mehr als 128 Spuren".
pub fn reason(t: Lang, diagnostic: &Diagnostic) -> String {
    match diagnostic.reason {
        Reason::TrackLimit => t.not_played_reason_track_limit(MAX_TRACKS),
        Reason::SlotLimit => t.not_played_reason_slot_limit(MAX_SLOTS),
        Reason::MidiEffectLimit => t.not_played_reason_midi_effect_limit(MAX_STAGES),
        Reason::DuplicateId => t.not_played_reason_duplicate_id().to_owned(),
        Reason::WrongChain => t.not_played_reason_wrong_chain().to_owned(),
        Reason::Misplaced => match diagnostic.subject {
            Subject::Device {
                role: DeviceRole::MidiEffect,
                ..
            } => t.not_played_reason_misplaced_midi_effect().to_owned(),
            _ => t.not_played_reason_misplaced_instrument().to_owned(),
        },
    }
}

/// The tooltip detail: "Spielt nicht: mehr als 128 Spuren".
pub fn detail(t: Lang, diagnostic: &Diagnostic) -> String {
    t.not_played_detail(reason(t, diagnostic))
}

/// The diagnostic of the track at `index` in `Project::tracks`, if it is left out.
pub fn for_track(diagnostics: &[Diagnostic], index: usize) -> Option<&Diagnostic> {
    diagnostics
        .iter()
        .find(|d| matches!(d.subject, Subject::Track { index: i, .. } if i == index))
}

/// The diagnostic of the device at `index` in the chain of the track at `track` in
/// `Project::tracks`, if it is left out.
pub fn for_device(diagnostics: &[Diagnostic], track: usize, index: usize) -> Option<&Diagnostic> {
    diagnostics.iter().find(|d| {
        matches!(
            d.subject,
            Subject::Device {
                chain: Chain::Track { index: t, .. },
                index: i,
                ..
            } if t == track && i == index
        )
    })
}

/// One line for the status bar and the export summary: the left-out tracks, then the devices,
/// counted per reason in the order they first appear, joined by " · ": "2 Spuren spielen nicht:
/// mehr als 128 Spuren · 1 Gerät spielt nicht: mehr als 1024 Geräte". `None` without diagnostics.
pub fn summary(t: Lang, diagnostics: &[Diagnostic]) -> Option<String> {
    // (is a device, reason text, count), in first appearance; tracks before devices.
    let mut groups: Vec<(bool, String, usize)> = Vec::new();
    for d in diagnostics {
        let device = matches!(d.subject, Subject::Device { .. });
        let text = reason(t, d);
        match groups
            .iter_mut()
            .find(|(dev, reason, _)| *dev == device && *reason == text)
        {
            Some((_, _, count)) => *count += 1,
            None => groups.push((device, text, 1)),
        }
    }
    groups.sort_by_key(|(device, _, _)| *device);
    let parts: Vec<String> = groups
        .into_iter()
        .map(|(device, reason, count)| {
            if device {
                t.not_played_devices(count, reason)
            } else {
                t.not_played_tracks(count, reason)
            }
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use komponist_i18n::{de, en};
    use komponist_model::{DeviceId, TrackId};

    use super::*;

    fn track(index: usize, reason: Reason) -> Diagnostic {
        Diagnostic {
            subject: Subject::Track {
                track: TrackId(index as u64 + 1),
                index,
            },
            reason,
        }
    }

    fn device(track: usize, index: usize, role: DeviceRole, reason: Reason) -> Diagnostic {
        Diagnostic {
            subject: Subject::Device {
                chain: Chain::Track {
                    track: TrackId(track as u64 + 1),
                    index: track,
                },
                device: DeviceId(100 + index as u64),
                index,
                role,
            },
            reason,
        }
    }

    #[test]
    fn every_reason_has_a_text_in_both_languages() {
        let cases = [
            (
                track(0, Reason::TrackLimit),
                "mehr als 128 Spuren",
                "more than 128 tracks",
            ),
            (
                device(0, 0, DeviceRole::Effect, Reason::SlotLimit),
                "mehr als 1024 Geräte",
                "more than 1024 devices",
            ),
            (
                device(0, 9, DeviceRole::MidiEffect, Reason::MidiEffectLimit),
                "mehr als 8 MIDI-Effekte vor dem Instrument",
                "more than 8 MIDI effects before the instrument",
            ),
            (
                track(1, Reason::DuplicateId),
                de::NOT_PLAYED_REASON_DUPLICATE_ID,
                en::NOT_PLAYED_REASON_DUPLICATE_ID,
            ),
            (
                device(0, 3, DeviceRole::MidiEffect, Reason::Misplaced),
                de::NOT_PLAYED_REASON_MISPLACED_MIDI_EFFECT,
                en::NOT_PLAYED_REASON_MISPLACED_MIDI_EFFECT,
            ),
            (
                device(0, 4, DeviceRole::Instrument, Reason::Misplaced),
                de::NOT_PLAYED_REASON_MISPLACED_INSTRUMENT,
                en::NOT_PLAYED_REASON_MISPLACED_INSTRUMENT,
            ),
            (
                device(0, 0, DeviceRole::Instrument, Reason::WrongChain),
                de::NOT_PLAYED_REASON_WRONG_CHAIN,
                en::NOT_PLAYED_REASON_WRONG_CHAIN,
            ),
        ];
        for (diagnostic, german, english) in cases {
            assert_eq!(reason(Lang::De, &diagnostic), german);
            assert_eq!(reason(Lang::En, &diagnostic), english);
        }
        assert_eq!(
            detail(Lang::De, &track(0, Reason::TrackLimit)),
            "Spielt nicht: mehr als 128 Spuren"
        );
        assert_eq!(
            detail(Lang::En, &track(0, Reason::TrackLimit)),
            "Not played: more than 128 tracks"
        );
    }

    #[test]
    fn summary_counts_per_reason_tracks_first() {
        assert_eq!(summary(Lang::De, &[]), None);
        let diagnostics = [
            device(0, 1, DeviceRole::Effect, Reason::SlotLimit),
            track(128, Reason::TrackLimit),
            track(129, Reason::TrackLimit),
            device(1, 1, DeviceRole::Effect, Reason::SlotLimit),
            device(2, 0, DeviceRole::Instrument, Reason::WrongChain),
        ];
        assert_eq!(
            summary(Lang::De, &diagnostics).as_deref(),
            Some(
                "2 Spuren spielen nicht: mehr als 128 Spuren · 2 Geräte spielen nicht: mehr als \
                 1024 Geräte · 1 Gerät spielt nicht: Instrumente und MIDI-Effekte nur auf MIDI-Spuren"
            )
        );
        assert_eq!(
            summary(Lang::En, &diagnostics[1..2]).as_deref(),
            Some("1 track not played: more than 128 tracks")
        );
    }

    #[test]
    fn lookups_match_position_not_id() {
        let diagnostics = [
            track(3, Reason::DuplicateId),
            device(1, 2, DeviceRole::Effect, Reason::SlotLimit),
        ];
        assert_eq!(for_track(&diagnostics, 3), Some(&diagnostics[0]));
        assert_eq!(for_track(&diagnostics, 1), None);
        assert_eq!(for_device(&diagnostics, 1, 2), Some(&diagnostics[1]));
        assert_eq!(for_device(&diagnostics, 1, 1), None);
        assert_eq!(for_device(&diagnostics, 3, 2), None);
    }
}
