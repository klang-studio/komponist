//! Engine-clock capture for native controls and completed bridge blocks.

use komponist_model::automation::{AutomationMode, AutomationOwner};
use komponist_model::{DeviceId, SendId, Tick};

use super::Core;
use crate::api::{EngineEvent, Telemetry};
use crate::automation::{
    self, AutomationBoundary, CompiledTarget, Cursor, EngineTarget, PluginParam, PluginParamBatch,
};
use crate::garbage::Outbox;
use crate::processor::ParameterEditKind;
use crate::program::{ChainSlot, SlotId};
use crate::sequencer::Window;
use crate::slots::Slot;

/// The bridge's bounded output ring can contain at most this many edits per processor.
const MAX_PLUGIN_EDITS: usize = 1024;

impl Core {
    pub(super) fn slot_device(&self, slot: SlotId) -> Option<(AutomationOwner, DeviceId)> {
        let program = self.program.as_deref()?;
        if let Some(device) = program
            .master
            .chain
            .iter()
            .find(|device| device.slot == slot)
        {
            return Some((AutomationOwner::Master, device.device));
        }
        program.tracks.iter().find_map(|track| {
            track
                .chain
                .iter()
                .find(|device| device.slot == slot)
                .map(|device| (AutomationOwner::Track(track.id), device.device))
        })
    }

    pub(super) fn send_owner(&self, send: SendId) -> Option<AutomationOwner> {
        self.program.as_deref()?.tracks.iter().find_map(|track| {
            track
                .source
                .sends
                .iter()
                .any(|entry| entry.id == send)
                .then_some(AutomationOwner::Track(track.id))
        })
    }

    fn locate_automation(
        &mut self,
        owner: AutomationOwner,
    ) -> Option<(&[CompiledTarget], &mut automation::State)> {
        let program = self.program.as_deref()?;
        match owner {
            AutomationOwner::Master => {
                Some((&program.master.automation, &mut self.master_automation))
            }
            AutomationOwner::Track(id) => {
                let i = program.tracks.iter().position(|track| track.id == id)?;
                let state = &mut self.tracks[usize::from(self.map[i])];
                Some((&program.tracks[i].automation, &mut state.automation))
            }
        }
    }

    pub(super) fn automation_touch(
        &mut self,
        owner: AutomationOwner,
        target: EngineTarget,
        begin: bool,
    ) {
        let rate = f64::from(self.sample_rate);
        let tick = self.transport.tick(&self.song, rate);
        let fractional_tick = self
            .song
            .map
            .samples_to_ticks(self.transport.pos as f64, rate);
        let playing = self.transport.playing() && !self.offline;
        let time = self.automation_time;
        let Some((lanes, state)) = self.locate_automation(owner) else {
            return;
        };
        let Some((cursor, index)) = state.cursor(lanes, target) else {
            return;
        };
        let lane = &lanes[index];
        if lane.mode == AutomationMode::Off {
            return;
        }
        let value = if begin {
            None
        } else {
            cursor.pending_value.take()
        };
        let notify = if begin {
            cursor.begin(lane.mode, playing, None)
        } else {
            cursor.end(lane, fractional_tick, time)
        };
        if matches!(target, EngineTarget::Macro(_)) {
            cursor.reassert();
        }
        if let Some((tick, value)) = value {
            self.send(EngineEvent::AutomationValue {
                owner,
                target,
                tick,
                value,
            });
        }
        if notify {
            self.automation_pass_started |= begin;
            self.send(EngineEvent::AutomationTouch {
                owner,
                target,
                begin,
                tick,
            });
        }
    }

    pub(super) fn manual_automation(
        &mut self,
        owner: AutomationOwner,
        target: EngineTarget,
        plain: f64,
    ) -> bool {
        let tick = self.transport.tick(&self.song, f64::from(self.sample_rate));
        let Some((lanes, state)) = self.locate_automation(owner) else {
            return false;
        };
        state.cancel_restore(lanes, target);
        let Some((cursor, index)) = state.cursor(lanes, target) else {
            return false;
        };
        let lane = &lanes[index];
        if cursor.is_held() && (lane.reads() || cursor.writes()) {
            let value = match target {
                EngineTarget::Slot { .. } => lane.map.normalize(plain),
                EngineTarget::Gain | EngineTarget::Send(_) => {
                    automation::fader_to_normalized(plain as f32)
                }
                EngineTarget::Pan => (plain as f32 + 1.0) * 0.5,
                EngineTarget::Macro(_) => plain as f32,
            };
            cursor.manual_value(tick, value.clamp(0.0, 1.0));
            if matches!(target, EngineTarget::Macro(_)) {
                cursor.reassert();
            }
            return true;
        }
        if lane.reads() {
            // A state sync may update the base while the lane still owns the audible value.
            cursor.reassert();
        }
        false
    }

