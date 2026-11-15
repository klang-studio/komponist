//! Undoable device parameters and plugin states (owner decision 2026-11-15).
//!
//! Every change of a device's state is a tracked edit of the document, so Strg Z reverts a knob,
//! a sample link or a plugin's own edit like anything else:
//!
//! - **pisynth and the internal devices** (EQ, Delay, Hall, Limiter, presse, knurr, Arpeggiator,
//!   Tonleiter, Sampler): `PluginAction::SetParam` writes the value into the device state and
//!   sends it to the running processor as `Command::SetParam`. The processor already has the
//!   value, so the app does not run [`EngineSync::sync_device_states`] for it (that would hand
//!   pisynth a fresh processor on every frame of a drag): the change goes through the engine's
//!   fast path, which also defers the recompile to the gesture's end. Undo, redo, a cancelled
//!   gesture, presets and pastes do follow the document that way.
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
//!   of that device within [`ECHO_QUIET`] after such a hand-over are stored outside the history
//!   (`Document::apply_untracked`); a real edit that fast is not undoable.
//!
//! Still outside the history: transport settings, and sample paths rewritten for the file being
//! saved ([`App::store_state_untracked`]).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_engine::Command;
use komponist_model::edits;
use komponist_model::{DeviceId, Project};

use crate::app::App;
use crate::device_host::{internal_kind, is_pisynth};
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
    /// The device of the open parameter gesture.
    gesture_device: Option<DeviceId>,
    /// Bridged devices a parameter was turned on inside the open gesture.
    gesture_bridged: BTreeSet<DeviceId>,
    /// The label and time of the last card parameter turned on a bridged device.
    turned: HashMap<DeviceId, (String, Instant)>,
    /// The undo step the last snapshot of a device made, and when it was last amended.
    steps: HashMap<DeviceId, (u64, Instant)>,
    /// Until when snapshots of a device are the echo of a state the document handed it.
    echo: HashMap<DeviceId, Instant>,
    /// Snapshots that arrived during a gesture that is not theirs, in arrival order.
    deferred: Vec<(DeviceId, Arc<[u8]>)>,
    /// Bridged devices whose first snapshot is asked for as a baseline (their document state is
    /// still `None`).
    awaiting: BTreeSet<DeviceId>,
    /// What a bridged device held before its state entered the document: undoing back to "no
    /// state" hands the plugin this.
    baseline: HashMap<DeviceId, Arc<[u8]>>,
}

impl Default for ParamUndo {
    fn default() -> Self {
        ParamUndo {
            window: COALESCE,
            gesture_device: None,
            gesture_bridged: BTreeSet::new(),
            turned: HashMap::new(),
            steps: HashMap::new(),
            echo: HashMap::new(),
            deferred: Vec::new(),
            awaiting: BTreeSet::new(),
            baseline: HashMap::new(),
        }
    }
}

impl ParamUndo {
    /// A new document: device ids restart, nothing carries over.
    pub(crate) fn forget(&mut self) {
        *self = ParamUndo {
            window: self.window,
            ..ParamUndo::default()
        };
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
        let label = self.param_label(device, id);
        self.doc.begin_gesture(label);
        self.param_undo.gesture_device = Some(device);
    }

