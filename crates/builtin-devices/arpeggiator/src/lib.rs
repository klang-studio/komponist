//! Arpeggiator (WP-88): held notes become a pattern, one note per step, in time with the song.
//!
//! - **Muster** (`mode`): Auf, Ab, Auf/Ab (turns at both ends without repeating the end note),
//!   Zufall (a fixed pseudo-random sequence, restarted on every reset, so renders repeat),
//!   Gespielt (in the order the keys went down).
//! - **Rate**: note values 1/4 … 1/32, straight and triplets (`T`), in ticks of the project
//!   (960 per quarter note); steps lie on the song's tick grid, so they follow tempo changes and
//!   line up with clips. Every step lands on the sample `floor(tick × samples per tick + ½)`, the
//!   rounding of the sequencer (`TempoMap::tick_to_sample`), whatever the block boundaries are.
//! - **Gate**: note length as a share of the step; the note-off always comes after the note-on.
//! - **Oktaven** 1–4: the pattern runs through the notes in each octave in turn.
//! - **Halten** (latch): released keys stay in the pattern until a new key goes down while none is
//!   held; switching it off drops released keys.
//!
//! Time: while the transport runs the stage reads the song position from the processor call's
//! transport; stopped (live playing), it runs on its own clock. A jump of the position (start,
//! locate, loop wrap) ends the sounding note at once and puts the next step on the grid at the
//! new position. The pattern counter restarts when the first key of a chord goes down and
//! continues when keys change.
//!
//! The engine keeps notes from hanging whatever this stage does (`komponist_engine::EventProcessor`).

use komponist_device_core::layout::{single_page, toggle};
use komponist_device_core::{CatalogSpec, Preset, Spec, SpecCache, midi};
use komponist_engine::{Event, EventKind, EventProcessor, EventSink, NOTE_ID_NONE, ProcessContext};
use komponist_i18n::Lang;
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, WidgetKind};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Unit};

/// The built-in device specification.
pub static SPEC: Spec = Spec {
    catalog: CatalogSpec {
        id: "arpeggiator",
        name: "Arpeggiator",
        name_in: Lang::device_name_arpeggiator,
        role: komponist_model::DeviceRole::MidiEffect,
        features: &["note-effect", "arpeggiator"],
        vendor: "komponist",
        sidechain: false,
    },
    params,
    layout,
    dsp: midi::passthrough,
    presets,
    event_stage: Some(stage),
    preset_aliases: &[
        ("Achtel auf", "Eighths Up"),
        ("Sechzehntel Auf/Ab", "Sixteenths Up/Down"),
        ("Triolen ab", "Triplets Down"),
        ("Zufall", "Random"),
        ("Gespielt weich", "Soft As Played"),
    ],
    migrate_params: None,
    cache: SpecCache::new(),
};

/// Wire ids.
pub const MODE: u32 = 0;
pub const RATE: u32 = 1;
pub const GATE: u32 = 2;
pub const OCTAVES: u32 = 3;
pub const HOLD: u32 = 4;

/// Pattern modes (the `mode` choices, in order).
pub const MODES: [&str; 5] = ["Auf", "Ab", "Auf/Ab", "Zufall", "Gespielt"];

/// Rates: label and step length in ticks (the `rate` choices, in order).
pub const RATES: [(&str, f64); 8] = [
    ("1/4", 960.0),
    ("1/4T", 640.0),
    ("1/8", 480.0),
    ("1/8T", 320.0),
    ("1/16", 240.0),
    ("1/16T", 160.0),
    ("1/32", 120.0),
    ("1/32T", 80.0),
];

/// Most keys the pattern holds at once.
pub const MAX_HELD: usize = 32;

/// The factory presets (WP-96): a small starter set.
fn presets() -> &'static [Preset] {
    static LIST: [Preset; 5] = [
        Preset::new("Eighths Up", &[("rate", 2.0)]),
        Preset::new(
            "Sixteenths Up/Down",
            &[("mode", 2.0), ("gate", 0.5), ("octaves", 2.0)],
        ),
        Preset::new(
            "Triplets Down",
            &[
                ("mode", 1.0),
                ("rate", 3.0),
                ("gate", 0.7),
                ("octaves", 2.0),
            ],
        ),
        Preset::new("Random", &[("mode", 3.0), ("gate", 0.5), ("octaves", 2.0)]),
        Preset::new(
            "Soft As Played",
            &[("mode", 4.0), ("rate", 2.0), ("gate", 0.8)],
        ),
    ];
    &LIST
}

