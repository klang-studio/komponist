//! Applies track macro gestures and mapping changes to the document.

use komponist_engine::automation::PluginParamBatch;
use komponist_engine::processor::ParameterEditKind;
use komponist_engine::{Command, SlotId};
use komponist_model::automation::{AutomationOwner, AutomationTarget};
use komponist_model::edits::{self, ChannelProp, EditError, SendProp, TrackProp, macros};
use komponist_model::macros::{MAX_MAPPINGS, Mapping, initial_range};
use komponist_model::{DeviceId, Project, TrackId};

use super::App;
use crate::actions::SelectMode;
use crate::device_host::LegacyParameters;
use crate::ui_state::StatusKind;
use crate::views::automation::{Catalog, automation_control_value};
use crate::views::track_macros::{Action, effective_value, reference};

impl App {
    pub(super) fn track_macros_track(&self) -> Option<TrackId> {
        crate::views::browser::model::selected_track(self.project(), &self.ui.selection.tracks)
    }

    pub(super) fn track_macro_action(&mut self, action: Action) {
        match action {
            Action::Select { track, index } => {
                if self.track_macro_ref(track, index).is_some() {
                    self.ui.track_macros.selected.insert(track, index);
                }
            }
            Action::ToggleCollapsed { track } => {
                if !self.ui.track_macros.collapsed.remove(&track) {
                    self.ui.track_macros.collapsed.insert(track);
                }
            }
            Action::ToggleMapping { track } => {
                if self.ui.track_macros.mapping == Some(track) {
                    self.track_macro_action(Action::ExitMapping);
                } else if self.project().track(track).is_some() {
                    self.end_gesture();
                    self.ui.track_macros.mapping = Some(track);
                    self.ui.track_macros.collapsed.remove(&track);
                }
            }
            Action::ExitMapping => {
                if self.ui.track_macros.mapping.is_some() {
                    self.end_gesture();
                    self.ui.track_macros.mapping = None;
                }
            }
            Action::BeginValue { track, index } => {
                if let Some(label) = self.track_macro_undo(track, index) {
                    self.end_gesture();
                    self.doc.begin_gesture(label);
                    self.note_macro_gesture(track, index);
                }
            }
            Action::SetValue {
                track,
                index,
                value,
            } => {
                let Some(label) = self.track_macro_undo(track, index) else {
                    return;
                };
                if value.is_finite()
                    && self.manual_automation(
                        AutomationOwner::Track(track),
                        &AutomationTarget::Macro { index },
                        Command::SetMacro {
                            track,
                            index,
                            value,
                            edit: 0,
                        },
                    )
                {
                    return;
                }
                let before = self.doc.revision();
                self.apply(&label, |project| {
                    macros::set_macro_value(project, track, index, value)
                });
                let after = self.doc.revision();
                if after != before {
                    self.engine.refresh_snapshot_automation(self.doc.project());
                    let command = self.engine.macro_value_command(
                        track,
                        index,
                        crate::engine_sync::MacroValueEdit::Base(value.clamp(0.0, 1.0)),
                    );
                    if self.doc.in_gesture() {
                        self.engine.fast_path(command, before, after);
                    } else {
                        self.engine.send(command);
                    }
                }
            }
            Action::Rename { track, index, name } => {
                self.apply("undo-macro-rename", |p| {
                    macros::rename_macro(p, track, index, name)
                });
            }
            Action::Map {
                track,
                index,
                target,
            } => {
                self.map_track_macro(track, index, target, None);
            }
            Action::Remove {
                track,
                index,
                target,
            } => {
                self.unmap_track_macro(track, index, Some(&target));
            }
            Action::Clear { track, index } => {
                self.unmap_track_macro(track, index, None);
            }
            Action::Range {
                track,
                index,
                target,
                min,
                max,
            } => {
                self.apply("undo-macro-range", |p| {
                    macros::set_mapping_range(p, track, index, &target, min, max)
                });
            }
            Action::Invert {
                track,
                index,
                target,
                invert,
            } => {
                self.apply("undo-macro-invert", |p| {
                    macros::set_mapping_invert(p, track, index, &target, invert)
                });
            }
            Action::Bend {
                track,
                index,
                target,
                bend,
            } => {
                self.apply("undo-macro-bend", |p| {
                    macros::set_mapping_bend(p, track, index, &target, bend)
                });
            }
            Action::ShowDriver { track, target } => {
                self.reveal_track_macro(track, &target);
            }
            Action::Reveal { track, target } => {
                self.reveal_macro_target(track, target);
            }
        }
    }

