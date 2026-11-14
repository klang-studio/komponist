//! Property tests across the crate: random edit sequences through [`Document`] (undo all restores
//! the initial project, redo all the final one, every step keeps the invariants, errors change
//! nothing), gestures and untracked edits, and the save → load round trip of edited projects.

use std::sync::Arc;

use proptest::prelude::*;

use crate::document::Document;
use crate::edits::tests::check_invariants;
use crate::edits::{
    self, ChannelProp, ClipProp, EditError, ExclusiveFlag, NewClip, NewClipContent, NewDevice,
    NewNote, NewTrack, ProjectProp, StepClipProp, StepProp, StepRowProp, StepRowSpec, TrackProp,
    VelocityChange,
};
use crate::fixtures::{demo_project, empty_project};
use crate::ids::{ClipId, DeviceId, NoteId, TrackId};
use crate::persist::{from_json, to_json};
use crate::project::{
    ChannelRef, ClipContent, ClipKind, DeviceRole, Hue, PluginFormat, PluginRef, Project,
};
use crate::time::{Meter, StepResolution, Tempo, Tick, TickRange};

// ------------------------------------------------------------------------------------------------
// Operations
// ------------------------------------------------------------------------------------------------

/// One edit with abstract targets; `pick` values select an existing object modulo the count (or a
/// missing one when there is none, which exercises the error paths).
#[derive(Debug, Clone)]
enum Op {
    Project(u8, i32),
    Master(u8, i32),
    AddTrack {
        instrument: u8,
        index: Option<u8>,
        arm: bool,
    },
    RemoveTrack(u16),
    MoveTracks(u16, u16, u8),
    TrackProp(u16, u8, i32),
    Exclusive(u16, bool),
    AddClip {
        track: u16,
        start: i64,
        length: i64,
        step: bool,
        notes: Vec<(i64, i64, u8, u8)>,
    },
    RemoveClip(u16),
    MoveClips(u16, u16, i64, i32),
    CopyClips(u16, u16, i64, i32),
    Duplicate(u16),
    Length(u16, i64),
    Loop(u16, Option<i64>, i64),
    Split(u16, i64),
    Mute(u16, bool),
    ClipProp(u16, u8, i32),
    AddNotes(u16, Vec<(i64, i64, u8, u8)>),
    RemoveNote(u16, u16),
    MoveNotes(u16, u16, i64, i32),
    CopyNotes(u16, u16, i64, i32),
    Resize(u16, u16, i64),
    Velocity(u16, u16, bool, i16),
    Quantize(u16, Option<u16>, u8),
    Step(u16, u8, u8, u8, u8),
    Pattern(u16, u8, u8),
    Row(u16, u8, u8, u8),
    Rows(u16, u8),
    ToMidi(u16),
    InsertDevice {
        track: u16,
        master: bool,
        index: u8,
        instrument: bool,
    },
    ReplaceInstrument(u16),
    RemoveDevice(u16),
    MoveDevice(u16, u8),
    Bypass(u16, bool),
    State(u16, Vec<u8>),
    AddTake(u16),
    SelectTake(u16, u8),
}

fn notes_strategy() -> impl Strategy<Value = Vec<(i64, i64, u8, u8)>> {
    prop::collection::vec((-100i64..8000, -10i64..4000, 0u8..=255, 0u8..=255), 0..6)
}

