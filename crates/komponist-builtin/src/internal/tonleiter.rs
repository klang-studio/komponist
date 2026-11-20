//! Tonleiter (WP-88): forces notes onto a scale. A root and a scale type give the allowed pitch
//! classes; a note outside them is moved to the next allowed key above, below or the nearest one
//! (a tie goes down), or filtered out.
//!
//! The stage remembers what it did with every key that went down, per channel, so a note-off
//! always ends the note its note-on started, also after a parameter change in between. Two keys
//! that land on the same key share one note: it starts with the first and ends with the last
//! release. Other MIDI messages pass through.

use komponist_engine::{Event, EventKind, EventProcessor, EventSink, NOTE_ID_NONE, ProcessContext};
use komponist_i18n::Lang;
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, WidgetKind};
use komponist_model::params::{ParamId, ParamInfo, ParamKind};

use super::midi::PITCH_CLASSES;
use super::{Preset, Spec, midi, single_page};

/// The device.
pub(super) static SPEC: Spec = Spec {
    catalog: komponist_device_core::CatalogSpec {
        id: "tonleiter",
        name: "Tonleiter",
        name_in: Lang::device_name_tonleiter,
        role: komponist_model::DeviceRole::MidiEffect,
        features: &["note-effect", "scale"],
        vendor: "komponist",
        sidechain: false,
    },
    params,
    layout,
    dsp: midi::passthrough,
    presets,
    event_stage: Some(stage),
    preset_aliases: &[
        ("C-Dur", "C Major"),
        ("A-Moll", "A Minor"),
        ("D-Dorisch", "D Dorian"),
        ("A-Moll-Pentatonik", "A Minor Pentatonic"),
        ("C-Blues", "C Blues"),
        ("C-Dur filtern", "C Major Filtered"),
    ],
    migrate_params: None,
    cache: komponist_device_core::SpecCache::new(),
};

/// Wire ids.
pub const ROOT: u32 = 0;
pub const SCALE: u32 = 1;
pub const MODE: u32 = 2;

/// Scale types: name and the allowed semitones above the root, as a bit mask (bit 0 = root).
pub const SCALES: [(&str, u16); 14] = [
    ("Dur", mask(&[0, 2, 4, 5, 7, 9, 11])),
    ("Moll", mask(&[0, 2, 3, 5, 7, 8, 10])),
    ("Harmonisch Moll", mask(&[0, 2, 3, 5, 7, 8, 11])),
    ("Melodisch Moll", mask(&[0, 2, 3, 5, 7, 9, 11])),
    ("Dorisch", mask(&[0, 2, 3, 5, 7, 9, 10])),
    ("Phrygisch", mask(&[0, 1, 3, 5, 7, 8, 10])),
    ("Lydisch", mask(&[0, 2, 4, 6, 7, 9, 11])),
    ("Mixolydisch", mask(&[0, 2, 4, 5, 7, 9, 10])),
    ("Lokrisch", mask(&[0, 1, 3, 5, 6, 8, 10])),
    ("Pentatonik Dur", mask(&[0, 2, 4, 7, 9])),
    ("Pentatonik Moll", mask(&[0, 3, 5, 7, 10])),
    ("Blues", mask(&[0, 3, 5, 6, 7, 10])),
    ("Ganzton", mask(&[0, 2, 4, 6, 8, 10])),
    ("Chromatisch", 0x0fff),
];

/// What happens to a key outside the scale (the `mode` choices, in order).
pub const MODES: [&str; 4] = ["Nächster Ton", "Aufwärts", "Abwärts", "Filtern"];

/// How a key outside the scale is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    /// The nearest allowed key; equally near keys: the lower one.
    Nearest,
    /// The next allowed key above.
    Up,
    /// The next allowed key below.
    Down,
    /// The note is dropped.
    Filter,
}

const fn mask(semitones: &[u8]) -> u16 {
    let mut m = 0;
    let mut i = 0;
    while i < semitones.len() {
        m |= 1 << semitones[i];
        i += 1;
    }
    m
}

