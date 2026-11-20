//! The app's demo project (`komponist --demo`): "Night Drive" with built-in devices only.
//!
//! The song is [`komponist_model::fixtures::demo_song`], with the tempo, tracks and clips of the
//! design fixture `demo_project()` and looping off. The showcase uses built-in devices and the
//! Arp track plays held chords for the Arpeggiator:
//!
//! | Track | Volume | Chain (preset) |
//! |---|---|---|
//! | Rhythm | 0 dB | group (folder) of Drums and Bass, outputs to the master |
//! | Drums | see `VOLUMES_DB` | Klopfer (test kit), presse („Tight Drums“) |
//! | Bass | see `VOLUMES_DB` | volt („Fat Saw“), EQ („Warm Bass“), presse („Ducking“, gentler: ratio 3, −20 dB, 120 ms; sidechain: Drums, pre-fader) |
//! | Shaker | −12 dB | mono audio in Rhythm; Re-Pitch 120 → 124 BPM, clip gain and fades |
//! | Pulse 90 | −24 dB | synthesized stereo 90 BPM loop; Warp follows the 124 BPM project, bars 5–7 |
//! | Sweep | −12 dB | stereo audio riser into bar 9, clip gain and fade-in |
//! | Lead | see `VOLUMES_DB` | hexa („Saw Lead“), EQ („Low Cut 80 Hz“), Delay („Dotted Eighths“); post-fader send to Reverb |
//! | Pad | see `VOLUMES_DB` | volt („Wide Pad“), EQ („Low Cut 80 Hz“); post-fader send to Reverb |
//! | Keys | −8 dB | hexa („E-Piano“), Delay („Soft Tape“) |
//! | Arp | −10 dB | Tonleiter („A Minor Pentatonic“), Arpeggiator („Sixteenths Up/Down“), hexa („Synth Pluck“), Delay („Eighth Ping-Pong“) |
//! | Glass Pluck | −24 dB | Sampler, tempo-synced melody with filter modulation |
//! | Glass Slices | −24 dB | Sampler, MIDI-triggered slices of the same recording |
//! | Hall | 0 dB | effect bus: Hall („Large Hall“), fed by the sends of Lead and Pad |
//! | Master | 0 dB | Limiter (ceiling −0.3 dB) |
//!
//! The routing is built with the model's edits (`add_bus`, `add_send`, `set_send`, `set_sidechain`),
//! so the demo holds exactly what the UI creates: a group, an effect bus with sends and a sidechain.
//! The Arp track's context menu can flatten its MIDI effects, instrument and delay together into
//! audio. Playback stops during rendering and the other tracks play their original material
//! when playback resumes.
//!
//! Device states are written like the presets of the UI do: `{"params": …, "preset": "<name>"}`
//! ([`Internal::preset_state`]); hexa's state is the factory patch.
//! The audio links are metadata until `samples::demo_with_kit` synthesizes their WAVs at load.

use std::sync::Arc;

use komponist_builtin::Internal;
use komponist_hexa::{encode_state, preset_patch, presets as hexa_presets};
use komponist_model::edits::{self, NewBus, SendProp};
use komponist_model::fixtures::demo_song;
use komponist_model::{
    ClipContent, Device, DeviceId, DeviceRole, MidiClip, Note, NoteId, Project, Sidechain, Take,
    Tap, Tick,
};

pub(crate) mod audio;
mod sampler;

/// Volumes of the tracks in dB, in track order (Drums, Bass, Lead, Pad, Keys, Arp), set by
/// render analysis: the built-in instruments are louder than the old plugin chain's.
const VOLUMES_DB: [f32; 6] = [-16.0, -6.0, 0.0, -10.0, 0.0, 0.0];

/// „Ducking“, gentler: the kick moves the bass down a few dB and it comes back before the next beat.
const BASS_DUCKING: &[(&str, f64)] = &[("ratio", 3.0), ("threshold", -20.0), ("release", 120.0)];

/// Name of the group of Drums and Bass.
pub const GROUP_NAME: &str = "Rhythm";
/// Name of the effect bus that carries the reverb.
pub const REVERB_BUS_NAME: &str = "Reverb";
/// Levels in dB of the post-fader sends of Lead and Pad to the Hall bus.
const HALL_SENDS_DB: [f32; 2] = [-12.0, -8.0];

