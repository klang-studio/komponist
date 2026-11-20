//! The temporary project used for a pre-fader, single-track render.

use std::collections::HashSet;
use std::sync::Arc;

use komponist_model::automation::AutomationTarget;
use komponist_model::routing::{self, EdgeKind};
use komponist_model::{
    Channel, ChannelRef, DeviceId, Project, Tick, TickRange, TrackId, TrackKind, TrackOutput,
};

use super::{ExportError, recompile};
use crate::{Engine, Program, compile, route::Span};

/// The chain to render and any tracks that supply its sidechain signals.
pub struct TrackExport {
    id: TrackId,
    project: Project,
    range: TickRange,
    preroll: Tick,
}

impl TrackExport {
    /// Builds the isolated render project. Takes the whole span of the MIDI and step clips,
    /// including any gaps. Upstream audio is used only as a control signal for the chain.
    pub fn new(project: &Project, id: TrackId) -> Result<Self, ExportError> {
        let target = project
            .track(id)
            .filter(|t| t.kind == TrackKind::Midi)
            .ok_or(ExportError::InvalidTrack)?;
        let first = target.clips.first().ok_or(ExportError::InvalidTrack)?;
        let end = target
            .clips
            .iter()
            .map(|c| c.end())
            .max()
            .unwrap_or(first.end());
        let range = TickRange::new(first.start, end);
        let gates = routing::gates(project);
        let edges: Vec<_> = routing::edges(project)
            .into_iter()
            .filter(|edge| match edge.kind {
                EdgeKind::Output => match edge.from {
                    ChannelRef::Track(id) => gates.output_open(id),
                    ChannelRef::Master => false,
                },
                EdgeKind::Send(id) => gates.send_open(id),
                EdgeKind::Sidechain(id) => project
                    .find_device(id)
                    .is_some_and(|(_, _, device)| !device.bypass),
                EdgeKind::Input => true,
            })
            .collect();
        let mut needed = HashSet::new();
        let mut pending = vec![ChannelRef::Track(id)];
        while let Some(node) = pending.pop() {
            if needed.insert(node) {
                pending.extend(edges.iter().filter(|e| e.to == node).map(|e| e.from));
            }
        }
        let mut scoped = project.clone();
        scoped.master = Arc::new(Channel::default());
        scoped.master_automation = Default::default();
        scoped
            .tracks
            .retain(|t| needed.contains(&ChannelRef::Track(t.id)));
        for track in &mut scoped.tracks {
            let t = Arc::make_mut(track);
            if t.id == id {
                t.channel.volume_db = 0.0;
                t.channel.pan = 0.0;
                // Flatten prints the chain before the track strip. Mixer lanes survive
                // on the audio track, while device curves are baked into the render.
                t.automation.lanes.retain(|lane| {
                    !matches!(
                        lane.target,
                        AutomationTarget::Volume | AutomationTarget::Pan
                    )
                });
                for slot in &mut t.macros.0 {
                    slot.mappings
                        .retain(|mapping| matches!(mapping.target, AutomationTarget::Param { .. }));
                }
                t.channel.mute = false;
                t.channel.solo = false;
                t.output = TrackOutput::Master;
                t.sends.clear();
            } else {
                if !needed.contains(&routing::output_node(t.output)) || !gates.output_open(t.id) {
                    t.output = TrackOutput::Master;
                }
                t.sends.retain(|s| {
                    needed.contains(&ChannelRef::Track(s.target)) && gates.send_open(s.id)
                });
            }
        }
        let preroll = scoped
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.start)
            .min()
            .unwrap_or(range.start);
        Ok(Self {
            id,
            project: scoped,
            range,
            preroll,
        })
    }

    /// The clip range before the tail.
    pub fn range(&self) -> TickRange {
        self.range
    }

    /// The tracks and devices required for this render, for file and device preflight.
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Active devices of the selected track and its sidechain sources.
    pub fn devices(&self) -> impl Iterator<Item = DeviceId> + '_ {
        self.project
            .tracks
            .iter()
            .flat_map(|t| &t.channel.devices)
            .filter(|d| !d.bypass)
            .map(|d| d.id)
    }

    pub(super) fn id(&self) -> TrackId {
        self.id
    }

    pub(super) fn preroll(&self) -> Tick {
        self.preroll
    }

    /// Reuses the live processor slots and recompiles compensation from their offline latencies.
    /// Only the selected track feeds the temporary master. Keep the original gates on its sources.
    pub(super) fn compile(
        &self,
        engine: &mut Engine,
        original: &Project,
        live: &Program,
    ) -> Result<Program, ExportError> {
        let mut program = recompile(engine, &self.project, live);
        for device in self.devices() {
            if live
                .slots
                .slot(device)
                .is_none_or(|slot| program.slots.slot(device) != Some(slot))
            {
                return Err(ExportError::DeviceFailed(device));
            }
        }
        let index = program
            .tracks
            .iter()
            .position(|t| t.id == self.id)
            .ok_or(ExportError::InvalidTrack)?;
        if program.routing.is_flat() {
            return if program.tracks.len() == 1 {
                Ok(program)
            } else {
                Err(ExportError::InvalidTrack)
            };
        }
        let mut gates = compile::gates(original, &program);
        gates.set_muted(index, false);
        gates.set_output_open(index, true);
        let routing = Arc::make_mut(&mut program.routing);
        routing.gates = Some(gates);
        let master = &routing.master.inputs;
        let input = master
            .range()
            .find(|&i| usize::from(routing.inputs[i].from) == index)
            .ok_or(ExportError::InvalidTrack)?;
        routing.master.inputs = Span {
            start: input as u32,
            len: 1,
        };
        routing.master.latency_in = routing.nodes[index].latency_out;
        Arc::make_mut(&mut program.tracks[index]).pdc_delay = 0;
        Ok(program)
    }
}
