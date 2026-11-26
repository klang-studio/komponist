//! Undoable device parameters and plugin states (owner decision 2026-11-15).
//!
//! Every change of a device's state is a tracked edit of the document, so Strg Z reverts a knob,
//! a sample link or a plugin's own edit like anything else:
//!
//! - **hexa and the internal devices** (EQ, Delay, Hall, Limiter, presse, volt, Arpeggiator,
//!   Tonleiter, Sampler): `PluginAction::SetParam` writes the value into the device state and
//!   sends it to the running processor as `Command::SetParam`. The processor already has the
//!   value, so the app does not run [`EngineSync::sync_device_states`] for it: the change goes
//!   through the engine's fast path, which also defers the recompile to the gesture's end. Undo,
//!   redo, a cancelled gesture, presets and pastes do follow the document that way, again as
//!   `SetParam`s of the values that differ, so a sounding hexa note goes on (#19); only a hexa
//!   change no card parameter reaches (most presets) gives hexa a fresh processor.
//! - **Gestures:** the view brackets a drag or a burst of wheel notches in
//!   `PluginAction::BeginParamGesture` … `Edit(EndGesture)`
//!   ([`crate::views::mixer::gesture_actions`]), so it is one undo step named after the parameter
//!   („Cutoff ändern“); a click, a key step, a text entry, a reset or a dropdown choice is a
//!   step of its own. Esc cancels the gesture and the device goes back to the value it had
//!   ([`App::cancel_gesture`]). A sample load is one step as well, however many values it resets.
//! - **Bridged plugins:** the card's `SetParam` goes over the control socket
//!   ([`crate::device_host::DeviceHost::set_param`]); the document learns the new state from the
//!   plugin's *snapshot*, which arrives debounced after the turn (or at once, when the gesture
//!   ends: [`App::end_gesture`] asks for it). The same snapshots carry the edits made in the
//!   plugin's own GUI. They are tracked ([`App::store_snapshot`]) and **coalesced**: snapshots of
//!   one device that follow each other within [`COALESCE`] amend one undo step, labelled after
//!   the last card parameter turned („Cutoff ändern“) or „<Plugin>: Zustand ändern“; a snapshot
//!   during a card gesture of the same device joins the gesture's step; one that arrives during
//!   another gesture (a fader drag, a recording) waits for its end. A snapshot equal to the stored
//!   state is skipped, so closing a GUI without edits leaves the document clean.
//! - **Baseline:** a bridged device that comes up without a state in the document is asked for
//!   one ([`App::device_up`]); the answer is kept aside, not stored. Undoing the first edit goes
//!   back to "no state", and the plugin then gets the baseline; a snapshot equal to it is not an
//!   edit.
//! - **Echo of an undo:** undo, redo or a preset hands a bridged plugin its state (`SetState`),
//!   and a plugin may answer with a snapshot whose bytes differ though nothing changed. Snapshots
//!   of that device within [`ECHO_QUIET`] after such a hand-over are stored outside the history,
//!   in the current root only ([`App::store_root_state`]): the other undo and redo steps keep
//!   their own states (#86). A real edit that fast is not undoable.
//!
//! Still outside the history: transport settings, the baseline of a bridged device taken for the
//! file (root only, like the echo), and sample paths rewritten for the file being saved, which
//! every step gets from its own state (`crate::project_assets`, `Document::rewrite_untracked`).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_builtin::Internal;
use komponist_engine::Command;
use komponist_model::automation::AutomationTarget;
use komponist_model::edits;
use komponist_model::macros::MAX_MAPPINGS;
use komponist_model::project::SavedPluginState;
use komponist_model::{Device, DeviceId, Project, TrackId};

use crate::app::App;
use crate::device_host::{LegacyParameters, internal_kind};
use crate::undo_label;

/// Snapshots of a device that arrive within this time of the last one amend its undo step.
pub const COALESCE: Duration = Duration::from_secs(1);

/// Snapshots of a bridged device within this time after the document handed it a state are the
/// plugin's echo and stay out of the history.
pub const ECHO_QUIET: Duration = Duration::from_secs(2);

/// Added to the window after a card gesture: the bridge's debounced snapshot of its last turns.
const SNAPSHOT_SLACK: Duration = Duration::from_millis(500);

