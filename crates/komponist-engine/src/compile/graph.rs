//! The routing part of the compiler (ARCHITECTURE §6.8.4–6.8.6): outputs, sends and sidechains
//! of the compiled tracks → [`Routing`] (job order, bus inputs, taps, delays, gates) and the
//! delay of every track's main output.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use komponist_model::routing::{gates as model_gates, topo_order};
use komponist_model::{
    ChannelRef, DeviceId, DeviceRole, Project, SendId, Sidechain, Tap, Track, TrackId, TrackOutput,
};

use super::{CompileEnv, chain_latency, fader_gain};
use crate::MAX_PDC_FRAMES;
use crate::program::ChainSlot;
use crate::route::{
    Delay, GateTable, LineId, LineKey, LineMap, MAX_LINES, MAX_SENDS, MasterRoute, NodeRoute,
    RouteInput, Routing, SendRoute, SidechainRoute, Span,
};

/// The routing of a program and the delay of every track's main output (`pdc_delay`).
pub(super) struct Compiled {
    pub(super) routing: Arc<Routing>,
    pub(super) delays: Vec<u32>,
}

/// A send that made it into the program.
struct Send {
    id: SendId,
    from: usize,
    to: usize,
    gate: u16,
    gain: f32,
    tap: Tap,
    line: LineId,
}

/// A sidechain that made it into the program: device `device` of `holder` (`None`: the master).
struct Side {
    holder: Option<usize>,
    device: usize,
    from: usize,
    tap: Tap,
    line: LineId,
}

/// Hands out edge-state lines: an edge keeps the line it had in the previous program; new edges
/// take the lowest line that is free.
struct Lines<'a> {
    previous: Option<&'a LineMap>,
    used: Vec<bool>,
    entries: Vec<(LineKey, LineId)>,
}

impl Lines<'_> {
    fn new(previous: Option<&LineMap>) -> Lines<'_> {
        Lines {
            previous,
            used: vec![false; MAX_LINES],
            entries: Vec::new(),
        }
    }

    /// Keeps the previous line of `key` for it, so that new edges do not take it.
    fn reserve(&mut self, key: LineKey) {
        if let Some(line) = self.previous.and_then(|p| p.line(key))
            && let Some(used) = self.used.get_mut(usize::from(line.0))
        {
            *used = true;
        }
    }

    /// The line of `key`: its previous one, else the lowest free one. `None` when the table is
    /// full.
    fn assign(&mut self, key: LineKey) -> Option<LineId> {
        let kept = self.previous.and_then(|p| p.line(key)).filter(|line| {
            // Reserved lines are free for the key that reserved them.
            self.entries.iter().all(|(_, l)| l != line)
        });
        let line = kept.or_else(|| {
            let free = self.used.iter().position(|u| !u)?;
            Some(LineId(free as u16))
        })?;
        self.used[usize::from(line.0)] = true;
        self.entries.push((key, line));
        Some(line)
    }

    fn finish(mut self) -> LineMap {
        self.entries.sort_unstable();
        LineMap {
            entries: self.entries,
        }
    }
}

/// Clamped delay of `frames`.
fn delay_frames(frames: u32) -> u32 {
    frames.min(MAX_PDC_FRAMES as u32)
}

/// Latency of the devices in front of chain position `k` (not bypassed ones).
fn prefix_latency(chain: &[ChainSlot], k: usize, env: &dyn CompileEnv) -> u32 {
    chain_latency(&chain[..k.min(chain.len())], env)
}

/// The output delays of a flat program: every track lines up with the slowest chain.
fn flat_delays(latencies: &[u32]) -> Vec<u32> {
    let max = latencies.iter().copied().max().unwrap_or(0);
    latencies.iter().map(|l| delay_frames(max - l)).collect()
}

