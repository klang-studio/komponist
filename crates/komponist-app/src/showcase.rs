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
//! | Vowel Pad | −10 dB | surf („Vowel Pad“): a voice over the Pad from bar 9, LFOs on both table positions, automation of Position A (through the whole table) and Position B |
//! | Hall | 0 dB | effect bus: Hall („Large Hall“), fed by the sends of Lead and Pad |
//! | Master | 0 dB | Limiter (ceiling −0.3 dB) |
//!
//! The routing is built with the model's edits (`add_bus`, `add_send`, `set_send`, `set_sidechain`),
//! so the demo holds exactly what the UI creates: a group, an effect bus with sends and a sidechain.
//! The Arp track's context menu can flatten its MIDI effects, instrument and delay together into
//! audio. Playback stops during rendering and the other tracks play their original material
//! when playback resumes.
//!
//! The packaged demo loads the devices' factory presets and snapshots their ordinary CLAP state.
//! Its sample files are synthesized at startup and linked as project assets.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use komponist_model::edits::{self, NewBus, SendProp};
use komponist_model::fixtures::demo_song;
use komponist_model::params::ParamId;
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{
    ClipContent, Device, DeviceId, MidiClip, Note, NoteId, Project, Sidechain, Take, Tap, Tick,
};
use komponist_plugins::bundled_initial_state::InitialDevice;

pub(crate) mod audio;
mod sampler;
#[doc(hidden)]
pub use sampler::{GeneralSamplerFixture, general_sampler_fixture};
pub mod surf;

/// Builds the demo from the authenticated installed plugins and generates its audio assets.
pub fn bundled_demo(
    host: &komponist_session::device_host::PluginHost,
    kit_dir: &Path,
) -> Result<Project, String> {
    std::fs::create_dir_all(kit_dir).map_err(|error| error.to_string())?;
    let mut project = build(host, kit_dir)?;
    let mut write_audio = || -> std::io::Result<()> {
        let drums = komponist_audiofile::fixtures::write_kit(kit_dir)?;
        let drum_track = project
            .tracks
            .iter_mut()
            .find(|track| track.name == "Drums")
            .expect("Drums");
        let drum_device = &mut Arc::make_mut(drum_track).channel.devices[0];
        Arc::make_mut(drum_device).asset_links = drums
            .iter()
            .enumerate()
            .map(|(slot, path)| (slot as u32, AssetLink::new(path, Some(kit_dir))))
            .collect();
        audio::write_files(&mut project, kit_dir)
    };
    write_audio().map_err(|error| error.to_string())?;
    Ok(project)
}

/// Writes the demo's 90 BPM source recording for audio-only tests.
#[doc(hidden)]
pub fn write_pulse_fixture(dir: &Path) -> std::io::Result<()> {
    audio::write_pulse_fixture(dir)
}

/// Volumes of the tracks in dB, in track order (Drums, Bass, Lead, Pad, Keys, Arp), set by
/// render analysis: the built-in instruments are louder than the old plugin chain's.
const VOLUMES_DB: [f32; 6] = [-16.0, -6.0, 0.0, -10.0, 0.0, 0.0];

/// „Ducking“, gentler: the kick moves the bass down a few dB and it comes back before the next beat.
fn bass_ducking() -> [(&'static str, f64); 3] {
    [
        ("ratio", log_position(3.0, 1.0, 20.0)),
        ("threshold", 40.0 / 60.0),
        ("release", log_position(120.0, 5.0, 2000.0)),
    ]
}

fn log_position(value: f64, min: f64, max: f64) -> f64 {
    (value / min).ln() / (max / min).ln()
}

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

fn prepare_device(
    host: &komponist_session::device_host::PluginHost,
    suffix: &str,
    preset: Option<&str>,
    edits: &[(&str, f64)],
    targets: &[&str],
) -> Result<InitialDevice, String> {
    let id = format!("org.klang-studio.komponist.{suffix}");
    host.manager()
        .prepare_bundled_device(&id, preset, edits, targets)
}

/// Allocates device IDs and retains the authored automation bindings used in the song.
struct Devices<'a> {
    host: &'a komponist_session::device_host::PluginHost,
    next: u64,
    targets: BTreeMap<(DeviceId, String), (ParamId, f32)>,
}

