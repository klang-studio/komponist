//! The edited document with undo, redo and gestures (ARCHITECTURE §5.3, decision D8).
//!
//! Undo keeps whole immutable snapshots (`Arc<Project>`); thanks to structural sharing a snapshot
//! costs only the nodes an edit touched. Undo and redo swap roots.
//!
//! A drag is one *gesture*: [`Document::begin_gesture`] remembers the root, every
//! [`Document::apply`] during the gesture replaces the current root without pushing, and
//! [`Document::end_gesture`] pushes one undo step (if anything changed). Recording uses the same
//! mechanism: the take grows inside a gesture, stop ends it as one undo step "Take n", Esc cancels it
//! ([`Document::cancel_gesture`]).
//!
//! Thread: UI thread only. The engine and workers get `Arc<Project>` snapshots.

use std::sync::Arc;

use crate::edits::EditError;
use crate::project::Project;

/// Maximum number of undo steps kept; the oldest step is dropped beyond it.
pub const UNDO_LIMIT: usize = 500;

/// One entry of the undo or redo stack.
#[derive(Debug, Clone, PartialEq)]
pub struct UndoStep {
    /// Label of the edit for the UI ("Clip verschieben"), chosen by the caller.
    pub label: String,
    /// The root to restore.
    pub project: Arc<Project>,
}

/// An open gesture.
#[derive(Debug, Clone, PartialEq)]
pub struct Gesture {
    /// Label of the undo step the gesture will push.
    pub label: String,
    /// Root at [`Document::begin_gesture`].
    pub before: Arc<Project>,
}

/// The project being edited, with history.
#[derive(Debug, Clone)]
pub struct Document {
    current: Arc<Project>,
    undo: Vec<UndoStep>,
    redo: Vec<UndoStep>,
    gesture: Option<Gesture>,
    revision: u64,
    saved_revision: u64,
}

impl Document {
    /// A document with `project` as its root, empty history, not dirty.
    pub fn new(project: Project) -> Document {
        Document {
            current: Arc::new(project),
            undo: Vec::new(),
            redo: Vec::new(),
            gesture: None,
            revision: 0,
            saved_revision: 0,
        }
    }

    /// The current root.
    pub fn project(&self) -> &Arc<Project> {
        &self.current
    }

    /// Counter that increases on every change of the current root (edit, undo, redo, gesture
    /// update or cancel). `EngineSync` compares it to decide when to recompile.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Applies `edit` to the current root.
    ///
    /// Outside a gesture: on success pushes the previous root as an undo step labelled `label`,
    /// clears the redo stack and returns `Ok`. During a gesture: replaces the root without pushing
    /// (`label` is ignored). On error nothing changes.
    pub fn apply(
        &mut self,
        label: impl Into<String>,
        edit: impl FnOnce(&Project) -> Result<Project, EditError>,
    ) -> Result<(), EditError> {
        todo!("WP-12")
    }

    /// Like [`Document::apply`] for edits that also return a value (the id of a created object).
    pub fn apply_with<T>(
        &mut self,
        label: impl Into<String>,
        edit: impl FnOnce(&Project) -> Result<(Project, T), EditError>,
    ) -> Result<T, EditError> {
        todo!("WP-12")
    }

    /// Applies `edit` outside the history: to the current root (its error is returned) and then to
    /// every undo and redo snapshot and the open gesture's snapshot (errors there are ignored, that
    /// snapshot stays as it was).
    ///
    /// For values that must not be undone and must not be reverted by undo: transport settings
    /// (loop on/off, metronome, count-in), plugin state snapshots (`edits::set_device_state`) and
    /// the loop range set from the ruler. Marks the document dirty.
    pub fn apply_untracked(
        &mut self,
        edit: impl Fn(&Project) -> Result<Project, EditError>,
    ) -> Result<(), EditError> {
        todo!("WP-12")
    }

    /// Starts a gesture labelled `label`. A gesture already open is ended first
    /// ([`Document::end_gesture`]).
    pub fn begin_gesture(&mut self, label: impl Into<String>) {
        todo!("WP-12")
    }

    /// Whether a gesture is open.
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// Ends the open gesture: pushes one undo step with the root from `begin_gesture` if any
    /// [`Document::apply`] happened since (the root is no longer `Arc::ptr_eq` to it), and clears the
    /// redo stack in that case. No-op without an open gesture.
    pub fn end_gesture(&mut self) {
        todo!("WP-12")
    }

    /// Cancels the open gesture: restores the root from `begin_gesture`, pushes nothing. No-op
    /// without an open gesture.
    pub fn cancel_gesture(&mut self) {
        todo!("WP-12")
    }

    /// Steps back one undo step (ends an open gesture first). Returns `false` if there is nothing
    /// to undo.
    pub fn undo(&mut self) -> bool {
        todo!("WP-12")
    }

    /// Steps forward one redo step. Returns `false` if there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        todo!("WP-12")
    }

    /// Label of the step [`Document::undo`] would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    /// Label of the step [`Document::redo`] would restore.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Whether the root changed since the last [`Document::mark_saved`] (or creation). Shown as the
    /// dot after the project name in the transport bar.
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Marks the current root as saved.
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }
}