/// Compiles the routing of `tracks` (the program's tracks, with their compiled `chains`) and of
/// the master chain; `previous` is the previous program's routing (line assignment).
pub(super) fn compile(
    project: &Project,
    tracks: &[&Arc<Track>],
    chains: &[Vec<ChainSlot>],
    master_chain: &[ChainSlot],
    env: &dyn CompileEnv,
    previous: Option<&Routing>,
) -> Compiled {
    let n = tracks.len();
    let latencies: Vec<u32> = chains.iter().map(|c| chain_latency(c, env)).collect();
    let flat = || Compiled {
        routing: Arc::new(Routing::default()),
        delays: flat_delays(&latencies),
    };
    let index: HashMap<TrackId, usize> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id, i))
        .collect();
    let bus = |id: TrackId, from: usize| {
        index
            .get(&id)
            .copied()
            .filter(|&b| b != from && tracks[b].is_bus())
    };

    // Edges that can play: targets are buses of the program, nothing routes into itself.
    let outputs: Vec<Option<usize>> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| match t.output {
            TrackOutput::Bus(b) => bus(b, i),
            TrackOutput::Master => None,
        })
        .collect();
    let mut wanted_sends = Vec::new();
    for (i, track) in tracks.iter().enumerate() {
        let mut targets = HashSet::new();
        for send in &track.sends {
            if wanted_sends.len() == MAX_SENDS {
                break;
            }
            if let Some(b) = bus(send.target, i)
                && targets.insert(b)
            {
                wanted_sends.push((i, b, send));
            }
        }
    }
    let sidechain_of: HashMap<DeviceId, Sidechain> = project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .chain(&project.master.devices)
        .filter_map(|d| d.sidechain.map(|sc| (d.id, sc)))
        .collect();
    let holders = (0..n)
        .map(|i| (Some(i), &chains[i][..]))
        .chain(std::iter::once((None, master_chain)));
    let mut wanted_sides = Vec::new();
    for (holder, chain) in holders {
        for (k, slot) in chain.iter().enumerate() {
            if let Some(sc) = sidechain_of.get(&slot.device)
                && let Some(&from) = index.get(&sc.source)
                && Some(from) != holder
            {
                wanted_sides.push((holder, k, from, sc.tap, slot.device));
            }
        }
    }
    if outputs.iter().all(Option::is_none) && wanted_sends.is_empty() && wanted_sides.is_empty() {
        return flat();
    }

    // Job order: the model's topological order of the program's tracks.
    let Ok(topo) = topo_order(project) else {
        return flat();
    };
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut pos = vec![usize::MAX; n];
    for node in topo {
        if let ChannelRef::Track(id) = node
            && let Some(&i) = index.get(&id)
            && pos[i] == usize::MAX
        {
            pos[i] = order.len();
            order.push(i);
        }
    }
    let forward = |from: usize, to: usize| pos[from] < pos[to];
    let consistent = order.len() == n
        && outputs
            .iter()
            .enumerate()
            .all(|(i, o)| o.is_none_or(|b| forward(i, b)))
        && wanted_sends.iter().all(|&(f, t, _)| forward(f, t))
        && wanted_sides
            .iter()
            .all(|&(h, _, f, _, _)| h.is_none_or(|h| forward(f, h)));
    if !consistent {
        // Only a project that breaks the routing invariants gets here.
        return flat();
    }

    // Lines for sends and sidechains (heads come after the latencies).
    let mut lines = Lines::new(previous.map(|r| &*r.lines));
    let keys = tracks
        .iter()
        .filter(|t| !t.is_bus())
        .map(|t| LineKey::Head(t.id))
        .chain(wanted_sends.iter().map(|(_, _, s)| LineKey::Send(s.id)))
        .chain(wanted_sides.iter().map(|s| LineKey::Sidechain(s.4)));
    for key in keys {
        lines.reserve(key);
    }
    let mut sends = Vec::new();
    for (from, to, send) in wanted_sends {
        if let Some(line) = lines.assign(LineKey::Send(send.id)) {
            sends.push(Send {
                id: send.id,
                from,
                to,
                gate: sends.len() as u16,
                gain: fader_gain(send.level_db),
                tap: send.tap,
                line,
            });
        }
    }
    let mut sides = Vec::new();
    for (holder, device, from, tap, id) in wanted_sides {
        if let Some(line) = lines.assign(LineKey::Sidechain(id)) {
            sides.push(Side {
                holder,
                device,
                from,
                tap,
                line,
            });
        }
    }

    // Latencies along the graph (§6.8.5): `in(v)` = max over what reaches v, raised by sidechains
    // that would arrive late; `out(v)` = `in(v)` + chain.
    let mut lat_in = vec![0u32; n];
    let mut lat_out = vec![0u32; n];
    // Whether the main input of device `k` of `holder` can be delayed: on a bus and the master
    // always (their inputs are delayed), on a MIDI track behind the instrument (the head delay).
    let alignable = |holder: Option<usize>, k: usize| match holder {
        Some(h) if !tracks[h].is_bus() => chains[h][k].role == DeviceRole::Effect,
        _ => true,
    };
    let chain_of = |holder: Option<usize>| holder.map_or(master_chain, |h| &chains[h][..]);
    let side_need = |side: &Side, lat_out: &[u32]| {
        lat_out[side.from].saturating_sub(prefix_latency(chain_of(side.holder), side.device, env))
    };
    for &v in &order {
        let mut l = 0;
        for (u, o) in outputs.iter().enumerate() {
            if *o == Some(v) {
                l = l.max(lat_out[u]);
            }
        }
        for s in sends.iter().filter(|s| s.to == v) {
            l = l.max(lat_out[s.from]);
        }
        for side in sides.iter().filter(|s| s.holder == Some(v)) {
            if alignable(side.holder, side.device) {
                l = l.max(side_need(side, &lat_out));
            }
        }
        lat_in[v] = l;
        lat_out[v] = l.saturating_add(latencies[v]);
    }
    let mut master_in = 0;
    for (u, o) in outputs.iter().enumerate() {
        if o.is_none() {
            master_in = master_in.max(lat_out[u]);
        }
    }
    for side in sides.iter().filter(|s| s.holder.is_none()) {
        master_in = master_in.max(side_need(side, &lat_out));
    }
    let target_in = |target: Option<usize>| target.map_or(master_in, |b| lat_in[b]);

    let delays: Vec<u32> = (0..n)
        .map(|u| delay_frames(target_in(outputs[u]).saturating_sub(lat_out[u])))
        .collect();
    let heads: Vec<Option<Delay>> = (0..n)
        .map(|v| {
            // Every MIDI track whose effects take a sidechain has a head line, also while its
            // delay is 0, so the line forgets its audio when the delay drops to 0.
            let sidechained = sides
                .iter()
                .any(|s| s.holder == Some(v) && alignable(s.holder, s.device));
            if tracks[v].is_bus() || !sidechained {
                return None;
            }
            let line = lines.assign(LineKey::Head(tracks[v].id))?;
            Some(Delay {
                line,
                frames: delay_frames(lat_in[v]),
            })
        })
        .collect();

    // Bus and master inputs in summing order: by the source's place in the job order, its output
    // before its send.
    let mut nodes = vec![NodeRoute::default(); n];
    let mut inputs = Vec::new();
    let consumers = order.iter().map(|&v| Some(v)).chain(std::iter::once(None));
    let mut master = MasterRoute {
        latency_in: master_in,
        ..MasterRoute::default()
    };
    for consumer in consumers {
        let mut list: Vec<(usize, u8, RouteInput)> = Vec::new();
        for (u, o) in outputs.iter().enumerate() {
            if *o == consumer {
                list.push((pos[u], 0, RouteInput {
                    from: u as u16,
                    send: None,
                }));
            }
        }
        if let Some(v) = consumer {
            for s in sends.iter().filter(|s| s.to == v) {
                list.push((pos[s.from], 1, RouteInput {
                    from: s.from as u16,
                    send: Some(SendRoute {
                        gate: s.gate,
                        gain: s.gain,
                        tap: s.tap,
                        delay: Delay {
                            line: s.line,
                            frames: delay_frames(lat_in[v].saturating_sub(lat_out[s.from])),
                        },
                    }),
                }));
            }
        }
        list.sort_by_key(|&(p, kind, _)| (p, kind));
        let span = Span {
            start: inputs.len() as u32,
            len: list.len() as u32,
        };
        inputs.extend(list.into_iter().map(|(_, _, input)| input));
        match consumer {
            Some(v) => nodes[v].inputs = span,
            None => master.inputs = span,
        }
    }

    // Sidechains per holder, by chain position.
    let mut sidechains = Vec::new();
    let holders = order.iter().map(|&v| Some(v)).chain(std::iter::once(None));
    for holder in holders {
        let start = sidechains.len() as u32;
        let main_in = target_in(holder);
        for side in sides.iter().filter(|s| s.holder == holder) {
            let at = main_in + prefix_latency(chain_of(holder), side.device, env);
            sidechains.push(SidechainRoute {
                device: side.device as u16,
                from: side.from as u16,
                tap: side.tap,
                delay: Delay {
                    line: side.line,
                    frames: delay_frames(at.saturating_sub(lat_out[side.from])),
                },
            });
        }
        let span = Span {
            start,
            len: sidechains.len() as u32 - start,
        };
        match holder {
            Some(v) => nodes[v].sidechains = span,
            None => master.sidechains = span,
        }
    }

    // Dependencies between jobs (the master is not a job): each source once per target.
    let mut edges: Vec<(usize, usize)> = Vec::new();
    edges.extend(
        outputs
            .iter()
            .enumerate()
            .filter_map(|(u, o)| o.map(|v| (u, v))),
    );
    edges.extend(sends.iter().map(|s| (s.from, s.to)));
    edges.extend(sides.iter().filter_map(|s| s.holder.map(|h| (s.from, h))));
    edges.sort_unstable();
    edges.dedup();
    let mut dependents = Vec::with_capacity(edges.len());
    for (u, node) in nodes.iter_mut().enumerate() {
        let start = dependents.len() as u32;
        dependents.extend(
            edges
                .iter()
                .filter(|&&(from, _)| from == u)
                .map(|&(_, to)| to as u16),
        );
        node.dependents = Span {
            start,
            len: dependents.len() as u32 - start,
        };
        node.waits = edges.iter().filter(|&&(_, to)| to == u).count() as u16;
    }

    for (v, node) in nodes.iter_mut().enumerate() {
        node.bus = tracks[v].is_bus();
        node.output = outputs[v].map(|b| b as u16);
        node.head = heads[v];
        node.latency_in = lat_in[v];
        node.latency_out = lat_out[v];
    }
    let taps = sends
        .iter()
        .map(|s| (s.from, s.tap))
        .chain(sides.iter().map(|s| (s.from, s.tap)));
    for (from, tap) in taps {
        match tap {
            Tap::Pre => nodes[from].pre = true,
            Tap::Post => nodes[from].post = true,
        }
    }

    let send_ids: Box<[SendId]> = sends.iter().map(|s| s.id).collect();
    let ids: Vec<TrackId> = tracks.iter().map(|t| t.id).collect();
    let gates = gate_table(project, &ids, &send_ids);
    Compiled {
        routing: Arc::new(Routing {
            order: order.iter().map(|&i| i as u16).collect(),
            nodes: nodes.into(),
            inputs: inputs.into(),
            sidechains: sidechains.into(),
            dependents: dependents.into(),
            master,
            sends: send_ids,
            gates: Some(gates),
            lines: Arc::new(lines.finish()),
        }),
        delays,
    }
}

/// The gate table of `project` for a program whose tracks are `tracks` (by program index) and
/// whose sends are `sends` (by gate index).
pub(super) fn gate_table(
    project: &Project,
    tracks: &[TrackId],
    sends: &[SendId],
) -> GateTable {
    let gates = model_gates(project);
    let mut table = GateTable::default();
    for (i, &id) in tracks.iter().enumerate() {
        let gate = gates.output(id);
        table.set_muted(i, gate.muted);
        table.set_output_open(i, gate.open());
    }
    for (k, &id) in sends.iter().enumerate() {
        table.set_send_open(k, gates.send_open(id));
    }
    table
}