/// A card parameter turned this long ago still names the snapshot step that follows it.
const TURN_LABEL: Duration = Duration::from_secs(5);

/// What the app remembers to group device changes into undo steps (module docs).
#[derive(Debug)]
pub(crate) struct ParamUndo {
    /// How long snapshots of one device amend one step ([`COALESCE`]; tests shorten it).
    window: Duration,
    next_text_edit: u64,
    text_edits: HashMap<(DeviceId, u32), (u64, Option<f64>)>,
    /// The device of the open parameter gesture.
    gesture_device: Option<DeviceId>,
    /// Bridged devices a parameter was turned on inside the open gesture.
    gesture_bridged: BTreeSet<DeviceId>,
    /// Bridges that receive macro-driven parameter values during this gesture.
    macro_gesture: BTreeSet<DeviceId>,
    /// Recently closed macro gestures to which a verified macro snapshot can still be attached.
    macro_steps: HashMap<DeviceId, (u64, Instant)>,
    /// The label and time of the last card parameter turned on a bridged device.
    turned: HashMap<DeviceId, (String, Instant)>,
    /// The undo step the last snapshot of a device made, and when it was last amended.
    steps: HashMap<DeviceId, (u64, Instant)>,
    /// Bridged devices whose card gesture ended before any snapshot made its step: the step the
    /// next snapshot makes counts as amended at this time at the earliest, so the plugin's
    /// debounced snapshot of the same turns still joins it ([`SNAPSHOT_SLACK`]).
    tails: HashMap<DeviceId, Instant>,
    /// Until when snapshots of a device are the echo of a state the document handed it.
    echo: HashMap<DeviceId, Instant>,
    /// Snapshots that arrived during a gesture that is not theirs, in arrival order.
    deferred: Vec<(DeviceId, SavedPluginState)>,
    /// Bridged devices whose first snapshot is asked for as a baseline (their document state is
    /// still `None`).
    awaiting: BTreeSet<DeviceId>,
    /// What a bridged device held before its state entered the document: undoing back to "no
    /// state" hands the plugin this.
    baseline: HashMap<DeviceId, SavedPluginState>,
}

impl Default for ParamUndo {
    fn default() -> Self {
        ParamUndo {
            window: COALESCE,
            next_text_edit: 0,
            text_edits: HashMap::new(),
            gesture_device: None,
            gesture_bridged: BTreeSet::new(),
            macro_gesture: BTreeSet::new(),
            macro_steps: HashMap::new(),
            turned: HashMap::new(),
            steps: HashMap::new(),
            tails: HashMap::new(),
            echo: HashMap::new(),
            deferred: Vec::new(),
            awaiting: BTreeSet::new(),
            baseline: HashMap::new(),
        }
    }
}

impl ParamUndo {
    pub(crate) fn cancel_text_edits(&mut self, device: DeviceId) {
        self.text_edits.retain(|(owner, _), _| *owner != device);
    }

    pub(crate) fn cancel_text_edit(&mut self, device: DeviceId, id: u32) {
        self.text_edits.remove(&(device, id));
    }

    /// A new document: device ids restart, nothing carries over.
    pub(crate) fn forget(&mut self) {
        *self = ParamUndo {
            window: self.window,
            next_text_edit: self.next_text_edit,
            ..ParamUndo::default()
        };
    }

    fn forget_device(&mut self, device: DeviceId) {
        self.cancel_text_edits(device);
        if self.gesture_device == Some(device) {
            self.gesture_device = None;
        }
        self.gesture_bridged.remove(&device);
        self.macro_gesture.remove(&device);
        self.macro_steps.remove(&device);
        self.turned.remove(&device);
        self.steps.remove(&device);
        self.tails.remove(&device);
        self.echo.remove(&device);
        self.deferred.retain(|(id, _)| *id != device);
        self.awaiting.remove(&device);
        self.baseline.remove(&device);
    }
}

impl App {
    /// Sets how long snapshots of one device amend one undo step (default [`COALESCE`]).
    pub fn set_snapshot_window(&mut self, window: Duration) {
        self.param_undo.window = window;
    }

    /// `PluginAction::BeginParamGesture`: opens the gesture named after the parameter.
    pub(crate) fn begin_param_gesture(&mut self, device: DeviceId, id: u32) {
        self.end_gesture();
        self.checkpoint_pending_param_edits(device);
        let label = self.param_label(device, id);
        self.doc.begin_gesture(label);
        self.param_undo.gesture_device = Some(device);
    }