    fn reveal_macro_target(&mut self, track: TrackId, target: AutomationTarget) {
        if !self
            .project()
            .track(track)
            .is_some_and(|track| track.macros.driver(&target).is_some())
        {
            return;
        }
        self.select_tracks(vec![track], SelectMode::Replace);
        match &target {
            AutomationTarget::Param { device, .. } => {
                self.set_dock_tab(crate::ui_state::DockTab::Devices);
                self.ui.devices.collapsed.remove(device);
                self.ui.devices.reveal = Some((*device, crate::ui_state::Reveal::Visible));
            }
            AutomationTarget::Volume | AutomationTarget::Pan | AutomationTarget::Send { .. } => {
                self.set_dock_tab(crate::ui_state::DockTab::Mixer);
            }
            AutomationTarget::Macro { .. } => return,
        }
        self.ui.track_macros.revealed = Some((track, target));
    }

    fn track_macro_ref(&self, track: TrackId, index: u8) -> Option<String> {
        let slot = self
            .project()
            .track(track)?
            .macros
            .0
            .get(usize::from(index))?;
        Some(reference(self.lang, index, slot))
    }

    fn track_macro_undo(&self, track: TrackId, index: u8) -> Option<String> {
        let slot = self
            .project()
            .track(track)?
            .macros
            .0
            .get(usize::from(index))?;
        Some(crate::undo_label::track_macro(&slot.name, index))
    }

    pub(super) fn macro_target_captures_gesture(
        &self,
        owner: AutomationOwner,
        target: &AutomationTarget,
    ) -> bool {
        let AutomationOwner::Track(track) = owner else {
            return false;
        };
        let mapped = self
            .project()
            .track(track)
            .is_some_and(|t| t.macros.driver(target).is_some());
        (mapped || self.ui.track_macros.mapping == Some(track))
            && self
                .macro_catalog(track)
                .is_some_and(|catalog| catalog.resolve(target).is_ok())
    }

    /// Intercept an ordinary control edit before automation or the parameter writer sees it.
    pub(super) fn handle_macro_target_edit(
        &mut self,
        owner: AutomationOwner,
        target: &AutomationTarget,
    ) -> bool {
        let AutomationOwner::Track(track) = owner else {
            return false;
        };
        if self.ui.track_macros.mapping == Some(track) {
            let index = self.ui.track_macros.selected(track);
            return self
                .map_track_macro(track, index, target.clone(), None)
                .is_some();
        }
        self.reveal_track_macro(track, target)
    }

    fn reveal_track_macro(&mut self, track: TrackId, target: &AutomationTarget) -> bool {
        let Some(index) = self
            .project()
            .track(track)
            .and_then(|t| t.macros.driver(target))
        else {
            return false;
        };
        let Some(descriptor) = self
            .macro_catalog(track)
            .and_then(|catalog| catalog.resolve(target).ok())
        else {
            return false;
        };
        let Some(name) = self.track_macro_ref(track, index) else {
            return false;
        };
        self.select_tracks(vec![track], SelectMode::Replace);
        self.ui.track_macros.selected.insert(track, index);
        self.ui.track_macros.collapsed.remove(&track);
        self.ui.layout.inspector_open = true;
        self.set_status(
            self.lang.status_macros_locked(&descriptor.info.name, name),
            StatusKind::Info,
        );
        true
    }

