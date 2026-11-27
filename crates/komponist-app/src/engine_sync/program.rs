//! Project compilation, device slots and the last program sent to the engine.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use komponist_audio::MidiInputHub;
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::{Command, MAX_SLOTS, MidiPort, Program, SlotId, Telemetry, TrackMeter};
use komponist_model::project::SavedPluginState;
use komponist_model::{DeviceId, Document, Project, Tick, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};
use komponist_processor::processor::ParameterEditKind;

use crate::device_host::DeviceHost;
use crate::recording::automation_capture::AutomationCapture;

use super::io::EngineIo;

/// The compiler's view of the app: MIDI ports from the hub, latencies from the device host.
struct Env<'a> {
    midi: Option<&'a MidiInputHub>,
    host: &'a dyn DeviceHost,
    /// The stream's sample rate: audio clips are compiled to frames of it.
    rate: f64,
}

impl CompileEnv for Env<'_> {
    fn midi_port(&self, device_name: &str) -> Option<MidiPort> {
        self.midi?.port_id(device_name).map(|id| MidiPort(id.0))
    }

    fn device_latency(&self, device: komponist_model::DeviceId) -> u32 {
        self.host.latency(device)
    }

    fn automation_parameter(
        &self,
        device: &komponist_model::Device,
        param: &komponist_model::params::ParamId,
    ) -> Option<komponist_engine::automation::Parameter> {
        use komponist_engine::automation::{Parameter, PlainMap};
        if matches!(
            self.host.status(device.id),
            DeviceStatus::Missing | DeviceStatus::Crashed(_)
        ) {
            return None;
        }
        let wire_id = param.plugin_id()?;
        let info = self
            .host
            .params(device.id)?
            .iter()
            .find(|p| p.id == wire_id)?;
        komponist_plugins::params::is_automatable(info).then(|| Parameter {
            wire_id,
            map: PlainMap::linear(info.min, info.max, info.step_count),
            base: self
                .host
                .param_base(device.id, wire_id)
                .unwrap_or(info.default),
        })
    }

    fn sample_rate(&self) -> f64 {
        self.rate
    }

    fn project_dir(&self) -> Option<std::path::PathBuf> {
        self.host.project_dir()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum MacroValueEdit {
    Base(f32),
    AutomationOverride(f32),
}

/// Completed automation heard during playback while it is still outside the document history.
#[derive(Clone, Copy, Default)]
pub(crate) struct DraftOverlay<'a> {
    automation: [Option<&'a AutomationCapture>; 2],
}

impl<'a> DraftOverlay<'a> {
    pub(crate) fn new(
        pending: Option<&'a AutomationCapture>,
        active: &'a AutomationCapture,
    ) -> Self {
        // The audio writer owns an earlier pass. Apply it before any newer, live segments.
        // Compact empty passes so transferring a pass to the writer keeps the same revision.
        let mut passes = [pending, Some(active)]
            .into_iter()
            .flatten()
            .filter(|pass| pass.completed_revision() != 0);
        Self {
            automation: [passes.next(), passes.next()],
        }
    }

    fn revision(self) -> [u64; 2] {
        self.automation
            .map(|pass| pass.map_or(0, AutomationCapture::completed_revision))
    }

    fn compose(self, project: &Project) -> Option<Project> {
        let [first, second] = self.automation;
        let draft = first?.apply(project).ok()?;
        match second {
            Some(pass) => pass.apply(&draft).ok(),
            None => Some(draft),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectRevision {
    document: u64,
    draft: [u64; 2],
}

pub(super) struct ProgramSync {
    host: Box<dyn DeviceHost>,
    // The compiler and slot diff must use the same, last published program.
    previous: Option<Program>,
    diagnostics: Vec<compile::Diagnostic>,
    diagnostics_notice: bool,
    synced: Option<ProjectRevision>,
    programs_sent: u64,
    fast: Option<u64>,
    reloads: u64,
    // Host events without a device ID that survive a project switch.
    carried: Vec<PluginManagerEvent>,
    macro_params: Vec<(DeviceId, u32, f64)>,
    macro_edit: u64,
    pending_macro_edits: HashMap<(TrackId, u8), (u64, MacroValueEdit)>,
    // The rebuilt engine needs the next program's tempo map before it can locate.
    locate_after_rebuild: Option<Tick>,
}

impl ProgramSync {
    pub(super) fn new(host: Box<dyn DeviceHost>) -> Self {
        Self {
            host,
            previous: None,
            diagnostics: Vec::new(),
            diagnostics_notice: false,
            synced: None,
            programs_sent: 0,
            fast: None,
            reloads: 0,
            carried: Vec::new(),
            macro_params: Vec::new(),
            macro_edit: 0,
            pending_macro_edits: HashMap::new(),
            locate_after_rebuild: None,
        }
    }

    pub(super) fn engine_rebuilt(&mut self, playhead: Tick) {
        self.previous = None;
        self.invalidate_project();
        self.macro_params.clear();
        self.pending_macro_edits.clear();
        self.locate_after_rebuild = Some(playhead);
    }

    pub(super) fn note_transport_command(&mut self, command: &Command) {
        // A newer position wins over the rebuild position. The first new program supplies
        // the tempo map needed to locate there.
        if let Command::Locate(at) = *command
            && self.locate_after_rebuild.is_some()
        {
            self.locate_after_rebuild = Some(at);
        }
    }

    pub(super) fn refresh_macro_state(&mut self, telemetry: &Telemetry) {
        let applied_edit = telemetry.applied_macro_edit();
        self.pending_macro_edits
            .retain(|_, (edit, _)| *edit > applied_edit);
        self.sync_macro_params(telemetry);
    }

    #[cfg(test)]
    pub(super) fn queue_diagnostic(&mut self, diagnostic: compile::Diagnostic) {
        self.diagnostics.push(diagnostic);
        self.diagnostics_notice = true;
    }

    fn release_processors(&mut self, io: &mut EngineIo) {
        if let Some(previous) = self.previous.take() {
            for &(device, slot) in previous.slots.entries() {
                io.send(Command::RemoveProcessor { slot });
                self.host.release(device);
            }
        }
    }

    pub(super) fn sync_with_draft(
        &mut self,
        io: &mut EngineIo,
        doc: &Document,
        draft: DraftOverlay<'_>,
    ) {
        let revision = ProjectRevision {
            document: doc.revision(),
            draft: draft.revision(),
        };
        if self.synced == Some(revision)
            || (doc.in_gesture()
                && self.fast == Some(revision.document)
                && self.synced.is_some_and(|last| last.draft == revision.draft))
        {
            return;
        }
        let project = draft.compose(doc.project());
        self.send_project(io, project.as_ref().unwrap_or(doc.project()));
        self.synced = Some(revision);
        self.fast = None;
    }

    pub(super) fn reload_device(
        &mut self,
        io: &mut EngineIo,
        project: &Project,
        device: komponist_model::DeviceId,
    ) {
        let Some(slot) = self.previous.as_ref().and_then(|p| p.slots.slot(device)) else {
            return;
        };
        let Some((_, _, d)) = project.find_device(device) else {
            return;
        };
        let setup = io.setup();
        if let Some(processor) = self.host.create_processor(d, setup) {
            self.reloads += 1;
            io.send(Command::InsertProcessor { slot, processor });
            self.host.set_macro_params(&self.macro_params);
            if let Some(program) = &self.previous {
                self.host
                    .set_automated_params(&Self::automated_parameters(program, project));
            }
        } else if self.host.status(device) == DeviceStatus::Missing {
            io.send(Command::RemoveProcessor { slot });
        }
    }

    pub(super) fn sync_device_states(
        &mut self,
        io: &mut EngineIo,
        before: &Project,
        after: &Project,
        startup_states: &HashMap<DeviceId, SavedPluginState>,
    ) {
        let devices = after
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter())
            .chain(after.master.devices.iter());
        for device in devices {
            let Some((_, _, old)) = before.find_device(device.id) else {
                continue;
            };
            if Arc::ptr_eq(old, device) {
                continue;
            }
            if let Some(slot) = self.slot_of(device.id) {
                if old.plugin != device.plugin {
                    // Migration can retain the device ID while changing its host and state format.
                    io.send(Command::RemoveProcessor { slot });
                    self.host.release(device.id);
                    self.reload_device(io, after, device.id);
                } else {
                    let state_changed =
                        old.state != device.state || old.parameter_bases != device.parameter_bases;
                    if state_changed {
                        self.follow_state(io, device, after, startup_states.get(&device.id));
                    }
                    if state_changed || old.asset_links != device.asset_links {
                        self.host.sync_asset_links(
                            device.id,
                            &old.asset_links,
                            &device.asset_links,
                        );
                    }
                }
            }
        }
    }

    pub(super) fn mark_fast_path(&mut self, before: u64, after: u64) {
        if self.synced.is_some_and(|last| last.document == before) || self.fast == Some(before) {
            self.fast = Some(after);
        }
    }

    pub(super) fn poll_device_events(&mut self, io: &mut EngineIo) -> Vec<PluginManagerEvent> {
        let mut events = std::mem::take(&mut self.carried);
        events.extend(self.host.poll());
        for event in &events {
            if let PluginManagerEvent::StateApplied { device } = *event {
                if let Some(slot) = self.slot_of(device) {
                    io.send(Command::AutomationReassert { slot });
                }
                self.reassert_device_macros(io, device);
            }
        }
        events
    }

    pub(super) fn set_host(&mut self, io: &mut EngineIo, host: Box<dyn DeviceHost>) {
        self.release_processors(io);
        self.host = host;
        self.invalidate_project();
        self.macro_params.clear();
    }

    fn send_project(&mut self, io: &mut EngineIo, project: &Project) {
        self.sync_macro_params(io.telemetry());
        let next = self.compile_project(io, project);
        let changes = compile::slot_changes(self.previous.as_ref(), &next.program);
        // The first build reserves the slots it removes. If that exhausts the table, see
        // whether the released slots can now take devices excluded from the first program.
        let check_freed_slots = !changes.removed.is_empty()
            && next.program.slots.entries().len() + changes.removed.len() == MAX_SLOTS;
        self.send_compiled_project(io, project, next, changes);
        if check_freed_slots {
            let next = self.compile_project(io, project);
            let changes = compile::slot_changes(self.previous.as_ref(), &next.program);
            if !changes.added.is_empty() {
                self.send_compiled_project(io, project, next, changes);
            }
        }
    }

    fn compile_project(&self, io: &EngineIo, project: &Project) -> compile::CompileResult {
        let env = Env {
            midi: io.midi(),
            host: &*self.host,
            rate: f64::from(io.sample_rate()),
        };
        compile::build_with_diagnostics(project, self.previous.as_ref(), &env)
    }

    fn send_compiled_project(
        &mut self,
        io: &mut EngineIo,
        project: &Project,
        mut next: compile::CompileResult,
        changes: compile::SlotChanges,
    ) {
        let setup = io.setup();
        let mut new_processor_metadata = false;
        for (device, slot) in changes.added {
            let Some((_, _, device)) = project.find_device(device) else {
                continue;
            };
            if let Some(processor) = self.host.create_processor(device, setup) {
                new_processor_metadata |= processor.latency() > 0
                    || self
                        .host
                        .params(device.id)
                        .is_some_and(|params| !params.is_empty());
                io.send(Command::InsertProcessor { slot, processor });
            }
        }
        if new_processor_metadata {
            // Latency and CLAP parameter metadata can first become available on insertion.
            // Compile the automation and compensation with those descriptors before publishing
            // the program, including when export detaches it before the next host poll.
            // Keep slots whose removals have not yet been queued reserved.
            next = self.compile_project(io, project);
        }
        let values = self.macro_parameters(io.telemetry(), &next.program);
        if values != self.macro_params {
            let retained: HashSet<_> = values.iter().map(|&(device, id, _)| (device, id)).collect();
            let released = self.macro_params.iter().any(|&(device, id, _)| {
                next.program.slots.slot(device).is_some() && !retained.contains(&(device, id))
            });
            self.set_macro_params(values);
            if released {
                // Unmapping makes the last controlled value the plugin's ordinary base. Any
                // automation lane that becomes active again must compile against that base.
                next = self.compile_project(io, project);
            }
        }
        let compile::CompileResult {
            program: next,
            diagnostics,
        } = next;
        self.host
            .set_automated_params(&Self::automated_parameters(&next, project));
        io.send(Command::SetProgram(Box::new(next.clone())));
        // A rebuilt engine stays stopped where the old one was; ticks map to frames only with
        // the program's tempo map, so after it.
        if let Some(at) = self.locate_after_rebuild.take() {
            io.send(Command::Locate(at));
        }
        self.programs_sent += 1;
        self.previous = Some(next);
        self.diagnostics_notice |= diagnostics.iter().any(|d| !self.diagnostics.contains(d));
        self.diagnostics = diagnostics;
        for (device, slot) in changes.removed {
            io.send(Command::RemoveProcessor { slot });
            self.host.release(device);
        }
    }

    fn follow_state(
        &mut self,
        io: &mut EngineIo,
        device: &komponist_model::Device,
        project: &Project,
        startup_state: Option<&SavedPluginState>,
    ) {
        if let Some(state) = device.saved_state() {
            if !self.host.restore_saved_state(device.id, &state)
                && komponist_plugins::requires_bundled(&device.plugin)
                && self.host.status(device.id) == DeviceStatus::Loading
            {
                // A replacement bridge is still loading the previous revision (for example
                // Redo immediately after Undo recreated the default instance).
                self.host.release(device.id);
                self.reload_device(io, project, device.id);
            }
        } else if komponist_plugins::requires_bundled(&device.plugin) {
            if startup_state.is_some_and(|state| self.host.restore_saved_state(device.id, state)) {
                return;
            }
            // Undo may restore the new instance's absent state. A regular reload preserves
            // the previous processor's snapshot, so start a fresh instance at its defaults.
            let slot = self.slot_of(device.id).expect("checked by the caller");
            io.send(Command::RemoveProcessor { slot });
            self.host.release(device.id);
            self.reload_device(io, project, device.id);
        }
    }

    fn reassert_device_macros(&mut self, io: &mut EngineIo, device: DeviceId) {
        use komponist_model::automation::AutomationTarget;
        let owner = self.previous.as_ref().and_then(|program| {
            program.tracks.iter().find(|track| {
                track.source.macros.0.iter().any(|mac| {
                    mac.mappings.iter().any(|mapping| {
                        matches!(mapping.target, AutomationTarget::Param { device: id, .. } if id == device)
                    })
                })
            })
        });
        if let Some(track) = owner.map(|track| track.id) {
            io.send(Command::MacroReassert { track });
        }
    }

    pub(super) fn invalidate_project(&mut self) {
        self.synced = None;
        self.fast = None;
    }

    pub(super) fn programs_sent(&self) -> u64 {
        self.programs_sent
    }

    pub(super) fn device_reloads(&self) -> u64 {
        self.reloads
    }

    pub(super) fn refresh_snapshot_automation(&mut self, project: &Project) {
        if let Some(program) = &self.previous {
            self.host
                .set_automated_params(&Self::automated_parameters(program, project));
        }
    }

    pub(super) fn macro_value_command(
        &mut self,
        track: TrackId,
        index: u8,
        change: MacroValueEdit,
    ) -> Command {
        self.macro_edit += 1;
        let edit = self.macro_edit;
        self.pending_macro_edits
            .insert((track, index), (edit, change));
        let value = match change {
            MacroValueEdit::Base(value) | MacroValueEdit::AutomationOverride(value) => value,
        };
        Command::SetMacro {
            track,
            index,
            value,
            edit,
        }
    }

    pub(super) fn discard_pending_macro_overrides(&mut self) {
        self.pending_macro_edits
            .retain(|_, (_, change)| matches!(change, MacroValueEdit::Base(_)));
    }

    pub(super) fn gates(&self, project: &Project) -> Option<Command> {
        let program = self.previous.as_ref()?;
        Some(Command::SetGates(compile::gates(project, program)))
    }

    pub(super) fn plugin_parameter_slot_matches(&self, slot: SlotId, device: DeviceId) -> bool {
        self.program()
            .is_some_and(|program| program.slots.slot(device) == Some(slot))
    }

    pub(super) fn plugin_mapping_parameter(&self, device: DeviceId, id: u32) -> bool {
        self.host.params(device).is_some_and(|params| {
            params
                .iter()
                .any(|info| info.id == id && komponist_plugins::params::is_automatable(info))
        })
    }

    pub(super) fn update_plugin_parameter(
        &mut self,
        device: DeviceId,
        edit: ParameterEditKind,
        temporary: bool,
        learning: bool,
    ) -> Option<f64> {
        let id = match edit {
            ParameterEditKind::Value { id, .. } | ParameterEditKind::Gesture { id, .. } => id,
        };
        let previous = if learning {
            self.macro_params
                .iter()
                .find(|&&(target, wire, _)| target == device && wire == id)
                .map(|&(_, _, value)| value)
                .or_else(|| self.host.param_value(device, id).map(|p| p.value))
                .or_else(|| self.host.param_base(device, id))
        } else {
            None
        };
        if let ParameterEditKind::Value { value, .. } = edit {
            if temporary || learning {
                self.host.preview_param(device, id, value);
            } else {
                self.host.edited_param(device, id, value);
            }
        }
        previous
    }

    pub(super) fn program(&self) -> Option<&Program> {
        self.previous.as_ref()
    }

    pub(super) fn diagnostics(&self) -> &[compile::Diagnostic] {
        &self.diagnostics
    }

    pub(super) fn take_diagnostics_notice(&mut self) -> bool {
        std::mem::take(&mut self.diagnostics_notice)
    }

    pub(super) fn track_meter(&self, track: TrackId) -> Option<&Arc<TrackMeter>> {
        self.previous.as_ref()?.meter(track)
    }

    pub(super) fn host(&self) -> &dyn DeviceHost {
        &*self.host
    }

    pub(super) fn host_mut(&mut self) -> &mut dyn DeviceHost {
        &mut *self.host
    }

    pub(super) fn device_status(&self, device: komponist_model::DeviceId) -> DeviceStatus {
        self.host.status(device)
    }

    fn slot_of(&self, device: komponist_model::DeviceId) -> Option<SlotId> {
        self.previous.as_ref().and_then(|p| p.slots.slot(device))
    }

    fn automated_parameters(
        program: &Program,
        project: &Project,
    ) -> Vec<(DeviceId, u32, Option<f64>)> {
        let mut values = Vec::new();
        program.for_each_automated_parameter(
            |track, index| Some(project.track(track)?.macros.0[usize::from(index)].value),
            |device, id, base| values.push((device, id, base)),
        );
        values
    }

    fn set_macro_params(&mut self, values: Vec<(DeviceId, u32, f64)>) {
        if values != self.macro_params {
            self.host.set_macro_params(&values);
            self.macro_params = values;
        }
    }

    fn sync_macro_params(&mut self, telemetry: &Telemetry) {
        if let Some(program) = &self.previous {
            let values = self.macro_parameters(telemetry, program);
            self.set_macro_params(values);
        }
    }

    fn macro_parameters(
        &self,
        telemetry: &Telemetry,
        program: &Program,
    ) -> Vec<(DeviceId, u32, f64)> {
        let mut values = Vec::new();
        program.for_each_macro_parameter(
            |track| telemetry.macro_values(track),
            |slot, id, value| {
                if let Some(device) = program.slots.device(slot)
                    && self.host.params(device).is_some()
                {
                    values.push((device, id, value));
                }
            },
        );
        values
    }

    pub(super) fn pending_macro_value(
        &self,
        telemetry: &Telemetry,
        track: TrackId,
        index: u8,
        base: f32,
    ) -> Option<f32> {
        let &(edit, change) = self.pending_macro_edits.get(&(track, index))?;
        if edit <= telemetry.applied_macro_edit() {
            return None;
        }
        match change {
            // Undo or cancellation may have already replaced the pending base in the document.
            MacroValueEdit::Base(value) => (value == base).then_some(value),
            MacroValueEdit::AutomationOverride(value) => Some(value),
        }
    }

    pub(super) fn reset_project(&mut self, io: &mut EngineIo) {
        self.release_processors(io);
        self.invalidate_project();
        self.locate_after_rebuild = None;
        // The next project's diagnostics are all new, even those equal to the old project's.
        self.diagnostics.clear();
        self.diagnostics_notice = false;
        self.macro_params.clear();
        self.pending_macro_edits.clear();
        let events = self.host.poll();
        self.carried.extend(events.into_iter().filter(|e| {
            matches!(
                e,
                PluginManagerEvent::ScanFinished(_) | PluginManagerEvent::Log { device: None, .. }
            )
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::engine_sync::{EngineSync, tests::wait_until};
    use komponist_model::edits;

    #[test]
    fn the_compiler_uses_the_authored_clap_parameter_ids() {
        let host = App::headless_plugin_host();
        let prepared = host
            .manager()
            .prepare_bundled_device(
                "org.klang-studio.komponist.hexa",
                Some("E-Piano"),
                &[],
                &["feedback", "volume", "spread"],
            )
            .expect("build the Hexa fixture with scripts/build-test-plugins.sh");
        let (mut project, track) = edits::add_track(
            &Project::new("Native automation"),
            edits::NewTrack {
                name: None,
                hue: None,
                instrument: Some(prepared.plugin.clone()),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let targets = prepared.targets.clone();
        let index = project.track_index(track).unwrap();
        let entry = &mut Arc::make_mut(&mut project.tracks[index]).channel.devices[0];
        let device = entry.id;
        *entry = Arc::new(prepared.into_device(device));
        let doc = Document::new(project);
        let mut sync = EngineSync::null(Box::new(host));
        sync.sync(&doc);
        assert!(wait_until(|| {
            sync.poll();
            sync.host().params(device).is_some()
        }));
        let model = doc.project().find_device(device).unwrap().2;
        let env = Env {
            midi: None,
            host: sync.host(),
            rate: 48_000.0,
        };
        for (name, (id, normalized)) in targets {
            let parameter = env
                .automation_parameter(model, &id)
                .unwrap_or_else(|| panic!("{name}"));
            assert_eq!(parameter.wire_id, id.plugin_id().unwrap());
            let default = sync.host().param_base(device, parameter.wire_id).unwrap();
            assert!((default - parameter.base).abs() < 1e-6, "{name}");
            assert!(
                (parameter.map.plain(normalized) - parameter.base).abs() < 1e-6,
                "{name}"
            );
        }
    }
}