    /// `Edit(EndGesture)`: pushes the gesture's undo step. After a gesture that turned a bridged
    /// plugin's parameter the plugin's state is asked for at once, so the step has it when the
    /// pointer is released.
    pub(crate) fn end_gesture(&mut self) {
        let top = self.doc.top_step();
        self.doc.end_gesture();
        self.param_undo.gesture_device = None;
        let pushed = self.doc.top_step() != top;
        let now = Instant::now();
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
                }
                _ => {
                    self.param_undo.steps.remove(&device);
                }
            }
        }
        self.flush_deferred();
    }

    /// `Edit(CancelGesture)` (Esc during a drag): the document goes back to the start of the
    /// gesture and so do the devices: the engine follows the document, a bridged plugin gets the
    /// document's state.
    pub(crate) fn cancel_gesture(&mut self) {
        let before = Arc::clone(self.doc.project());
        self.doc.cancel_gesture();
        self.param_undo.gesture_device = None;
        self.follow_device_states(&before);
        let now = Instant::now();
        for device in std::mem::take(&mut self.param_undo.gesture_bridged) {
            let state = self
                .project()
                .find_device(device)
                .and_then(|(_, _, d)| d.state.clone());
            if let Some(state) = state
                && self.engine.host_mut().set_state(device, &state)
            {
                self.param_undo.echo.insert(device, now + ECHO_QUIET);
            }
        }
        self.flush_deferred();
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

    /// `PluginAction::SetParam` (module docs).
    pub(crate) fn set_param(&mut self, device: DeviceId, id: u32, value: f64) {
        // Bridged devices take parameters over their control socket; the document follows with the
        // plugin's snapshot.
        if self.engine.host_mut().set_param(device, id, value) {
            let label = self.param_label(device, id);
            self.param_undo
                .turned
                .insert(device, (label, Instant::now()));
            if self.doc.in_gesture() && self.param_undo.gesture_device == Some(device) {
                self.param_undo.gesture_bridged.insert(device);
            }
            return;
        }
        // pisynth and the internal devices: the value goes into the state and to the processor.
        let before = self.doc.revision();
        let changed = match self.param_state(device, id, value) {
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
        } else {
            self.engine.send(command);
        }
    }

    /// The state of `device` with parameter `id` at `value`; `None` for a device that has none
    /// (bridged, unknown) or an unknown id.
    fn param_state(&self, device: DeviceId, id: u32, value: f64) -> Option<Arc<[u8]>> {
        let (_, _, d) = self.project().find_device(device)?;
        if is_pisynth(d) {
            // The `Field::set` the processor applies.
            let field = komponist_builtin::device::Field::from_wire(id)?;
            let mut patch = komponist_builtin::decode_state(d.state.as_deref())
                .unwrap_or_else(|_| komponist_builtin::default_patch());
            field.set(&mut patch, value);
            return Some(komponist_builtin::encode_state(&patch).into());
        }
        let kind = internal_kind(d)?;
        kind.with_value(d.state.as_deref(), id, value)
            .map(Into::into)
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
        if is_pisynth(d) {
            return komponist_builtin::device::params()
                .iter()
                .find(|p| p.wire == id)
                .map(|p| p.info.name.clone());
        }
        if let Some(kind) = internal_kind(d) {
            return kind.params().get(id as usize).map(|p| p.name.clone());
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

    /// Runs `edit` as one undo step named `label`, however many device changes it makes. Inside
    /// an open gesture it is part of that one.
    pub(crate) fn in_one_step(&mut self, label: &str, edit: impl FnOnce(&mut App)) {
        if self.doc.in_gesture() {
            edit(self);
            return;
        }
        self.doc.begin_gesture(label);
        edit(self);
        self.end_gesture();
    }

    /// A state snapshot of a bridged plugin (its GUI, a card parameter, saving) goes into the
    /// document as a tracked change, coalesced (module docs). A state equal to the stored one is
    /// skipped.
    pub(crate) fn store_snapshot(&mut self, device: DeviceId, state: &Arc<[u8]>) {
        if self.has_state(device, state) {
            return;
        }
        if self.param_undo.awaiting.remove(&device) {
            // The baseline asked for when the plugin came up: not an edit.
            self.param_undo.baseline.insert(device, Arc::clone(state));
            return;
        }
        // A device without state in the document that still holds what it came up with is
        // unchanged too (a GUI opened and closed).
        let untouched = self
            .param_undo
            .baseline
            .get(&device)
            .is_some_and(|b| **b == **state)
            && self
                .project()
                .find_device(device)
                .is_some_and(|(_, _, d)| d.state.is_none());
        if untouched {
            return;
        }
        let now = Instant::now();
        if self.param_undo.echo.get(&device).is_some_and(|&t| now < t) {
            self.store_state_untracked(device, state);
            return;
        }
        let in_gesture = self.doc.in_gesture();
        if in_gesture && self.param_undo.gesture_device != Some(device) {
            // Not this gesture's change: after it.
            self.param_undo.deferred.retain(|(d, _)| *d != device);
            self.param_undo.deferred.push((device, Arc::clone(state)));
            return;
        }
        let set = |p: &Project| edits::set_device_state(p, device, Some(Arc::clone(state)));
        if !in_gesture
            && let Some(&(step, at)) = self.param_undo.steps.get(&device)
            && now.duration_since(at) < self.param_undo.window
            && self.doc.amend(step, set).unwrap_or(false)
        {
            self.param_undo.steps.insert(device, (step, now));
            return;
        }
        let label = match self.param_undo.turned.get(&device) {
            Some((label, at)) if now.duration_since(*at) < TURN_LABEL => label.clone(),
            _ => undo_label::plugin_state(&self.device_name(device)),
        };
        if self.doc.apply(label, set).is_ok()
            && !in_gesture
            && let Some(step) = self.doc.top_step()
        {
            self.param_undo.steps.insert(device, (step, now));
        }
    }

    /// A snapshot taken for the file being saved: like [`App::store_snapshot`], but a device
    /// that still holds what it came up with ([`App::device_up`]) gets that state into the
    /// document too (untracked: it is no edit), so the file has it.
    pub(crate) fn store_snapshot_for_save(&mut self, device: DeviceId, state: &Arc<[u8]>) {
        let untouched = self
            .param_undo
            .baseline
            .get(&device)
            .is_some_and(|b| **b == **state)
            && self
                .project()
                .find_device(device)
                .is_some_and(|(_, _, d)| d.state.is_none());
        if untouched {
            self.store_state_untracked(device, state);
        } else {
            self.store_snapshot(device, state);
        }
    }

    /// Stores `state` outside the history, rebased into every undo step: for what is not an edit
    /// (sample paths rewritten for the file being saved, the echo of an undo).
    pub(crate) fn store_state_untracked(&mut self, device: DeviceId, state: &Arc<[u8]>) {
        if self.has_state(device, state) {
            return;
        }
        let _ = self
            .doc
            .apply_untracked(|p| edits::set_device_state(p, device, Some(Arc::clone(state))));
    }

    /// A bridged device came up (`DeviceStatus::Ok`) without a state in the document: asks the
    /// plugin what it holds, so undoing the first edit has something to hand back.
    pub(crate) fn device_up(&mut self, device: DeviceId) {
        let bridged = self.project().find_device(device).is_some_and(|(_, _, d)| {
            d.state.is_none() && !is_pisynth(d) && internal_kind(d).is_none()
        });
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
            if old.state != device.state && !is_pisynth(device) && internal_kind(device).is_none() {
                if device.state.is_none()
                    && let Some(baseline) = self.param_undo.baseline.get(&device.id).cloned()
                {
                    self.engine.host_mut().set_state(device.id, &baseline);
                }
                self.param_undo.echo.insert(device.id, until);
            }
        }
    }
}
