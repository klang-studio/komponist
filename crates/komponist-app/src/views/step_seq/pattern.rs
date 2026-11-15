//! What the step sequencer computes without pixels: the running light, the value cycles of
//! Shift-click and Alt-click, the lane values, the accessible names of the steps and the pointer
//! gesture on the grid (`Prototyp.dc.html` `stepDown`, `stepEnter`, `paintUp`, `stepLane`; S03
//! notes).
//!
//! The grid gesture (S03 `notes`):
//!
//! | Press | Then |
//! |---|---|
//! | on a step that is off | it turns on (and sounds); dragging ↔ along the row turns more on |
//! | on a step that is on | release: it turns off; drag ↕ (more than 3 px): its velocity, 1 px = 1; drag ↔ to another step: steps of the row turn off |
//! | Shift on a step that is on | ratchet 1 → 2 → 3 → 4 → 1 |
//! | Alt on a step that is on | drag ↕: probability, 1 px = 1 %; release without dragging: 100 → 75 → 50 → 25 → 100 % |
//!
//! Painting stays in the row the press started in ("mehrere Steps in einer Zeile malen").

use komponist_engine::TransportState;
use komponist_i18n::Lang;
use komponist_model::edits::StepProp;
use komponist_model::{Step, StepClip, StepResolution, Tick};

use super::metrics::DRAG_THRESHOLD;
use crate::ui_state::StepLane;

/// The "Auflösung" entries in list order.
pub const RESOLUTIONS: [StepResolution; 5] = [
    StepResolution::Eighth,
    StepResolution::Sixteenth,
    StepResolution::ThirtySecond,
    StepResolution::EighthTriplet,
    StepResolution::SixteenthTriplet,
];

/// The label of a resolution entry ("1/16", "1/8 Triole"; "1/8 Triplet").
pub fn resolution_label(lang: Lang, resolution: StepResolution) -> &'static str {
    match resolution {
        StepResolution::Eighth => lang.grid_eighth(),
        StepResolution::Sixteenth => lang.grid_sixteenth(),
        StepResolution::ThirtySecond => lang.grid_thirty_second(),
        StepResolution::EighthTriplet => lang.grid_eighth_triplet(),
        StepResolution::SixteenthTriplet => lang.grid_sixteenth_triplet(),
    }
}

/// The probabilities Alt-click cycles through (`stepDown`: `o=[100,75,50,25]`).
pub const PROBABILITY_CYCLE: [u8; 4] = [100, 75, 50, 25];

/// Index of `resolution` in [`RESOLUTIONS`].
pub fn resolution_index(resolution: StepResolution) -> usize {
    RESOLUTIONS
        .iter()
        .position(|r| *r == resolution)
        .unwrap_or(1)
}

/// The pattern index of the step the playhead is in ("Lauflicht"), while the transport plays or
/// records and the playhead is inside the clip (`stepView`: `cur`). Swing does not move it.
pub fn current_step(
    pattern: &StepClip,
    clip_start: Tick,
    clip_length: Tick,
    playhead: Tick,
    transport: TransportState,
) -> Option<usize> {
    let running = matches!(
        transport,
        TransportState::Playing | TransportState::Recording
    );
    let local = playhead.0 - clip_start.0;
    if !running || local < 0 || local >= clip_length.0 {
        return None;
    }
    let step = pattern.resolution.ticks().0.max(1);
    let length = i64::from(pattern.length_steps.max(1));
    Some(((local / step) % length) as usize)
}

/// Shift-click: the next ratchet, 1 → 2 → 3 → 4 → 1.
pub fn next_ratchet(ratchet: u8) -> u8 {
    ratchet.clamp(1, 4) % 4 + 1
}

/// Alt-click: the next probability of [`PROBABILITY_CYCLE`]; a value outside it goes to 100 %
/// (`stepDown`: `indexOf` −1 + 1 = 0).
pub fn next_probability(probability: u8) -> u8 {
    let i = PROBABILITY_CYCLE.iter().position(|p| *p == probability);
    PROBABILITY_CYCLE[i.map_or(0, |i| (i + 1) % PROBABILITY_CYCLE.len())]
}

