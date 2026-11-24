//! surf in the demo (#107): a „Vowel Pad“ voice over the Pad's chords in the second half. Its LFOs
//! move both table positions a little; automation morphs oscillator A once through the whole vowel
//! table (A, E, I, O, U and back towards E) and opens oscillator B's position from bar 9 to bar 16.

use std::sync::Arc;

use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
use komponist_model::edits::{self, NewClip, NewClipContent, NewNote, NewTrack};
use komponist_model::{Hue, Project, Tick};

use super::DeviceSource;

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
/// Position A (the oscillator the graph shows first): bar and value, from „A“ at bar 9 through
/// the whole vowel table to „U“ at bar 13, then back towards „E“ by the end of bar 16.
const POSITION_A: [(i64, f32); 3] = [(8, 0.0), (12, 1.0), (16, 0.3)];

pub(super) fn add_track(project: &mut Project, source: DeviceSource<'_>) -> Result<(), String> {
    let prepared = source.prepare("surf", Some(PRESET), &[], &["osc_a_pos", "osc_b_pos"])?;
    let position_a = prepared.targets["osc_a_pos"].0.clone();
    let position_b = prepared.targets["osc_b_pos"].0.clone();
    let (next, track) = edits::add_track(
        project,
        NewTrack {
            name: Some(TRACK.into()),
            hue: Some(Hue::INDIGO),
            instrument: Some(prepared.plugin.clone()),
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
    let device = {
        let t = next
            .tracks
            .iter_mut()
            .find(|candidate| candidate.id == track)
            .expect("showcase surf track");
        let t = Arc::make_mut(t);
        t.channel.volume_db = VOLUME_DB;
        t.channel.pan = 0.15;
        let id = t.channel.devices[0].id;
        t.channel.devices[0] = Arc::new(prepared.into_device(id));
        id
    };
    let owner = AutomationOwner::Track(track);
    let target = AutomationTarget::Param {
        device,
        param: position_a,
    };
    let (with_lane, lane) =
        edits::automation::add_lane(&next, owner, target, "Surf · Position A".into())
            .expect("showcase surf lane A");
    let points = POSITION_A
        .iter()
        .map(|&(bar, value)| Point {
            tick: Tick::from_steps(bar * 16),
            value,
            shape: Shape::Curve { tension: 0.2 },
        })
        .collect();
    next = edits::automation::set_lane_points(&with_lane, owner, lane, Points::new(points))
        .expect("showcase surf curve A");
    let target = AutomationTarget::Param {
        device,
        param: position_b,
    };
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
    Ok(())
}
