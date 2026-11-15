//! The 32 fixed algorithms (DX7 style) as templates over pisynth's free routing.
//!
//! pisynth routes freely: every operator has `modulates` targets with `depths`, an `output` flag
//! and its own feedback. A fixed algorithm is a template written into that routing; a patch "has"
//! algorithm *n* when its routing matches template *n*, otherwise its routing is free.
//!
//! Ported from pisynth's display UI (`../pisynth/crates/synth-ui/src/algo.rs`, the HEXA-6 design's
//! `ALGOS`): same table, same matching and the same way of applying a template, so a patch shows
//! the same algorithm number here as on the synth. komponist cannot depend on `synth-ui` (it links
//! LVGL), hence the copy.

use hexa_core::{OPERATORS, Patch};

/// A fixed algorithm: per operator the operators it modulates (bit mask, bit `t` = operator
/// index `t`), and the index of the operator with feedback. Carriers modulate nothing.
#[derive(Clone, Copy, Debug)]
pub struct Algorithm {
    /// Per operator index: bit mask of the operator indices it modulates.
    pub modulates: [u8; OPERATORS],
    /// Index of the operator carrying the feedback.
    pub feedback: usize,
}

impl Algorithm {
    /// Operator `op` is heard (it modulates nothing).
    pub fn is_carrier(&self, op: usize) -> bool {
        self.modulates[op] == 0
    }
}

/// The 32 algorithms; index 0 is algorithm 1.
#[rustfmt::skip]
pub const ALGORITHMS: [Algorithm; 32] = [
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b001000, 0b010000], feedback: 5 }, //  1
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b001000, 0b010000], feedback: 1 }, //  2
    Algorithm { modulates: [0b000000, 0b000001, 0b000010, 0b000000, 0b001000, 0b010000], feedback: 5 }, //  3
    Algorithm { modulates: [0b000000, 0b000001, 0b000010, 0b000000, 0b001000, 0b010000], feedback: 3 }, //  4
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b000000, 0b010000], feedback: 5 }, //  5
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b000000, 0b010000], feedback: 4 }, //  6
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b000100, 0b010000], feedback: 5 }, //  7
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b000100, 0b010000], feedback: 3 }, //  8
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b000100, 0b010000], feedback: 1 }, //  9
    Algorithm { modulates: [0b000000, 0b000001, 0b000010, 0b000000, 0b001000, 0b001000], feedback: 2 }, // 10
    Algorithm { modulates: [0b000000, 0b000001, 0b000010, 0b000000, 0b001000, 0b001000], feedback: 5 }, // 11
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b000100, 0b000100], feedback: 1 }, // 12
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b000100, 0b000100], feedback: 5 }, // 13
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b001000, 0b001000], feedback: 5 }, // 14
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b001000, 0b001000], feedback: 1 }, // 15
    Algorithm { modulates: [0b000000, 0b000001, 0b000001, 0b000100, 0b000001, 0b010000], feedback: 5 }, // 16
    Algorithm { modulates: [0b000000, 0b000001, 0b000001, 0b000100, 0b000001, 0b010000], feedback: 1 }, // 17
    Algorithm { modulates: [0b000000, 0b000001, 0b000001, 0b000001, 0b001000, 0b010000], feedback: 2 }, // 18
    Algorithm { modulates: [0b000000, 0b000001, 0b000010, 0b000000, 0b000000, 0b011000], feedback: 5 }, // 19
    Algorithm { modulates: [0b000000, 0b000000, 0b000011, 0b000000, 0b001000, 0b001000], feedback: 2 }, // 20
    Algorithm { modulates: [0b000000, 0b000000, 0b000011, 0b000000, 0b000000, 0b011000], feedback: 2 }, // 21
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000000, 0b000000, 0b011100], feedback: 5 }, // 22
    Algorithm { modulates: [0b000000, 0b000000, 0b000010, 0b000000, 0b000000, 0b011000], feedback: 5 }, // 23
    Algorithm { modulates: [0b000000, 0b000000, 0b000000, 0b000000, 0b000000, 0b011100], feedback: 5 }, // 24
    Algorithm { modulates: [0b000000, 0b000000, 0b000000, 0b000000, 0b000000, 0b011000], feedback: 5 }, // 25
    Algorithm { modulates: [0b000000, 0b000000, 0b000010, 0b000000, 0b001000, 0b001000], feedback: 5 }, // 26
    Algorithm { modulates: [0b000000, 0b000000, 0b000010, 0b000000, 0b001000, 0b001000], feedback: 2 }, // 27
    Algorithm { modulates: [0b000000, 0b000001, 0b000000, 0b000100, 0b001000, 0b000000], feedback: 4 }, // 28
    Algorithm { modulates: [0b000000, 0b000000, 0b000000, 0b000100, 0b000000, 0b010000], feedback: 5 }, // 29
    Algorithm { modulates: [0b000000, 0b000000, 0b000000, 0b000100, 0b001000, 0b000000], feedback: 4 }, // 30
    Algorithm { modulates: [0b000000, 0b000000, 0b000000, 0b000000, 0b000000, 0b010000], feedback: 5 }, // 31
    Algorithm { modulates: [0b000000, 0b000000, 0b000000, 0b000000, 0b000000, 0b000000], feedback: 5 }, // 32
];