/// The chords of the Arp clip, one per bar: C, C, Am, F, C, C, Am, G (MIDI pitches).
const CHORDS: [[u8; 3]; 8] = [
    [60, 64, 67],
    [60, 64, 67],
    [57, 60, 64],
    [57, 60, 65],
    [60, 64, 67],
    [60, 64, 67],
    [57, 60, 64],
    [55, 59, 62],
];

/// Velocity of the Arp chords.
const CHORD_VELOCITY: u8 = 84;

/// Allocates ids and builds the devices.
struct Devices {
    next: u64,
}

impl Devices {
    fn take(&mut self) -> DeviceId {
        let id = DeviceId(self.next);
        self.next += 1;
        id
    }

    /// An internal device with factory preset `preset`.
    fn internal(&mut self, kind: Internal, preset: &str) -> Arc<Device> {
        let index = kind
            .presets()
            .iter()
            .position(|p| p.name == preset)
            .unwrap_or_else(|| panic!("{} has no preset {preset}", kind.name()));
        let state = kind.preset_state(None, index).expect("the preset exists");
        self.with_state(kind, state)
    }

    /// An internal device with factory preset `preset`, then `changes` on top (the device shows
    /// the preset name with the „ *“ mark).
    fn tuned(&mut self, kind: Internal, preset: &str, changes: &[(&str, f64)]) -> Arc<Device> {
        let device = self.internal(kind, preset);
        let mut state = device
            .state
            .as_deref()
            .expect("a preset has state")
            .to_vec();
        for (key, value) in changes {
            let wire = kind
                .wire(&komponist_model::params::ParamId::new(*key))
                .unwrap_or_else(|| panic!("{} has no parameter {key}", kind.name()));
            state = kind
                .with_value(Some(&state), wire, *value)
                .expect("a known wire id");
        }
        Arc::new(Device {
            state: Some(state.into()),
            ..Device::clone(&device)
        })
    }

    /// An internal device with its default parameters (no preset name).
    fn plain(&mut self, kind: Internal, changes: &[(&str, f64)]) -> Arc<Device> {
        let mut state = kind.encode_state(&kind.defaults());
        for (key, value) in changes {
            let wire = kind
                .wire(&komponist_model::params::ParamId::new(*key))
                .unwrap_or_else(|| panic!("{} has no parameter {key}", kind.name()));
            state = kind
                .with_value(Some(&state), wire, *value)
                .expect("a known wire id");
        }
        self.with_state(kind, state)
    }

    fn with_state(&mut self, kind: Internal, state: Vec<u8>) -> Arc<Device> {
        Arc::new(Device {
            id: self.take(),
            role: kind.role(),
            plugin: kind.plugin_ref(),
            bypass: false,
            state: Some(state.into()),
            sidechain: None,
        })
    }

    /// The sampler without state: `samples::demo_with_kit` puts the test kit on its pads.
    fn sampler(&mut self) -> Arc<Device> {
        Arc::new(Device {
            id: self.take(),
            role: DeviceRole::Instrument,
            plugin: Internal::Sampler.plugin_ref(),
            bypass: false,
            state: None,
            sidechain: None,
        })
    }

    /// hexa with factory preset `preset`.
    fn hexa(&mut self, preset: &str) -> Arc<Device> {
        let info = hexa_presets()
            .find(|p| p.name == preset)
            .unwrap_or_else(|| panic!("hexa has no preset {preset}"));
        let patch = preset_patch(info.index).expect("a listed preset exists");
        Arc::new(Device {
            id: self.take(),
            role: DeviceRole::Instrument,
            plugin: crate::app::hexa_ref(),
            bypass: false,
            state: Some(encode_state(&patch).into()),
            sidechain: None,
        })
    }
}

