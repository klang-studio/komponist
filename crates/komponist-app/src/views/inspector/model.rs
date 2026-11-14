//! The inspector as data: which sections show for the selection, the device rows with their
//! status, the take rows, and the text of the clip fields (positions, lengths, signed values)
//! with their parsers. Pure, so it is tested without pixels.

use std::collections::BTreeSet;

use komponist_model::format::MINUS;
use komponist_model::{
    ClipContent, ClipId, Device, DeviceId, DeviceRole, Meter, Project, TICKS_PER_STEP, Tick,
    TrackId,
};
use komponist_plugins::{DeviceStatus, FailReason};

use crate::device_host::DeviceHost;
use crate::ui_state::{DockTab, PanelLayout};
use crate::views::browser::model::selected_track;
use crate::views::format_tag;

/// German texts of the inspector (`Screens.dc.html` `sec`, `F`, `trackSec`, the inspector
/// markup; `ks-view.js` `inspView`; S01, S02, S03, S10).
pub mod names {
    /// Section "Clip".
    pub const CLIP: &str = "Clip";
    /// Section "Spur".
    pub const TRACK: &str = "Spur";
    /// Section "Geräte".
    pub const DEVICES: &str = "Geräte";
    /// Section "Kanal".
    pub const CHANNEL: &str = "Kanal";
    /// Section "Projekt" (nothing selected).
    pub const PROJECT: &str = "Projekt";
    /// Section "Auswahl" (nothing selected).
    pub const SELECTION: &str = "Auswahl";
    /// Its text (S10).
    pub const NOTHING_SELECTED: &str = "Nichts ausgewählt. Wenn nichts gewählt ist, zeigt der Inspector die Projekt-Einstellungen.";
    /// Clip kind of a MIDI clip (`meta:'MIDI'`).
    pub const MIDI: &str = "MIDI";
    /// Clip kind of a step clip (`meta:'STEP'`).
    pub const STEP: &str = "STEP";
    /// The clip being recorded (`meta:'nimmt auf'`).
    pub const RECORDING: &str = "nimmt auf";
    /// Field "Start".
    pub const START: &str = "Start";
    /// Field "Länge".
    pub const LENGTH: &str = "Länge";
    /// Field "Loop".
    pub const LOOP: &str = "Loop";
    /// Field "Loop-Länge".
    pub const LOOP_LENGTH: &str = "Loop-Länge";
    /// Field "Transponieren".
    pub const TRANSPOSE: &str = "Transponieren";
    /// Its unit.
    pub const SEMITONES: &str = "st";
    /// Field "Velocity".
    pub const VELOCITY: &str = "Velocity";
    /// Unit of velocity and swing.
    pub const PERCENT: &str = "%";
    /// Field "Pattern" (step clips).
    pub const PATTERN: &str = "Pattern";
    /// Its unit.
    pub const STEPS: &str = "Steps";
    /// Field "Auflösung" (step clips).
    pub const RESOLUTION: &str = "Auflösung";
    /// Field "Swing" (step clips).
    pub const SWING: &str = "Swing";
    /// Field "Noten" (recording).
    pub const NOTES: &str = "Noten";
    /// Field "Quantisieren" (recording).
    pub const QUANTIZE: &str = "Quantisieren";
    /// Value "an".
    pub const ON: &str = "an";
    /// Value "aus".
    pub const OFF: &str = "aus";
    /// The takes list (S02 "Jeder Durchlauf wird ein eigener Take im selben Clip, wählbar im
    /// Inspector"; "Takes" as in the browser's Projekt section).
    pub const TAKES: &str = "Takes";
    /// Dropdown "Instrument".
    pub const INSTRUMENT: &str = "Instrument";
    /// Its entry for a track without instrument (not in the design).
    pub const NO_INSTRUMENT: &str = "Kein Instrument";
    /// Dropdown "Eingang".
    pub const INPUT: &str = "Eingang";
    /// Its default entry.
    pub const ALL_INPUTS: &str = "Alle MIDI-Eingänge";
    /// Dropdown "Kanal" (MIDI channel).
    pub const MIDI_CHANNEL: &str = "Kanal";
    /// Its default entry.
    pub const OMNI: &str = "Omni";
    /// Meta action of "Geräte" (`meta:'+ Effekt'`).
    pub const ADD_EFFECT: &str = "+ Effekt";
    /// Meta action of "Kanal" (`meta:'im Mixer zeigen'`).
    pub const SHOW_IN_MIXER: &str = "im Mixer zeigen";
    /// The level in the compact strip ("Lautstärke").
    pub const VOLUME: &str = "Lautstärke";
    /// The pan knob ("Pan").
    pub const PAN: &str = "Pan";
    /// The meter of the compact strip.
    pub const METER: &str = "Pegel";
    /// Field "Tempo".
    pub const TEMPO: &str = "Tempo";
    /// Its unit.
    pub const BPM: &str = "BPM";
    /// Field "Taktart".
    pub const METER_FIELD: &str = "Taktart";
    /// Field "Abtastrate" (S10).
    pub const SAMPLE_RATE: &str = "Abtastrate";
    /// Its unit.
    pub const KHZ: &str = "kHz";
    /// Field "Spuren".
    pub const TRACKS: &str = "Spuren";
    /// Unit of the project length.
    pub const BARS: &str = "Takte";
    /// Accessible name of the power square of a device row (as the device card's switch).
    pub const ACTIVE: &str = "Aktiv";
    /// Status of a loading device (`ks-view.js` `fmt:'lädt'`).
    pub const LOADING: &str = "lädt";
    /// Status of a crashed device (S05 `D('Airwindows Cons.','abgestürzt',{err:1})`).
    pub const CRASHED: &str = "abgestürzt";
    /// Status of a device that failed to load (as the card's „Lässt sich nicht laden“).
    pub const LOAD_FAILED: &str = "lässt sich nicht laden";
    /// Status of a device that stopped answering (as the card's „Hängt“).
    pub const UNRESPONSIVE: &str = "hängt";
    /// Status of a missing device (by analogy, S06 „Plugin fehlt“).
    pub const MISSING: &str = "fehlt";
    pub use crate::views::late::{LATE, late_blocks};
    /// Undo label of a clip field drag.
    pub const UNDO_CLIP: &str = "Clip ändern";
    /// Undo label of a tempo drag.
    pub const UNDO_TEMPO: &str = "Tempo";
    /// The name field while renaming.
    pub const RENAME: &str = "Name";