/// The operators `op` modulates in `patch`, as a bit mask (itself excluded).
fn targets(patch: &Patch, op: usize) -> u8 {
    let p = &patch.operators[op];
    (0..OPERATORS)
        .filter(|&t| t != op && p.modulates[t])
        .fold(0, |m, t| m | 1 << t)
}

/// The routing `patch` really has, fixed algorithm or not: what the FM display draws for free
/// routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Routing {
    /// Per operator index: bit mask of the operator indices it modulates.
    pub modulates: [u8; OPERATORS],
    /// Bit mask of the operators that are heard (`output` flag): carriers, including operators that
    /// also modulate others.
    pub outputs: u8,
    /// The operator with the most feedback (the lowest number on a tie); `None` without feedback.
    pub feedback: Option<usize>,
}

/// The routing of `patch` as it is (the `modulates` masks and the feedback operator).
pub fn routing(patch: &Patch) -> Routing {
    let mut feedback = None;
    let mut most = 0.0_f32;
    for (op, p) in patch.operators.iter().enumerate() {
        if p.feedback > most {
            most = p.feedback;
            feedback = Some(op);
        }
    }
    Routing {
        modulates: std::array::from_fn(|op| targets(patch, op)),
        outputs: (0..OPERATORS)
            .filter(|&op| patch.operators[op].output)
            .fold(0, |mask, op| mask | (1 << op)),
        feedback,
    }
}

/// The fixed algorithm (0-based index into [`ALGORITHMS`]) whose routing `patch` has, if any.
/// Among algorithms differing only in the feedback operator, the one whose feedback operator has
/// feedback wins.
pub fn detect(patch: &Patch) -> Option<usize> {
    let mut first = None;
    for (i, algorithm) in ALGORITHMS.iter().enumerate() {
        let matches = (0..OPERATORS).all(|op| {
            targets(patch, op) == algorithm.modulates[op]
                && patch.operators[op].output == algorithm.is_carrier(op)
        });
        if !matches {
            continue;
        }
        if patch.operators[algorithm.feedback].feedback > 0.0 {
            return Some(i);
        }
        first.get_or_insert(i);
    }
    first
}

/// Writes fixed algorithm `index` (0-based) into `patch`: its modulation routes (an existing route
/// keeps its depth, a new one gets full depth), carriers heard, the others not, and the strongest
/// feedback of any operator moved to the algorithm's feedback operator (all others get none).
/// Does nothing for an index out of range.
pub fn apply(patch: &mut Patch, index: usize) {
    let Some(algorithm) = ALGORITHMS.get(index) else {
        return;
    };
    let feedback = patch
        .operators
        .iter()
        .map(|o| o.feedback)
        .fold(0.0, f32::max);
    for (op, params) in patch.operators.iter_mut().enumerate() {
        for target in 0..OPERATORS {
            let on = target != op && algorithm.modulates[op] & (1 << target) != 0;
            if on && !params.modulates[target] {
                params.depths[target] = 1.0;
            }
            params.modulates[target] = on;
        }
        params.output = algorithm.is_carrier(op);
        params.feedback = if op == algorithm.feedback {
            feedback
        } else {
            0.0
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_routing_is_the_patch_as_it_is() {
        let mut patch = Patch::default();
        apply(&mut patch, 4);
        assert_eq!(routing(&patch).modulates, ALGORITHMS[4].modulates);
        // A route no algorithm has: operator 1 modulates operator 6.
        patch.operators[0].modulates[5] = true;
        patch.operators[2].feedback = 0.3;
        patch.operators[4].feedback = 0.8;
        assert_eq!(detect(&patch), None);
        let r = routing(&patch);
        assert_eq!(r.modulates[0], 0b100000);
        assert_eq!(r.feedback, Some(4));
        patch.operators[4].feedback = 0.0;
        patch.operators[2].feedback = 0.0;
        assert_eq!(routing(&patch).feedback, None);
    }

    #[test]
    fn applied_algorithms_are_detected() {
        for (i, algorithm) in ALGORITHMS.iter().enumerate() {
            let mut patch = Patch::default();
            patch.operators[3].feedback = 0.5;
            apply(&mut patch, i);
            assert_eq!(detect(&patch), Some(i), "algorithm {}", i + 1);
            assert_eq!(patch.operators[algorithm.feedback].feedback, 0.5);
            let others = (0..OPERATORS).filter(|&op| op != algorithm.feedback);
            assert!(
                others
                    .into_iter()
                    .all(|op| patch.operators[op].feedback == 0.0)
            );
        }
    }

    #[test]
    fn the_init_patch_is_free() {
        // Operator 2 → 1, the others unused: no template has unused operators.
        assert_eq!(detect(&Patch::default()), None);
    }

    #[test]
    fn existing_routes_keep_their_depth() {
        let mut patch = Patch::default();
        patch.operators[1].depths[0] = 0.25;
        patch.operators[3].depths[2] = 0.5;
        apply(&mut patch, 0); // 2 → 1, 4 → 3, 5 → 4, 6 → 5
        assert_eq!(patch.operators[1].depths[0], 0.25);
        // 4 → 3 is new: full depth.
        assert_eq!(patch.operators[3].depths[2], 1.0);
        apply(&mut patch, 99);
        assert_eq!(detect(&patch), Some(0));
    }
}
