//! The compiled routing graph of a program (ARCHITECTURE §6.8.4–6.8.6).
//!
//! Built by [`crate::compile::build`] on the UI thread from the project's outputs, sends and
//! sidechains; read (never written) by the engine. Nodes are the program's tracks, addressed by
//! their index in [`Program::tracks`](crate::Program::tracks) (buses are tracks), and the master.
//!
//! - **Jobs:** one per track, run in an order that respects every edge ([`Routing::order`],
//!   `komponist_model::routing::topo_order`). A bus job first sums its [`RouteInput`]s, in a fixed
//!   order, then runs its chain and strip like a MIDI track. The master sums its inputs on the
//!   audio thread after all jobs.
//! - **Taps:** a node keeps its pre-fader signal ([`NodeRoute::pre`]) and its post-fader signal
//!   ([`NodeRoute::post`], fader, pan and mute) only when a send or a sidechain reads them. Its main
//!   output (also gated by solo) is delayed by its `TrackProgram::pdc_delay` for its target.
//! - **Delay compensation:** every send, every sidechain and the head of a MIDI track whose
//!   sidechain arrives late has its own [`Delay`]: a line in the engine's edge table, assigned by
//!   the compiler per [`LineKey`] and kept across recompiles ([`LineMap`]), so a recompile neither
//!   allocates on the audio thread nor clicks.
//! - **Gates:** which outputs and sends carry signal under mute and solo ([`GateTable`]), from
//!   `komponist_model::routing::gates`; the UI sends a new table on the fast path
//!   (`Command::SetGates`).
//!
//! A program without buses, sends and sidechains has the **flat** routing ([`Routing::default`]):
//! every track outputs to the master, the jobs are independent, and mute and solo follow the old
//! rule `!mute && (!any_solo || solo)` with the per-track fast paths. Programs built by hand (tests)
//! are flat unless they set a routing.

use std::ops::Range;
use std::sync::Arc;

use komponist_model::{DeviceId, SendId, Tap, TrackId};

/// Most sends a program carries (the bits of [`GateTable`]); the compiler drops further ones.
pub const MAX_SENDS: usize = 256;

/// Size of the engine's table of edge states (delay lines and send gains): sends, sidechains and
/// head delays of one program together. The compiler drops edges beyond it.
pub const MAX_LINES: usize = 512;

/// A range of a [`Routing`] list (`start .. start + len`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    /// First element.
    pub start: u32,
    /// Number of elements.
    pub len: u32,
}

impl Span {
    /// The range in the list.
    pub fn range(self) -> Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }

    /// Whether the span is empty.
    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Index of an edge state in the engine's table, `0 .. MAX_LINES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(pub u16);

/// What an edge state belongs to: the key under which the compiler keeps its [`LineId`] from one
/// program to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LineKey {
    /// A send.
    Send(SendId),
    /// The sidechain input of a device.
    Sidechain(DeviceId),
    /// The input delay of a MIDI track whose sidechain arrives late (§6.8.5).
    Head(TrackId),
    /// Post-fader bus input of an audio track.
    AudioInput(TrackId),
}

/// A delay with its own state in the engine's edge table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Delay {
    /// The edge state.
    pub line: LineId,
    /// Frames, ≤ [`crate::MAX_PDC_FRAMES`].
    pub frames: u32,
}

/// Which line each edge uses. Owned by a program's routing; built by the compiler from the
/// previous program's map, so an edge keeps its line (and the line its delayed audio and gain
/// ramp) as long as the edge exists. UI thread; the engine reads [`LineMap::entries`] when it
/// installs a program and resets every line whose key changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineMap {
    /// Pairs sorted by key.
    pub(crate) entries: Vec<(LineKey, LineId)>,
}

impl LineMap {
    /// The line of `key` in this map.
    pub fn line(&self, key: LineKey) -> Option<LineId> {
        self.entries
            .binary_search_by_key(&key, |(k, _)| *k)
            .ok()
            .map(|i| self.entries[i].1)
    }

    /// All `(key, line)` pairs, sorted by key.
    pub fn entries(&self) -> &[(LineKey, LineId)] {
        &self.entries
    }
}

