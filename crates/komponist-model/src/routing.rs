//! The routing graph (wave 9, ARCHITECTURE §6.8).
//!
//! Nodes are the tracks (MIDI tracks and buses, later audio tracks) and the master, addressed by
//! [`ChannelRef`]. Edges:
//!
//! - **Output**: every track's [`Track::output`] → the master or a bus.
//! - **Send**: every [`TrackSend`](crate::TrackSend) → its target bus.
//! - **Sidechain**: a device's [`Sidechain`](crate::Sidechain) source → the track (or master) that
//!   holds the device. It carries no audio into the mix, but its source must be processed first,
//!   so it orders the graph like any other edge.
//!
//! Invariants (kept by every edit in [`crate::edits`], checked by [`check`], repaired or rejected
//! when a file loads):
//!
//! - The graph over all three edge kinds is **acyclic**. No node routes, sends or taps into itself.
//! - The master is the only sink: it has no outgoing edges and is never a sidechain source.
//! - Output and send targets are buses; a track sends at most once to the same bus.
//! - A group (a bus with [`Track::folder`]) is followed directly in display order by its members,
//!   the tracks that output to it, as one block ([`normalized_order`]). Membership is the
//!   routing: there is no separate folder tree.
//! - A bus holds no clips, no instrument, no MIDI effects, and is never armed or monitoring. Only
//!   a group can be folded.
//! - Edits give buses no sends (owner decision 11 A); the graph would allow them.
//!
//! Everything here is pure: the UI greys out targets with [`output_targets`], [`send_targets`] and
//! [`sidechain_sources`], the engine compiler (WP-102) orders its jobs with [`topo_order`] and
//! gates outputs and sends with [`gates`].

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use crate::ids::{DeviceId, SendId, TrackId};
use crate::project::{ChannelRef, DeviceRole, Project, Track, TrackOutput};

// ------------------------------------------------------------------------------------------------
// Edges
// ------------------------------------------------------------------------------------------------

/// What an [`Edge`] stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// The source's main output.
    Output,
    /// A send of the source.
    Send(SendId),
    /// The sidechain input of this device (held by the edge's `to` node).
    Sidechain(DeviceId),
}

/// One edge of the routing graph: `from` must be processed before `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    /// Source node (never the master).
    pub from: ChannelRef,
    /// Target node.
    pub to: ChannelRef,
    /// Output, send or sidechain.
    pub kind: EdgeKind,
}

/// All edges of `project` as stored, in display order of their source (outputs, then sends, then
/// the sidechains of the track's devices), the master's sidechains last. References to tracks that
/// do not exist are included; [`check`] reports them.
pub fn edges(project: &Project) -> Vec<Edge> {
    let mut edges = Vec::new();
    for track in &project.tracks {
        let from = ChannelRef::Track(track.id);
        edges.push(Edge {
            from,
            to: output_node(track.output),
            kind: EdgeKind::Output,
        });
        edges.extend(track.sends.iter().map(|s| Edge {
            from,
            to: ChannelRef::Track(s.target),
            kind: EdgeKind::Send(s.id),
        }));
    }
    let holders = project
        .tracks
        .iter()
        .map(|t| (ChannelRef::Track(t.id), &t.channel.devices))
        .chain(std::iter::once((
            ChannelRef::Master,
            &project.master.devices,
        )));
    for (holder, devices) in holders {
        edges.extend(devices.iter().filter_map(|d| {
            d.sidechain.map(|sc| Edge {
                from: ChannelRef::Track(sc.source),
                to: holder,
                kind: EdgeKind::Sidechain(d.id),
            })
        }));
    }
    edges
}

/// The node an output goes to.
pub fn output_node(output: TrackOutput) -> ChannelRef {
    match output {
        TrackOutput::Master => ChannelRef::Master,
        TrackOutput::Bus(id) => ChannelRef::Track(id),
    }
}

/// The adjacency of the routing graph: tracks by display index, the master at index `n`. Edges to
/// or from unknown tracks are left out.
struct Graph {
    ids: Vec<TrackId>,
    index: HashMap<TrackId, usize>,
    out: Vec<Vec<usize>>,
    into: Vec<Vec<usize>>,
}