    /// The next gesture must start from the values already sent to the plugin, even if its
    /// state snapshot still describes earlier values. Keep the bytes opaque and checkpoint
    /// the outstanding values as host-owned bases in the preceding undo step.
    fn checkpoint_pending_param_edits(&mut self, device: DeviceId) {
        let pending = self.pending_param_bases(device);
        if pending.is_empty() {
            return;
        }
        let Some(mut state) = self
            .project()
            .find_device(device)
            .and_then(|(_, _, target)| target.saved_state())
            .or_else(|| self.param_undo.baseline.get(&device).cloned())
        else {
            return;
        };
        state.parameter_bases.extend(pending);
        self.param_undo.awaiting.remove(&device);
        self.param_undo.echo.remove(&device);
        self.param_undo
            .deferred
            .retain(|(pending, _)| *pending != device);
        self.apply_device_snapshot(device, &state);
    }

    /// Associates a macro's bridged targets with the open gesture, so their verified snapshots
    /// can amend the gesture rather than making separate undo steps.
    pub fn note_macro_gesture(&mut self, track: TrackId, index: u8) {
        if !self.doc.in_gesture() {
            return;
        }
        let Some(track) = self.doc.project().track(track) else {
            return;
        };
        let Some(mac) = track.macros.0.get(usize::from(index)) else {
            return;
        };
        self.param_undo
            .macro_gesture
            .extend(mac.mappings.iter().take(MAX_MAPPINGS).filter_map(
                |mapping| match mapping.target {
                    AutomationTarget::Param { device, .. }
                        if self.engine.host().params(device).is_some() =>
                    {
                        Some(device)
                    }
                    _ => None,
                },
            ));
    }

    /// `Edit(EndGesture)`: pushes the gesture's undo step. After a gesture that turned a bridged
    /// plugin's parameter the plugin's state is asked for at once, so the step has it when the
    /// pointer is released.
    pub(crate) fn end_gesture(&mut self) {
        self.release_automation_controls();
        let top = self.doc.top_step();
        self.doc.end_gesture();
        self.param_undo.gesture_device = None;
        let pushed = self.doc.top_step() != top;
        let now = Instant::now();
        for device in std::mem::take(&mut self.param_undo.macro_gesture) {
            if let Some(step) = self.doc.top_step().filter(|_| pushed) {
                self.param_undo.macro_steps.insert(device, (step, now));
                self.engine.host_mut().request_state(device);
            }
        }
        for device in std::mem::take(&mut self.param_undo.gesture_bridged) {
            self.engine.host_mut().request_state(device);
            match self.doc.top_step() {
                // The snapshot that came during the drag made the step; the last one amends it.
                Some(step) if pushed => {
                    // The plugin's own debounce (1 s after the last turn) may still fire after
                    // the immediate answer: the window starts a little later.
                    self.param_undo
                        .steps
                        .insert(device, (step, now + SNAPSHOT_SLACK));
                    self.param_undo.tails.remove(&device);
                }
                // No snapshot came during the drag: the immediate answer will make the step, and
                // the debounced one follows about the debounce after the last turn. Without the
                // slack its window would start at the immediate answer and leave the debounced
                // snapshot only the answer's own latency to arrive in (a second step on a busy
                // machine, seen on the Windows CI runner).
                _ => {
                    self.param_undo.steps.remove(&device);
                    self.param_undo.tails.insert(device, now + SNAPSHOT_SLACK);
                }
            }
        }
        self.flush_deferred();
    }

    /// `Edit(CancelGesture)` (Esc during a drag): the document goes back to the start of the
    /// gesture and so do the devices: the engine follows the document, a bridged plugin gets the
    /// document's state.
    pub(crate) fn cancel_gesture(&mut self) {
        self.release_automation_controls();
        let before = Arc::clone(self.doc.project());
        self.ui.track_macros.range_drag_epoch =
            self.ui.track_macros.range_drag_epoch.wrapping_add(1);
        self.doc.cancel_gesture();
        self.param_undo.gesture_device = None;
        self.param_undo.macro_gesture.clear();
        self.follow_device_states(&before);
        let now = Instant::now();
        for device in std::mem::take(&mut self.param_undo.gesture_bridged) {
            self.forget_param_edits(device);
            let state = self
                .project()
                .find_device(device)
                .and_then(|(_, _, d)| d.saved_state())
                .or_else(|| self.param_undo.baseline.get(&device).cloned());
            if let Some(state) = state
                && self.engine.host_mut().restore_saved_state(device, &state)
            {
                self.param_undo.echo.insert(device, now + ECHO_QUIET);
                if !state.parameter_bases.is_empty() {
                    self.engine.host_mut().request_state(device);
                }
            }
        }
        self.flush_deferred();
    }