    /// „3 / 6“: the track's position.
    pub fn position(index: usize, count: usize) -> String {
        format!("{} / {count}", index + 1)
    }

    /// „Farbe Grün“: a colour swatch.
    pub fn swatch(hue: &str) -> String {
        format!("Farbe {hue}")
    }

    /// „Gerät LSP Para EQ x8“: a device row.
    pub fn device(name: &str) -> String {
        format!("Gerät {name}")
    }

    /// „14 Noten“: the notes of a take.
    pub fn notes(n: usize) -> String {
        if n == 1 {
            "1 Note".to_owned()
        } else {
            format!("{n} Noten")
        }
    }

    /// „Take 2“ for a take without name (a hand-drawn clip's only take has none).
    pub fn take(index: usize) -> String {
        format!("Take {}", index + 1)
    }
}

/// What the inspector shows (`inspView`): the clip section for the one selected clip or the take
/// being recorded; for the selected track (the clip's track) its track, devices and channel
/// sections, the channel without the mixer in view (S04); with nothing selected the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sections {
    /// The clip section.
    pub clip: Option<ClipSection>,
    /// The track whose sections show.
    pub track: Option<TrackId>,
    /// The compact channel strip.
    pub channel: bool,
}

/// Which clip the clip section shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipSection {
    /// A clip of the project.
    Clip(ClipId),
    /// The take being recorded on this track (S02 "nimmt auf").
    Recording(TrackId),
}

impl Sections {
    /// The sections for `selected_tracks` and `selected_clips`; `recording` are the tracks
    /// recording right now; the channel strip hides while the dock shows the mixer.
    pub fn of(
        project: &Project,
        selected_tracks: &BTreeSet<TrackId>,
        selected_clips: &BTreeSet<ClipId>,
        recording: &[TrackId],
        layout: &PanelLayout,
    ) -> Sections {
        let track = selected_track(project, selected_tracks);
        let clip = match track.filter(|t| recording.contains(t)) {
            Some(t) => Some(ClipSection::Recording(t)),
            None if selected_clips.len() == 1 => selected_clips
                .iter()
                .next()
                .filter(|c| project.find_clip(**c).is_some())
                .map(|c| ClipSection::Clip(*c)),
            None => None,
        };
        let mixer = layout.dock_open && layout.dock_tab == DockTab::Mixer;
        Sections {
            clip,
            track,
            channel: track.is_some() && !mixer,
        }
    }

    /// Whether nothing is selected: the project and "Auswahl" show.
    pub fn is_empty(&self) -> bool {
        self.clip.is_none() && self.track.is_none()
    }
}