impl Graph {
    fn new(project: &Project, keep: impl Fn(&EdgeKind) -> bool) -> Graph {
        let ids: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
        let mut index = HashMap::with_capacity(ids.len());
        for (i, id) in ids.iter().enumerate() {
            index.entry(*id).or_insert(i);
        }
        let n = ids.len() + 1;
        let mut graph = Graph {
            ids,
            index,
            out: vec![Vec::new(); n],
            into: vec![Vec::new(); n],
        };
        for edge in edges(project).into_iter().filter(|e| keep(&e.kind)) {
            if let (Some(a), Some(b)) = (graph.node(edge.from), graph.node(edge.to)) {
                graph.out[a].push(b);
                graph.into[b].push(a);
            }
        }
        graph
    }

    fn all(project: &Project) -> Graph {
        Graph::new(project, |_| true)
    }

    fn master(&self) -> usize {
        self.ids.len()
    }

    fn node(&self, node: ChannelRef) -> Option<usize> {
        match node {
            ChannelRef::Track(id) => self.index.get(&id).copied(),
            ChannelRef::Master => Some(self.master()),
        }
    }

    fn channel(&self, node: usize) -> ChannelRef {
        self.ids
            .get(node)
            .map_or(ChannelRef::Master, |&id| ChannelRef::Track(id))
    }

    /// Nodes reachable from `starts` (including them), forwards or backwards.
    fn reach(&self, starts: impl IntoIterator<Item = usize>, backwards: bool) -> Vec<bool> {
        let adjacency = if backwards { &self.into } else { &self.out };
        let mut seen = vec![false; adjacency.len()];
        let mut stack: Vec<usize> = starts.into_iter().collect();
        while let Some(node) = stack.pop() {
            if !std::mem::replace(&mut seen[node], true) {
                stack.extend(adjacency[node].iter().copied().filter(|&n| !seen[n]));
            }
        }
        seen
    }

    /// Kahn's algorithm; among ready nodes the lowest index (display order, master last) first.
    fn topo(&self) -> Option<Vec<usize>> {
        let n = self.out.len();
        let mut missing: Vec<usize> = self.into.iter().map(Vec::len).collect();
        let mut ready: BinaryHeap<Reverse<usize>> =
            (0..n).filter(|&i| missing[i] == 0).map(Reverse).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(Reverse(node)) = ready.pop() {
            order.push(node);
            for &next in &self.out[node] {
                missing[next] -= 1;
                if missing[next] == 0 {
                    ready.push(Reverse(next));
                }
            }
        }
        (order.len() == n).then_some(order)
    }
}

// ------------------------------------------------------------------------------------------------
// Queries
// ------------------------------------------------------------------------------------------------

/// Whether a path of routing edges (any kind) leads from `from` to `to`. A node reaches itself.
/// `false` when either node does not exist.
pub fn reaches(project: &Project, from: ChannelRef, to: ChannelRef) -> bool {
    let graph = Graph::all(project);
    match (graph.node(from), graph.node(to)) {
        (Some(a), Some(b)) => graph.reach([a], false)[b],
        _ => false,
    }
}

/// Whether a new edge `from → to` would close a cycle (including `from == to`).
pub fn would_create_cycle(project: &Project, from: ChannelRef, to: ChannelRef) -> bool {
    from == to || reaches(project, to, from)
}

/// Every output `track` can be routed to without a cycle: the master first, then the buses in
/// display order (not the track itself, not a bus that already feeds it through any path).
/// Empty when the track does not exist.
pub fn output_targets(project: &Project, track: TrackId) -> Vec<TrackOutput> {
    match acyclic_buses(project, track) {
        Some(buses) => std::iter::once(TrackOutput::Master)
            .chain(buses.into_iter().map(TrackOutput::Bus))
            .collect(),
        None => Vec::new(),
    }
}

/// Every bus `track` can add a send to: the buses of [`output_targets`] it does not send to yet.
/// Empty when the track does not exist.
pub fn send_targets(project: &Project, track: TrackId) -> Vec<TrackId> {
    let Some(buses) = acyclic_buses(project, track) else {
        return Vec::new();
    };
    let sends = &project.track(track).expect("checked").sends;
    buses
        .into_iter()
        .filter(|b| sends.iter().all(|s| s.target != *b))
        .collect()
}

/// Every track or bus that can feed the sidechain of `device` without a cycle, in display order:
/// not the track that holds it and nothing that track feeds (any path). A device on the master can
/// take every track. Empty when the device does not exist.
pub fn sidechain_sources(project: &Project, device: DeviceId) -> Vec<TrackId> {
    let Some((holder, _, _)) = project.find_device(device) else {
        return Vec::new();
    };
    let graph = Graph::all(project);
    let Some(holder) = graph.node(holder) else {
        return Vec::new();
    };
    let downstream = graph.reach([holder], false);
    graph
        .ids
        .iter()
        .enumerate()
        .filter(|&(i, _)| !downstream[i])
        .map(|(_, id)| *id)
        .collect()
}