    pub(super) fn macro_input(&mut self, track: komponist_model::TrackId, index: u8, value: f32) {
        if !value.is_finite() {
            return;
        }
        let owner = AutomationOwner::Track(track);
        let target = EngineTarget::Macro(index);
        let rate = self.sample_rate;
        let time = self.automation_time;
        let playing = self.transport.playing() && !self.offline;
        let tick = self.transport.tick(&self.song, f64::from(rate));
        if let Some((lanes, state)) = self.locate_automation(owner)
            && let Some((cursor, lane)) = state.cursor(lanes, target)
            && lanes[lane].mode != AutomationMode::Off
        {
            if cursor.implicit_touch(lanes[lane].mode, playing, time, rate) {
                self.automation_pass_started = true;
                self.send(EngineEvent::AutomationTouch {
                    owner,
                    target,
                    begin: true,
                    tick,
                });
            }
            if self.manual_automation(owner, target, f64::from(value)) {
                return;
            }
        }
        if let Some(state) = self.state_of(track) {
            state.automation.macros.set(index, value);
        }
    }

    pub(super) fn chase_automation_slot(&mut self, slot: SlotId) {
        if let Some((owner, _)) = self.slot_device(slot)
            && let Some((lanes, state)) = self.locate_automation(owner)
        {
            state.chase_slot(lanes, slot);
        }
    }

    pub(super) fn chase_automation(&mut self) {
        self.visit_automation(|_, _, state, _, _| state.chase());
    }

    pub(super) fn reassert_automation(&mut self) {
        self.visit_automation(|_, _, state, _, _| {
            for cursor in state.cursors() {
                cursor.reassert();
            }
        });
    }

    pub(super) fn cancel_automation_write(&mut self) {
        self.automation_pass_started = false;
        self.visit_automation(|_, lanes, state, _, _| {
            for (lane, cursor) in lanes.iter().zip(state.cursors()) {
                cursor.cancel_write(lane);
            }
        });
    }

    pub(super) fn finish_automation_write(&mut self) {
        self.automation_pass_started = false;
        self.visit_automation(|_, lanes, state, _, _| {
            for (lane, cursor) in lanes.iter().zip(state.cursors()) {
                cursor.finish_write(lane);
            }
        });
    }

    pub(super) fn automation_boundary(
        &mut self,
        kind: AutomationBoundary,
        tick: Tick,
        restart: Option<Tick>,
    ) {
        let mut was_writing = false;
        self.visit_automation(|owner, lanes, state, outbox, telemetry| {
            for (lane, cursor) in lanes.iter().zip(state.cursors()) {
                if cursor.writes() {
                    was_writing = true;
                    flush_value(cursor, owner, lane.target, Some(tick), outbox, telemetry);
                }
                cursor.interrupt(restart.is_some(), lane.mode);
            }
        });
        if kind != AutomationBoundary::LoopWrap {
            was_writing |= std::mem::take(&mut self.automation_pass_started);
        }
        if was_writing && !self.offline {
            self.send(EngineEvent::AutomationBoundary { tick, kind });
        }
    }

    pub(super) fn restart_automation_touch(&mut self, tick: Tick) {
        self.visit_automation(|owner, lanes, state, outbox, telemetry| {
            for (lane, cursor) in lanes.iter().zip(state.cursors()) {
                // Latch was ended above. Only a still-held Touch restarts across wrap.
                if cursor.writes() {
                    outbox.send(
                        EngineEvent::AutomationTouch {
                            owner,
                            target: lane.target,
                            begin: true,
                            tick,
                        },
                        telemetry,
                    );
                    flush_value(cursor, owner, lane.target, Some(tick), outbox, telemetry);
                }
            }
        });
    }

