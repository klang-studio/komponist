//! surf in the demo (#107): a „Vowel Pad“ voice over the Pad's chords in the second half, whose
//! LFOs move both table positions and whose automation opens oscillator B's position from bar 9
//! to bar 16.

use std::sync::Arc;

use komponist_builtin::Internal;
use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
use komponist_model::edits::{self, NewClip, NewClipContent, NewNote, NewTrack};
use komponist_model::params::ParamId;
use komponist_model::{Hue, Project, Tick};

/// Name of the track.
pub const TRACK: &str = "Vowel Pad";
/// The factory preset it plays.
pub const PRESET: &str = "Vowel Pad";
/// Its volume in dB (set by `render_levels`).
const VOLUME_DB: f32 = -10.0;
/// The voicings over the Pad's chords, two bars each from bar 9: G4 + C5 over C, E4 + A4 over Am.
const VOICINGS: [[u8; 2]; 4] = [[67, 72], [64, 69], [67, 72], [64, 69]];
/// Position B: from almost closed at bar 9 to wide open at bar 16 (normalized, the parameter runs
/// 0 … 1).
const POSITION_B: [f32; 2] = [0.15, 0.85];

pub(super) fn add_track(project: &mut Project) {
    let kind = Internal::Surf;
    let (next, track) = edits::add_track(
        project,
        NewTrack {
            name: Some(TRACK.into()),
            hue: Some(Hue::INDIGO),
            instrument: Some(kind.plugin_ref()),
            index: None,
            arm_exclusive: false,
        },
    )
    .expect("showcase surf track");
    let notes: Vec<NewNote> = VOICINGS
        .iter()
        .enumerate()
        .flat_map(|(i, pair)| {
            pair.map(|pitch| NewNote {
                start: Tick::from_steps(i as i64 * 32),
                length: Tick::from_steps(30),
                pitch,
                velocity: 80,
            })
        })
        .collect();
    let (mut next, _) = edits::add_clip(
        &next,
        track,
        NewClip {
            start: Tick::from_steps(8 * 16),
            length: Tick::from_steps(8 * 16),
            name: "Vowels".into(),
            content: NewClipContent::Midi {
                notes,
                take_name: String::new(),
            },
        },
    )
    .expect("showcase surf clip");
    let preset = kind
        .presets()
        .iter()
        .position(|p| p.name == PRESET)
        .expect("surf has the Vowel Pad");
    let state = kind.preset_state(None, preset).expect("the preset exists");
    let device = {
        let t = next
            .tracks
            .iter_mut()
            .find(|candidate| candidate.id == track)
            .expect("showcase surf track");
        let t = Arc::make_mut(t);
        t.channel.volume_db = VOLUME_DB;
        t.channel.pan = 0.15;
        let device = Arc::make_mut(&mut t.channel.devices[0]);
        device.state = Some(state.into());
        device.id
    };
    let target = AutomationTarget::Param {
        device,
        param: ParamId::new("osc_b_pos"),
    };
    let owner = AutomationOwner::Track(track);
    let (with_lane, lane) =
        edits::automation::add_lane(&next, owner, target, "Surf · Position B".into())
            .expect("showcase surf lane");
    next = edits::automation::set_lane_points(
        &with_lane,
        owner,
        lane,
        Points::new(vec![
            Point {
                tick: Tick::from_steps(8 * 16),
                value: POSITION_B[0],
                shape: Shape::Curve { tension: 0.3 },
            },
            Point::linear(Tick::from_steps(16 * 16), POSITION_B[1]),
        ]),
    )
    .expect("showcase surf curve");
    *project = next;
}