/// The buses an edge from `track` may point at: every bus except `track` and the nodes upstream of
/// it. `None` when the track does not exist.
fn acyclic_buses(project: &Project, track: TrackId) -> Option<Vec<TrackId>> {
    let graph = Graph::all(project);
    let node = graph.node(ChannelRef::Track(track))?;
    let upstream = graph.reach([node], true);
    Some(
        project
            .tracks
            .iter()
            .enumerate()
            .filter(|&(i, t)| t.is_bus() && !upstream[i] && graph.index.get(&t.id) == Some(&i))
            .map(|(_, t)| t.id)
            .collect(),
    )
}

/// The routing graph has a cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleError;

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the routing graph has a cycle")
    }
}

impl std::error::Error for CycleError {}

/// All nodes in an order that respects every edge (sources before their targets, sidechain
/// sources before the device's holder), the master last. Deterministic: among nodes that are ready
/// at the same time, the one higher up in the track list comes first, so a project without buses
/// gives the display order. The engine compiler uses it for the job graph and the summing order.
pub fn topo_order(project: &Project) -> Result<Vec<ChannelRef>, CycleError> {
    let graph = Graph::all(project);
    let order = graph.topo().ok_or(CycleError)?;
    Ok(order.into_iter().map(|i| graph.channel(i)).collect())
}

// ------------------------------------------------------------------------------------------------
// Groups (folders)
// ------------------------------------------------------------------------------------------------

/// The group `track` is a member of: its output bus when that bus is a group.
pub fn group_of(project: &Project, track: TrackId) -> Option<TrackId> {
    match project.track(track)?.output {
        TrackOutput::Bus(b) if project.track(b).is_some_and(|t| t.is_group()) => Some(b),
        _ => None,
    }
}

/// The members of the group `bus` (the tracks that output to it), in display order. Empty when
/// `bus` is not a group.
pub fn group_members(project: &Project, bus: TrackId) -> Vec<TrackId> {
    if !project.track(bus).is_some_and(|t| t.is_group()) {
        return Vec::new();
    }
    project
        .tracks
        .iter()
        .filter(|t| t.output == TrackOutput::Bus(bus))
        .map(|t| t.id)
        .collect()
}

/// The groups `track` is in, nearest first (a group inside a group nests). Stops at a loop
/// (only in a graph with a cycle).
pub fn group_ancestors(project: &Project, track: TrackId) -> Vec<TrackId> {
    let mut ancestors: Vec<TrackId> = Vec::new();
    let mut next = group_of(project, track);
    while let Some(id) = next {
        if id == track || ancestors.contains(&id) {
            break;
        }
        ancestors.push(id);
        next = group_of(project, id);
    }
    ancestors
}

/// The display order with every group's members directly behind it, as indices into `tracks`.
///
/// A stable tree flattening: top-level tracks (those that do not output to a group) keep their
/// relative order; each group is followed by its members in their relative order, recursively.
/// Without groups this is the identity. Tracks caught in an output loop (only in a graph with a
/// cycle) come last in their relative order.
pub fn normalized_order(tracks: &[Arc<Track>]) -> Vec<usize> {
    let mut index = HashMap::with_capacity(tracks.len());
    for (i, t) in tracks.iter().enumerate() {
        index.entry(t.id).or_insert(i);
    }
    let parent = |i: usize| -> Option<usize> {
        let TrackOutput::Bus(bus) = tracks[i].output else {
            return None;
        };
        let p = *index.get(&bus)?;
        (p != i && tracks[p].is_group()).then_some(p)
    };
    let mut children = vec![Vec::new(); tracks.len()];
    let mut roots = Vec::new();
    for i in 0..tracks.len() {
        match parent(i) {
            Some(p) => children[p].push(i),
            None => roots.push(i),
        }
    }
    let mut order = Vec::with_capacity(tracks.len());
    let mut seen = vec![false; tracks.len()];
    for root in roots.into_iter().chain(0..tracks.len()) {
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            order.push(i);
            stack.extend(children[i].iter().rev().copied().filter(|&c| !seen[c]));
        }
    }
    order
}

/// Puts `project.tracks` into [`normalized_order`]; leaves the list untouched when it already is.
pub(crate) fn normalize_groups(project: &mut Project) {
    let order = normalized_order(&project.tracks);
    if order.iter().enumerate().any(|(i, &j)| i != j) {
        let tracks = std::mem::take(&mut project.tracks);
        project.tracks = order.into_iter().map(|i| Arc::clone(&tracks[i])).collect();
    }
}