    pub(super) fn expire_automation_touches(&mut self, max: usize) -> usize {
        let rate = f64::from(self.sample_rate);
        let time = self.automation_time;
        let pos = self.transport.pos;
        let map = self.song.map;
        let playing = self.transport.playing();
        let mut frames = max;
        self.visit_automation(|owner, lanes, state, outbox, telemetry| {
            for (lane, cursor) in lanes.iter().zip(state.cursors()) {
                let Some(end) = cursor.implicit_end() else {
                    continue;
                };
                if end > time {
                    frames = frames.min((end - time) as usize);
                    continue;
                }
                let frame = if playing {
                    pos.saturating_sub((time - end) as i64)
                } else {
                    pos
                };
                let tick = map.sample_to_tick(frame, rate);
                let notify = cursor.end(lane, map.samples_to_ticks(frame as f64, rate), end);
                if matches!(lane.target, EngineTarget::Macro(_)) {
                    cursor.reassert();
                }
                if notify {
                    flush_value(cursor, owner, lane.target, None, outbox, telemetry);
                    outbox.send(
                        EngineEvent::AutomationTouch {
                            owner,
                            target: lane.target,
                            begin: false,
                            tick,
                        },
                        telemetry,
                    );
                }
            }
        });
        frames
    }

    /// Runs only after all worker jobs have joined. Plugin edits are reported in stable track and
    /// slot order even when the bridges finished on different worker threads.
    pub(super) fn collect_parameter_edits(&mut self, window: Window<'_>) {
        if self.offline {
            return;
        }
        let Some(program) = self.program.as_deref() else {
            return;
        };
        let clock = CaptureClock {
            window,
            time: self.automation_time,
        };
        let mut capture = Capture {
            clock,
            outbox: &mut self.outbox,
            telemetry: &self.telemetry,
            pass_started: &mut self.automation_pass_started,
        };
        for (track, &index) in program.tracks.iter().zip(self.map.iter()) {
            capture.chain(
                AutomationOwner::Track(track.id),
                &track.chain,
                (&track.automation, Some(&track.macros)),
                &mut self.tracks[usize::from(index)].automation,
                &mut self.slots,
            );
        }
        capture.chain(
            AutomationOwner::Master,
            &program.master.chain,
            (&program.master.automation, None),
            &mut self.master_automation,
            &mut self.slots,
        );
    }

    fn visit_automation(
        &mut self,
        mut visit: impl FnMut(
            AutomationOwner,
            &[CompiledTarget],
            &mut automation::State,
            &mut Outbox,
            &Telemetry,
        ),
    ) {
        let Some(program) = self.program.as_deref() else {
            return;
        };
        for (track, &index) in program.tracks.iter().zip(self.map.iter()) {
            visit(
                AutomationOwner::Track(track.id),
                &track.automation,
                &mut self.tracks[usize::from(index)].automation,
                &mut self.outbox,
                &self.telemetry,
            );
        }
        visit(
            AutomationOwner::Master,
            &program.master.automation,
            &mut self.master_automation,
            &mut self.outbox,
            &self.telemetry,
        );
    }
}

#[derive(Clone, Copy)]
struct CaptureClock<'a> {
    window: Window<'a>,
    time: u64,
}

impl CaptureClock<'_> {
    fn at(self, offset: u32) -> (Tick, u64, f64) {
        let offset = offset.min(self.window.frames - 1);
        let frame = self.window.start
            + if self.window.playing {
                i64::from(offset)
            } else {
                0
            };
        let tick = self.window.map.sample_to_tick(frame, self.window.rate);
        (
            tick,
            self.time + u64::from(offset),
            automation::tick_at(self.window, frame),
        )
    }
}

struct Capture<'a> {
    clock: CaptureClock<'a>,
    outbox: &'a mut Outbox,
    telemetry: &'a Telemetry,
    pass_started: &'a mut bool,
}