    pub(crate) fn deferred_plugin_states(&self) -> &[(DeviceId, SavedPluginState)] {
        &self.param_undo.deferred
    }

    /// Stores the snapshots that waited for a gesture, once none is open.
    pub(crate) fn flush_deferred(&mut self) {
        if self.doc.in_gesture() || self.param_undo.deferred.is_empty() {
            return;
        }
        for (device, state) in std::mem::take(&mut self.param_undo.deferred) {
            self.store_snapshot(device, &state);
        }
    }

    pub(crate) fn set_param_text(
        &mut self,
        device: DeviceId,
        id: u32,
        text: String,
        fallback: Option<f64>,
    ) {
        self.param_undo.cancel_text_edit(device, id);
        self.param_undo.next_text_edit = self.param_undo.next_text_edit.wrapping_add(1);
        let request = self.param_undo.next_text_edit;
        if self
            .engine
            .host_mut()
            .request_param_text(device, request, id, text)
        {
            self.param_undo
                .text_edits
                .insert((device, id), (request, fallback));
        } else if let Some(value) = fallback {
            self.dispatch(crate::actions::Action::Plugin(
                crate::actions::PluginAction::SetParam { device, id, value },
            ));
        }
    }

    pub(crate) fn finish_param_text(
        &mut self,
        device: DeviceId,
        id: u32,
        request: u64,
        value: Option<f64>,
    ) {
        if let Some(&(current, fallback)) = self.param_undo.text_edits.get(&(device, id))
            && current == request
        {
            self.param_undo.cancel_text_edit(device, id);
            if let Some(value) = value.or(fallback) {
                self.dispatch(crate::actions::Action::Plugin(
                    crate::actions::PluginAction::SetParam { device, id, value },
                ));
            }
        }
    }

    /// `PluginAction::SetParam` (module docs).
    pub(crate) fn set_param(&mut self, device: DeviceId, id: u32, value: f64) {
        // Most devices take edits through the engine slot. The control socket orders bridge
        // edits alongside editor transactions and pending state restores.
        let slot = self.engine.program().and_then(|p| p.slots.slot(device));
        if let (Some(slot), Some((owner, target))) =
            (slot, self.device_automation_target(device, id))
            && self.manual_automation(owner, &target, Command::SetParam { slot, id, value })
        {
            return;
        }
        let bridged = if let Some(slot) = slot
            && self
                .engine
                .host_mut()
                .edit_param_through_render(device, id, value)
        {
            self.engine.send(Command::SetParam { slot, id, value });
            true
        } else {
            self.engine.host_mut().set_param(device, id, value)
        };
        if bridged {
            self.remember_param_edit(device, id);
            let label = self.param_label(device, id);
            self.param_undo
                .turned
                .insert(device, (label, Instant::now()));
            if self.doc.in_gesture() && self.param_undo.gesture_device == Some(device) {
                self.param_undo.gesture_bridged.insert(device);
            }
            return;
        }
        // hexa and the internal devices: the value goes into the state and to the processor.
        let before = self.doc.revision();
        let state = self
            .project()
            .find_device(device)
            .and_then(|(_, _, d)| device_param_state(d, id, value));
        let changed = match state {
            Some(state) => {
                let label = if self.doc.in_gesture() {
                    String::new()
                } else {
                    self.param_label(device, id)
                };
                self.store_param_state(device, &state, &label)
            }
            None => false,
        };
        let after = self.doc.revision();
        let Some(slot) = self.engine.program().and_then(|p| p.slots.slot(device)) else {
            return;
        };
        let command = Command::SetParam { slot, id, value };
        if changed {
            self.engine.fast_path(command, before, after);
            if let Some((_, _, d)) = self.project().find_device(device)
                && internal_kind(d) == Some(Internal::GeneralSampler)
                && Internal::GeneralSampler
                    .params()
                    .get(id as usize)
                    .is_some_and(|param| matches!(param.id.as_str(), "sync" | "source_bpm"))
            {
                self.engine.prepare_sampler_sync(self.project(), d);
            }
        } else {
            self.engine.send(command);
        }
    }