impl Devices<'_> {
    fn take(&mut self) -> DeviceId {
        let id = DeviceId(self.next);
        self.next += 1;
        id
    }

    fn add(
        &mut self,
        id: &str,
        preset: Option<&str>,
        edits: &[(&str, f64)],
        targets: &[&str],
    ) -> Result<Arc<Device>, String> {
        let prepared = prepare_device(self.host, id, preset, edits, targets)?;
        let id = self.take();
        self.targets.extend(
            prepared
                .targets
                .iter()
                .map(|(author, target)| ((id, author.clone()), target.clone())),
        );
        Ok(Arc::new(prepared.into_device(id)))
    }

    fn preset(&mut self, id: &str, name: &str) -> Result<Arc<Device>, String> {
        self.add(id, Some(name), &[], &[])
    }

    fn parameter(&self, device: DeviceId, author: &str) -> (ParamId, f32) {
        self.targets[&(device, author.into())].clone()
    }
}

fn build(
    host: &komponist_session::device_host::PluginHost,
    kit_dir: &Path,
) -> Result<Project, String> {
    let mut project = demo_song();
    let mut d = Devices {
        host,
        next: project.next_id,
        targets: BTreeMap::new(),
    };
    let chains = [
        vec![
            d.add("sampler", None, &[], &[])?,
            d.preset("presse", "Tight Drums")?,
        ],
        vec![
            d.preset("volt", "Fat Saw")?,
            d.preset("eq", "Warm Bass")?,
            d.add("presse", Some("Ducking"), &bass_ducking(), &[])?,
        ],
        vec![
            d.add("hexa", Some("Saw Lead"), &[], &["feedback"])?,
            d.preset("eq", "Low Cut 80 Hz")?,
            d.add("delay", Some("Dotted Eighths"), &[], &["mix"])?,
        ],
        vec![
            d.add("volt", Some("Wide Pad"), &[], &["cutoff"])?,
            d.preset("eq", "Low Cut 80 Hz")?,
        ],
        vec![
            d.preset("hexa", "E-Piano")?,
            d.preset("delay", "Soft Tape")?,
        ],
        vec![
            d.preset("scale", "A Minor Pentatonic")?,
            d.preset("arpeggiator", "Sixteenths Up/Down")?,
            d.preset("hexa", "Synth Pluck")?,
            d.preset("delay", "Eighth Ping-Pong")?,
        ],
    ];
    // The limiter adds 3 dB and stops at −0.3 dBFS.
    let master = vec![d.add(
        "limiter",
        None,
        &[("gain", 3.0 / 24.0), ("ceiling", 11.7 / 12.0)],
        &[],
    )?];
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
    add_routing(&mut project, &mut d)?;
    audio::add_tracks(&mut project);
    sampler::add_tracks(&mut project, host, kit_dir)?;
    surf::add_track(&mut project, host)?;
    add_automation(&mut project, &d);
    add_track_macros(&mut project, &d);
    Ok(project)
}

/// Group, effect bus, sends and sidechain, through the model's routing edits.
fn add_routing(project: &mut Project, d: &mut Devices<'_>) -> Result<(), String> {
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
    d.next = p.next_id;
    let reverb = d.preset("hall", "Large Hall")?;
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
    Ok(())
}

/// The Pad macro automates its Hall send. A clip envelope drives the Lead delay mix.
fn add_track_macros(project: &mut Project, d: &Devices<'_>) {
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};
    use komponist_model::edits::{automation, macros};
    use komponist_model::macros::Mapping;
    use komponist_model::params::db_to_p;

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
    let delay = &lead.channel.devices[2];
    let (mix, base) = d.parameter(delay.id, "mix");
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
fn add_automation(project: &mut Project, d: &Devices<'_>) {
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
    use komponist_model::edits::automation as edits;
    use komponist_model::params::db_to_p;

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
    let (cutoff, cutoff_value) = d.parameter(pad_device.id, "cutoff");
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
    let (feedback, base) = d.parameter(hexa.id, "feedback");
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

    add_master_fade(project);
}

fn add_master_fade(project: &mut Project) {
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};
    use komponist_model::edits::automation as edits;
    use komponist_model::params::db_to_p;

    let owner = AutomationOwner::Master;
    let base = 1.0 - db_to_p(project.master.volume_db);
    let (next, lane) = edits::add_lane(project, owner, AutomationTarget::Volume, "Volume".into())
        .expect("showcase master volume lane");
    *project = edits::set_lane_points(
        &next,
        owner,
        lane,
        Points::new(vec![
            Point::linear(Tick::from_steps(15 * 16), base),
            Point::linear(Tick::from_steps(16 * 16), 0.0),
        ]),
    )
    .expect("showcase master volume envelope");
}