/// Swing as the toolbar shows it, in percent (0.12 → 12).
pub fn swing_percent(swing: f32) -> u8 {
    (swing.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// The value `step` shows in `lane`, as a fraction of the lane height (velocity of 127,
/// probability of 100 %, ratchet of 4).
pub fn lane_fraction(lane: StepLane, step: &Step) -> f32 {
    match lane {
        StepLane::Velocity => f32::from(step.velocity.min(127)) / 127.0,
        StepLane::Probability => f32::from(step.probability.min(100)) / 100.0,
        StepLane::Ratchet => f32::from(step.ratchet.clamp(1, 4)) / 4.0,
    }
}

/// The value a press at `fraction` (0 = bottom, 1 = top) of the lane sets (`stepLane`:
/// `round((H - y) / H × 127)`).
pub fn lane_prop(lane: StepLane, fraction: f32) -> StepProp {
    let f = fraction.clamp(0.0, 1.0);
    match lane {
        StepLane::Velocity => StepProp::Velocity(((f * 127.0).round() as u8).max(1)),
        StepLane::Probability => StepProp::Probability((f * 100.0).round() as u8),
        StepLane::Ratchet => StepProp::Ratchet(((f * 4.0).ceil() as u8).clamp(1, 4)),
    }
}

/// Whether `prop` would change `step`.
pub fn changes(step: &Step, prop: StepProp) -> bool {
    match prop {
        StepProp::On(on) => step.on != on,
        StepProp::Velocity(v) => step.velocity != v.clamp(1, 127),
        StepProp::Probability(p) => step.probability != p.min(100),
        StepProp::Ratchet(r) => step.ratchet != r.clamp(1, 4),
    }
}

/// The status bar text of a step change the prototype announces: "Velocity 52", "Ratchet ×3",
/// "Wahrscheinlichkeit 50 %"; none for switching a step on or off.
pub fn status_text(lang: Lang, prop: StepProp) -> Option<String> {
    match prop {
        StepProp::On(_) => None,
        StepProp::Velocity(v) => Some(lang.step_seq_status_velocity(v)),
        StepProp::Probability(p) => Some(lang.step_seq_status_probability(p)),
        StepProp::Ratchet(r) => Some(lang.step_seq_status_ratchet(r)),
    }
}

/// The accessible name of a step: "Step 5, Kick, an, Velocity 100", with ", Wahrscheinlichkeit
/// 50 %" and ", Ratchet 3" where they differ from the default; "Step 5, Kick, aus".
pub fn step_name(lang: Lang, index: usize, row: &str, step: &Step) -> String {
    if !step.on {
        return lang.step_seq_step_off(index + 1, row);
    }
    let mut name = lang.step_seq_step_on(index + 1, row, step.velocity);
    if step.probability < 100 {
        name.push_str(&lang.step_seq_step_probability(step.probability));
    }
    if step.ratchet > 1 {
        name.push_str(&lang.step_seq_step_ratchet(step.ratchet));
    }
    name
}

/// Modifiers held at the press on a step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PressMods {
    /// Shift: ratchet.
    pub shift: bool,
    /// Alt: probability.
    pub alt: bool,
}

/// A pointer gesture on the grid, from the press to the release.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// The press turned a step on; steps of `row` the pointer reaches turn on.
    PaintOn {
        /// The row.
        row: usize,
    },
    /// The press hit a set step and the pointer has not left it: release turns it off.
    Pending {
        /// The row.
        row: usize,
        /// The step.
        step: usize,
        /// Its velocity at the press.
        velocity: u8,
        /// Pointer y at the press.
        y: f32,
    },
    /// Drag ↕ on a set step: its velocity follows (1 px = 1).
    Velocity {
        /// The row.
        row: usize,
        /// The step.
        step: usize,
        /// Its velocity at the press.
        velocity: u8,
        /// Pointer y at the press.
        y: f32,
    },
    /// The pointer left a pressed set step: steps of `row` it reaches turn off.
    PaintOff {
        /// The row.
        row: usize,
    },
    /// Alt-press on a set step: drag ↕ sets its probability (1 px = 1 %); a release without
    /// dragging cycles it.
    Probability {
        /// The row.
        row: usize,
        /// The step.
        step: usize,
        /// Its probability at the press.
        probability: u8,
        /// Pointer y at the press.
        y: f32,
        /// The pointer moved more than the threshold.
        moved: bool,
    },
}