/// Routing of one track (program track index = index in [`Routing::nodes`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NodeRoute {
    /// A bus: the job sums [`NodeRoute::inputs`] before its chain (MIDI tracks start from their
    /// instrument).
    pub bus: bool,
    /// Live post-fader bus input; not an output/send sum.
    pub audio_input: Option<AudioInputRoute>,
    /// Where the main output goes: `None` = the master, `Some(i)` = the bus at track index `i`.
    pub output: Option<u16>,
    /// The inputs of a bus in [`Routing::inputs`], in summing order.
    pub inputs: Span,
    /// The sidechain inputs of the node's devices in [`Routing::sidechains`], by chain position.
    pub sidechains: Span,
    /// The jobs that read this one (each once) in [`Routing::dependents`].
    pub dependents: Span,
    /// Number of distinct jobs this one reads (its inputs and sidechain sources): the job may run
    /// when all of them are done.
    pub waits: u16,
    /// A send or sidechain reads the pre-fader signal.
    pub pre: bool,
    /// A send or sidechain reads the post-fader signal (fader, pan, mute).
    pub post: bool,
    /// MIDI tracks: delay behind the instrument, so a late sidechain source lines up (§6.8.5).
    pub head: Option<Delay>,
    /// Latency at which the node's inputs line up, `in(v)` of §6.8.5 (diagnostics, tests).
    pub latency_in: u32,
    /// `out(v)`: `latency_in` plus the latency of the node's chain.
    pub latency_out: u32,
}

/// A bus input read after its source job has completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInputRoute {
    /// Source program track index.
    pub from: u16,
    /// Align the bus tap with other inputs before the audio track's effects.
    pub delay: Delay,
}

/// A send into a bus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SendRoute {
    /// Index of the send in [`GateTable`] and [`Routing::sends`].
    pub gate: u16,
    /// Send level, linear.
    pub gain: f32,
    /// Where the source is tapped.
    pub tap: Tap,
    /// Delay compensation of the send, with its state (also its gain ramp).
    pub delay: Delay,
}

/// One input of a bus or of the master.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteInput {
    /// The source (program track index).
    pub from: u16,
    /// `None`: the source's main output (delayed and gated by the source). `Some`: a send.
    pub send: Option<SendRoute>,
}

/// The sidechain input of a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SidechainRoute {
    /// Position of the device in the holder's chain.
    pub device: u16,
    /// The source (program track index).
    pub from: u16,
    /// Where the source is tapped.
    pub tap: Tap,
    /// Delay that lines the tap up with the device's main input.
    pub delay: Delay,
}

/// The master as a node: what sums into it, what its devices' sidechains read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MasterRoute {
    /// Its inputs (main outputs only) in [`Routing::inputs`], in summing order.
    pub inputs: Span,
    /// Sidechain inputs of the master's devices in [`Routing::sidechains`].
    pub sidechains: Span,
    /// `in(master)`: the latency at which everything reaching the master lines up.
    pub latency_in: u32,
}

/// The compiled routing graph of a program; see the module docs. [`Routing::default`] is flat.
#[derive(Debug, Clone, Default)]
pub struct Routing {
    /// Program track indices in job order (sources before their targets). Empty: flat (every
    /// track outputs to the master, in display order).
    pub order: Box<[u16]>,
    /// Per program track; empty when flat.
    pub nodes: Box<[NodeRoute]>,
    /// Inputs of buses and of the master, grouped per node ([`NodeRoute::inputs`],
    /// [`MasterRoute::inputs`]).
    pub inputs: Box<[RouteInput]>,
    /// Sidechain inputs, grouped per holder ([`NodeRoute::sidechains`],
    /// [`MasterRoute::sidechains`]).
    pub sidechains: Box<[SidechainRoute]>,
    /// Downstream jobs, grouped per node ([`NodeRoute::dependents`]).
    pub dependents: Box<[u16]>,
    /// The master.
    pub master: MasterRoute,
    /// The send of each gate index. UI thread (fast-path gate tables).
    pub sends: Box<[SendId]>,
    /// Mute and solo as compiled; `None` when flat (the old rule).
    pub gates: Option<GateTable>,
    /// Line assignment of the edges. Read by the engine only when the program is installed.
    pub lines: Arc<LineMap>,
    /// Largest number of frames of requested compensation missing at one delay line.
    pub pdc_shortfall: u32,
}

