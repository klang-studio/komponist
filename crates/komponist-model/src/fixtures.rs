//! Ready-made projects for the app, tests and the demo player.

use std::sync::Arc;

use crate::ids::{ClipId, DeviceId, NoteId, TrackId};
use crate::project::{
    Channel, Clip, ClipContent, DRUM_SAMPLER_CLAP_ID, DRUM_SAMPLER_NAME, Device, DeviceRole, Hue,
    MAX_STEPS, MidiChannelSel, MidiClip, MidiInputSel, Note, PluginFormat, PluginRef, Project,
    Step, StepClip, StepRow, TRACK_HEIGHT_DEFAULT, Take, Track, TrackKind, TrackOutput,
};
use crate::time::{StepResolution, Tempo, Tick, TickRange};

/// Name of the demo project.
pub const DEMO_PROJECT_NAME: &str = "Night Drive";

/// The demo project "Night Drive", ported 1:1 from `demoProject()` in `ks-engine.js`.
///
/// 124 bpm, 4/4, loop on over bars 9–13 (prototype `loopS = 128`, `loopE = 192` steps → ticks
/// 30720 .. 46080), metronome off, count-in 1 bar. Six tracks in this order, with the prototype's
/// hues, volumes, pans, clips (bar positions → ticks, note positions in 1/16 steps → ticks via
/// [`crate::time::Tick::from_steps`]) and device chains:
///
/// | Track | Hue | dB | Pan | Instrument | Effects |
/// |---|---|---|---|---|---|
/// | Drums | 45 | −6 | 0 | Klopfer (builtin) | LSP Compressor |
/// | Bass | 250 | −4 | 0 | hexa (builtin) | LSP Para EQ x8 |
/// | Lead | 150 | −3 | 0 | hexa (builtin) | LSP Para EQ x8, TAL-Chorus-LX (bypassed), Dragonfly Hall Reverb |
/// | Pad | 300 | −9 | −0.1 | Surge XT (CLAP) | LSP Para EQ x8, Dragonfly Hall Reverb |
/// | Keys | 95 | −8 | 0.2 | Dexed (VST3) | – |
/// | Arp | 200 | −10 | 0 | Vital (VST3), track muted | – |
///
/// Drum clips are step clips ("Beat A" / "Beat B" patterns from `beat()`, 16 steps at 1/16,
/// velocities from the prototype's step values). Their rows carry the prototype names with the sampler's
/// pad notes 36–43 (Kick 36 … Perc 43; decided at G1, ARCHITECTURE §13.3), not the GM notes of
/// [`crate::project::DEFAULT_STEP_ROWS`]. Third-party plugins are referenced by their real CLAP ids
/// / VST3 class ids, so they show as missing on machines without them. Ids are allocated in document
/// order starting at 1: per track the track, its devices, then each clip followed by its notes.
pub fn demo_project() -> Project {
    let mut project = song(
        [
            &[SAMPLER, LSP_COMPRESSOR],
            &[HEXA, LSP_PARA_EQ],
            &[HEXA, LSP_PARA_EQ, TAL_CHORUS, DRAGONFLY_HALL],
            &[SURGE_XT, LSP_PARA_EQ, DRAGONFLY_HALL],
            &[DEXED],
            &[VITAL],
        ],
        Ids(1),
    );
    project.loop_range = TickRange::new(Tick::from_steps(128), Tick::from_steps(192));
    project.loop_on = true;
    // Prototype: `t[5].M = true`.
    Arc::make_mut(&mut project.tracks[5]).channel.mute = true;
    project
}

/// The song "Night Drive" of [`demo_project`] with the default transport settings, no devices
/// and the Arp track unmuted: the same tempo, tracks and clips with their notes.
/// The app's showcase demo adds its own device chains. Ids continue after the song's (`next_id`).
pub fn demo_song() -> Project {
    song([&[]; 6], Ids(1))
}