/// What a press on a step does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Press {
    /// The gesture that follows (`None`: the press is done, e.g. Shift-click).
    pub gesture: Option<Gesture>,
    /// The change of the pressed step.
    pub edit: Option<StepProp>,
    /// The row sounds (a step turned on).
    pub audition: bool,
}

/// A press on step `index` of `row` holding `step`, at pointer height `y`.
pub fn press(row: usize, index: usize, step: &Step, mods: PressMods, y: f32) -> Press {
    let done = |edit| Press {
        gesture: None,
        edit: Some(edit),
        audition: false,
    };
    if step.on && mods.shift {
        return done(StepProp::Ratchet(next_ratchet(step.ratchet)));
    }
    if step.on && mods.alt {
        return Press {
            gesture: Some(Gesture::Probability {
                row,
                step: index,
                probability: step.probability,
                y,
                moved: false,
            }),
            edit: None,
            audition: false,
        };
    }
    if !step.on {
        return Press {
            gesture: Some(Gesture::PaintOn { row }),
            edit: Some(StepProp::On(true)),
            audition: true,
        };
    }
    Press {
        gesture: Some(Gesture::Pending {
            row,
            step: index,
            velocity: step.velocity,
            y,
        }),
        edit: None,
        audition: false,
    }
}

impl Gesture {
    /// The row the gesture works in.
    pub fn row(&self) -> usize {
        match *self {
            Gesture::PaintOn { row }
            | Gesture::PaintOff { row }
            | Gesture::Pending { row, .. }
            | Gesture::Velocity { row, .. }
            | Gesture::Probability { row, .. } => row,
        }
    }

    /// The pointer is at height `y` over step `over` of the gesture's row (`None`: outside the
    /// pattern); `steps` is that row now. Returns the changes, as (step, change), and updates
    /// the gesture.
    pub fn moved(&mut self, steps: &[Step], over: Option<usize>, y: f32) -> Vec<(usize, StepProp)> {
        let mut out = Vec::new();
        let set = |i: usize, prop: StepProp, out: &mut Vec<(usize, StepProp)>| {
            if steps.get(i).is_some_and(|s| changes(s, prop)) {
                out.push((i, prop));
            }
        };
        match *self {
            Gesture::PaintOn { .. } => {
                if let Some(i) = over {
                    set(i, StepProp::On(true), &mut out);
                }
            }
            Gesture::PaintOff { .. } => {
                if let Some(i) = over {
                    set(i, StepProp::On(false), &mut out);
                }
            }
            Gesture::Pending {
                row,
                step,
                velocity,
                y: y0,
            } => {
                if (y0 - y).abs() > DRAG_THRESHOLD {
                    *self = Gesture::Velocity {
                        row,
                        step,
                        velocity,
                        y: y0,
                    };
                    return self.moved(steps, over, y);
                }
                if let Some(i) = over.filter(|i| *i != step) {
                    *self = Gesture::PaintOff { row };
                    set(step, StepProp::On(false), &mut out);
                    set(i, StepProp::On(false), &mut out);
                }
            }
            Gesture::Velocity {
                step,
                velocity,
                y: y0,
                ..
            } => {
                let v = (f32::from(velocity) + (y0 - y).round()).clamp(1.0, 127.0) as u8;
                set(step, StepProp::Velocity(v), &mut out);
            }
            Gesture::Probability {
                row,
                step,
                probability,
                y: y0,
                moved,
            } => {
                let dy = y0 - y;
                if moved || dy.abs() > DRAG_THRESHOLD {
                    *self = Gesture::Probability {
                        row,
                        step,
                        probability,
                        y: y0,
                        moved: true,
                    };
                    let p = (f32::from(probability) + dy.round()).clamp(0.0, 100.0) as u8;
                    set(step, StepProp::Probability(p), &mut out);
                }
            }
        }
        out
    }