    /// The undo label of a change of parameter `id`: „Cutoff ändern“.
    fn param_label(&self, device: DeviceId, id: u32) -> String {
        match self.param_name(device, id) {
            Some(name) => undo_label::param(&name),
            None => undo_label::device_param(&self.device_name(device)),
        }
    }

    fn param_name(&self, device: DeviceId, id: u32) -> Option<String> {
        let (_, _, d) = self.project().find_device(device)?;
        if let Some(parameters) = LegacyParameters::new(d) {
            return parameters.card_param_name(self.lang, id).map(str::to_owned);
        }
        self.engine
            .host()
            .params(device)?
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
    }

    /// Whether `device` is gone or already has exactly `state`.
    fn has_state(&self, device: DeviceId, state: &[u8]) -> bool {
        self.project()
            .find_device(device)
            .is_none_or(|(_, _, d)| d.state.as_deref() == Some(state))
    }

    /// Stores `state` as a tracked change of `device` that the running processor already has (a
    /// knob value it just got as `SetParam`, a sample link it just loaded): one undo step named
    /// `label`, or part of the open gesture; the engine is not told. Returns whether the document
    /// changed (an equal state is skipped).
    pub(crate) fn store_param_state(
        &mut self,
        device: DeviceId,
        state: &Arc<[u8]>,
        label: &str,
    ) -> bool {
        if self.has_state(device, state) {
            return false;
        }
        self.doc
            .apply(label, |p| {
                edits::set_device_state(p, device, Some(Arc::clone(state)))
            })
            .is_ok()
    }

    fn has_saved_state(&self, device: DeviceId, state: &SavedPluginState) -> bool {
        self.project()
            .find_device(device)
            .is_none_or(|(_, _, d)| d.has_saved_state(state))
    }

    /// Records an opaque edit already applied to the live plugin as a distinct undo step.
    fn record_live_state_edit(
        &mut self,
        device: DeviceId,
        previous_state: &Arc<[u8]>,
        state: &Arc<[u8]>,
        label: &str,
    ) {
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
        let no_document_state = target.state.is_none();
        self.param_undo.awaiting.remove(&device);
        self.param_undo.echo.remove(&device);
        self.param_undo.turned.remove(&device);
        self.param_undo.steps.remove(&device);
        self.param_undo.tails.remove(&device);
        if no_document_state {
            self.param_undo
                .baseline
                .entry(device)
                .or_insert_with(|| previous_state.clone().into());
        }
        if no_document_state && previous_state == state {
            return;
        }
        if let Err(error) = self.doc.apply(label, |project| {
            edits::set_device_state(project, device, Some(Arc::clone(state)))
        }) {
            self.record_edit_error(error);
        }
    }

    /// Give the marker operation its exact captured undo state. A preceding plugin-side
    /// change may still be waiting for its snapshot when the marker edit completes.
    pub(crate) fn record_waveform_marker_edit(
        &mut self,
        device: DeviceId,
        previous_state: &Arc<[u8]>,
        state: &Arc<[u8]>,
    ) {
        if previous_state == state {
            return;
        }
        if self.doc.in_gesture() {
            self.end_gesture();
        }
        if self.reconcile_previous_live_state(device, previous_state) {
            self.record_live_state_edit(device, previous_state, state, "undo-sample-slices");
            if self.has_state(device, state) {
                self.reconcile_param_edits(device, state);
            }
        }
    }

    /// Preserve an earlier plugin edit whose snapshot has yet to reach the document.
    fn reconcile_previous_live_state(
        &mut self,
        device: DeviceId,
        previous_state: &Arc<[u8]>,
    ) -> bool {
        let Some((_, _, target)) = self.project().find_device(device) else {
            return false;
        };
        let recorded = target.state.as_ref().or_else(|| {
            self.param_undo
                .baseline
                .get(&device)
                .map(|state| &state.bytes)
        });
        if recorded.is_some_and(|recorded| recorded != previous_state) {
            let label = undo_label::plugin_state(&self.device_name(device));
            if let Err(error) = self.doc.apply(label, |project| {
                edits::set_device_state(project, device, Some(Arc::clone(previous_state)))
            }) {
                self.record_edit_error(error);
                return false;
            }
        }
        true
    }