// ------------------------------------------------------------------------------------------------
// Check
// ------------------------------------------------------------------------------------------------

/// A broken routing invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingIssue {
    /// An output, send or sidechain names a track that does not exist.
    MissingTrack {
        /// The track (or the track holding the device) with the reference.
        track: Option<TrackId>,
        /// The missing id.
        missing: TrackId,
    },
    /// An output or send names a track that is not a bus.
    NotABus {
        /// The track with the reference.
        track: TrackId,
        /// The referenced track.
        target: TrackId,
    },
    /// A track outputs or sends into itself, or a device takes its own track's sidechain.
    SelfReference(TrackId),
    /// A track sends to the same bus twice.
    DuplicateSend {
        /// The sending track.
        track: TrackId,
        /// The bus.
        target: TrackId,
    },
    /// The graph of outputs, sends and sidechains has a cycle.
    Cycle,
    /// A group's members do not follow it as one block.
    GroupOrder,
    /// A bus holds clips, an instrument or a MIDI effect, or is armed or monitoring.
    BusContent(TrackId),
    /// A track that is not a bus is a folder, or one that is not a group is folded.
    NotAGroup(TrackId),
}

impl fmt::Display for RoutingIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoutingIssue::MissingTrack { track, missing } => match track {
                Some(t) => write!(f, "track {} refers to missing track {}", t.0, missing.0),
                None => write!(f, "the master refers to missing track {}", missing.0),
            },
            RoutingIssue::NotABus { track, target } => {
                write!(
                    f,
                    "track {} routes to track {}, not a bus",
                    track.0, target.0
                )
            }
            RoutingIssue::SelfReference(t) => write!(f, "track {} routes to itself", t.0),
            RoutingIssue::DuplicateSend { track, target } => {
                write!(f, "track {} sends to bus {} twice", track.0, target.0)
            }
            RoutingIssue::Cycle => write!(f, "the routing graph has a cycle"),
            RoutingIssue::GroupOrder => write!(f, "group members do not follow their group"),
            RoutingIssue::BusContent(t) => {
                write!(f, "bus {} holds clips, an instrument or a MIDI effect", t.0)
            }
            RoutingIssue::NotAGroup(t) => write!(f, "track {} is folded but not a group", t.0),
        }
    }
}

impl std::error::Error for RoutingIssue {}

/// Checks every routing invariant of the module docs; the first issue found.
pub fn check(project: &Project) -> Result<(), RoutingIssue> {
    let kinds: HashMap<TrackId, bool> = project.tracks.iter().map(|t| (t.id, t.is_bus())).collect();
    let bus_ref = |track: TrackId, target: TrackId| -> Result<(), RoutingIssue> {
        match kinds.get(&target) {
            None => Err(RoutingIssue::MissingTrack {
                track: Some(track),
                missing: target,
            }),
            Some(_) if target == track => Err(RoutingIssue::SelfReference(track)),
            Some(false) => Err(RoutingIssue::NotABus { track, target }),
            Some(true) => Ok(()),
        }
    };
    for track in &project.tracks {
        if let TrackOutput::Bus(target) = track.output {
            bus_ref(track.id, target)?;
        }
        let mut targets = HashSet::new();
        for send in &track.sends {
            bus_ref(track.id, send.target)?;
            if !targets.insert(send.target) {
                return Err(RoutingIssue::DuplicateSend {
                    track: track.id,
                    target: send.target,
                });
            }
        }
        if (track.folder && !track.is_bus()) || (track.folded && !track.folder) {
            return Err(RoutingIssue::NotAGroup(track.id));
        }
        if track.is_bus()
            && (!track.clips.is_empty()
                || track.arm
                || track.monitor
                || track
                    .channel
                    .devices
                    .iter()
                    .any(|d| d.role != DeviceRole::Effect))
        {
            return Err(RoutingIssue::BusContent(track.id));
        }
        for device in &track.channel.devices {
            if let Some(sc) = device.sidechain {
                if sc.source == track.id {
                    return Err(RoutingIssue::SelfReference(track.id));
                }
                if !kinds.contains_key(&sc.source) {
                    return Err(RoutingIssue::MissingTrack {
                        track: Some(track.id),
                        missing: sc.source,
                    });
                }
            }
        }
    }
    for device in &project.master.devices {
        if let Some(sc) = device.sidechain
            && !kinds.contains_key(&sc.source)
        {
            return Err(RoutingIssue::MissingTrack {
                track: None,
                missing: sc.source,
            });
        }
    }
    if Graph::all(project).topo().is_none() {
        return Err(RoutingIssue::Cycle);
    }
    let order = normalized_order(&project.tracks);
    if order.iter().enumerate().any(|(i, &j)| i != j) {
        return Err(RoutingIssue::GroupOrder);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Mute and solo
// ------------------------------------------------------------------------------------------------

/// Mute and solo state of one output or send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gate {
    /// The source is muted ("M").
    pub muted: bool,
    /// Solo is on somewhere and this edge does not lead from or toward a soloed node ("stumm
    /// durch Solo").
    pub solo_muted: bool,
}

impl Gate {
    /// Whether signal passes.
    pub fn open(self) -> bool {
        !self.muted && !self.solo_muted
    }
}

/// Which outputs and sends carry signal under the current mute and solo states (ARCHITECTURE
/// §6.8.6). Sidechain taps are not gates: a pre tap ignores the source's fader, mute and solo, a
/// post tap follows its fader and mute (the engine's fader stage) but never solo (§6.8.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gates {
    outputs: HashMap<TrackId, Gate>,
    sends: HashMap<SendId, (Gate, bool)>,
}