fn op() -> impl Strategy<Value = Op> {
    let pick = any::<u16>;
    prop_oneof![
        (0u8..7, -100i32..300).prop_map(|(w, v)| Op::Project(w, v)),
        (0u8..4, -100i32..100).prop_map(|(w, v)| Op::Master(w, v)),
        (0u8..4, prop::option::of(0u8..8), any::<bool>()).prop_map(|(instrument, index, arm)| {
            Op::AddTrack {
                instrument,
                index,
                arm,
            }
        }),
        pick().prop_map(Op::RemoveTrack),
        (pick(), pick(), 0u8..8).prop_map(|(a, b, to)| Op::MoveTracks(a, b, to)),
        (pick(), 0u8..10, -200i32..400).prop_map(|(t, w, v)| Op::TrackProp(t, w, v)),
        (pick(), any::<bool>()).prop_map(|(t, arm)| Op::Exclusive(t, arm)),
        (
            pick(),
            -500i64..20_000,
            -10i64..8000,
            any::<bool>(),
            notes_strategy()
        )
            .prop_map(|(track, start, length, step, notes)| Op::AddClip {
                track,
                start,
                length,
                step,
                notes,
            }),
        pick().prop_map(Op::RemoveClip),
        (pick(), pick(), -8000i64..8000, -3i32..3)
            .prop_map(|(a, b, d, t)| Op::MoveClips(a, b, d, t)),
        (pick(), pick(), -8000i64..8000, -3i32..3)
            .prop_map(|(a, b, d, t)| Op::CopyClips(a, b, d, t)),
        pick().prop_map(Op::Duplicate),
        (pick(), -10i64..20_000).prop_map(|(c, l)| Op::Length(c, l)),
        (pick(), prop::option::of(-10i64..8000), -10i64..20_000)
            .prop_map(|(c, l, len)| Op::Loop(c, l, len)),
        (pick(), -100i64..8000).prop_map(|(c, offset)| Op::Split(c, offset)),
        (pick(), any::<bool>()).prop_map(|(c, m)| Op::Mute(c, m)),
        (pick(), 0u8..4, -200i32..20_000).prop_map(|(c, w, v)| Op::ClipProp(c, w, v)),
        (pick(), notes_strategy()).prop_map(|(c, n)| Op::AddNotes(c, n)),
        (pick(), pick()).prop_map(|(c, n)| Op::RemoveNote(c, n)),
        (pick(), pick(), -4000i64..4000, -40i32..40)
            .prop_map(|(c, n, d, p)| Op::MoveNotes(c, n, d, p)),
        (pick(), pick(), -4000i64..4000, -40i32..40)
            .prop_map(|(c, n, d, p)| Op::CopyNotes(c, n, d, p)),
        (pick(), pick(), -2000i64..2000).prop_map(|(c, n, d)| Op::Resize(c, n, d)),
        (pick(), pick(), any::<bool>(), -300i16..300)
            .prop_map(|(c, n, set, v)| Op::Velocity(c, n, set, v)),
        (pick(), prop::option::of(pick()), 0u8..6).prop_map(|(c, n, g)| Op::Quantize(c, n, g)),
        (pick(), 0u8..10, 0u8..34, 0u8..4, any::<u8>())
            .prop_map(|(c, r, s, w, v)| Op::Step(c, r, s, w, v)),
        (pick(), 0u8..3, any::<u8>()).prop_map(|(c, w, v)| Op::Pattern(c, w, v)),
        (pick(), 0u8..10, 0u8..4, any::<u8>()).prop_map(|(c, r, w, v)| Op::Row(c, r, w, v)),
        (pick(), 0u8..12).prop_map(|(c, n)| Op::Rows(c, n)),
        pick().prop_map(Op::ToMidi),
        (pick(), any::<bool>(), 0u8..6, any::<bool>()).prop_map(
            |(track, master, index, instrument)| Op::InsertDevice {
                track,
                master,
                index,
                instrument,
            }
        ),
        pick().prop_map(Op::ReplaceInstrument),
        pick().prop_map(Op::RemoveDevice),
        (pick(), 0u8..6).prop_map(|(d, to)| Op::MoveDevice(d, to)),
        (pick(), any::<bool>()).prop_map(|(d, b)| Op::Bypass(d, b)),
        (pick(), prop::collection::vec(any::<u8>(), 0..16)).prop_map(|(d, s)| Op::State(d, s)),
        pick().prop_map(Op::AddTake),
        (pick(), 0u8..4).prop_map(|(c, t)| Op::SelectTake(c, t)),
    ]
}