impl Capture<'_> {
    fn chain(
        &mut self,
        owner: AutomationOwner,
        chain: &[ChainSlot],
        (lanes, macros): (&[CompiledTarget], Option<&crate::program::MacroProgram>),
        state: &mut automation::State,
        slots: &mut [Slot],
    ) {
        for device in chain {
            let Some(slot) = slots.get_mut(device.slot.0 as usize) else {
                continue;
            };
            let runs = slot.runs(device.bypass);
            let Some(processor) = &mut slot.processor else {
                continue;
            };
            let mut batch = PluginParamBatch::new();
            for _ in 0..MAX_PLUGIN_EDITS {
                let Some(edit) = processor.pop_parameter_edit() else {
                    break;
                };
                // Bypass stops DSP, but earlier replies may still contain GUI edits.
                // With no rendered segment for this slot, stamp those edits at the boundary.
                let (tick, time, fractional_tick) =
                    self.clock.at(if runs { edit.offset } else { 0 });
                let wire_id = match edit.kind {
                    ParameterEditKind::Value { id, .. } | ParameterEditKind::Gesture { id, .. } => {
                        id
                    }
                };
                let target = EngineTarget::Slot {
                    slot: device.slot,
                    device: device.device,
                    wire_id,
                };
                if matches!(edit.kind, ParameterEditKind::Value { .. }) {
                    state.cancel_restore(lanes, target);
                }
                let mut temporary = macros.is_some_and(|program| {
                    state.macros.plugin_edit(
                        program,
                        target,
                        edit.kind,
                        time,
                        self.clock.window.rate,
                    )
                });
                let mut starts_write = false;
                if !temporary
                    && let Some((cursor, index)) = state.cursor(lanes, target)
                    && lanes[index].mode != AutomationMode::Off
                {
                    let lane = &lanes[index];
                    let edge = match edit.kind {
                        ParameterEditKind::Value { value, .. } => {
                            let notify = cursor.implicit_touch(
                                lane.mode,
                                self.clock.window.playing,
                                time,
                                self.clock.window.rate as f32,
                            );
                            temporary = lane.reads() || cursor.writes();
                            cursor.manual_value(tick, lane.map.normalize(value));
                            notify.then_some(true)
                        }
                        ParameterEditKind::Gesture { begin: true, .. } => cursor
                            .begin(lane.mode, self.clock.window.playing, None)
                            .then_some(true),
                        ParameterEditKind::Gesture { begin: false, .. } => {
                            // The edit tick still belongs to the segment just rendered.
                            // A host return ramp can first change audio in the next one.
                            let playback_start =
                                self.clock.time + u64::from(self.clock.window.frames);
                            let notify = cursor.end(lane, fractional_tick, playback_start);
                            if notify {
                                // Earlier native edits belong before this write's final sample
                                // and release, even when the processor reports them together.
                                self.flush_plugin_params(device, &mut batch);
                            }
                            flush_value(cursor, owner, target, None, self.outbox, self.telemetry);
                            notify.then_some(false)
                        }
                    };
                    starts_write = edge == Some(true);
                    if edge == Some(false) {
                        self.outbox.send(
                            EngineEvent::AutomationTouch {
                                owner,
                                target,
                                begin: false,
                                tick,
                            },
                            self.telemetry,
                        );
                    }
                }
                let full = batch.push(
                    PluginParam {
                        tick,
                        kind: edit.kind,
                    },
                    temporary,
                );
                if starts_write {
                    *self.pass_started = true;
                    batch.mark_write_start(owner);
                }
                if full || starts_write {
                    self.flush_plugin_params(device, &mut batch);
                }
            }
            self.flush_plugin_params(device, &mut batch);
        }
        let (tick, _, _) = self.clock.at(self.clock.window.frames - 1);
        for (lane, cursor) in lanes.iter().zip(state.cursors()) {
            lane.publish_override(cursor.override_value(lane, self.clock.window, self.clock.time));
            let repeated = (cursor.writes() && self.clock.window.playing).then_some(tick);
            flush_value(
                cursor,
                owner,
                lane.target,
                repeated,
                self.outbox,
                self.telemetry,
            );
        }
    }

    fn flush_plugin_params(&mut self, device: &ChainSlot, batch: &mut PluginParamBatch) {
        if !batch.as_slice().is_empty() {
            self.outbox.send(
                EngineEvent::PluginParams {
                    slot: device.slot,
                    device: device.device,
                    events: std::mem::replace(batch, PluginParamBatch::new()),
                },
                self.telemetry,
            );
        }
    }
}

fn flush_value(
    cursor: &mut Cursor,
    owner: AutomationOwner,
    target: EngineTarget,
    repeat_at: Option<Tick>,
    outbox: &mut Outbox,
    telemetry: &Telemetry,
) {
    if let Some((tick, value)) = cursor
        .pending_value
        .take()
        .or_else(|| repeat_at.zip(cursor.manual()))
    {
        outbox.send(
            EngineEvent::AutomationValue {
                owner,
                target,
                tick,
                value,
            },
            telemetry,
        );
    }
}