/// The key `key` becomes with scale `scale` (index into [`SCALES`]) on `root` (0 = C … 11 = B),
/// `None` when it is filtered out or no allowed key exists inside 0 ..= 127 in that direction.
pub fn map_key(key: u8, root: u8, scale: usize, snap: Snap) -> Option<u8> {
    let allowed = SCALES[scale.min(SCALES.len() - 1)].1;
    let key = i32::from(key);
    let in_scale = |k: i32| -> bool {
        (0..=127).contains(&k) && allowed & (1 << ((k - i32::from(root)).rem_euclid(12))) != 0
    };
    if in_scale(key) {
        return Some(key as u8);
    }
    // Distances to the next allowed key in each direction (at most 11 apart in any scale).
    let up = (1..12).find(|d| in_scale(key + d));
    let down = (1..12).find(|d| in_scale(key - d));
    let target = match snap {
        Snap::Filter => None,
        Snap::Up => up.map(|d| key + d),
        Snap::Down => down.map(|d| key - d),
        Snap::Nearest => match (up, down) {
            (Some(u), Some(d)) => Some(if d <= u { key - d } else { key + u }),
            (Some(u), None) => Some(key + u),
            (None, Some(d)) => Some(key - d),
            (None, None) => None,
        },
    };
    target.map(|k| k as u8)
}

/// The factory presets (WP-96): a small starter set.
fn presets() -> &'static [Preset] {
    static LIST: [Preset; 6] = [
        Preset::new("C Major", &[]),
        Preset::new("A Minor", &[("root", 9.0), ("scale", 1.0)]),
        Preset::new("D Dorian", &[("root", 2.0), ("scale", 4.0)]),
        Preset::new("A Minor Pentatonic", &[("root", 9.0), ("scale", 10.0)]),
        Preset::new("C Blues", &[("scale", 11.0)]),
        Preset::new("C Major Filtered", &[("mode", 3.0)]),
    ];
    &LIST
}

/// The names of the scale types in `t`, in the order of [`SCALES`].
fn scale_names(t: Lang) -> [&'static str; 14] {
    [
        t.choice_scale_major(),
        t.choice_scale_minor(),
        t.choice_scale_harmonic_minor(),
        t.choice_scale_melodic_minor(),
        t.choice_scale_dorian(),
        t.choice_scale_phrygian(),
        t.choice_scale_lydian(),
        t.choice_scale_mixolydian(),
        t.choice_scale_locrian(),
        t.choice_scale_pentatonic_major(),
        t.choice_scale_pentatonic_minor(),
        t.choice_scale_blues(),
        t.choice_scale_whole_tone(),
        t.choice_scale_chromatic(),
    ]
}

/// The names of the modes in `t`, in the order of [`MODES`].
fn mode_names(t: Lang) -> [&'static str; 4] {
    [
        t.choice_scale_mode_nearest(),
        t.choice_scale_mode_up(),
        t.choice_scale_mode_down(),
        t.choice_scale_mode_filter(),
    ]
}

fn params(t: Lang) -> Vec<ParamInfo> {
    let names = |list: &[&str]| ParamKind::Enum(list.iter().map(|n| (*n).to_owned()).collect());
    vec![
        ParamInfo::new("root", t.param_scale_root(), names(&PITCH_CLASSES), 0.0),
        ParamInfo::new("scale", t.param_scale_scale(), names(&scale_names(t)), 0.0),
        ParamInfo::new("mode", t.param_mode(), names(&mode_names(t)), 0.0),
    ]
}

/// Five columns (the head of the card needs the room for the name): root and mode as dropdowns,
/// the scale under them.
fn layout(_: Lang) -> DeviceLayout {
    let id = |key: &str| CellParam::Id(ParamId::new(key));
    single_page(
        5,
        vec![
            Cell::half(0, 0, 2, Half::Top, WidgetKind::Dropdown, id("root")),
            Cell::half(2, 0, 3, Half::Top, WidgetKind::Dropdown, id("mode")),
            Cell::half(0, 0, 5, Half::Bottom, WidgetKind::Dropdown, id("scale")),
        ],
    )
}

/// The stage with the parameters at their defaults (C, Dur, nearest).
pub(super) fn stage() -> Box<dyn EventProcessor + Send> {
    Box::new(Scale::new())
}

/// No key mapped.
const NONE: u8 = u8::MAX;