/// One row of the devices list.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRow {
    /// The device.
    pub device: DeviceId,
    /// Plugin name.
    pub name: String,
    /// The track's instrument (`surface.3`, semibold).
    pub instrument: bool,
    /// Bypassed (`text.3`, power square with contour).
    pub bypass: bool,
    /// Runtime state.
    pub status: RowStatus,
}

/// The state a device row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Running.
    Ok,
    /// Loading: power square `text.3`.
    Loading,
    /// Crashed: red name and "abgestürzt".
    Crashed,
    /// Loading failed: red name and "lässt sich nicht laden" (not a crash).
    LoadFailed,
    /// Stopped answering: red name and "hängt" (not a crash).
    Unresponsive,
    /// Missing: red name and "fehlt".
    Missing,
}

impl DeviceRow {
    /// The text on the right: the format tag, or the failure.
    pub fn tag(&self, format: &'static str) -> &'static str {
        match self.status {
            RowStatus::Crashed => names::CRASHED,
            RowStatus::LoadFailed => names::LOAD_FAILED,
            RowStatus::Unresponsive => names::UNRESPONSIVE,
            RowStatus::Missing => names::MISSING,
            RowStatus::Ok | RowStatus::Loading => format,
        }
    }
}

/// The rows of `devices` with their status from `host`, and each one's format tag.
pub fn device_rows(
    devices: &[std::sync::Arc<Device>],
    host: &dyn DeviceHost,
) -> Vec<(DeviceRow, &'static str)> {
    devices
        .iter()
        .map(|d| {
            let status = match host.status(d.id) {
                DeviceStatus::Ok => RowStatus::Ok,
                DeviceStatus::Loading => RowStatus::Loading,
                DeviceStatus::Crashed(FailReason::Exited { .. }) => RowStatus::Crashed,
                DeviceStatus::Crashed(FailReason::LoadFailed(_)) => RowStatus::LoadFailed,
                DeviceStatus::Crashed(FailReason::Unresponsive) => RowStatus::Unresponsive,
                DeviceStatus::Missing => RowStatus::Missing,
            };
            (
                DeviceRow {
                    device: d.id,
                    name: d.plugin.name.clone(),
                    instrument: d.role == DeviceRole::Instrument,
                    bypass: d.bypass,
                    status,
                },
                format_tag(d.plugin.format),
            )
        })
        .collect()
}

/// One take of a MIDI clip: name, note count, whether it plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeRow {
    /// Shown name ("Take 2").
    pub name: String,
    /// Notes in the take.
    pub notes: usize,
    /// The active take.
    pub active: bool,
}

/// The takes of `content` if there is more than one (after loop recording), else none.
pub fn take_rows(content: &ClipContent) -> Vec<TakeRow> {
    let ClipContent::Midi(m) = content else {
        return Vec::new();
    };
    if m.takes.len() < 2 {
        return Vec::new();
    }
    m.takes
        .iter()
        .enumerate()
        .map(|(i, t)| TakeRow {
            name: if t.name.is_empty() {
                names::take(i)
            } else {
                t.name.clone()
            },
            notes: t.notes.len(),
            active: i == m.active_take,
        })
        .collect()
}

/// A signed whole number with U+2212 for the minus: "+3", "−12"; zero as "0", or "+0" with
/// `plus_zero` (the design's "Velocity +0 %", "Transponieren 0 st").
pub fn signed(value: i32, plus_zero: bool) -> String {
    match value {
        0 if plus_zero => "+0".to_owned(),
        0 => "0".to_owned(),
        v if v > 0 => format!("+{v}"),
        v => format!("{MINUS}{}", -v),
    }
}

/// Parses a signed whole number: "+3", "-3", "−3", "3".
pub fn parse_signed(text: &str) -> Option<f64> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let t = t.replace(MINUS, "-");
    let t = t.strip_prefix('+').unwrap_or(&t);
    t.parse::<i32>().ok().map(f64::from)
}

fn parts(text: &str) -> Option<Vec<i64>> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if t.is_empty() {
        return None;
    }
    t.split('.')
        .map(|p| p.parse::<i64>().ok().filter(|v| *v >= 0))
        .collect()
}

/// Parses a position as bar, beat and 1/16 (1-based, "9.1.1"; "9.2" and "9" are allowed) into
/// ticks; `None` if a part is out of range.
pub fn parse_bar_pos(text: &str, meter: Meter) -> Option<Tick> {
    let p = parts(text)?;
    let (bar, beat, six) = match p.as_slice() {
        [bar] => (*bar, 1, 1),
        [bar, beat] => (*bar, *beat, 1),
        [bar, beat, six] => (*bar, *beat, *six),
        _ => return None,
    };
    let per_beat = meter.ticks_per_beat();
    let steps = per_beat / TICKS_PER_STEP;
    let beats = i64::from(meter.num);
    if bar < 1 || !(1..=beats).contains(&beat) || !(1..=steps.max(1)).contains(&six) {
        return None;
    }
    Some(Tick(
        (bar - 1) * meter.ticks_per_bar() + (beat - 1) * per_beat + (six - 1) * TICKS_PER_STEP,
    ))
}