// ------------------------------------------------------------------------------------------------
// Resolving and applying
// ------------------------------------------------------------------------------------------------

/// The `pick`-th of `items`, or `missing` when there are none.
fn choose<T: Copy>(items: &[T], pick: u16, missing: T) -> T {
    if items.is_empty() {
        missing
    } else {
        items[usize::from(pick) % items.len()]
    }
}

fn tracks(p: &Project) -> Vec<TrackId> {
    p.tracks.iter().map(|t| t.id).collect()
}

fn clips(p: &Project, kind: Option<ClipKind>) -> Vec<ClipId> {
    p.tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| kind.is_none_or(|k| c.kind() == k))
        .map(|c| c.id)
        .collect()
}

fn notes_of(p: &Project, clip: ClipId) -> Vec<NoteId> {
    match p.find_clip(clip).map(|(_, c)| &c.content) {
        Some(ClipContent::Midi(m)) => m.active().notes.iter().map(|n| n.id).collect(),
        _ => Vec::new(),
    }
}

fn devices(p: &Project) -> Vec<DeviceId> {
    p.tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .chain(p.master.devices.iter())
        .map(|d| d.id)
        .collect()
}

const NO_TRACK: TrackId = TrackId(u64::MAX);
const NO_CLIP: ClipId = ClipId(u64::MAX);
const NO_NOTE: NoteId = NoteId(u64::MAX);
const NO_DEVICE: DeviceId = DeviceId(u64::MAX);

fn plugin(n: u8) -> PluginRef {
    let (format, id, name) = match n % 4 {
        0 => (PluginFormat::Builtin, "pisynth", "pisynth"),
        1 => (PluginFormat::Builtin, "klopfer", "klopfer"),
        2 => (PluginFormat::Clap, "test.synth", "Synth"),
        _ => (
            PluginFormat::Vst3,
            "0123456789ABCDEF0123456789ABCDEF",
            "Effekt",
        ),
    };
    PluginRef {
        format,
        id: id.to_owned(),
        name: name.to_owned(),
        vendor: "Test".to_owned(),
        version: "1".to_owned(),
        module: Box::default(),
    }
}

fn new_notes(notes: &[(i64, i64, u8, u8)]) -> Vec<NewNote> {
    notes
        .iter()
        .map(|&(start, length, pitch, velocity)| NewNote {
            start: Tick(start),
            length: Tick(length),
            pitch,
            velocity,
        })
        .collect()
}

const GRIDS: [i64; 6] = [3840, 960, 480, 240, 160, 120];