/// The song with `chains` (first = instrument) for Drums, Bass, Lead, Pad, Keys and Arp, and
/// `ids` as the first id.
fn song(chains: [&[DemoPlugin]; 6], mut ids: Ids) -> Project {
    let ids = &mut ids;
    let [drums, bass, lead, pad, keys, arp_chain] = chains;
    let tracks = vec![
        demo_track(ids, ("Drums", Hue::VERMILION, -6.0, 0.0), drums, |ids| {
            vec![
                step_clip(ids, 0, 4, "Beat A", &beat(true)),
                step_clip(ids, 4, 4, "Beat A", &beat(true)),
                step_clip(ids, 8, 4, "Beat B", &beat(false)),
                step_clip(ids, 12, 4, "Beat B", &beat(false)),
            ]
        }),
        demo_track(ids, ("Bass", Hue::BLUE, -4.0, 0.0), bass, |ids| {
            vec![
                midi_clip(ids, 0, 8, "Bass", &repeat(&BASS, 32, 4)),
                midi_clip(ids, 8, 8, "Bass 2", &repeat(&BASS, 32, 4)),
            ]
        }),
        demo_track(ids, ("Lead", Hue::GREEN, -3.0, 0.0), lead, |ids| {
            vec![
                midi_clip(ids, 8, 2, "Lead A", &LEAD),
                midi_clip(ids, 10, 2, "Lead A", &LEAD),
                midi_clip(ids, 12, 4, "Lead B", &repeat(&LEAD, 32, 2)),
            ]
        }),
        demo_track(ids, ("Pad", Hue::VIOLET, -9.0, -0.1), pad, |ids| {
            vec![midi_clip(ids, 0, 16, "Pad", &repeat(&PAD, 64, 4))]
        }),
        demo_track(ids, ("Keys", Hue::YELLOW, -8.0, 0.2), keys, |ids| {
            vec![
                midi_clip(ids, 4, 4, "Keys", &repeat(&KEYS, 32, 2)),
                midi_clip(ids, 12, 4, "Keys", &repeat(&KEYS, 32, 2)),
            ]
        }),
        demo_track(ids, ("Arp", Hue::TEAL, -10.0, 0.0), arp_chain, |ids| {
            vec![midi_clip(ids, 8, 8, "Arp", &arp())]
        }),
    ];

    let mut project = Project::new(DEMO_PROJECT_NAME);
    project.tempo = Tempo::new(124.0);
    project.tracks = tracks.into_iter().map(Arc::new).collect();
    project.next_id = ids.0;
    project
}

/// An empty project as created by Ctrl N (S10): [`Project::new`] with
/// [`crate::project::DEFAULT_PROJECT_NAME`].
pub fn empty_project() -> Project {
    Project::new(crate::project::DEFAULT_PROJECT_NAME)
}

// ------------------------------------------------------------------------------------------------
// Demo data from `ks-engine.js`
// ------------------------------------------------------------------------------------------------

/// Notes as `[start, length, pitch, velocity]` in 1/16 steps (prototype `N`, velocity default 90).
type ProtoNote = [i64; 4];

/// Prototype `LEAD`.
const LEAD: [ProtoNote; 17] = [
    [0, 8, 60, 70],
    [0, 8, 64, 62],
    [0, 2, 67, 100],
    [2, 2, 69, 80],
    [4, 4, 71, 112],
    [8, 8, 62, 58],
    [8, 2, 69, 72],
    [10, 2, 67, 60],
    [12, 4, 64, 96],
    [16, 8, 57, 82],
    [16, 8, 60, 74],
    [16, 6, 72, 124],
    [22, 2, 71, 76],
    [24, 8, 55, 64],
    [24, 8, 59, 58],
    [24, 4, 67, 90],
    [28, 4, 69, 48],
];

