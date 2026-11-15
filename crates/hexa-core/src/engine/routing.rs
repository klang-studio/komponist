//! A patch's modulation routing as bit masks.

use crate::Patch;
use crate::operator::{ModulationMode, OPERATORS};

/// The routing of a patch as bit masks (bit `i` = operator `i`).
#[derive(Clone, Copy)]
pub(super) struct Routing {
    /// Operators that are heard.
    pub(super) outputs: u8,
    /// Per operator: the other operators modulating it, by their mode.
    pub(super) phase: [u8; OPERATORS],
    pub(super) frequency: [u8; OPERATORS],
    pub(super) pulse_width: [u8; OPERATORS],
    pub(super) sync: [u8; OPERATORS],
    pub(super) ring: [u8; OPERATORS],
    /// Operators that are heard or modulate another operator; the rest is skipped.
    pub(super) active: u8,
}

impl Routing {
    pub(super) fn new(patch: &Patch) -> Self {
        let mut routing = Routing {
            outputs: 0,
            phase: [0; OPERATORS],
            frequency: [0; OPERATORS],
            pulse_width: [0; OPERATORS],
            sync: [0; OPERATORS],
            ring: [0; OPERATORS],
            active: 0,
        };
        for (i, op) in patch.operators.iter().enumerate() {
            if op.output {
                routing.outputs |= 1 << i;
                routing.active |= 1 << i;
            }
            let sources = match op.mode {
                ModulationMode::Phase => &mut routing.phase,
                ModulationMode::Frequency => &mut routing.frequency,
                ModulationMode::PulseWidth => &mut routing.pulse_width,
                ModulationMode::Sync => &mut routing.sync,
                ModulationMode::Ring => &mut routing.ring,
            };
            for (target, &modulates) in op.modulates.iter().enumerate() {
                if modulates && target != i {
                    sources[target] |= 1 << i;
                    routing.active |= 1 << i;
                }
            }
        }
        routing
    }
}

/// Bits of `mask` as indices, lowest first.
pub(super) fn bits(mut mask: u8) -> impl Iterator<Item = usize> {
    core::iter::from_fn(move || {
        (mask != 0).then(|| {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            i
        })
    })
}