/// The demo project: see the module docs.
pub fn showcase_project() -> Project {
    let mut project = demo_song();
    let mut d = Devices {
        next: project.next_id,
    };
    let chains = [
        vec![d.sampler(), d.internal(Internal::Presse, "Tight Drums")],
        vec![
            d.internal(Internal::Volt, "Fat Saw"),
            d.internal(Internal::Eq, "Warm Bass"),
            d.tuned(Internal::Presse, "Ducking", BASS_DUCKING),
        ],
        vec![
            d.hexa("Saw Lead"),
            d.internal(Internal::Eq, "Low Cut 80 Hz"),
            d.internal(Internal::Delay, "Dotted Eighths"),
        ],
        vec![
            d.internal(Internal::Volt, "Wide Pad"),
            d.internal(Internal::Eq, "Low Cut 80 Hz"),
        ],
        vec![d.hexa("E-Piano"), d.internal(Internal::Delay, "Soft Tape")],
        vec![
            d.internal(Internal::Tonleiter, "A Minor Pentatonic"),
            d.internal(Internal::Arpeggiator, "Sixteenths Up/Down"),
            d.hexa("Synth Pluck"),
            d.internal(Internal::Delay, "Eighth Ping-Pong"),
        ],
    ];
    let master = vec![d.plain(Internal::Limiter, &[("gain", 3.0), ("ceiling", -0.3)])];
    for ((track, chain), volume) in project.tracks.iter_mut().zip(chains).zip(VOLUMES_DB) {
        let track = Arc::make_mut(track);
        track.channel.devices = chain;
        track.channel.volume_db = volume;
    }
    Arc::make_mut(&mut project.master).devices = master;

    // The Arp track plays held chords: the Arpeggiator turns them into the pattern.
    let mut notes = Vec::new();
    for (bar, chord) in CHORDS.iter().enumerate() {
        for &pitch in chord {
            notes.push(Note {
                id: NoteId(d.take().0),
                start: Tick::from_steps(bar as i64 * 16),
                length: Tick::from_steps(16),
                pitch,
                velocity: CHORD_VELOCITY,
            });
        }
    }
    let arp = Arc::make_mut(&mut project.tracks[5]);
    let clip = Arc::make_mut(&mut arp.clips[0]);
    clip.content = ClipContent::Midi(MidiClip {
        takes: vec![Take {
            name: String::new(),
            notes: Arc::new(notes),
        }],
        active_take: 0,
    });
    project.next_id = d.next;
    add_routing(&mut project);
    audio::add_tracks(&mut project);
    sampler::add_tracks(&mut project);
    add_automation(&mut project);
    add_track_macros(&mut project);
    project
}

/// Group, effect bus, sends and sidechain, through the model's routing edits.
fn add_routing(project: &mut Project) {
    let id_of = |p: &Project, name: &str| {
        p.tracks
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("no track {name}"))
            .id
    };
    let (drums, bass) = (id_of(project, "Drums"), id_of(project, "Bass"));
    let (lead, pad) = (id_of(project, "Lead"), id_of(project, "Pad"));

    // Drums and Bass output to the group „Rhythm“, which takes the place of Drums.
    let mut spec = NewBus::group(vec![drums, bass]);
    spec.name = Some(GROUP_NAME.to_owned());
    let (p, _) = edits::add_bus(project, spec).expect("the demo routing is valid: group");
    *project = p;

    // The effect bus „Reverb“ with the reverb; Lead and Pad send to it, post-fader.
    let mut spec = NewBus::effect(vec![lead, pad]);
    spec.name = Some(REVERB_BUS_NAME.to_owned());
    let (mut p, hall) = edits::add_bus(project, spec).expect("the demo routing is valid: bus");
    for (track, db) in [lead, pad].into_iter().zip(HALL_SENDS_DB) {
        let send = p
            .track(track)
            .and_then(|t| t.send_to(hall))
            .expect("the send exists")
            .id;
        p = edits::set_send(&p, send, SendProp::LevelDb(db))
            .expect("the demo routing is valid: level");
        p = edits::set_send(&p, send, SendProp::Tap(Tap::Post))
            .expect("the demo routing is valid: tap");
    }
    let mut d = Devices { next: p.next_id };
    let reverb = d.internal(Internal::Hall, "Large Hall");
    p.next_id = d.next;
    let bus = p
        .tracks
        .iter()
        .position(|t| t.id == hall)
        .expect("the bus exists");
    Arc::make_mut(&mut p.tracks[bus]).channel.devices = vec![reverb];

    // The kick ducks the bass: presse on the Bass, keyed by Drums before its fader.
    let presse = p
        .track(bass)
        .and_then(|t| t.channel.devices.last())
        .expect("the Bass chain ends with presse")
        .id;
    let sidechain = Sidechain {
        source: drums,
        tap: Tap::Pre,
    };
    *project = edits::set_sidechain(&p, presse, Some(sidechain))
        .expect("the demo routing is valid: sidechain");
}