/// Prototype `BASS` (velocity 90).
const BASS: [ProtoNote; 10] = [
    [0, 3, 40, 90],
    [4, 2, 40, 90],
    [6, 2, 43, 90],
    [8, 3, 45, 90],
    [12, 4, 43, 90],
    [16, 3, 38, 90],
    [20, 2, 38, 90],
    [22, 2, 41, 90],
    [24, 4, 43, 90],
    [28, 4, 45, 90],
];

/// Prototype `PADN`.
const PAD: [ProtoNote; 6] = [
    [0, 32, 60, 70],
    [0, 32, 64, 64],
    [0, 32, 67, 60],
    [32, 32, 57, 70],
    [32, 32, 60, 64],
    [32, 32, 64, 60],
];

/// Prototype `KEYS` (velocity 90).
const KEYS: [ProtoNote; 10] = [
    [0, 4, 64, 90],
    [0, 4, 67, 90],
    [6, 2, 64, 90],
    [8, 4, 62, 90],
    [8, 4, 65, 90],
    [14, 2, 62, 90],
    [16, 4, 60, 90],
    [16, 4, 64, 90],
    [24, 4, 59, 90],
    [24, 4, 62, 90],
];

/// Prototype `rep(arr, len, times)`: `times` copies, each `len` steps later.
fn repeat(notes: &[ProtoNote], len: i64, times: i64) -> Vec<ProtoNote> {
    (0..times)
        .flat_map(|i| {
            notes
                .iter()
                .map(move |&[s, l, p, v]| [s + i * len, l, p, v])
        })
        .collect()
}

/// The Arp clip: 128 sixteenths cycling through 60, 64, 67, 72, 67, 64 at velocity 70.
fn arp() -> Vec<ProtoNote> {
    const CYCLE: [i64; 6] = [60, 64, 67, 72, 67, 64];
    (0..128)
        .map(|i| [i, 1, CYCLE[i as usize % 6], 70])
        .collect()
}

/// Step rows of the demo's drum clips: prototype names on the sampler's pads 1–8 (notes 36–43).
const DRUM_ROWS: [&str; 8] = [
    "Kick",
    "Snare",
    "Clap",
    "Closed Hat",
    "Open Hat",
    "Tom",
    "Rim",
    "Perc",
];

/// Note of the sampler's first pad.
const SAMPLER_FIRST_NOTE: u8 = 36;

/// Prototype `beat(a)`: velocities per row and step (0 = off) of "Beat A" (`a`) or "Beat B".
fn beat(a: bool) -> [[u8; 16]; 8] {
    let mut steps = [[0u8; 16]; 8];
    let mut set = |row: usize, indices: &[usize], velocities: &[u8]| {
        for (&i, &v) in indices.iter().zip(velocities) {
            steps[row][i] = v;
        }
    };
    let kick: &[usize] = if a {
        &[0, 4, 8, 12]
    } else {
        &[0, 4, 8, 12, 14]
    };
    set(0, kick, &[120, 100, 118, 104, 70]);
    set(1, &[4, 12], &[110, 116]);
    set(
        3,
        &[0, 2, 4, 6, 8, 10, 12, 14],
        &[90, 50, 86, 48, 92, 52, 88, 46],
    );
    if !a {
        set(2, &[4, 12], &[80, 84]);
        set(4, &[3, 11], &[70, 74]);
        set(5, &[13, 15], &[90, 96]);
    }
    steps
}

/// The next ids in document order.
struct Ids(u64);

impl Ids {
    fn next(&mut self) -> u64 {
        let id = self.0;
        self.0 += 1;
        id
    }
}