    /// The release: the changes it makes, as (step, change).
    pub fn released(&self) -> Vec<(usize, StepProp)> {
        match *self {
            Gesture::Pending { step, .. } => vec![(step, StepProp::On(false))],
            Gesture::Probability {
                step,
                probability,
                moved: false,
                ..
            } => vec![(step, StepProp::Probability(next_probability(probability)))],
            _ => Vec::new(),
        }
    }

    /// The undo label of the gesture.
    pub fn label(&self) -> &'static str {
        match self {
            Gesture::Probability { .. } => "undo-probability",
            Gesture::Velocity { .. } => "undo-velocity",
            _ => "undo-steps",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::project::MAX_STEPS;
    use komponist_model::{StepRow, Tick};

    fn on(velocity: u8) -> Step {
        Step {
            on: true,
            velocity,
            ..Step::default()
        }
    }

    fn pattern(length: u8, resolution: StepResolution) -> StepClip {
        StepClip {
            rows: vec![StepRow {
                name: "Kick".into(),
                note: 36,
                mute: false,
                solo: false,
                steps: [Step::default(); MAX_STEPS],
            }],
            length_steps: length,
            resolution,
            swing: 0.0,
        }
    }

    #[test]
    fn the_running_light_follows_the_playhead_inside_the_clip() {
        let p = pattern(16, StepResolution::Sixteenth);
        let (start, len) = (Tick(3840), Tick(4 * 3840));
        let at = |tick, t| current_step(&p, start, len, Tick(tick), t);
        assert_eq!(at(3840, TransportState::Playing), Some(0));
        assert_eq!(at(3840 + 4 * 240 + 10, TransportState::Playing), Some(4));
        // The pattern repeats every 16 steps.
        assert_eq!(at(3840 + 17 * 240, TransportState::Recording), Some(1));
        assert_eq!(at(3839, TransportState::Playing), None);
        assert_eq!(at(3840 + 4 * 3840, TransportState::Playing), None);
        assert_eq!(at(3840, TransportState::Stopped), None);
        assert_eq!(at(3840, TransportState::CountIn), None);
        let p = pattern(3, StepResolution::Eighth);
        assert_eq!(
            current_step(
                &p,
                start,
                len,
                Tick(3840 + 4 * 480),
                TransportState::Playing
            ),
            Some(1)
        );
    }

    #[test]
    fn cycles_follow_the_prototype() {
        assert_eq!(
            [1, 2, 3, 4].map(next_ratchet),
            [2, 3, 4, 1],
            "Shift-Klick: 1 → 2 → 3 → 4"
        );
        assert_eq!([100, 75, 50, 25].map(next_probability), [75, 50, 25, 100]);
        assert_eq!(next_probability(60), 100);
        assert_eq!(swing_percent(0.12), 12);
        assert_eq!(resolution_index(StepResolution::Sixteenth), 1);
        assert_eq!(
            resolution_label(
                Lang::De,
                RESOLUTIONS[resolution_index(StepResolution::SixteenthTriplet)]
            ),
            "1/16 Triole"
        );
    }

    #[test]
    fn lane_values_map_the_height() {
        assert_eq!(lane_prop(StepLane::Velocity, 1.0), StepProp::Velocity(127));
        assert_eq!(lane_prop(StepLane::Velocity, 0.0), StepProp::Velocity(1));
        assert_eq!(lane_prop(StepLane::Velocity, 0.5), StepProp::Velocity(64));
        assert_eq!(
            lane_prop(StepLane::Probability, 0.5),
            StepProp::Probability(50)
        );
        assert_eq!(lane_prop(StepLane::Ratchet, 0.0), StepProp::Ratchet(1));
        assert_eq!(lane_prop(StepLane::Ratchet, 0.6), StepProp::Ratchet(3));
        assert_eq!(lane_prop(StepLane::Ratchet, 1.0), StepProp::Ratchet(4));
        let s = Step {
            probability: 50,
            ratchet: 2,
            ..on(127)
        };
        assert_eq!(lane_fraction(StepLane::Velocity, &s), 1.0);
        assert_eq!(lane_fraction(StepLane::Probability, &s), 0.5);
        assert_eq!(lane_fraction(StepLane::Ratchet, &s), 0.5);
    }

    #[test]
    fn step_names_read_the_state() {
        assert_eq!(
            step_name(Lang::De, 4, "Kick", &on(100)),
            "Step 5, Kick, an, Velocity 100"
        );
        assert_eq!(
            step_name(Lang::De, 0, "Snare", &Step::default()),
            "Step 1, Snare, aus"
        );
        let s = Step {
            probability: 50,
            ratchet: 3,
            ..on(52)
        };
        assert_eq!(
            step_name(Lang::De, 10, "Hat zu", &s),
            "Step 11, Hat zu, an, Velocity 52, Wahrscheinlichkeit 50 %, Ratchet 3"
        );
    }

    #[test]
    fn a_press_on_an_off_step_paints_on_along_the_row() {
        let mut row = [Step::default(); 4];
        let p = press(0, 1, &row[1], PressMods::default(), 10.0);
        assert_eq!(p.edit, Some(StepProp::On(true)));
        assert!(p.audition);
        let mut g = p.gesture.unwrap();
        row[1].on = true;
        assert_eq!(g.moved(&row, Some(1), 10.0), vec![]);
        assert_eq!(g.moved(&row, Some(2), 40.0), vec![(2, StepProp::On(true))]);
        row[2].on = true;
        assert_eq!(g.moved(&row, Some(2), 40.0), vec![], "no repeated edits");
        assert_eq!(g.released(), vec![]);
    }

    #[test]
    fn a_click_on_a_set_step_turns_it_off() {
        let row = [on(90); 4];
        let g = press(0, 2, &row[2], PressMods::default(), 10.0)
            .gesture
            .unwrap();
        assert_eq!(g.released(), vec![(2, StepProp::On(false))]);
    }

    #[test]
    fn a_vertical_drag_on_a_set_step_changes_its_velocity() {
        let row = [on(90); 4];
        let p = press(0, 2, &row[2], PressMods::default(), 100.0);
        assert_eq!(p.edit, None);
        let mut g = p.gesture.unwrap();
        // Within the threshold nothing happens.
        assert_eq!(g.moved(&row, Some(2), 98.0), vec![]);
        assert_eq!(
            g.moved(&row, Some(2), 80.0),
            vec![(2, StepProp::Velocity(110))]
        );
        assert!(matches!(g, Gesture::Velocity { .. }));
        // Clamped; moving onto another step keeps the velocity drag.
        assert_eq!(
            g.moved(&row, Some(3), 300.0),
            vec![(2, StepProp::Velocity(1))]
        );
        assert_eq!(g.released(), vec![], "the step stays on");
    }

    #[test]
    fn leaving_a_pressed_set_step_paints_off() {
        let mut row = [on(90); 4];
        let mut g = press(0, 1, &row[1], PressMods::default(), 10.0)
            .gesture
            .unwrap();
        assert_eq!(
            g.moved(&row, Some(2), 11.0),
            vec![(1, StepProp::On(false)), (2, StepProp::On(false))]
        );
        row[1].on = false;
        row[2].on = false;
        assert_eq!(g.moved(&row, Some(3), 11.0), vec![(3, StepProp::On(false))]);
        assert_eq!(g.released(), vec![]);
    }

    #[test]
    fn shift_cycles_the_ratchet_and_alt_the_probability() {
        let s = Step {
            ratchet: 4,
            ..on(90)
        };
        let shift = PressMods {
            shift: true,
            alt: false,
        };
        let p = press(0, 0, &s, shift, 0.0);
        assert_eq!((p.gesture, p.edit), (None, Some(StepProp::Ratchet(1))));
        // Shift on an off step just sets it.
        let p = press(0, 0, &Step::default(), shift, 0.0);
        assert_eq!(p.edit, Some(StepProp::On(true)));

        let alt = PressMods {
            shift: false,
            alt: true,
        };
        let g = press(0, 3, &on(90), alt, 50.0).gesture.unwrap();
        assert_eq!(g.released(), vec![(3, StepProp::Probability(75))]);
        let row = [on(90); 4];
        let mut g = press(0, 3, &row[3], alt, 50.0).gesture.unwrap();
        assert_eq!(
            g.moved(&row, Some(3), 70.0),
            vec![(3, StepProp::Probability(80))]
        );
        assert_eq!(g.released(), vec![], "a drag does not cycle");
    }
}