/// Parses a length as bars, beats and display ticks (0-based, "2.0.0", "1.2.96"; "2.2" and "2"
/// are allowed) into ticks; `None` if a part is out of range.
pub fn parse_length(text: &str, meter: Meter) -> Option<Tick> {
    let p = parts(text)?;
    let (bars, beats, ticks) = match p.as_slice() {
        [bars] => (*bars, 0, 0),
        [bars, beats] => (*bars, *beats, 0),
        [bars, beats, ticks] => (*bars, *beats, *ticks),
        _ => return None,
    };
    let per_beat = meter.ticks_per_beat();
    if beats >= i64::from(meter.num) || ticks >= komponist_model::DISPLAY_TICKS_PER_BEAT {
        return None;
    }
    Some(Tick(
        bars * meter.ticks_per_bar()
            + beats * per_beat
            + ticks * per_beat / komponist_model::DISPLAY_TICKS_PER_BEAT,
    ))
}

/// A sample rate in kHz as S10 shows it: "48", "44.1", "96".
pub fn khz(rate: u32) -> String {
    let khz = f64::from(rate) / 1000.0;
    if khz.fract() == 0.0 {
        format!("{khz:.0}")
    } else {
        format!("{khz}")
    }
}

/// Ticks as 1/16 steps (the unit the clip's number fields drag in: 1 px = one 1/16).
pub fn to_steps(tick: Tick) -> f64 {
    tick.0 as f64 / TICKS_PER_STEP as f64
}