/// A demo track: `(name, hue, volume dB, pan)`, the device chain (first = instrument) and the
/// clips, with ids in document order.
fn demo_track(
    ids: &mut Ids,
    (name, hue, volume_db, pan): (&str, Hue, f32, f32),
    chain: &[DemoPlugin],
    clips: impl FnOnce(&mut Ids) -> Vec<Clip>,
) -> Track {
    let id = TrackId(ids.next());
    let devices = chain
        .iter()
        .enumerate()
        .map(|(index, plugin)| {
            Arc::new(Device {
                id: DeviceId(ids.next()),
                role: if index == 0 {
                    DeviceRole::Instrument
                } else {
                    DeviceRole::Effect
                },
                plugin: plugin.to_ref(),
                // Prototype: `mkDev('chorus', {bypass: true})`.
                bypass: plugin.id == TAL_CHORUS.id,
                parameter_bases: Default::default(),
                asset_links: Default::default(),
                state: None,
                sidechain: None,
                preset: None,
            })
        })
        .collect();
    Track {
        id,
        name: name.to_owned(),
        hue,
        kind: TrackKind::Midi,
        channel: Channel {
            volume_db,
            pan,
            mute: false,
            solo: false,
            devices,
        },
        arm: false,
        monitor: false,
        input: MidiInputSel::All,
        midi_channel: MidiChannelSel::Omni,
        audio: crate::AudioIo::default(),
        height: TRACK_HEIGHT_DEFAULT,
        clips: clips(ids).into_iter().map(Arc::new).collect(),
        output: TrackOutput::Master,
        sends: Vec::new(),
        folder: false,
        folded: false,
        automation: Default::default(),
        macros: Default::default(),
    }
}

/// A clip at bar `bar` (0-based, prototype `x`) of `bars` bars (prototype `w`).
fn clip(id: ClipId, bar: i64, bars: i64, name: &str, content: ClipContent) -> Clip {
    Clip {
        id,
        name: name.to_owned(),
        start: Tick::from_steps(bar * 16),
        length: Tick::from_steps(bars * 16),
        loop_length: None,
        mute: false,
        transpose: 0,
        velocity_pct: 0,
        automation: Vec::new(),
        content,
    }
}

/// Prototype `mClip`: a MIDI clip with one unnamed take.
fn midi_clip(ids: &mut Ids, bar: i64, bars: i64, name: &str, notes: &[ProtoNote]) -> Clip {
    let id = ClipId(ids.next());
    let mut notes = notes.to_vec();
    notes.sort_by_key(|&[s, _, p, _]| (s, p));
    let notes = notes
        .into_iter()
        .map(|[s, l, p, v]| Note {
            id: NoteId(ids.next()),
            start: Tick::from_steps(s),
            length: Tick::from_steps(l),
            pitch: p as u8,
            velocity: v as u8,
        })
        .collect();
    let content = ClipContent::Midi(MidiClip {
        takes: vec![Take {
            name: String::new(),
            notes: Arc::new(notes),
        }],
        active_take: 0,
    });
    clip(id, bar, bars, name, content)
}

/// Prototype `sClip`: a 16-step pattern at 1/16 on the demo drum rows.
fn step_clip(ids: &mut Ids, bar: i64, bars: i64, name: &str, velocities: &[[u8; 16]; 8]) -> Clip {
    let rows = DRUM_ROWS
        .iter()
        .zip(velocities)
        .zip(SAMPLER_FIRST_NOTE..)
        .map(|((&row_name, row), note)| {
            let mut steps = [Step::default(); MAX_STEPS];
            for (step, &velocity) in steps.iter_mut().zip(row) {
                if velocity > 0 {
                    *step = Step {
                        on: true,
                        velocity,
                        ..Step::default()
                    };
                }
            }
            StepRow {
                name: row_name.to_owned(),
                note,
                mute: false,
                solo: false,
                steps,
            }
        })
        .collect();
    let content = ClipContent::Step(StepClip {
        rows,
        length_steps: 16,
        resolution: StepResolution::Sixteenth,
        swing: 0.0,
    });
    clip(ClipId(ids.next()), bar, bars, name, content)
}