/// The names of the pattern modes in `t`, in the order of [`MODES`].
fn mode_names(t: Lang) -> [&'static str; 5] {
    [
        t.choice_arp_up(),
        t.choice_arp_down(),
        t.choice_arp_up_down(),
        t.choice_arp_random(),
        t.choice_arp_as_played(),
    ]
}

fn params(t: Lang) -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(
            "mode",
            t.param_arp_pattern(),
            ParamKind::Enum(mode_names(t).iter().map(|m| (*m).to_owned()).collect()),
            0.0,
        ),
        ParamInfo::new(
            "rate",
            t.param_rate(),
            ParamKind::Enum(RATES.iter().map(|r| r.0.to_owned()).collect()),
            4.0,
        ),
        ParamInfo::new("gate", t.param_gate(), ParamKind::lin(0.05, 1.0), 0.6).unit(Unit::Percent),
        ParamInfo::new(
            "octaves",
            t.param_arp_octaves(),
            ParamKind::Int { min: 1, max: 4 },
            1.0,
        ),
        ParamInfo::new("hold", t.param_arp_hold(), ParamKind::Bool, 0.0),
    ]
}

/// Five columns (the head of the card needs the room for the name): pattern and rate as dropdowns,
/// the latch and the octave count below them, the gate as a knob on the right.
fn layout(_: Lang) -> DeviceLayout {
    let id = |key: &str| CellParam::Id(ParamId::new(key));
    single_page(
        5,
        vec![
            Cell::half(0, 0, 2, Half::Top, WidgetKind::Dropdown, id("mode")),
            Cell::half(2, 0, 2, Half::Top, WidgetKind::Dropdown, id("rate")),
            toggle(0, 0, 2, Half::Bottom, id("hold")),
            Cell::half(2, 0, 1, Half::Bottom, WidgetKind::Number, id("octaves")),
            Cell::span(4, 0, 1, 1, WidgetKind::Knob, id("gate")),
        ],
    )
}

/// The stage with the parameters at their defaults.
fn stage() -> Box<dyn EventProcessor + Send> {
    Box::new(Arp::new())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Up,
    Down,
    UpDown,
    Random,
    AsPlayed,
}

#[derive(Debug, Clone, Copy)]
struct Held {
    key: u8,
    velocity: u8,
    channel: u8,
}

/// The note that sounds now and when its note-off is due.
#[derive(Debug, Clone, Copy)]
struct Sounding {
    key: u8,
    channel: u8,
    /// Tick of the note-off.
    off_at: f64,
}

/// Ticks of the quarter note.
const PPQ: f64 = 960.0;

/// Half the least significant bit of CLAP's beat position, expressed in project ticks.
const CLAP_POSITION_ROUNDING_TICKS: f64 = PPQ / (1u64 << 32) as f64;

/// A deterministic xorshift32 for the random pattern.
const RNG_SEED: u32 = 0x9e37_79b9;

struct Arp {
    mode: Mode,
    /// Step length in ticks.
    step_ticks: f64,
    gate: f64,
    octaves: u32,
    hold: bool,
    /// Keys in the pattern, in the order they went down.
    held: [Held; MAX_HELD],
    count: usize,
    /// `held` indices sorted by key.
    sorted: [u8; MAX_HELD],
    /// Keys that are physically down (bit = key), separately for each MIDI channel.
    down: [u128; 16],
    /// Steps played since the chord started.
    step: u32,
    rng: u32,
    sounding: Option<Sounding>,
    /// Tick of the next step.
    next_tick: f64,
    /// Where the next call should start if the position keeps running.
    expected: f64,
    /// The stage's own clock while the transport is stopped.
    free_pos: f64,
    started: bool,
    was_playing: bool,
    /// The grid changed (rate): put the next step on the new grid at the next call.
    regrid: bool,
}

impl Arp {
    fn new() -> Arp {
        Arp {
            mode: Mode::Up,
            step_ticks: RATES[4].1,
            gate: 0.6,
            octaves: 1,
            hold: false,
            held: [Held {
                key: 0,
                velocity: 0,
                channel: 0,
            }; MAX_HELD],
            count: 0,
            sorted: [0; MAX_HELD],
            down: [0; 16],
            step: 0,
            rng: RNG_SEED,
            sounding: None,
            next_tick: 0.0,
            expected: 0.0,
            free_pos: 0.0,
            started: false,
            was_playing: false,
            regrid: false,
        }
    }