/// The edit `op` stands for, resolved against `p`.
fn run(p: &Project, op: &Op) -> Result<Project, EditError> {
    let track = |pick| choose(&tracks(p), pick, NO_TRACK);
    let clip = |pick| choose(&clips(p, None), pick, NO_CLIP);
    let midi = |pick| choose(&clips(p, Some(ClipKind::Midi)), pick, NO_CLIP);
    let step = |pick| choose(&clips(p, Some(ClipKind::Step)), pick, NO_CLIP);
    let device = |pick| choose(&devices(p), pick, NO_DEVICE);
    let note = |clip, pick| choose(&notes_of(p, clip), pick, NO_NOTE);
    match op.clone() {
        Op::Project(which, v) => {
            let prop = match which {
                0 => ProjectProp::Name(format!("Projekt {v}")),
                1 => ProjectProp::Tempo(Tempo::new(f64::from(v))),
                2 => ProjectProp::Meter(Meter {
                    num: v.rem_euclid(40) as u8,
                    den: 1 << v.rem_euclid(6),
                }),
                3 => ProjectProp::LoopRange(TickRange::new(Tick(i64::from(v) * 100), Tick(9000))),
                4 => ProjectProp::LoopOn(v % 2 == 0),
                5 => ProjectProp::Metronome(v % 2 == 0),
                _ => ProjectProp::CountInBars(v.rem_euclid(10) as u8),
            };
            edits::set_project_prop(p, prop)
        }
        Op::Master(which, v) => edits::set_master_prop(p, channel_prop(which, v)),
        Op::AddTrack {
            instrument,
            index,
            arm,
        } => {
            let spec = NewTrack {
                name: None,
                hue: None,
                instrument: (instrument < 3).then(|| plugin(instrument)),
                index: index.map(usize::from),
                arm_exclusive: arm,
            };
            edits::add_track(p, spec).map(|(p, _)| p)
        }
        Op::RemoveTrack(t) => edits::remove_tracks(p, &[track(t)]),
        Op::MoveTracks(a, b, to) => edits::move_tracks(p, &[track(a), track(b)], usize::from(to)),
        Op::TrackProp(t, which, v) => {
            let prop = match which {
                0 => TrackProp::Name(format!("Spur {v}")),
                1 => TrackProp::Hue(Hue(v.rem_euclid(360) as u16)),
                2 => TrackProp::Hue(Hue::ALL[v.rem_euclid(14) as usize]),
                3 => TrackProp::Arm(v % 2 == 0),
                4 => TrackProp::Monitor(v % 2 == 0),
                5 => TrackProp::Height(v.clamp(0, 400) as u16),
                6 => TrackProp::MidiChannel(crate::MidiChannelSel::Channel(v.rem_euclid(20) as u8)),
                7 => TrackProp::Input(crate::MidiInputSel::Device(format!("Port {v}"))),
                w => TrackProp::Channel(channel_prop(w - 8, v)),
            };
            edits::set_track_prop(p, &[track(t)], prop)
        }
        Op::Exclusive(t, arm) => {
            let flag = if arm {
                ExclusiveFlag::Arm
            } else {
                ExclusiveFlag::Solo
            };
            edits::set_exclusive(p, track(t), flag)
        }
        Op::AddClip {
            track: t,
            start,
            length,
            step,
            notes,
        } => {
            let content = if step {
                NewClipContent::Step { rows: Vec::new() }
            } else {
                NewClipContent::Midi {
                    notes: new_notes(&notes),
                    take_name: String::new(),
                }
            };
            let spec = NewClip {
                start: Tick(start),
                length: Tick(length),
                name: "Clip".to_owned(),
                content,
            };
            edits::add_clip(p, track(t), spec).map(|(p, _)| p)
        }
        Op::RemoveClip(c) => edits::remove_clips(p, &[clip(c)]),
        Op::MoveClips(a, b, d, t) => edits::move_clips(p, &[clip(a), clip(b)], Tick(d), t),
        Op::CopyClips(a, b, d, t) => {
            edits::copy_clips(p, &[clip(a), clip(b)], Tick(d), t).map(|(p, _)| p)
        }
        Op::Duplicate(c) => edits::duplicate_clips(p, &[clip(c)]).map(|(p, _)| p),
        Op::Length(c, l) => edits::set_clip_length(p, clip(c), Tick(l)),
        Op::Loop(c, l, len) => edits::set_clip_loop(p, clip(c), l.map(Tick), Tick(len)),
        Op::Split(c, offset) => {
            // Relative to the clip start, so most splits hit the clip.
            let c = clip(c);
            let start = p.find_clip(c).map_or(Tick::ZERO, |(_, c)| c.start);
            edits::split_clips(p, &[c], start + Tick(offset)).map(|(p, _)| p)
        }
        Op::Mute(c, m) => edits::set_clips_muted(p, &[clip(c)], m),
        Op::ClipProp(c, which, v) => {
            let prop = match which {
                0 => ClipProp::Name(format!("Clip {v}")),
                1 => ClipProp::Start(Tick(i64::from(v))),
                2 => ClipProp::Transpose(v.clamp(-128, 127) as i8),
                _ => ClipProp::VelocityPct(v.clamp(-128, 127) as i8),
            };
            edits::set_clip_prop(p, clip(c), prop)
        }
        Op::AddNotes(c, notes) => edits::add_notes(p, midi(c), &new_notes(&notes)).map(|(p, _)| p),
        Op::RemoveNote(c, n) => {
            let c = midi(c);
            edits::remove_notes(p, c, &[note(c, n)])
        }
        Op::MoveNotes(c, n, d, pitch) => {
            let c = midi(c);
            edits::move_notes(p, c, &[note(c, n), note(c, n / 2)], Tick(d), pitch)
        }
        Op::CopyNotes(c, n, d, pitch) => {
            let c = midi(c);
            edits::copy_notes(p, c, &[note(c, n)], Tick(d), pitch).map(|(p, _)| p)
        }
        Op::Resize(c, n, d) => {
            let c = midi(c);
            edits::resize_notes(p, c, &[note(c, n)], Tick(d))
        }
        Op::Velocity(c, n, set, v) => {
            let c = midi(c);
            let change = if set {
                VelocityChange::Set(v.clamp(0, 255) as u8)
            } else {
                VelocityChange::Offset(v)
            };
            edits::set_notes_velocity(p, c, &[note(c, n)], change)
        }
        Op::Quantize(c, n, g) => {
            let c = midi(c);
            let selection = n.map(|n| vec![note(c, n)]);
            edits::quantize_notes(p, c, selection.as_deref(), Tick(GRIDS[usize::from(g)]))
        }
        Op::Step(c, row, s, which, v) => {
            let prop = match which {
                0 => StepProp::On(v.is_multiple_of(2)),
                1 => StepProp::Velocity(v),
                2 => StepProp::Probability(v),
                _ => StepProp::Ratchet(v % 6),
            };
            edits::set_step(p, step(c), usize::from(row), usize::from(s), prop)
        }
        Op::Pattern(c, which, v) => {
            let prop = match which {
                0 => StepClipProp::LengthSteps(v % 40),
                1 => StepClipProp::Resolution(
                    [
                        StepResolution::Eighth,
                        StepResolution::Sixteenth,
                        StepResolution::ThirtySecond,
                        StepResolution::EighthTriplet,
                        StepResolution::SixteenthTriplet,
                    ][usize::from(v % 5)],
                ),
                _ => StepClipProp::Swing(f32::from(v) / 200.0),
            };
            edits::set_step_clip_prop(p, step(c), prop)
        }
        Op::Row(c, row, which, v) => {
            let prop = match which {
                0 => StepRowProp::Name(format!("Pad {v}")),
                1 => StepRowProp::Note(v),
                2 => StepRowProp::Mute(v.is_multiple_of(2)),
                _ => StepRowProp::Solo(v.is_multiple_of(2)),
            };
            edits::set_step_row(p, step(c), usize::from(row), prop)
        }
        Op::Rows(c, count) => {
            let rows: Vec<StepRowSpec> = (0..count)
                .map(|i| StepRowSpec {
                    name: format!("Pad {}", i + 1),
                    note: 36 + i,
                })
                .collect();
            edits::set_step_rows(p, step(c), &rows)
        }
        Op::ToMidi(c) => edits::step_clip_to_midi(p, step(c)),
        Op::InsertDevice {
            track: t,
            master,
            index,
            instrument,
        } => {
            let channel = if master {
                ChannelRef::Master
            } else {
                ChannelRef::Track(track(t))
            };
            let spec = NewDevice {
                role: if instrument {
                    DeviceRole::Instrument
                } else {
                    DeviceRole::Effect
                },
                plugin: plugin(index),
                state: None,
            };
            edits::insert_device(p, channel, usize::from(index), spec).map(|(p, _)| p)
        }
        Op::ReplaceInstrument(t) => {
            edits::replace_instrument(p, track(t), plugin(t as u8)).map(|(p, _)| p)
        }
        Op::RemoveDevice(d) => edits::remove_device(p, device(d)),
        Op::MoveDevice(d, to) => edits::move_device(p, device(d), usize::from(to)),
        Op::Bypass(d, b) => edits::set_device_bypass(p, device(d), b),
        Op::State(d, bytes) => {
            let state = (!bytes.is_empty()).then(|| Arc::from(bytes));
            edits::set_device_state(p, device(d), state)
        }
        Op::AddTake(c) => edits::add_take(p, midi(c), format!("Take {c}")).map(|(p, _)| p),
        Op::SelectTake(c, t) => edits::select_take(p, midi(c), usize::from(t)),
    }
}