impl Routing {
    /// Whether this is the flat routing (no buses, sends or sidechains).
    pub fn is_flat(&self) -> bool {
        self.order.is_empty()
    }

    /// The route of track `index`; the flat default (output to the master) when flat.
    pub fn node(&self, index: usize) -> NodeRoute {
        self.nodes.get(index).copied().unwrap_or_default()
    }

    /// The inputs in `span`.
    pub fn inputs(&self, span: Span) -> &[RouteInput] {
        self.inputs.get(span.range()).unwrap_or(&[])
    }

    /// The sidechains in `span`.
    pub fn sidechains(&self, span: Span) -> &[SidechainRoute] {
        self.sidechains.get(span.range()).unwrap_or(&[])
    }

    /// The dependents in `span`.
    pub fn dependents(&self, span: Span) -> &[u16] {
        self.dependents.get(span.range()).unwrap_or(&[])
    }

    /// The project latency `out(master)` without the master chain: the latency at which every
    /// path reaches the master.
    pub fn latency(&self) -> u32 {
        self.master.latency_in
    }
}

const TRACK_WORDS: usize = crate::MAX_TRACKS / 64;
const SEND_WORDS: usize = MAX_SENDS / 64;

/// Mute and solo of every output and send of a program, one bit each (ARCHITECTURE §6.8.6).
///
/// Built on the UI thread from `komponist_model::routing::gates` ([`crate::compile::gates`]):
/// tracks by program track index, sends by gate index ([`SendRoute::gate`]). Fixed size, so it
/// travels in `Command::SetGates` without allocation. All bits clear: everything closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GateTable {
    muted: [u64; TRACK_WORDS],
    output: [u64; TRACK_WORDS],
    sends: [u64; SEND_WORDS],
}

fn get(words: &[u64], index: usize) -> bool {
    words
        .get(index / 64)
        .is_some_and(|w| w & (1 << (index % 64)) != 0)
}

fn set(words: &mut [u64], index: usize, on: bool) {
    if let Some(w) = words.get_mut(index / 64) {
        if on {
            *w |= 1 << (index % 64);
        } else {
            *w &= !(1 << (index % 64));
        }
    }
}

impl GateTable {
    /// Whether track `track` is muted (its post-fader tap is silent). `false` beyond the table.
    pub fn muted(&self, track: usize) -> bool {
        get(&self.muted, track)
    }

    /// Whether track `track`'s main output carries signal (not muted, not solo-muted).
    pub fn output_open(&self, track: usize) -> bool {
        get(&self.output, track)
    }

    /// Whether send `gate` carries signal (enabled, source not muted, not solo-muted).
    pub fn send_open(&self, gate: usize) -> bool {
        get(&self.sends, gate)
    }

    /// Sets the mute of `track` (ignored beyond the table).
    pub fn set_muted(&mut self, track: usize, muted: bool) {
        set(&mut self.muted, track, muted);
    }

    /// Opens or closes `track`'s main output.
    pub fn set_output_open(&mut self, track: usize, open: bool) {
        set(&mut self.output, track, open);
    }

    /// Opens or closes send `gate`.
    pub fn set_send_open(&mut self, gate: usize, open: bool) {
        set(&mut self.sends, gate, open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_bits_are_independent() {
        let mut g = GateTable::default();
        assert!(!g.output_open(0) && !g.muted(127) && !g.send_open(255));
        g.set_output_open(127, true);
        g.set_muted(64, true);
        g.set_send_open(255, true);
        g.set_send_open(300, true);
        assert!(g.output_open(127) && !g.output_open(126));
        assert!(g.muted(64) && !g.muted(63));
        assert!(g.send_open(255) && !g.send_open(254) && !g.send_open(300));
        g.set_output_open(127, false);
        assert!(!g.output_open(127));
    }

    #[test]
    fn flat_routing_routes_everything_to_the_master() {
        let r = Routing::default();
        assert!(r.is_flat());
        assert_eq!(r.node(5), NodeRoute::default());
        assert_eq!(r.node(5).output, None);
        assert!(r.inputs(Span { start: 3, len: 2 }).is_empty());
    }
}