/// A catalog entry of the prototype (`CAT` in `ks-engine.js`) with its real plugin identity.
struct DemoPlugin {
    format: PluginFormat,
    id: &'static str,
    name: &'static str,
    vendor: &'static str,
    version: &'static str,
}

impl DemoPlugin {
    fn to_ref(&self) -> PluginRef {
        PluginRef {
            format: self.format,
            id: self.id.to_owned(),
            name: self.name.to_owned(),
            vendor: self.vendor.to_owned(),
            version: self.version.to_owned(),
            // The demo plugins stand in for "<name>.<format>" bundles.
            module: match self.format {
                PluginFormat::Clap if self.id == HEXA.id => {
                    "bundled-clap/komponist_hexa_clap.clap".into()
                }
                PluginFormat::Clap if self.id == SAMPLER.id => {
                    "bundled-clap/komponist_sampler_clap.clap".into()
                }
                PluginFormat::Clap => format!("{}.clap", self.name).into(),
                PluginFormat::Vst3 => format!("{}.vst3", self.name).into(),
            },
        }
    }
}

const HEXA: DemoPlugin = DemoPlugin {
    format: PluginFormat::Clap,
    id: "org.klang-studio.komponist.hexa",
    name: "Hexa",
    vendor: "klang-studio",
    version: "0.1.0",
};

/// The shipped drum sampler.
const SAMPLER: DemoPlugin = DemoPlugin {
    format: PluginFormat::Clap,
    id: DRUM_SAMPLER_CLAP_ID,
    name: DRUM_SAMPLER_NAME,
    vendor: "komponist",
    version: "0.1.0",
};

const SURGE_XT: DemoPlugin = DemoPlugin {
    format: PluginFormat::Clap,
    id: "org.surge-synth-team.surge-xt",
    name: "Surge XT",
    vendor: "Surge Synth Team",
    version: "1.3.4",
};

/// Class id of the audio module class in Dexed's `moduleinfo.json`.
const DEXED: DemoPlugin = DemoPlugin {
    format: PluginFormat::Vst3,
    id: "ABCDEF019182FAEB4447534244657864",
    name: "Dexed",
    vendor: "Digital Suburban",
    version: "1.0.1",
};

/// JUCE VST2-compatible class id: "VST" + VST2 id `Vita` + "vital".
const VITAL: DemoPlugin = DemoPlugin {
    format: PluginFormat::Vst3,
    id: "56535456697461766974616C00000000",
    name: "Vital",
    vendor: "Vital Audio",
    version: "1.6.4",
};

const LSP_PARA_EQ: DemoPlugin = DemoPlugin {
    format: PluginFormat::Clap,
    id: "in.lsp-plug.para_equalizer_x8_stereo",
    name: "LSP Para EQ x8",
    vendor: "LSP CLAP",
    version: "",
};

const LSP_COMPRESSOR: DemoPlugin = DemoPlugin {
    format: PluginFormat::Clap,
    id: "in.lsp-plug.compressor_stereo",
    name: "LSP Compressor",
    vendor: "LSP CLAP",
    version: "",
};

const DRAGONFLY_HALL: DemoPlugin = DemoPlugin {
    format: PluginFormat::Clap,
    id: "michaelwillis.dragonfly.hall",
    name: "Dragonfly Hall Reverb",
    vendor: "Michael Willis and Rob vd Berg",
    version: "",
};

/// JUCE VST2-compatible class id: "VST" + VST2 id `cHL1` + "tal-choru".
const TAL_CHORUS: DemoPlugin = DemoPlugin {
    format: PluginFormat::Vst3,
    id: "56535463484C3174616C2D63686F7275",
    name: "TAL-Chorus-LX",
    vendor: "TAL-Togu Audio Line",
    version: "1.6.3",
};