fn channel_prop(which: u8, v: i32) -> ChannelProp {
    match which {
        0 => ChannelProp::VolumeDb(v as f32 / 4.0),
        1 => ChannelProp::Pan(v as f32 / 60.0),
        2 => ChannelProp::Mute(v % 2 == 0),
        _ => ChannelProp::Solo(v % 2 == 0),
    }
}

/// Applies `op` as one undo step and checks the outcome: on success the invariants hold, on
/// error the document is unchanged.
fn apply(doc: &mut Document, op: &Op) -> Result<bool, TestCaseError> {
    let before = Arc::clone(doc.project());
    let revision = doc.revision();
    match doc.apply("Test", |p| run(p, op)) {
        Ok(()) => {
            check_invariants(doc.project());
            prop_assert!(!Arc::ptr_eq(doc.project(), &before));
            prop_assert_eq!(doc.undo_label(), Some("Test"));
            Ok(true)
        }
        Err(_) => {
            prop_assert!(Arc::ptr_eq(doc.project(), &before));
            prop_assert_eq!(doc.revision(), revision);
            Ok(false)
        }
    }
}

/// Undoes everything and compares with `history` (oldest first), then redoes everything.
fn check_history(doc: &mut Document, history: &[Arc<Project>]) -> Result<(), TestCaseError> {
    for expected in history.iter().rev().skip(1) {
        prop_assert!(doc.undo());
        prop_assert_eq!(&**doc.project(), &**expected);
    }
    prop_assert!(!doc.undo());
    for expected in history.iter().skip(1) {
        prop_assert!(doc.redo());
        prop_assert_eq!(&**doc.project(), &**expected);
    }
    prop_assert!(!doc.redo());
    Ok(())
}