impl Gates {
    /// Gate of `track`'s main output; open for unknown tracks.
    pub fn output(&self, track: TrackId) -> Gate {
        self.outputs.get(&track).copied().unwrap_or_default()
    }

    /// Gate of a send; open for unknown sends.
    pub fn send(&self, send: SendId) -> Gate {
        self.sends.get(&send).map(|&(g, _)| g).unwrap_or_default()
    }

    /// Whether `track`'s main output carries signal.
    pub fn output_open(&self, track: TrackId) -> bool {
        self.output(track).open()
    }

    /// Whether a send carries signal: enabled, source not muted, not solo-muted.
    pub fn send_open(&self, send: SendId) -> bool {
        self.sends
            .get(&send)
            .is_none_or(|&(gate, enabled)| enabled && gate.open())
    }
}

/// The gates of every output and send.
///
/// Rules, with `S` = the soloed tracks and buses (solo on the master is ignored):
/// - Mute closes the track's output and all its sends (pre and post fader).
/// - Without solo nothing else closes.
/// - With solo: `S*` = `S` plus everything that reaches a soloed node through **outputs** only
///   (soloing a group or bus solos what sums into it). An output or send `u → v` stays open when
///   `u` lies on or behind `S*` (the soloed track's bus path, its returns, the master), or when `v`
///   leads into `S*` (sends into a soloed return keep feeding it). Everything else is
///   solo-muted. Without buses this is the old rule `!mute && (!any_solo || solo)`.
pub fn gates(project: &Project) -> Gates {
    let graph = Graph::new(project, |k| !matches!(k, EdgeKind::Sidechain(_)));
    let soloed: Vec<usize> = project
        .tracks
        .iter()
        .enumerate()
        .filter(|(i, t)| t.channel.solo && graph.index.get(&t.id) == Some(i))
        .map(|(i, _)| i)
        .collect();
    let solo_open: Box<dyn Fn(usize, ChannelRef) -> bool> = if soloed.is_empty() {
        Box::new(|_, _| true)
    } else {
        let outputs_only = Graph::new(project, |k| *k == EdgeKind::Output);
        let star = outputs_only.reach(soloed.iter().copied(), true);
        let starts = || (0..star.len()).filter(|&i| star[i]);
        let down = graph.reach(starts(), false);
        let up = graph.reach(starts(), true);
        let index = graph.index.clone();
        let master = graph.master();
        Box::new(move |from: usize, to: ChannelRef| {
            let to = match to {
                ChannelRef::Track(id) => index.get(&id).copied(),
                ChannelRef::Master => Some(master),
            };
            down[from] || to.is_some_and(|t| up[t])
        })
    };
    let mut gates = Gates::default();
    for (i, track) in project.tracks.iter().enumerate() {
        if graph.index.get(&track.id) != Some(&i) {
            continue;
        }
        let muted = track.channel.mute;
        let gate = |to| Gate {
            muted,
            solo_muted: !solo_open(i, to),
        };
        gates
            .outputs
            .insert(track.id, gate(output_node(track.output)));
        for send in &track.sends {
            gates.sends.insert(
                send.id,
                (gate(ChannelRef::Track(send.target)), send.enabled),
            );
        }
    }
    gates
}