struct Scale {
    root: u8,
    scale: usize,
    snap: Snap,
    /// Per channel and input key: the key it was played as, or [`NONE`].
    played_as: Box<[[u8; 128]; 16]>,
    /// Per channel and input key: the ID of the note currently holding the key.
    input_ids: Box<[[u32; 128]; 16]>,
    /// Per channel and output key: how many input keys hold it.
    holders: Box<[[u8; 128]; 16]>,
    /// The ID of the note-on forwarded for each held output key.
    forwarded_ids: Box<[[u32; 128]; 16]>,
}

impl Scale {
    fn new() -> Scale {
        Scale {
            root: 0,
            scale: 0,
            snap: Snap::Nearest,
            played_as: Box::new([[NONE; 128]; 16]),
            input_ids: Box::new([[NOTE_ID_NONE; 128]; 16]),
            holders: Box::new([[0; 128]; 16]),
            forwarded_ids: Box::new([[NOTE_ID_NONE; 128]; 16]),
        }
    }
}

impl EventProcessor for Scale {
    fn set_param(&mut self, id: u32, value: f64) {
        match id {
            ROOT => self.root = (value as u8).min(11),
            SCALE => self.scale = (value as usize).min(SCALES.len() - 1),
            MODE => {
                self.snap = match value as usize {
                    1 => Snap::Up,
                    2 => Snap::Down,
                    3 => Snap::Filter,
                    _ => Snap::Nearest,
                };
            }
            _ => {}
        }
    }

    fn process_events(&mut self, _ctx: &ProcessContext, input: &[Event], out: &mut EventSink<'_>) {
        for e in input {
            match e.kind {
                EventKind::NoteOn {
                    key,
                    velocity,
                    channel,
                    note_id,
                } => {
                    let (ch, k) = (usize::from(channel & 15), usize::from(key & 127));
                    if self.played_as[ch][k] != NONE {
                        // A note-on for a key that is down: the old note ends first.
                        self.end(ch, k, e.offset, out);
                    }
                    let Some(mapped) = map_key(key, self.root, self.scale, self.snap) else {
                        continue;
                    };
                    self.played_as[ch][k] = mapped;
                    self.input_ids[ch][k] = note_id;
                    let holders = &mut self.holders[ch][usize::from(mapped)];
                    *holders = holders.saturating_add(1);
                    if *holders == 1 {
                        self.forwarded_ids[ch][usize::from(mapped)] = note_id;
                        out.push(
                            e.offset,
                            EventKind::NoteOn {
                                key: mapped,
                                velocity,
                                channel,
                                note_id,
                            },
                        );
                    }
                }
                EventKind::NoteOff {
                    key,
                    channel,
                    note_id,
                } => {
                    let (ch, k) = (usize::from(channel & 15), usize::from(key & 127));
                    if note_id == NOTE_ID_NONE || note_id == self.input_ids[ch][k] {
                        self.end(ch, k, e.offset, out);
                    }
                }
                EventKind::AllNotesOff => {
                    self.reset();
                    out.push(e.offset, EventKind::AllNotesOff);
                }
                other => {
                    out.push(e.offset, other);
                }
            }
        }
    }

    fn reset(&mut self) {
        *self.played_as = [[NONE; 128]; 16];
        self.input_ids.fill([NOTE_ID_NONE; 128]);
        *self.holders = [[0; 128]; 16];
        self.forwarded_ids.fill([NOTE_ID_NONE; 128]);
    }
}

impl Scale {
    /// The note started by input key `k` on channel `ch` ends (the last holder sends the off).
    fn end(&mut self, ch: usize, k: usize, offset: u32, out: &mut EventSink<'_>) {
        let mapped = std::mem::replace(&mut self.played_as[ch][k], NONE);
        if mapped == NONE {
            return;
        }
        self.input_ids[ch][k] = NOTE_ID_NONE;
        let holders = &mut self.holders[ch][usize::from(mapped)];
        *holders = holders.saturating_sub(1);
        if *holders == 0 {
            out.push(
                offset,
                EventKind::NoteOff {
                    key: mapped,
                    channel: ch as u8,
                    note_id: self.forwarded_ids[ch][usize::from(mapped)],
                },
            );
        }
    }
}