    /// Records a preset already applied to the live plugin as a distinct undo step.
    pub(crate) fn record_clap_preset(
        &mut self,
        device: DeviceId,
        previous_state: &Arc<[u8]>,
        state: &Arc<[u8]>,
    ) {
        if self.doc.in_gesture() {
            self.end_gesture();
        }
        if self.reconcile_previous_live_state(device, previous_state) {
            self.record_live_state_edit(device, previous_state, state, "undo-load-preset");
        }
    }

    /// A state snapshot of a bridged plugin (its GUI, a card parameter, saving) goes into the
    /// document as a tracked change, coalesced (module docs). A state equal to the stored one is
    /// skipped.
    pub(crate) fn store_snapshot(&mut self, device: DeviceId, state: &SavedPluginState) {
        self.apply_device_snapshot(device, state);
        if self.has_saved_state(device, state) {
            self.reconcile_param_edits(device, &state.bytes);
        }
    }

    fn apply_device_snapshot(&mut self, device: DeviceId, state: &SavedPluginState) {
        if self.has_saved_state(device, state) {
            return;
        }
        if self.param_undo.awaiting.remove(&device) {
            // The baseline asked for when the plugin came up: not an edit.
            self.param_undo.baseline.insert(device, state.clone());
            return;
        }
        // A device without state in the document that still holds what it came up with is
        // unchanged too (a GUI opened and closed).
        let untouched = self
            .param_undo
            .baseline
            .get(&device)
            .is_some_and(|b| b == state)
            && self
                .project()
                .find_device(device)
                .is_some_and(|(_, _, d)| d.state.is_none());
        if untouched {
            return;
        }
        let now = Instant::now();
        if self.param_undo.echo.get(&device).is_some_and(|&t| now < t) {
            self.store_root_state(device, state);
            return;
        }
        if self.engine.host().is_macro_snapshot(device, &state.bytes) {
            self.store_macro_snapshot(device, state, now);
            return;
        }
        let in_gesture = self.doc.in_gesture();
        if in_gesture && self.param_undo.gesture_device != Some(device) {
            // Not this gesture's change: after it.
            self.param_undo.deferred.retain(|(d, _)| *d != device);
            self.param_undo.deferred.push((device, state.clone()));
            return;
        }
        let set = |p: &Project| edits::set_saved_plugin_state(p, device, state);
        if !in_gesture
            && let Some(&(step, at)) = self.param_undo.steps.get(&device)
            && now.duration_since(at) < self.param_undo.window
            && self.doc.amend(step, set).unwrap_or(false)
        {
            // The window runs from the later of the amendment and the slack after a gesture
            // (`end_gesture`): the immediate answer must not cut the slack short.
            self.param_undo.steps.insert(device, (step, at.max(now)));
            return;
        }
        let label = match self.param_undo.turned.get(&device) {
            Some((label, at)) if now.duration_since(*at) < TURN_LABEL => label.clone(),
            _ => undo_label::plugin_state(&self.device_name(device)),
        };
        let tail = self.param_undo.tails.remove(&device);
        // An edit that changes nothing pushes no step (#220): the top step is then an earlier,
        // unrelated one, which the next snapshot must not amend.
        let top = self.doc.top_step();
        if self.doc.apply(label, set).is_ok()
            && !in_gesture
            && let Some(step) = self.doc.top_step().filter(|step| Some(*step) != top)
        {
            let at = tail.map_or(now, |tail| tail.max(now));
            self.param_undo.steps.insert(device, (step, at));
        }
    }

    fn store_macro_snapshot(&mut self, device: DeviceId, state: &SavedPluginState, now: Instant) {
        let set = |p: &Project| edits::set_saved_plugin_state(p, device, state);
        if self.doc.in_gesture() && self.param_undo.macro_gesture.contains(&device) {
            let _ = self.doc.apply("", set);
        } else if !self
            .param_undo
            .macro_steps
            .get(&device)
            .is_some_and(|&(step, at)| {
                now.duration_since(at) < self.param_undo.window
                    && self.doc.amend(step, set).unwrap_or(false)
            })
        {
            self.store_root_state(device, state);
        }
    }

