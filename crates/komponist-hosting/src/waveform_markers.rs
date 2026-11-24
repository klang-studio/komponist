//! Project-state capture for waveform marker edits on either hosting route.

use komponist_ipc::waveform_markers::{EditResult, EditStatus, MAX_STATE_BYTES, Request};

use crate::HostedPlugin;

/// Run one revision-checked edit between two opaque project state captures. The host must
/// exclude processing, and pass the persistent parameter values expected at the processor.
pub fn edit_waveform_marker_state(
    plugin: &mut dyn HostedPlugin,
    slot: u32,
    request: Request,
    expected: &[(u32, f64)],
) -> EditResult {
    let Some(snapshot) = plugin.waveform_markers(slot) else {
        return EditResult::Rejected(EditStatus::Unavailable);
    };
    if snapshot.revision != request.revision {
        return EditResult::Rejected(EditStatus::Stale);
    }
    if !snapshot.editable
        || plugin.has_unapplied_edits()
        || expected.iter().any(|&(id, value)| {
            !value.is_finite()
                || !plugin
                    .param_value(id)
                    .is_some_and(|actual| (actual - value).abs() <= 1e-6 * value.abs().max(1.0))
        })
    {
        return EditResult::Rejected(EditStatus::Unavailable);
    }
    edit_and_capture(
        plugin,
        |plugin| plugin.save_state().map_err(|error| error.to_string()),
        |plugin| {
            plugin
                .edit_waveform_markers(slot, request)
                .unwrap_or(EditStatus::Unavailable)
        },
        |plugin, state| plugin.load_state(state).map_err(|error| error.to_string()),
    )
}

fn edit_and_capture<T: ?Sized>(
    target: &mut T,
    mut save: impl FnMut(&mut T) -> Result<Vec<u8>, String>,
    edit: impl FnOnce(&mut T) -> EditStatus,
    restore: impl FnOnce(&mut T, &[u8]) -> Result<(), String>,
) -> EditResult {
    let mut capture = |target: &mut T| {
        let state = save(target)?;
        if state.len() > MAX_STATE_BYTES {
            return Err("Waveform marker state exceeds the capture limit".into());
        }
        Ok(state)
    };
    let previous_state = match capture(target) {
        Ok(state) => state,
        Err(message) => {
            return EditResult::Failed {
                message,
                rollback_failed: false,
            };
        }
    };
    let status = edit(target);
    if status != EditStatus::Applied {
        return EditResult::Rejected(status);
    }
    let message = match capture(target) {
        Ok(state) if state != previous_state => {
            return EditResult::Applied {
                previous_state,
                state,
            };
        }
        Ok(_) => "Plugin did not save the waveform marker edit".into(),
        Err(message) => message,
    };
    match restore(target, &previous_state) {
        Ok(()) => EditResult::Failed {
            message,
            rollback_failed: false,
        },
        Err(rollback) => EditResult::Failed {
            message: format!("{message}. Restoring the previous state failed: {rollback}"),
            rollback_failed: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_applied_edit_captures_both_project_states() {
        let mut marker = 1;
        let result = edit_and_capture(
            &mut marker,
            |marker| Ok(vec![*marker]),
            |marker| {
                *marker = 2;
                EditStatus::Applied
            },
            |_, _| panic!("successful edit must not be rolled back"),
        );
        assert_eq!(
            result,
            EditResult::Applied {
                previous_state: vec![1],
                state: vec![2],
            }
        );
        assert_eq!(marker, 2);
    }

    #[test]
    fn an_edit_that_does_not_apply_is_never_rolled_back() {
        let mut capture_count = 0;
        let result = edit_and_capture(
            &mut (),
            |()| {
                capture_count += 1;
                Ok(vec![1])
            },
            |()| EditStatus::Stale,
            |(), _| panic!("rejected edit must not be rolled back"),
        );
        assert_eq!(result, EditResult::Rejected(EditStatus::Stale));
        assert_eq!(capture_count, 1);
    }

    #[test]
    fn a_failed_capture_after_edit_restores_the_previous_state() {
        let mut marker = 1;
        for rollback_failed in [false, true] {
            let result = edit_and_capture(
                &mut marker,
                |marker| match *marker {
                    1 => Ok(vec![1]),
                    _ => Err("save failed".into()),
                },
                |marker| {
                    *marker = 2;
                    EditStatus::Applied
                },
                |marker, state| {
                    if rollback_failed {
                        return Err("restore failed".into());
                    }
                    *marker = state[0];
                    Ok(())
                },
            );
            assert!(matches!(
                result,
                EditResult::Failed { rollback_failed: actual, .. } if actual == rollback_failed
            ));
            assert_eq!(marker, if rollback_failed { 2 } else { 1 });
        }
    }

    #[test]
    fn a_failed_initial_capture_never_applies_the_edit() {
        let result = edit_and_capture(
            &mut (),
            |()| Err("save failed".into()),
            |()| panic!("failed capture must stop before editing"),
            |(), _| panic!("untouched state must not be rolled back"),
        );
        assert!(matches!(
            result,
            EditResult::Failed {
                rollback_failed: false,
                ..
            }
        ));
    }
}