/// MIDI and step envelopes, arrangement lanes and a master lane for automation integration tests.
pub fn automation_project() -> Project {
    use crate::automation::{
        AutomationMode, AutomationOwner, AutomationTarget, Point, Points, Shape,
    };
    use crate::edits::{self, NewBus, automation::*};
    use crate::params::ParamId;

    let project = demo_project();
    let track = project.tracks[2].id;
    let midi = project.tracks[2].clips[0].id;
    let drum = project.tracks[0].clips[0].id;
    let hexa = project.tracks[2].channel.instrument().unwrap().1.id;
    let (mut project, _) = edits::add_bus(&project, NewBus::effect(vec![track])).unwrap();
    let send = project.track(track).unwrap().sends[0].id;
    let owner = AutomationOwner::Track(track);
    let feedback = AutomationTarget::Param {
        device: hexa,
        param: ParamId::new("feedback"),
    };
    for (owner, target, label, mode) in [
        (
            owner,
            feedback.clone(),
            "Hexa · Feedback",
            AutomationMode::Read,
        ),
        (
            owner,
            AutomationTarget::Volume,
            "Volume",
            AutomationMode::Touch,
        ),
        (
            owner,
            AutomationTarget::Send { send },
            "Send",
            AutomationMode::Latch,
        ),
        (
            owner,
            AutomationTarget::Macro { index: 2 },
            "Macro 3",
            AutomationMode::Off,
        ),
        (
            AutomationOwner::Master,
            AutomationTarget::Pan,
            "Pan",
            AutomationMode::Read,
        ),
    ] {
        let (next, lane) = add_lane(&project, owner, target, label.into()).unwrap();
        let points = Points::new(vec![
            Point::linear(Tick::ZERO, 0.25),
            Point {
                tick: Tick(960),
                value: 0.5,
                shape: Shape::Curve { tension: 0.5 },
            },
            Point {
                tick: Tick(1920),
                value: 0.75,
                shape: Shape::Hold,
            },
            Point::linear(Tick(3840), 0.5),
        ]);
        project = set_lane_points(&next, owner, lane, points).unwrap();
        project = set_lane_mode(&project, owner, lane, mode).unwrap();
    }
    project = set_shown(&project, owner, true).unwrap();
    project = edits::set_clip_loop(&project, midi, Some(Tick(1920)), Tick(3840)).unwrap();
    for (clip, target, label) in [
        (midi, feedback, "Hexa · Feedback"),
        (drum, AutomationTarget::Pan, "Pan"),
    ] {
        project = add_clip_automation(&project, clip, target.clone(), label.into()).unwrap();
        project = set_clip_points(
            &project,
            clip,
            &target,
            Points::new(vec![
                Point::linear(Tick::ZERO, 0.25),
                Point::linear(Tick(1920), 0.75),
            ]),
        )
        .unwrap();
    }
    project
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::DEFAULT_PROJECT_NAME;

    fn notes(clip: &Clip) -> &[Note] {
        match &clip.content {
            ClipContent::Midi(m) => &m.active().notes,
            ClipContent::Step(_) | ClipContent::Audio(_) => panic!("not a MIDI clip"),
        }
    }

    #[test]
    fn demo_project_settings() {
        let p = demo_project();
        assert_eq!(p.name, "Night Drive");
        assert_eq!(p.tempo, Tempo::new(124.0));
        assert_eq!(p.meter, crate::time::Meter::FOUR_FOUR);
        assert_eq!(p.loop_range, TickRange::new(Tick(30_720), Tick(46_080)));
        assert!(p.loop_on && !p.metronome);
        assert_eq!(p.count_in_bars, 1);
        assert_eq!(p.song_end(), Tick(16 * 3840));
        assert!(p.master.devices.is_empty());
    }

    #[test]
    fn demo_tracks_match_the_prototype() {
        let p = demo_project();
        let summary: Vec<(&str, u16, f32, f32, bool)> = p
            .tracks
            .iter()
            .map(|t| {
                let c = &t.channel;
                (t.name.as_str(), t.hue.0, c.volume_db, c.pan, c.mute)
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Drums", 45, -6.0, 0.0, false),
                ("Bass", 250, -4.0, 0.0, false),
                ("Lead", 150, -3.0, 0.0, false),
                ("Pad", 300, -9.0, -0.1, false),
                ("Keys", 95, -8.0, 0.2, false),
                ("Arp", 200, -10.0, 0.0, true),
            ]
        );
        for t in &p.tracks {
            assert!(!t.arm && !t.monitor && !t.channel.solo);
            assert_eq!(t.height, TRACK_HEIGHT_DEFAULT);
            assert_eq!(t.channel.devices[0].role, DeviceRole::Instrument);
            assert!(
                t.channel.devices[1..]
                    .iter()
                    .all(|d| d.role == DeviceRole::Effect)
            );
        }
    }

    #[test]
    fn demo_device_chains() {
        let p = demo_project();
        let chain = |i: usize| -> Vec<(&str, bool)> {
            p.tracks[i]
                .channel
                .devices
                .iter()
                .map(|d| (d.plugin.name.as_str(), d.bypass))
                .collect()
        };
        assert_eq!(
            chain(0),
            [(DRUM_SAMPLER_NAME, false), ("LSP Compressor", false)]
        );
        assert_eq!(chain(1), [("Hexa", false), ("LSP Para EQ x8", false)]);
        assert_eq!(
            chain(2),
            [
                ("Hexa", false),
                ("LSP Para EQ x8", false),
                ("TAL-Chorus-LX", true),
                ("Dragonfly Hall Reverb", false),
            ]
        );
        assert_eq!(
            chain(3),
            [
                ("Surge XT", false),
                ("LSP Para EQ x8", false),
                ("Dragonfly Hall Reverb", false),
            ]
        );
        assert_eq!(chain(4), [("Dexed", false)]);
        assert_eq!(chain(5), [("Vital", false)]);

        let drums = &p.tracks[0].channel.devices[0].plugin;
        assert_eq!(
            (drums.format, drums.id.as_str()),
            (PluginFormat::Clap, DRUM_SAMPLER_CLAP_ID)
        );
        let pad = &p.tracks[3].channel.devices[0].plugin;
        assert_eq!(pad.format, PluginFormat::Clap);
        for device in p.tracks.iter().flat_map(|t| t.channel.devices.iter()) {
            if device.plugin.format == PluginFormat::Vst3 {
                let id = &device.plugin.id;
                assert_eq!(id.len(), 32, "{id}");
                assert!(
                    id.chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
                );
            }
        }
    }

    #[test]
    fn demo_clips_and_notes() {
        let p = demo_project();
        let clips: Vec<Vec<(&str, i64, i64, usize)>> = p
            .tracks
            .iter()
            .map(|t| {
                t.clips
                    .iter()
                    .map(|c| {
                        let count = match &c.content {
                            ClipContent::Midi(m) => m.active().notes.len(),
                            ClipContent::Step(s) => s.rows.len(),
                            ClipContent::Audio(_) => 0,
                        };
                        (c.name.as_str(), c.start.0 / 3840, c.length.0 / 3840, count)
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            clips,
            [
                vec![
                    ("Beat A", 0, 4, 8),
                    ("Beat A", 4, 4, 8),
                    ("Beat B", 8, 4, 8),
                    ("Beat B", 12, 4, 8),
                ],
                vec![("Bass", 0, 8, 40), ("Bass 2", 8, 8, 40)],
                vec![
                    ("Lead A", 8, 2, 17),
                    ("Lead A", 10, 2, 17),
                    ("Lead B", 12, 4, 34)
                ],
                vec![("Pad", 0, 16, 24)],
                vec![("Keys", 4, 4, 20), ("Keys", 12, 4, 20)],
                vec![("Arp", 8, 8, 128)],
            ]
        );
        // Lead A starts with C4, E4 (8 steps) and G4 (2 steps, velocity 100).
        let lead = notes(&p.tracks[2].clips[0]);
        let first: Vec<(i64, i64, u8, u8)> = lead[..3]
            .iter()
            .map(|n| (n.start.0, n.length.0, n.pitch, n.velocity))
            .collect();
        assert_eq!(
            first,
            [(0, 1920, 60, 70), (0, 1920, 64, 62), (0, 480, 67, 100)]
        );
        // Bass and Keys notes default to velocity 90; the second Bass repetition starts at 2 bars.
        let bass = notes(&p.tracks[1].clips[0]);
        assert!(bass.iter().all(|n| n.velocity == 90));
        assert_eq!((bass[10].start, bass[10].pitch), (Tick(7680), 40));
        // Arp cycles through six pitches in sixteenths.
        let arp = notes(&p.tracks[5].clips[0]);
        assert_eq!(arp[6].pitch, 60);
        assert_eq!(arp[127].start, Tick(127 * 240));
        for t in &p.tracks {
            for c in &t.clips {
                assert_eq!(c.loop_length, None);
                if let ClipContent::Midi(m) = &c.content {
                    assert!(
                        m.active()
                            .notes
                            .is_sorted_by_key(|n| (n.start, n.pitch, n.id))
                    );
                    assert_eq!((m.takes.len(), m.active().name.as_str()), (1, ""));
                }
            }
        }
    }

    #[test]
    fn demo_beats_use_the_samplers_pads() {
        let p = demo_project();
        let ClipContent::Step(beat_b) = &p.tracks[0].clips[2].content else {
            panic!("Beat B is a step clip");
        };
        let rows: Vec<(&str, u8)> = beat_b
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.note))
            .collect();
        assert_eq!(
            rows,
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
        assert_eq!(
            (beat_b.length_steps, beat_b.resolution, beat_b.swing),
            (16, StepResolution::Sixteenth, 0.0)
        );
        let on = |row: usize| -> Vec<(usize, u8)> {
            beat_b.rows[row].steps[..16]
                .iter()
                .enumerate()
                .filter(|(_, s)| s.on)
                .map(|(i, s)| (i, s.velocity))
                .collect()
        };
        assert_eq!(on(0), [(0, 120), (4, 100), (8, 118), (12, 104), (14, 70)]);
        assert_eq!(on(4), [(3, 70), (11, 74)]);
        assert_eq!(on(5), [(13, 90), (15, 96)]);
        assert!(on(6).is_empty());
        let s = beat_b.rows[0].steps[0];
        assert_eq!((s.probability, s.ratchet), (100, 1));
        // Beat A has no clap and no fifth kick.
        let ClipContent::Step(beat_a) = &p.tracks[0].clips[0].content else {
            panic!("Beat A is a step clip");
        };
        assert!(!beat_a.rows[0].steps[14].on);
        assert!(beat_a.rows[2].steps.iter().all(|s| !s.on));
    }

    #[test]
    fn demo_ids_follow_document_order() {
        let p = demo_project();
        let mut ids = Vec::new();
        for t in &p.tracks {
            ids.push(t.id.0);
            ids.extend(t.channel.devices.iter().map(|d| d.id.0));
            for c in &t.clips {
                ids.push(c.id.0);
                if let ClipContent::Midi(m) = &c.content {
                    ids.extend(m.active().notes.iter().map(|n| n.id.0));
                }
            }
        }
        let expected: Vec<u64> = (1..=ids.len() as u64).collect();
        assert_eq!(ids, expected);
        assert_eq!(p.next_id, ids.len() as u64 + 1);
    }

    #[test]
    fn empty_project_is_unnamed() {
        let p = empty_project();
        assert_eq!(p.name, DEFAULT_PROJECT_NAME);
        assert!(p.tracks.is_empty());
        assert_eq!(p.next_id, 1);
    }
}