/// The Pad macro automates its Hall send. A clip envelope drives the Lead delay mix.
fn add_track_macros(project: &mut Project) {
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};
    use komponist_model::edits::{automation, macros};
    use komponist_model::macros::Mapping;
    use komponist_model::params::{ParamId, db_to_p};

    let pad = project
        .tracks
        .iter()
        .find(|t| t.name == "Pad")
        .expect("Pad");
    let hall = project
        .tracks
        .iter()
        .find(|t| t.name == REVERB_BUS_NAME)
        .expect("Hall");
    let send = pad.send_to(hall.id).expect("Pad Hall send");
    let send_target = AutomationTarget::Send { send: send.id };
    let send_base = 1.0 - db_to_p(send.level_db);
    let mapping = Mapping {
        target: send_target.clone(),
        label: "Send · Hall".into(),
        min: 1.0 - db_to_p(-18.0),
        max: send_base,
        invert: false,
        bend: 0.0,
    };
    let pad = pad.id;
    let named = macros::rename_macro(project, pad, 0, "Open".into()).expect("Pad macro name");
    let open = macros::set_macro_value(&named, pad, 0, 1.0).expect("Pad macro value");
    let mapped = macros::add_mapping(&open, pad, 0, mapping).expect("Pad Hall mapping");
    let target = AutomationTarget::Macro { index: 0 };
    let owner = AutomationOwner::Track(pad);
    // Keep a direct envelope on the send so the demo shows how it resumes after unmapping.
    let (with_send, send_lane) =
        automation::add_lane(&mapped, owner, send_target, "Send · Hall".into())
            .expect("Pad Hall lane");
    let with_send = automation::set_lane_points(
        &with_send,
        owner,
        send_lane,
        Points::new(vec![
            Point::linear(Tick::ZERO, send_base),
            Point::linear(Tick::from_steps(8 * 16), 1.0 - db_to_p(-18.0)),
        ]),
    )
    .expect("Pad Hall curve");
    let label = "Track macro 1 · Open";
    let (with_lane, lane) = automation::add_lane(&with_send, owner, target.clone(), label.into())
        .expect("Pad macro lane");
    *project = automation::set_lane_points(
        &with_lane,
        owner,
        lane,
        Points::new(vec![
            Point::linear(Tick::ZERO, 1.0),
            Point::linear(Tick::from_steps(4 * 16), 0.35),
            Point::linear(Tick::from_steps(8 * 16), 1.0),
        ]),
    )
    .expect("Pad macro curve");
    let lead = project
        .tracks
        .iter()
        .find(|t| t.name == "Lead")
        .expect("Lead");
    let clip = lead
        .clips
        .iter()
        .find(|clip| clip.name == "Lead B")
        .expect("Lead B")
        .id;
    let delay = lead
        .channel
        .devices
        .iter()
        .find(|device| Internal::from_plugin(&device.plugin) == Some(Internal::Delay))
        .expect("Lead delay");
    let mix = ParamId::new("mix");
    let wire = Internal::Delay.wire(&mix).expect("Delay mix") as usize;
    let base = Internal::Delay
        .decode_state(delay.state.as_deref())
        .expect("Delay state")[wire];
    let base = Internal::Delay.params()[wire].to_normalized(base) as f32;
    let mapping = Mapping {
        target: AutomationTarget::Param {
            device: delay.id,
            param: mix,
        },
        label: "Delay · Mix".into(),
        min: base * 0.6,
        max: base,
        invert: false,
        bend: 0.0,
    };
    let lead = lead.id;
    let named = macros::rename_macro(project, lead, 0, "Echo".into()).expect("Lead macro name");
    let base = macros::set_macro_value(&named, lead, 0, 1.0).expect("Lead macro value");
    let mapped = macros::add_mapping(&base, lead, 0, mapping).expect("Lead delay mapping");
    let with_clip = automation::add_clip_automation(
        &mapped,
        clip,
        target.clone(),
        "Track macro 1 · Echo".into(),
    )
    .expect("Lead clip macro envelope");
    *project = automation::set_clip_points(
        &with_clip,
        clip,
        &target,
        Points::new(vec![
            Point::linear(Tick::ZERO, 0.25),
            Point::linear(Tick::from_steps(16), 1.0),
        ]),
    )
    .expect("Lead clip macro points");
}