    fn macro_catalog(&self, track: TrackId) -> Option<Catalog<'_>> {
        Catalog::for_project(
            self.project(),
            &self.engine,
            self.lang,
            AutomationOwner::Track(track),
        )
    }

    /// Returns None for controls that are ineligible, Some(false) for an eligible mapping that
    /// failed, and Some(true) when the target is now driven by this slot.
    fn map_track_macro(
        &mut self,
        track_id: TrackId,
        index: u8,
        target: AutomationTarget,
        previous: Option<f64>,
    ) -> Option<bool> {
        let track = self.project().track(track_id)?;
        let slot = track.macros.0.get(usize::from(index))?;
        let descriptor = self.macro_catalog(track_id)?.resolve(&target).ok()?;
        if slot.mappings.iter().any(|item| item.target == target) {
            return Some(true);
        }
        if slot.mappings.len() >= MAX_MAPPINGS {
            self.record_edit_error(EditError::MacroMappingLimit { index });
            return Some(false);
        }
        let old_driver = track.macros.driver(&target);
        let from_macro = old_driver.and_then(|driver| {
            let slot = &track.macros.0[usize::from(driver)];
            let mapping = slot.mappings.iter().find(|m| m.target == target)?;
            let value = effective_value(self.project(), &self.engine, track, driver);
            Some(
                descriptor
                    .info
                    .to_normalized(descriptor.info.to_plain(f64::from(mapping.value_at(value))))
                    as f32,
            )
        });
        let owner = AutomationOwner::Track(track_id);
        let current = from_macro
            .or_else(|| self.automation_capture.current_value(owner, &target))
            .or_else(|| automation_control_value(self.project(), &self.engine, owner, &target))
            .or_else(|| previous.map(|plain| descriptor.info.to_normalized(plain) as f32))
            .unwrap_or(descriptor.base);
        let value = effective_value(self.project(), &self.engine, track, index);
        let (min, max, invert) = initial_range(value, current);
        let mapping = Mapping {
            target,
            label: descriptor.label,
            min,
            max,
            invert,
            bend: 0.0,
        };

        self.end_gesture();
        let mapped = self.apply("undo-macro-add-mapping", |p| {
            macros::add_mapping(p, track_id, index, mapping)
        });
        if mapped
            && old_driver.is_some()
            && let Some(name) = self.track_macro_ref(track_id, index)
        {
            self.set_status(
                self.lang.status_macros_moved(&descriptor.info.name, name),
                StatusKind::Info,
            );
        }
        Some(mapped)
    }

    pub(super) fn native_plugin_params(
        &mut self,
        slot: SlotId,
        device: DeviceId,
        events: &PluginParamBatch,
        capture: bool,
    ) {
        if !self.engine.plugin_parameter_slot_matches(slot, device) {
            return;
        }
        let mapping = self.ui.track_macros.mapping.is_some_and(|track| {
            self.project()
                .find_device(device)
                .is_some_and(|(owner, _, _)| owner == komponist_model::ChannelRef::Track(track))
        });
        let write_start = events.write_start();
        for (edit, temporary) in events.iter() {
            let id = match edit.kind {
                ParameterEditKind::Value { id, .. }
                | ParameterEditKind::Gesture { id, begin: true } => Some(id),
                ParameterEditKind::Gesture { begin: false, .. } => None,
            };
            let candidate = id.filter(|&id| {
                mapping
                    && !self.automation_capture.plugin_parameter_writing(device, id)
                    && self.engine.plugin_mapping_parameter(device, id)
            });
            let previous = self.engine.update_plugin_parameter(
                device,
                edit.kind,
                temporary,
                candidate.is_some(),
            );
            if let Some(id) = candidate {
                self.map_native_plugin_input(device, id, previous);
            } else if !temporary && matches!(edit.kind, ParameterEditKind::Value { .. }) {
                // An edit in the plugin's own GUI modifies the loaded preset like a card edit.
                let label = crate::undo_label::plugin_state(&self.device_name(device));
                self.mark_preset_modified(device, &label);
            }
            if capture
                && let Some((owner, start)) = write_start
                && std::ptr::eq(edit, start)
                && let Some(id) = id
            {
                let target = komponist_engine::automation::EngineTarget::Slot {
                    slot,
                    device,
                    wire_id: id,
                };
                self.capture_native_automation(
                    device,
                    id,
                    owner,
                    target,
                    edit.tick,
                    candidate.is_some(),
                );
            }
        }
    }

    /// Native plugins edit their own controls before sending the gesture back to the host.
    fn map_native_plugin_input(&mut self, device: DeviceId, id: u32, previous: Option<f64>) {
        let Some((AutomationOwner::Track(track), target)) =
            self.device_automation_target(device, id)
        else {
            return;
        };
        let index = self.ui.track_macros.selected(track);
        let before = self.doc.revision();
        match self.map_track_macro(track, index, target.clone(), previous) {
            Some(true) if self.doc.revision() != before => {
                self.engine.send_project(self.doc.project());
            }
            Some(false) => {
                // The target can be eligible even when its destination macro has no free slots.
                // Restore a native turn that was consumed by the mapping mode.
                if self
                    .project()
                    .track(track)
                    .is_some_and(|owner| owner.macros.driver(&target).is_some())
                {
                    self.engine.send(Command::MacroReassert { track });
                } else if let (Some(value), Some(slot)) = (
                    previous,
                    self.engine.program().and_then(|p| p.slots.slot(device)),
                ) {
                    self.engine.send(Command::SetParam { slot, id, value });
                    self.engine.host_mut().preview_param(device, id, value);
                }
            }
            Some(true) | None => {}
        }
    }

    fn unmap_track_macro(&mut self, track: TrackId, index: u8, target: Option<&AutomationTarget>) {
        let Some(owner) = self.project().track(track) else {
            return;
        };
        let Some(slot) = owner.macros.0.get(usize::from(index)) else {
            return;
        };
        let Some(catalog) = self.macro_catalog(track) else {
            return;
        };
        let macro_value = effective_value(self.project(), &self.engine, owner, index);
        let values: Vec<_> = slot
            .mappings
            .iter()
            .take(MAX_MAPPINGS)
            .filter(|m| target.is_none_or(|target| &m.target == target))
            .filter_map(|m| {
                let info = catalog.resolve(&m.target).ok()?.info;
                let value = f64::from(m.value_at(macro_value));
                Some((m.target.clone(), info.to_plain(value)))
            })
            .collect();

        let label = if target.is_some() {
            "undo-macro-remove-mapping"
        } else {
            "undo-macro-clear"
        };
        if self.apply(label, |p| {
            let mut p = match target {
                Some(target) => macros::remove_mapping(p, track, index, target)?,
                None => macros::clear_macro(p, track, index)?,
            };
            for (target, value) in &values {
                p = preserve_unmapped_value(p, track, target, *value)?;
            }
            Ok(p)
        }) {
            let external: Vec<_> = values
                .into_iter()
                .filter_map(|(target, value)| {
                    let AutomationTarget::Param { device, param } = target else {
                        return None;
                    };
                    Some((device, param.plugin_id()?, value))
                })
                .collect();
            if !external.is_empty() {
                self.engine.send_project(self.doc.project());
                for (device, id, value) in external {
                    if let Some(slot) = self.engine.program().and_then(|p| p.slots.slot(device)) {
                        // A native gesture may still be holding a different value.
                        self.engine.send(Command::SetParam { slot, id, value });
                    }
                }
            }
        }
    }
}