    /// First grid tick at or after `pos`, including a grid position rounded up by the host.
    fn grid_from(&self, pos: f64) -> f64 {
        let tolerance = (self.step_ticks * 1e-9).max(CLAP_POSITION_ROUNDING_TICKS);
        ((pos - tolerance) / self.step_ticks).ceil() * self.step_ticks
    }

    fn resort(&mut self) {
        for i in 0..self.count {
            self.sorted[i] = i as u8;
        }
        let held = self.held;
        self.sorted[..self.count].sort_unstable_by_key(|&i| (held[usize::from(i)].key, i));
    }

    fn press(&mut self, key: u8, velocity: u8, channel: u8) {
        let channel = channel & 15;
        if self.hold && self.down.iter().all(|&down| down == 0) {
            self.count = 0;
        }
        self.down[usize::from(channel)] |= 1u128 << (key & 0x7f);
        if self.count == 0 {
            self.step = 0;
        }
        let entry = Held {
            key,
            velocity,
            channel,
        };
        match self.held[..self.count]
            .iter()
            .position(|h| h.key == key && h.channel == channel)
        {
            Some(i) => self.held[i] = entry,
            None if self.count < MAX_HELD => {
                self.held[self.count] = entry;
                self.count += 1;
            }
            None => {}
        }
        self.resort();
    }

    fn release(&mut self, key: u8, channel: u8) {
        self.down[usize::from(channel & 15)] &= !(1u128 << (key & 0x7f));
        if !self.hold {
            self.drop_released();
        }
    }

    /// Removes the keys that are not down any more from the pattern.
    fn drop_released(&mut self) {
        let down = self.down;
        let mut kept = 0;
        for i in 0..self.count {
            let note = self.held[i];
            if down[usize::from(note.channel)] & (1u128 << (note.key & 0x7f)) != 0 {
                self.held[kept] = note;
                kept += 1;
            }
        }
        self.count = kept;
        self.resort();
    }

    fn clear_notes(&mut self) {
        self.count = 0;
        self.down.fill(0);
    }

    fn clear_channel(&mut self, channel: u8) {
        self.down[usize::from(channel)] = 0;
        let mut kept = 0;
        for index in 0..self.count {
            let note = self.held[index];
            if note.channel != channel {
                self.held[kept] = note;
                kept += 1;
            }
        }
        self.count = kept;
        self.resort();
        if self.sounding.is_some_and(|note| note.channel == channel) {
            self.sounding = None;
        }
    }

    fn random(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// The note of the current step, `None` when the pattern is empty or the note is out of range.
    fn pick(&mut self) -> Option<(u8, u8, u8)> {
        if self.count == 0 {
            return None;
        }
        let total = self.count as u32 * self.octaves;
        let e = match self.mode {
            Mode::Up | Mode::AsPlayed => self.step % total,
            Mode::Down => total - 1 - self.step % total,
            Mode::UpDown => {
                let cycle = (2 * total).saturating_sub(2).max(1);
                let i = self.step % cycle;
                if i < total { i } else { cycle - i }
            }
            Mode::Random => self.random() % total,
        };
        self.step = self.step.wrapping_add(1);
        let (index, octave) = ((e % self.count as u32) as usize, e / self.count as u32);
        let held = match self.mode {
            Mode::AsPlayed => self.held[index],
            _ => self.held[usize::from(self.sorted[index])],
        };
        let key = u32::from(held.key) + 12 * octave;
        (key <= 127).then_some((key as u8, held.velocity, held.channel))
    }
}

/// The offset of tick `t` in a call that starts at tick `pos` with `spt` samples per tick: the
/// sample the sequencer would use (round half up), relative to the call. Allow for rounding
/// of a hosted position and floating-point noise so both paths resolve half-sample ties upwards.
fn offset(t: f64, pos: f64, spt: f64) -> i64 {
    let tolerance = CLAP_POSITION_ROUNDING_TICKS * spt + 1e-6;
    ((t - pos) * spt + 0.5 + tolerance).floor() as i64
}

impl EventProcessor for Arp {
    fn set_param(&mut self, id: u32, value: f64) {
        match id {
            MODE => {
                self.mode = match value as usize {
                    1 => Mode::Down,
                    2 => Mode::UpDown,
                    3 => Mode::Random,
                    4 => Mode::AsPlayed,
                    _ => Mode::Up,
                };
            }
            RATE => {
                let rate = RATES[(value as usize).min(RATES.len() - 1)].1;
                if rate != self.step_ticks {
                    self.step_ticks = rate;
                    self.regrid = true;
                }
            }
            GATE => self.gate = value.clamp(0.05, 1.0),
            OCTAVES => self.octaves = (value as u32).clamp(1, 4),
            HOLD => {
                self.hold = value >= 0.5;
                if !self.hold {
                    self.drop_released();
                }
            }
            _ => {}
        }
    }