/// The song demonstrates track, clip and master curves.
fn add_automation(project: &mut Project) {
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
    use komponist_model::edits::automation as edits;
    use komponist_model::params::{ParamId, db_to_p};

    let track = |project: &Project, name: &str| {
        project
            .tracks
            .iter()
            .find(|track| track.name == name)
            .expect("showcase track")
            .id
    };
    let bars = |bar: i64| Tick::from_steps(bar * 16);
    let lane = |project: &mut Project, owner, target, label: &str, points: Vec<Point>| {
        let (next, id) = edits::add_lane(project, owner, target, label.to_owned())
            .expect("showcase automation target");
        *project = edits::set_lane_points(&next, owner, id, Points::new(points))
            .expect("showcase lane points");
    };

    let pad = track(project, "Pad");
    let pad_device = &project.track(pad).expect("Pad").channel.devices[0];
    let cutoff = ParamId::new("cutoff");
    let wire = Internal::Volt.wire(&cutoff).expect("Volt cutoff") as usize;
    let base_cutoff = Internal::Volt
        .decode_state(pad_device.state.as_deref())
        .expect("Volt state")[wire];
    let cutoff_value = Internal::Volt.params()[wire].to_normalized(base_cutoff) as f32;
    let cutoff_target = AutomationTarget::Param {
        device: pad_device.id,
        param: cutoff,
    };
    lane(
        project,
        AutomationOwner::Track(pad),
        cutoff_target,
        "Volt · Cutoff",
        vec![
            Point {
                tick: bars(8),
                value: cutoff_value * 0.66,
                shape: Shape::Curve { tension: 0.35 },
            },
            Point::linear(bars(12), cutoff_value),
        ],
    );

    let keys = track(project, "Keys");
    let keys_level = project.track(keys).expect("Keys").channel.volume_db;
    lane(
        project,
        AutomationOwner::Track(keys),
        AutomationTarget::Volume,
        "Volume",
        vec![
            Point::linear(bars(4), 1.0 - db_to_p(-36.0)),
            Point::linear(bars(5), 1.0 - db_to_p(keys_level)),
        ],
    );

    let lead = track(project, "Lead");
    let lead_track = project.track(lead).expect("Lead");
    let hexa = &lead_track.channel.devices[0];
    let feedback = ParamId::new("feedback");
    let param = komponist_hexa::device::param(&feedback).expect("Hexa feedback");
    let patch = komponist_hexa::decode_state(hexa.state.as_deref()).expect("Hexa state");
    let base = param.info.to_normalized(param.field.get(&patch)) as f32;
    let target = AutomationTarget::Param {
        device: hexa.id,
        param: feedback,
    };
    let (clip_index, clip) = lead_track
        .clips
        .iter()
        .enumerate()
        .find(|(_, clip)| clip.name == "Lead B")
        .expect("Lead B");
    let clip_id = clip.id;
    let track_index = project
        .tracks
        .iter()
        .position(|track| track.id == lead)
        .expect("Lead index");
    let clip =
        Arc::make_mut(&mut Arc::make_mut(&mut project.tracks[track_index]).clips[clip_index]);
    clip.loop_length = Some(bars(2));
    *project =
        edits::add_clip_automation(project, clip_id, target.clone(), "Hexa · Feedback".into())
            .expect("showcase clip envelope");
    *project = edits::set_clip_points(
        project,
        clip_id,
        &target,
        Points::new(vec![
            Point::linear(Tick::ZERO, base),
            Point::linear(bars(1), (base + 0.16).min(1.0)),
            Point::linear(bars(2), base),
        ]),
    )
    .expect("showcase envelope points");
    *project =
        edits::set_shown(project, AutomationOwner::Track(lead), true).expect("showcase Lead lanes");

    let master_level = project.master.volume_db;
    lane(
        project,
        AutomationOwner::Master,
        AutomationTarget::Volume,
        "Volume",
        vec![
            Point::linear(bars(15), 1.0 - db_to_p(master_level)),
            Point::linear(bars(16), 0.0),
        ],
    );
}