fn preserve_unmapped_value(
    project: Project,
    track: TrackId,
    target: &AutomationTarget,
    value: f64,
) -> edits::EditResult {
    match target {
        AutomationTarget::Volume => edits::set_track_prop(
            &project,
            &[track],
            TrackProp::Channel(ChannelProp::VolumeDb(value as f32)),
        ),
        AutomationTarget::Pan => edits::set_track_prop(
            &project,
            &[track],
            TrackProp::Channel(ChannelProp::Pan(value as f32)),
        ),
        AutomationTarget::Send { send } => {
            edits::set_send(&project, *send, SendProp::LevelDb(value as f32))
        }
        AutomationTarget::Param { device, param } => {
            let state = project.find_device(*device).and_then(|(_, _, d)| {
                let parameters = LegacyParameters::new(d)?;
                parameters
                    .with_value(parameters.wire(param)?, value)
                    .map(Into::into)
            });
            if let Some(state) = state {
                edits::set_device_state(&project, *device, Some(state))
            } else {
                Ok(project)
            }
        }
        AutomationTarget::Macro { .. } => Ok(project),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{Action as AppAction, EditAction, TransportAction};

    #[test]
    fn macro_touch_records_into_its_own_lane_and_keeps_the_base() {
        use crate::views::automation::Action as Automation;
        use komponist_model::Tick;
        use komponist_model::automation::{AutomationMode, Point, Points};

        let (project, track) = edits::add_track(
            &Project::new("Macro automation"),
            edits::NewTrack {
                name: None,
                instrument: None,
                hue: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let owner = AutomationOwner::Track(track);
        let target = AutomationTarget::Macro { index: 0 };
        let (project, lane) =
            edits::automation::add_lane(&project, owner, target.clone(), "Macro".into()).unwrap();
        let before = Points::new(vec![Point::linear(Tick::ZERO, 0.3)]);
        let project =
            edits::automation::set_lane_points(&project, owner, lane, before.clone()).unwrap();
        let mut app = App::headless(project);
        app.engine_mut().hold_engine().unwrap();
        app.step();
        let run = |app: &mut App| {
            assert!(app.engine_mut().run_held(1024, &[], 0));
            app.step();
        };
        run(&mut app);
        assert_eq!(
            app.engine().telemetry().macro_values(track).unwrap()[0],
            0.3
        );
        app.dispatch(AppAction::Automation(Automation::Mode(
            owner,
            lane,
            AutomationMode::Touch,
        )));
        app.step();
        app.dispatch(AppAction::Transport(TransportAction::TogglePlay));
        run(&mut app);
        app.dispatch(AppAction::Macro(Action::BeginValue { track, index: 0 }));
        app.dispatch(AppAction::Macro(Action::SetValue {
            track,
            index: 0,
            value: 0.85,
        }));
        run(&mut app);
        assert!(app.automation_capture.is_writing(owner, lane));
        assert_eq!(
            app.engine().telemetry().macro_values(track).unwrap()[0],
            0.85
        );
        app.dispatch(AppAction::Edit(EditAction::EndGesture));
        run(&mut app);
        app.dispatch(AppAction::Transport(TransportAction::Stop));
        run(&mut app);
        let track = app.project().track(track).unwrap();
        assert_eq!(track.macros.0[0].value, 0.0);
        let points = &track.automation.lane(&target).unwrap().points;
        assert_ne!(points, &before);
        assert!(points.as_slice().iter().any(|p| p.value == 0.85));
    }

    #[test]
    fn a_mapping_uses_pending_base_and_automation_macro_edits() {
        use komponist_model::Tick;
        use komponist_model::automation::{AutomationMode, Point, Points};

        let (project, track) = edits::add_track(
            &Project::new("Macro test"),
            edits::NewTrack {
                name: None,
                instrument: None,
                hue: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let owner = AutomationOwner::Track(track);
        let target = AutomationTarget::Macro { index: 0 };
        for automated in [false, true] {
            let project = if automated {
                let (with_lane, lane) =
                    edits::automation::add_lane(&project, owner, target.clone(), "Macro".into())
                        .unwrap();
                edits::automation::set_lane_points(
                    &with_lane,
                    owner,
                    lane,
                    Points::new(vec![Point::linear(Tick::ZERO, 0.3)]),
                )
                .unwrap()
            } else {
                project.clone()
            };
            let mut app = App::headless(project);
            app.engine_mut().hold_engine().unwrap();
            app.step();
            assert!(app.engine_mut().run_held(128, &[], 0));
            if automated {
                assert_eq!(
                    app.engine()
                        .program()
                        .unwrap()
                        .automation_mode(owner, &target),
                    Some(AutomationMode::Read),
                );
                app.dispatch(AppAction::Macro(Action::BeginValue { track, index: 0 }));
            }

            app.dispatch(AppAction::Macro(Action::SetValue {
                track,
                index: 0,
                value: 0.75,
            }));
            assert_eq!(
                app.engine().telemetry().macro_values(track).unwrap()[0],
                if automated { 0.3 } else { 0.0 },
            );

            app.dispatch(AppAction::Macro(Action::Map {
                track,
                index: 0,
                target: AutomationTarget::Pan,
            }));
            app.step();
            assert!(app.engine_mut().run_held(128, &[], 0));
            let position = app.engine().telemetry().macro_values(track).unwrap()[0];
            assert_eq!(position, 0.75);
            let slot = &app.project().track(track).unwrap().macros.0[0];
            assert_eq!(slot.value, if automated { 0.0 } else { 0.75 });
            assert!((slot.mappings[0].value_at(position) - 0.5).abs() < 1e-6);
            assert_eq!(app.engine().pending_macro_value(track, 0, slot.value), None);
        }
    }

    #[test]
    fn mapping_edits_preserve_the_effective_macro_positions() {
        let (project, track) = edits::add_track(
            &Project::new("Macro test"),
            edits::NewTrack {
                name: None,
                instrument: None,
                hue: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let mut app = App::headless(project);
        app.engine_mut().hold_engine().unwrap();
        app.step();

        let render_at = |app: &mut App, positions: &[(u8, f32)]| {
            for &(index, value) in positions {
                app.engine_mut().send(Command::SetMacro {
                    track,
                    index,
                    value,
                    edit: 0,
                });
            }
            app.engine_mut().run_held_capture(128, &[], 0).unwrap();
        };
        let target = AutomationTarget::Pan;
        render_at(&mut app, &[(0, 0.25)]);
        app.map_track_macro(track, 0, target.clone(), None).unwrap();
        let original = app.project().track(track).unwrap().macros.0[0].mappings[0].clone();
        assert!((original.value_at(0.25) - 0.5).abs() < 1e-6);
        app.step();

        render_at(&mut app, &[(0, 0.75), (1, 0.6)]);
        app.map_track_macro(track, 1, target.clone(), Some(-1.0))
            .unwrap();
        let owner = app.project().track(track).unwrap();
        assert!(owner.macros.0[0].mappings.is_empty());
        assert_eq!(owner.macros.0[1].value, 0.0);
        let reassigned = owner.macros.0[1].mappings[0].clone();
        assert!((reassigned.value_at(0.6) - original.value_at(0.75)).abs() < 1e-6);
        app.step();

        render_at(&mut app, &[(1, 0.8)]);
        app.unmap_track_macro(track, 1, Some(&target));
        let owner = app.project().track(track).unwrap();
        assert!(owner.macros.0[1].mappings.is_empty());
        assert!((owner.channel.pan - (2.0 * reassigned.value_at(0.8) - 1.0)).abs() < 1e-6);
    }

    #[test]
    fn mapping_mode_preserves_pan_then_the_macro_pans_a_real_synth() {
        use crate::device_host::DeviceHost as _;
        use komponist_ipc::control::RenderMode;
        use komponist_plugins::DeviceStatus;
        use std::time::{Duration, Instant};

        let host = App::headless_plugin_host();
        let instrument = host
            .catalog()
            .entries()
            .iter()
            .find(|entry| entry.plugin.id == "org.klang-studio.komponist.volt")
            .expect("build the Volt fixture with scripts/build-test-plugins.sh")
            .plugin
            .clone();
        let (project, track) = edits::add_track(
            &Project::new("Macro test"),
            edits::NewTrack {
                name: Some("Volt".into()),
                instrument: Some(instrument),
                hue: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let device = project
            .track(track)
            .unwrap()
            .channel
            .instrument()
            .unwrap()
            .1
            .id;
        let mut app = App::headless(project);
        let wait_until_ready = |app: &mut App| {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                app.step();
                let host = app.engine().host();
                if host.status(device) == DeviceStatus::Ok && host.params(device).is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "the Volt fixture did not activate: {:?}",
                    host.status(device)
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };

        wait_until_ready(&mut app);
        if app.engine().host().bridge_pid(device).is_some() {
            // The held engine has no real-time deadline to meet while a bridge processes audio.
            app.engine_mut()
                .host_mut()
                .set_render_mode(RenderMode::Offline);
            wait_until_ready(&mut app);
        }
        app.engine_mut().hold_engine().unwrap();

        app.dispatch(AppAction::Macro(Action::ToggleMapping { track }));
        app.dispatch(AppAction::Edit(EditAction::SetTrackProp {
            tracks: vec![track],
            prop: TrackProp::Channel(ChannelProp::Pan(-0.75)),
        }));
        let owner = app.project().track(track).unwrap();
        assert_eq!(owner.channel.pan, 0.0);
        let mapping = &owner.macros.0[0].mappings[0];
        assert_eq!(mapping.target, AutomationTarget::Pan);
        assert_eq!(mapping.value_at(0.0), 0.5);
        app.dispatch(AppAction::Macro(Action::ToggleMapping { track }));
        wait_until_ready(&mut app);

        app.dispatch(AppAction::Transport(TransportAction::LiveNote {
            track: Some(track),
            key: 60,
            velocity: 100,
        }));
        let (left, right) = app.engine_mut().run_held_capture(4096, &[], 0).unwrap();
        let energy = |samples: &[f32]| samples.iter().map(|v| v * v).sum::<f32>();
        assert!(energy(&left) > 1e-5 && energy(&right) > 1e-5);

        app.dispatch(AppAction::Macro(Action::BeginValue { track, index: 0 }));
        app.dispatch(AppAction::Macro(Action::SetValue {
            track,
            index: 0,
            value: 1.0,
        }));
        wait_until_ready(&mut app);
        let (right_panned_l, right_panned_r) =
            app.engine_mut().run_held_capture(4096, &[], 0).unwrap();
        let left_energy = energy(&right_panned_l[2048..]);
        let right_energy = energy(&right_panned_r[2048..]);
        assert!(right_energy > 1e-5 && right_energy > 10.0 * left_energy);
        assert_eq!(
            app.engine().telemetry().macro_values(track).unwrap()[0],
            1.0
        );
        app.dispatch(AppAction::Edit(EditAction::EndGesture));

        app.dispatch(AppAction::Macro(Action::Remove {
            track,
            index: 0,
            target: AutomationTarget::Pan,
        }));
        let owner = app.project().track(track).unwrap();
        assert!(owner.macros.0[0].mappings.is_empty());
        assert_eq!(owner.channel.pan, 1.0);
        app.dispatch(AppAction::Edit(EditAction::Undo));
        let owner = app.project().track(track).unwrap();
        assert_eq!(owner.macros.driver(&AutomationTarget::Pan), Some(0));
        assert_eq!(owner.channel.pan, 0.0);
    }
}