    fn process_events(&mut self, ctx: &ProcessContext, input: &[Event], out: &mut EventSink<'_>) {
        let frames = i64::from(ctx.frames);
        let t = &ctx.transport;
        let spt = f64::from(ctx.sample_rate) * 60.0 / (t.tempo_bpm * PPQ);
        let playing = t.playing;
        if !playing && (self.was_playing || !self.started) {
            self.free_pos = t.position_ticks;
        }
        let pos = if playing {
            t.position_ticks
        } else {
            self.free_pos
        };
        let span = frames as f64 / spt;
        let jumped =
            !self.started || playing != self.was_playing || (pos - self.expected).abs() > 1.0 / spt;
        if jumped && let Some(s) = self.sounding.take() {
            out.push(
                0,
                EventKind::NoteOff {
                    key: s.key,
                    channel: s.channel,
                    note_id: NOTE_ID_NONE,
                },
            );
        }
        if jumped || self.regrid {
            self.next_tick = self.grid_from(pos);
            self.regrid = false;
        }
        self.started = true;

        let mut i = 0;
        loop {
            // The next event of the pattern: a note-off before a step at the same tick.
            let (arp_t, is_off) = match self.sounding {
                Some(s) if s.off_at <= self.next_tick => (s.off_at, true),
                _ => (self.next_tick, false),
            };
            let arp_offset = offset(arp_t, pos, spt).max(0);
            let arp_due = arp_offset < frames;
            let input_offset = input.get(i).map(|e| i64::from(e.offset));
            let arp_first = arp_due
                && match input_offset {
                    None => true,
                    Some(o) => arp_offset < o || (arp_offset == o && is_off),
                };
            if arp_first {
                let at = arp_offset as u32;
                if is_off {
                    if let Some(s) = self.sounding.take() {
                        out.push(
                            at,
                            EventKind::NoteOff {
                                key: s.key,
                                channel: s.channel,
                                note_id: NOTE_ID_NONE,
                            },
                        );
                    }
                } else {
                    if let Some((key, velocity, channel)) = self.pick() {
                        out.push(
                            at,
                            EventKind::NoteOn {
                                key,
                                velocity,
                                channel,
                                note_id: NOTE_ID_NONE,
                            },
                        );
                        // The off follows the on by at least one sample.
                        let mut off_at = arp_t + self.gate * self.step_ticks;
                        if offset(off_at, pos, spt) <= arp_offset {
                            off_at = pos + (arp_offset + 1) as f64 / spt;
                        }
                        self.sounding = Some(Sounding {
                            key,
                            channel,
                            off_at,
                        });
                    }
                    self.next_tick += self.step_ticks;
                }
            } else if let Some(e) = input.get(i) {
                i += 1;
                match e.kind {
                    EventKind::NoteOn {
                        key,
                        velocity,
                        channel,
                        ..
                    } => self.press(key, velocity, channel),
                    EventKind::NoteOff { key, channel, .. } => self.release(key, channel),
                    EventKind::Midi(bytes @ [status, 120 | 123, _]) if status & 0xf0 == 0xb0 => {
                        self.clear_channel(status & 15);
                        out.push(e.offset, EventKind::Midi(bytes));
                    }
                    EventKind::AllNotesOff => {
                        self.clear_notes();
                        if let Some(s) = self.sounding.take() {
                            out.push(
                                e.offset,
                                EventKind::NoteOff {
                                    key: s.key,
                                    channel: s.channel,
                                    note_id: NOTE_ID_NONE,
                                },
                            );
                        }
                    }
                    other => {
                        out.push(e.offset, other);
                    }
                }
            } else {
                break;
            }
        }
        self.was_playing = playing;
        if !playing {
            self.free_pos += span;
        }
        self.expected = pos + span;
    }

    fn reset(&mut self) {
        self.clear_notes();
        self.step = 0;
        self.rng = RNG_SEED;
        self.sounding = None;
        self.started = false;
    }
}

#[cfg(test)]
mod tests;
