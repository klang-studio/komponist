//! Applies track macro gestures and mapping changes to the document.

use komponist_engine::Command;
use komponist_model::automation::{AutomationOwner, AutomationTarget};
use komponist_model::edits::{self, ChannelProp, EditError, SendProp, TrackProp, macros};
use komponist_model::macros::{MAX_MAPPINGS, Mapping, initial_range};
use komponist_model::{DeviceId, Project, TrackId};

use super::App;
use crate::actions::SelectMode;
use crate::device_host::{internal_kind, is_hexa};
use crate::param_undo::device_param_state;
use crate::ui_state::StatusKind;
use crate::views::automation::{Catalog, automation_control_value};
use crate::views::track_macros::{Action, reference};

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
                    self.ui.track_macros.mapping = None;
                    return;
                }
                if self.project().track(track).is_some() {
                    self.ui.track_macros.mapping = Some(track);
                    self.ui.track_macros.collapsed.remove(&track);
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
                let before = self.doc.revision();
                self.apply(&label, |project| {
                    macros::set_macro_value(project, track, index, value)
                });
                let after = self.doc.revision();
                if after != before {
                    let command = Command::SetMacro {
                        track,
                        index,
                        value: value.clamp(0.0, 1.0),
                    };
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
            self.record_edit_error(EditError::MacroMappingLimit);
            self.set_status(self.lang.status_macros_full(), StatusKind::Error);
            return Some(false);
        }
        let old_driver = track.macros.driver(&target);
        let from_macro = old_driver.and_then(|driver| {
            let slot = &track.macros.0[usize::from(driver)];
            let mapping = slot.mappings.iter().find(|m| m.target == target)?;
            Some(
                descriptor.info.to_normalized(
                    descriptor
                        .info
                        .to_plain(f64::from(mapping.value_at(slot.value))),
                ) as f32,
            )
        });
        let owner = AutomationOwner::Track(track_id);
        let current = previous
            .map(|plain| descriptor.info.to_normalized(plain) as f32)
            .or(from_macro)
            .or_else(|| self.automation_capture.current_value(owner, &target))
            .or_else(|| automation_control_value(self.project(), &self.engine, owner, &target))
            .unwrap_or(descriptor.base);
        let (min, max, invert) = initial_range(slot.value, current);
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

    /// Native plugins edit their own controls before sending the gesture back to the host.
    pub(super) fn map_native_plugin_input(
        &mut self,
        device: DeviceId,
        id: u32,
        previous: Option<f64>,
    ) {
        let Some((AutomationOwner::Track(track), target)) =
            self.device_automation_target(device, id)
        else {
            return;
        };
        if self.ui.track_macros.mapping != Some(track) {
            return;
        }
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
        let values: Vec<_> = slot
            .mappings
            .iter()
            .take(MAX_MAPPINGS)
            .filter(|m| target.is_none_or(|target| &m.target == target))
            .filter_map(|m| {
                let info = catalog.resolve(&m.target).ok()?.info;
                let value = f64::from(m.value_at(slot.value));
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
                let wire = if is_hexa(d) {
                    komponist_builtin::device::param(param)
                        .map(|field| field.wire)
                        .or_else(|| {
                            komponist_builtin::Macro::ALL
                                .into_iter()
                                .find(|m| m.key() == param.as_str())
                                .map(|m| m.id())
                        })?
                } else {
                    internal_kind(d)?.wire(param)?
                };
                device_param_state(d, wire, value)
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
    use komponist_builtin::Internal;

    #[test]
    fn mapping_mode_preserves_pan_then_the_macro_pans_a_real_synth() {
        let (project, track) = edits::add_track(
            &Project::new("Macro test"),
            edits::NewTrack {
                name: Some("Volt".into()),
                instrument: Some(Internal::Volt.plugin_ref()),
                hue: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let mut app = App::headless(project);
        app.engine_mut().hold_engine().unwrap();
        app.step();

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
        app.step();

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
        app.step();
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