    /// Keep the last live edit in the document before its processor is retired.
    pub(crate) fn store_retiring_snapshot(&mut self, device: DeviceId, state: &SavedPluginState) {
        if self.doc.in_gesture() {
            self.end_gesture();
        }
        // This is the final state, even if the initial baseline request never completed.
        self.param_undo.awaiting.remove(&device);
        self.store_snapshot_for_save(device, state);
    }

    /// A snapshot taken for the file being saved: like [`App::store_snapshot`], but a device
    /// that still holds what it came up with ([`App::device_up`]) gets that state into the
    /// document too (untracked: it is no edit), so the file has it.
    pub(crate) fn store_snapshot_for_save(&mut self, device: DeviceId, state: &SavedPluginState) {
        self.param_undo
            .deferred
            .retain(|(pending, _)| *pending != device);
        let untouched = self
            .param_undo
            .baseline
            .get(&device)
            .is_some_and(|b| b == state)
            && self
                .project()
                .find_device(device)
                .is_some_and(|(_, _, d)| d.state.is_none());
        if untouched {
            self.store_root_state(device, state);
        } else {
            self.store_snapshot(device, state);
        }
    }

    /// Stores `state` of `device` outside the history and in the current root only
    /// (`Document::apply_untracked_to_root`): for bytes that restate what the root means, not an
    /// edit (the echo of an undo, a baseline taken for the file). The undo and redo steps keep
    /// their own states (#86).
    pub(crate) fn store_root_state(&mut self, device: DeviceId, state: &SavedPluginState) {
        if self.has_saved_state(device, state) {
            return;
        }
        let _ = self
            .doc
            .apply_untracked_to_root(|p| edits::set_saved_plugin_state(p, device, state));
    }

    /// A bridged device came up (`DeviceStatus::Ok`) without a state in the document: asks the
    /// plugin what it holds, so undoing the first edit has something to hand back.
    pub(crate) fn device_up(&mut self, device: DeviceId) {
        let bridged = self
            .project()
            .find_device(device)
            .is_some_and(|(_, _, d)| d.state.is_none() && LegacyParameters::new(d).is_none());
        if bridged
            && !self.param_undo.baseline.contains_key(&device)
            && self.param_undo.awaiting.insert(device)
        {
            self.engine.host_mut().request_state(device);
        }
    }

    /// After a tracked change that replaced device states (`follow_device_states`): the bridged
    /// devices among them got a `SetState` (a device back to "no state" gets its baseline), and
    /// their snapshots in the next moments are its echo.
    pub(crate) fn expect_echo(&mut self, before: &Project, after: &Project) {
        let until = Instant::now() + ECHO_QUIET;
        let devices = after
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter())
            .chain(after.master.devices.iter());
        for device in devices {
            let Some((_, _, old)) = before.find_device(device.id) else {
                continue;
            };
            if old.plugin != device.plugin {
                self.param_undo.forget_device(device.id);
                self.forget_param_edits(device.id);
                self.reset_pad_hit(device.id);
                continue;
            }
            if (old.state != device.state || old.parameter_bases != device.parameter_bases)
                && LegacyParameters::new(device).is_none()
            {
                self.param_undo.cancel_text_edits(device.id);
                self.forget_param_edits(device.id);
                if komponist_plugins::packaged_kind(&device.plugin).is_some() {
                    self.param_undo.awaiting.remove(&device.id);
                    if device.state.is_none() {
                        self.reset_pad_hit(device.id);
                    }
                }
                if device.state.is_none()
                    && komponist_plugins::packaged_kind(&device.plugin).is_none()
                    && let Some(baseline) = self.param_undo.baseline.get(&device.id).cloned()
                {
                    self.engine
                        .host_mut()
                        .restore_saved_state(device.id, &baseline);
                }
                self.param_undo.echo.insert(device.id, until);
            }
        }
    }
}

/// The document state after setting a built-in parameter, including all eight Hexa device macros.
/// Apply to the device from the latest edited root when updating several parameters together.
pub(crate) fn device_param_state(device: &Device, wire: u32, value: f64) -> Option<Arc<[u8]>> {
    LegacyParameters::new(device)?
        .with_value(wire, value)
        .map(Into::into)
}