/// 1/16 steps back to ticks, on whole steps.
pub fn from_steps(steps: f64) -> Tick {
    Tick((steps.round() as i64) * TICKS_PER_STEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device row names the failure like the device card does.
    #[test]
    fn device_rows_tell_failures_apart() {
        let row = |status| DeviceRow {
            device: komponist_model::DeviceId(1),
            name: String::new(),
            instrument: false,
            bypass: false,
            status,
        };
        assert_eq!(row(RowStatus::Crashed).tag("CLAP"), "abgestürzt");
        assert_eq!(
            row(RowStatus::LoadFailed).tag("CLAP"),
            "lässt sich nicht laden"
        );
        assert_eq!(row(RowStatus::Unresponsive).tag("CLAP"), "hängt");
        assert_eq!(row(RowStatus::Ok).tag("CLAP"), "CLAP");
    }

    /// A builtin host that has seen every device of `p` (unknown devices report `Ok`).
    fn host_for(p: &komponist_model::Project) -> BuiltinHost {
        let mut host = BuiltinHost::new();
        for t in &p.tracks {
            for d in &t.channel.devices {
                let _ = host.create_processor(
                    d,
                    komponist_plugins::ProcessorSetup {
                        sample_rate: 48_000.0,
                        max_block: 256,
                    },
                );
            }
        }
        host
    }
    use crate::device_host::BuiltinHost;
    use crate::ui_state::PanelLayout;
    use komponist_model::fixtures::demo_project;
    use komponist_model::format::{fmt_bar_pos, fmt_length};
    use komponist_model::{MidiClip, Take};
    use std::sync::Arc;

    #[test]
    fn sections_follow_the_selection() {
        let p = demo_project();
        let lead = p.tracks[2].id;
        let lead_a = p.tracks[2].clips[0].id;
        let layout = PanelLayout::default();
        let none = Sections::of(&p, &BTreeSet::new(), &BTreeSet::new(), &[], &layout);
        assert!(none.is_empty());
        let track = Sections::of(&p, &BTreeSet::from([lead]), &BTreeSet::new(), &[], &layout);
        assert_eq!(
            (track.clip, track.track, track.channel),
            (None, Some(lead), true)
        );
        let clip = Sections::of(
            &p,
            &BTreeSet::from([lead]),
            &BTreeSet::from([lead_a]),
            &[],
            &layout,
        );
        assert_eq!(clip.clip, Some(ClipSection::Clip(lead_a)));
        // Two clips: no clip section.
        let two = BTreeSet::from([lead_a, p.tracks[2].clips[1].id]);
        assert_eq!(
            Sections::of(&p, &BTreeSet::from([lead]), &two, &[], &layout).clip,
            None
        );
        // Recording on Lead wins over the clip.
        let rec = Sections::of(
            &p,
            &BTreeSet::from([lead]),
            &BTreeSet::from([lead_a]),
            &[lead],
            &layout,
        );
        assert_eq!(rec.clip, Some(ClipSection::Recording(lead)));
        // S04: the mixer is in view, the channel strip hides.
        let mixer = PanelLayout {
            dock_tab: DockTab::Mixer,
            dock_maximized: true,
            ..PanelLayout::default()
        };
        assert!(!Sections::of(&p, &BTreeSet::from([lead]), &BTreeSet::new(), &[], &mixer).channel);
    }

    #[test]
    fn device_rows_carry_status_and_tag() {
        let p = demo_project();
        let host = host_for(&p);
        let rows = device_rows(&p.tracks[2].channel.devices, &host);
        assert_eq!(rows.len(), 4);
        let (pisynth, tag) = &rows[0];
        assert!(pisynth.instrument && pisynth.status == RowStatus::Ok);
        assert_eq!(pisynth.tag(tag), "INTERN");
        let (eq, tag) = &rows[1];
        assert_eq!(eq.status, RowStatus::Missing);
        assert_eq!(eq.tag(tag), "fehlt");
        assert!(rows[2].0.bypass);
    }

    #[test]
    fn takes_show_after_loop_recording() {
        let take = |name: &str, n: usize| Take {
            name: name.into(),
            notes: Arc::new(
                (0..n)
                    .map(|i| komponist_model::Note {
                        id: komponist_model::NoteId(i as u64 + 100),
                        start: Tick(i as i64 * 240),
                        length: Tick(240),
                        pitch: 60,
                        velocity: 100,
                    })
                    .collect(),
            ),
        };
        let one = ClipContent::Midi(MidiClip {
            takes: vec![take("", 3)],
            active_take: 0,
        });
        assert!(take_rows(&one).is_empty());
        let two = ClipContent::Midi(MidiClip {
            takes: vec![take("Take 1", 2), take("Take 2", 1)],
            active_take: 1,
        });
        assert_eq!(
            take_rows(&two),
            [
                TakeRow {
                    name: "Take 1".into(),
                    notes: 2,
                    active: false
                },
                TakeRow {
                    name: "Take 2".into(),
                    notes: 1,
                    active: true
                }
            ]
        );
        assert_eq!(names::notes(1), "1 Note");
    }

    #[test]
    fn signed_values() {
        assert_eq!(signed(0, true), "+0");
        assert_eq!(signed(0, false), "0");
        assert_eq!(signed(12, false), "+12");
        assert_eq!(signed(-5, true), "\u{2212}5");
        assert_eq!(parse_signed("−5"), Some(-5.0));
        assert_eq!(parse_signed("+12"), Some(12.0));
        assert_eq!(parse_signed(" 3 "), Some(3.0));
        assert_eq!(parse_signed("x"), None);
    }

    #[test]
    fn positions_and_lengths_round_trip() {
        let m = Meter::FOUR_FOUR;
        // "9.1.1" = bar 9.
        assert_eq!(parse_bar_pos("9.1.1", m), Some(Tick(8 * 3840)));
        assert_eq!(parse_bar_pos("9.2", m), Some(Tick(8 * 3840 + 960)));
        assert_eq!(parse_bar_pos("1.1.3", m), Some(Tick(480)));
        assert_eq!(parse_bar_pos("0.1.1", m), None);
        assert_eq!(parse_bar_pos("1.5.1", m), None);
        assert_eq!(parse_bar_pos("1.1.5", m), None);
        assert_eq!(parse_bar_pos("a", m), None);
        for t in [0, 240, 3840 + 720, 8 * 3840] {
            assert_eq!(parse_bar_pos(&fmt_bar_pos(Tick(t), m), m), Some(Tick(t)));
        }
        assert_eq!(parse_length("2.0.0", m), Some(Tick(2 * 3840)));
        assert_eq!(parse_length("1.2.96", m), Some(Tick(3840 + 1920 + 384)));
        assert_eq!(parse_length("2", m), Some(Tick(7680)));
        assert_eq!(parse_length("1.4.0", m), None);
        for t in [240, 3840, 3840 + 2160, 4 * 3840] {
            assert_eq!(parse_length(&fmt_length(Tick(t), m), m), Some(Tick(t)));
        }
        assert_eq!(to_steps(Tick(3840)), 16.0);
        assert_eq!(khz(48_000), "48");
        assert_eq!(khz(44_100), "44.1");
        assert_eq!(khz(96_000), "96");
        assert_eq!(from_steps(16.4), Tick(3840));
    }
}