/// An entry of a session: a single edit, a gesture of edits, or an untracked transport change.
#[derive(Debug, Clone)]
enum Entry {
    Single(Op),
    Gesture { ops: Vec<Op>, cancel: bool },
    Untracked(u8, u8),
}

fn entry() -> impl Strategy<Value = Entry> {
    prop_oneof![
        3 => op().prop_map(Entry::Single),
        2 => (prop::collection::vec(op(), 0..5), any::<bool>())
            .prop_map(|(ops, cancel)| Entry::Gesture { ops, cancel }),
        1 => (0u8..3, any::<u8>()).prop_map(|(w, v)| Entry::Untracked(w, v)),
    ]
}

fn untracked(which: u8, v: u8) -> impl Fn(&Project) -> Result<Project, EditError> {
    move |p| {
        let prop = match which {
            0 => ProjectProp::LoopOn(v.is_multiple_of(2)),
            1 => ProjectProp::Metronome(v.is_multiple_of(2)),
            _ => ProjectProp::CountInBars(v % 6),
        };
        edits::set_project_prop(p, prop)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Random edit sequences: undo all = initial project, redo all = final project.
    #[test]
    fn undo_all_and_redo_all(demo in any::<bool>(), ops in prop::collection::vec(op(), 1..40)) {
        let initial = if demo { demo_project() } else { empty_project() };
        let mut doc = Document::new(initial.clone());
        let mut history = vec![Arc::clone(doc.project())];
        for op in &ops {
            if apply(&mut doc, op)? {
                history.push(Arc::clone(doc.project()));
            }
        }
        let last = Arc::clone(doc.project());
        check_history(&mut doc, &history)?;
        prop_assert_eq!(&*history[0], &initial);
        prop_assert!(Arc::ptr_eq(doc.project(), &last));
        // Save → load gives the same project.
        let loaded = from_json(&to_json(&last).unwrap()).unwrap();
        prop_assert_eq!(&loaded, &*last);
    }

    /// Gestures push one step (or none when cancelled or empty); untracked changes reach every
    /// snapshot and survive undo and redo.
    #[test]
    fn gestures_and_untracked_edits(entries in prop::collection::vec(entry(), 1..20)) {
        let mut doc = Document::new(demo_project());
        let mut history = vec![Arc::clone(doc.project())];
        for entry in &entries {
            match entry {
                Entry::Single(op) => {
                    if apply(&mut doc, op)? {
                        history.push(Arc::clone(doc.project()));
                    }
                }
                Entry::Gesture { ops, cancel } => {
                    let before = Arc::clone(doc.project());
                    doc.begin_gesture("Ziehen");
                    for op in ops {
                        // During a gesture, `apply` replaces the root without pushing.
                        let _ = doc.apply("ignored", |p| run(p, op));
                        check_invariants(doc.project());
                    }
                    if *cancel {
                        doc.cancel_gesture();
                        prop_assert!(Arc::ptr_eq(doc.project(), &before));
                    } else {
                        let changed = !Arc::ptr_eq(doc.project(), &before);
                        doc.end_gesture();
                        if changed {
                            prop_assert_eq!(doc.undo_label(), Some("Ziehen"));
                            history.push(Arc::clone(doc.project()));
                        }
                    }
                }
                Entry::Untracked(which, v) => {
                    let edit = untracked(*which, *v);
                    doc.apply_untracked(&edit).unwrap();
                    for state in &mut history {
                        *state = Arc::new(edit(state).unwrap());
                    }
                    prop_assert_eq!(&**doc.project(), &**history.last().unwrap());
                }
            }
        }
        check_history(&mut doc, &history)?;
    }

    /// Total-delta drags: each update starts from the gesture start, so the result equals one
    /// edit with the last delta, and ids allocated by copies are the same on every update.
    #[test]
    fn drags_apply_the_total_delta(
        deltas in prop::collection::vec((-8000i64..8000, -3i32..3), 1..8),
        copy in any::<bool>(),
    ) {
        let start = demo_project();
        let lead_a = start.tracks[2].clips[0].id;
        let mut doc = Document::new(start.clone());
        doc.begin_gesture("Clip verschieben");
        let mut copies = Vec::new();
        for &(delta, tracks) in &deltas {
            if copy {
                let ids = doc
                    .apply_in_gesture_with(|p| edits::copy_clips(p, &[lead_a], Tick(delta), tracks))
                    .unwrap();
                copies.push(ids);
            } else {
                doc.apply_in_gesture(|p| edits::move_clips(p, &[lead_a], Tick(delta), tracks))
                    .unwrap();
            }
        }
        doc.end_gesture();
        let (delta, tracks) = *deltas.last().unwrap();
        let expected = if copy {
            edits::copy_clips(&start, &[lead_a], Tick(delta), tracks).unwrap().0
        } else {
            edits::move_clips(&start, &[lead_a], Tick(delta), tracks).unwrap()
        };
        prop_assert_eq!(&**doc.project(), &expected);
        prop_assert!(copies.windows(2).all(|w| w[0] == w[1]));
        prop_assert!(doc.undo());
        prop_assert_eq!(&**doc.project(), &start);
        prop_assert!(!doc.undo());
    }
}
