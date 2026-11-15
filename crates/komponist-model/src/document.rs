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
//! Device parameters and plugin states are tracked like everything else (owner decision
//! 2026-11-15): the app wraps a knob drag or a wheel burst in a gesture. Only transport settings
//! and the like go through [`Document::apply_untracked`].
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
    /// Identifies the step in this document ([`Document::top_step`], [`Document::amend`]); unique,
    /// never reused.
    pub id: u64,
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
    next_step: u64,
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
            next_step: 0,
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
        self.apply_with(label, |p| edit(p).map(|p| (p, ())))
    }

    /// Like [`Document::apply`] for edits that also return a value (the id of a created object).
    pub fn apply_with<T>(
        &mut self,
        label: impl Into<String>,
        edit: impl FnOnce(&Project) -> Result<(Project, T), EditError>,
    ) -> Result<T, EditError> {
        let (next, value) = edit(&self.current)?;
        let previous = std::mem::replace(&mut self.current, Arc::new(next));
        if self.gesture.is_none() {
            self.push_undo(label.into(), previous);
        }
        self.revision += 1;
        Ok(value)
    }

    /// Applies `edit` outside the history: to the current root (its error is returned) and then to
    /// every undo and redo snapshot and the open gesture's snapshot (errors there are ignored, that
    /// snapshot stays as it was).
    ///
    /// For values that must not be undone and must not be reverted by undo: transport settings
    /// (loop on/off, metronome, count-in), the loop range set from the ruler, sample paths
    /// rewritten for the file being saved and the plugin state snapshots that merely echo an undo
    /// (`edits::set_device_state`). Device parameters and a plugin's own edits are tracked, not
    /// untracked. Marks the document dirty.
    pub fn apply_untracked(
        &mut self,
        edit: impl Fn(&Project) -> Result<Project, EditError>,
    ) -> Result<(), EditError> {
        let next = Arc::new(edit(&self.current)?);
        let rebase = |snapshot: &mut Arc<Project>| {
            if let Ok(p) = edit(snapshot) {
                *snapshot = Arc::new(p);
            }
        };
        self.undo.iter_mut().for_each(|s| rebase(&mut s.project));
        self.redo.iter_mut().for_each(|s| rebase(&mut s.project));
        if let Some(gesture) = &mut self.gesture {
            if Arc::ptr_eq(&gesture.before, &self.current) {
                // Nothing tracked happened in the gesture yet; keep it that way for `end_gesture`.
                gesture.before = Arc::clone(&next);
            } else {
                rebase(&mut gesture.before);
            }
        }
        self.current = next;
        self.revision += 1;
        Ok(())
    }

    /// Starts a gesture labelled `label`. A gesture already open is ended first
    /// ([`Document::end_gesture`]).
    pub fn begin_gesture(&mut self, label: impl Into<String>) {
        self.end_gesture();
        self.gesture = Some(Gesture {
            label: label.into(),
            before: Arc::clone(&self.current),
        });
    }

    /// During a gesture: applies `edit` to the root from [`Document::begin_gesture`] (not to the
    /// current root) and makes the result current, without pushing. For drags that send the total
    /// delta since the drag start: each call replaces the previous intermediate state, and ids
    /// allocated by the edit are the same on every call. Without an open gesture this is
    /// [`Document::apply`] with the label "".
    pub fn apply_in_gesture(
        &mut self,
        edit: impl FnOnce(&Project) -> Result<Project, EditError>,
    ) -> Result<(), EditError> {
        self.apply_in_gesture_with(|p| edit(p).map(|p| (p, ())))
    }

    /// Like [`Document::apply_in_gesture`] for edits that also return a value (Alt-drag copies
    /// return the ids of the copies).
    pub fn apply_in_gesture_with<T>(
        &mut self,
        edit: impl FnOnce(&Project) -> Result<(Project, T), EditError>,
    ) -> Result<T, EditError> {
        let Some(gesture) = &self.gesture else {
            return self.apply_with("", edit);
        };
        let (next, value) = edit(&gesture.before)?;
        self.current = Arc::new(next);
        self.revision += 1;
        Ok(value)
    }

    /// Whether a gesture is open.
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// Ends the open gesture: pushes one undo step with the root from `begin_gesture` if any
    /// [`Document::apply`] happened since (the root is no longer `Arc::ptr_eq` to it), and clears the
    /// redo stack in that case. No-op without an open gesture.
    pub fn end_gesture(&mut self) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if !Arc::ptr_eq(&gesture.before, &self.current) {
            self.push_undo(gesture.label, gesture.before);
        }
    }

    /// Cancels the open gesture: restores the root from `begin_gesture`, pushes nothing. No-op
    /// without an open gesture.
    pub fn cancel_gesture(&mut self) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if !Arc::ptr_eq(&gesture.before, &self.current) {
            self.current = gesture.before;
            self.revision += 1;
        }
    }

    /// Steps back one undo step (ends an open gesture first). Returns `false` if there is nothing
    /// to undo.
    pub fn undo(&mut self) -> bool {
        self.end_gesture();
        let Some(step) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.current, step.project);
        self.redo.push(UndoStep {
            id: step.id,
            label: step.label,
            project: current,
        });
        self.revision += 1;
        true
    }

    /// Steps forward one redo step. Returns `false` if there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        // An open gesture holds a snapshot of the root; end it first, as `undo` does. If it changed
        // anything, ending it clears the redo stack.
        self.end_gesture();
        let Some(step) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.current, step.project);
        self.undo.push(UndoStep {
            id: step.id,
            label: step.label,
            project: current,
        });
        self.revision += 1;
        true
    }

    /// Id of the step [`Document::undo`] would revert.
    pub fn top_step(&self) -> Option<u64> {
        self.undo.last().map(|s| s.id)
    }

    /// Applies `edit` to the current root as part of the newest undo step instead of a new one, if
    /// that step still has id `step`: nothing was pushed since, no gesture is open and the redo
    /// stack is empty. Returns `Ok(false)` (nothing changes) if not. For a change that follows
    /// its own step closely, such as the plugin GUI's state snapshots (one step per editing burst).
    pub fn amend(
        &mut self,
        step: u64,
        edit: impl FnOnce(&Project) -> Result<Project, EditError>,
    ) -> Result<bool, EditError> {
        if self.gesture.is_some() || !self.redo.is_empty() || self.top_step() != Some(step) {
            return Ok(false);
        }
        self.current = Arc::new(edit(&self.current)?);
        self.revision += 1;
        Ok(true)
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

    /// Pushes a new undo step: clears the redo stack and drops the oldest step beyond
    /// [`UNDO_LIMIT`].
    fn push_undo(&mut self, label: String, project: Arc<Project>) {
        self.redo.clear();
        self.next_step += 1;
        self.undo.push(UndoStep {
            id: self.next_step,
            label,
            project,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(name: &str) -> impl Fn(&Project) -> Result<Project, EditError> + '_ {
        move |p| {
            let mut p = p.clone();
            p.name = name.to_owned();
            Ok(p)
        }
    }

    fn fail(_: &Project) -> Result<Project, EditError> {
        Err(EditError::Invalid("test"))
    }

    fn name(doc: &Document) -> &str {
        &doc.project().name
    }

    #[test]
    fn apply_pushes_and_undo_redo_swap_roots() {
        let mut doc = Document::new(Project::new("A"));
        assert!(!doc.is_dirty());
        assert_eq!(doc.undo_label(), None);

        doc.apply("Umbenennen", rename("B")).unwrap();
        doc.apply("Umbenennen 2", rename("C")).unwrap();
        assert_eq!(name(&doc), "C");
        assert_eq!(doc.undo_label(), Some("Umbenennen 2"));
        assert_eq!(doc.revision(), 2);
        assert!(doc.is_dirty());

        assert!(doc.undo());
        assert_eq!(name(&doc), "B");
        assert_eq!(doc.redo_label(), Some("Umbenennen 2"));
        assert!(doc.undo());
        assert_eq!(name(&doc), "A");
        assert!(!doc.undo());
        assert!(doc.redo());
        assert!(doc.redo());
        assert_eq!(name(&doc), "C");
        assert!(!doc.redo());
        assert_eq!(doc.revision(), 6);
    }

    #[test]
    fn undo_restores_the_same_root() {
        let mut doc = Document::new(Project::new("A"));
        let before = Arc::clone(doc.project());
        doc.apply("x", rename("B")).unwrap();
        let after = Arc::clone(doc.project());
        doc.undo();
        assert!(Arc::ptr_eq(doc.project(), &before));
        doc.redo();
        assert!(Arc::ptr_eq(doc.project(), &after));
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.undo();
        doc.apply("2", rename("C")).unwrap();
        assert_eq!(doc.redo_label(), None);
        assert!(!doc.redo());
    }

    #[test]
    fn errors_change_nothing() {
        let mut doc = Document::new(Project::new("A"));
        let root = Arc::clone(doc.project());
        assert_eq!(doc.apply("x", fail), Err(EditError::Invalid("test")));
        assert!(Arc::ptr_eq(doc.project(), &root));
        assert_eq!(doc.revision(), 0);
        assert_eq!(doc.undo_label(), None);
        assert!(doc.apply_untracked(fail).is_err());
        assert_eq!(doc.revision(), 0);
    }

    #[test]
    fn apply_with_returns_the_value() {
        let mut doc = Document::new(Project::new("A"));
        let id = doc
            .apply_with("Spur", |p| {
                let mut p = p.clone();
                let id = p.alloc_id();
                Ok((p, id))
            })
            .unwrap();
        assert_eq!(id, 1);
        assert_eq!(doc.project().next_id, 2);
    }

    #[test]
    fn a_gesture_is_one_undo_step() {
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("Ziehen");
        assert!(doc.in_gesture());
        // Total-delta updates: every call starts from the root at `begin_gesture`.
        for n in 1..=3 {
            let id = doc
                .apply_in_gesture_with(|p| {
                    let mut p = p.clone();
                    let id = p.alloc_id();
                    p.name = format!("A{n}");
                    Ok((p, id))
                })
                .unwrap();
            assert_eq!(id, 1, "ids are the same on every call");
        }
        assert_eq!(name(&doc), "A3");
        assert_eq!(doc.undo_label(), None);
        // A plain apply during the gesture also joins it.
        doc.apply("ignored", rename("A4")).unwrap();
        doc.end_gesture();
        assert!(!doc.in_gesture());
        assert_eq!(doc.undo_label(), Some("Ziehen"));
        doc.undo();
        assert_eq!(name(&doc), "A");
        assert_eq!(doc.undo_label(), None);
    }

    #[test]
    fn an_empty_gesture_pushes_nothing() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.undo();
        doc.begin_gesture("Klick");
        doc.end_gesture();
        assert_eq!(doc.undo_label(), None);
        assert_eq!(
            doc.redo_label(),
            Some("1"),
            "redo survives an empty gesture"
        );
        // A failing update changes nothing either.
        doc.begin_gesture("Klick");
        assert!(doc.apply_in_gesture(fail).is_err());
        doc.end_gesture();
        assert_eq!(doc.undo_label(), None);
    }

    #[test]
    fn cancel_restores_the_gesture_start() {
        let mut doc = Document::new(Project::new("A"));
        let root = Arc::clone(doc.project());
        doc.begin_gesture("Ziehen");
        doc.apply_in_gesture(rename("B")).unwrap();
        let revision = doc.revision();
        doc.cancel_gesture();
        assert!(Arc::ptr_eq(doc.project(), &root));
        assert!(doc.revision() > revision);
        assert_eq!(doc.undo_label(), None);
        // No-op without a gesture.
        doc.cancel_gesture();
        doc.end_gesture();
        assert!(Arc::ptr_eq(doc.project(), &root));
    }

    #[test]
    fn begin_gesture_ends_an_open_one() {
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("1");
        doc.apply_in_gesture(rename("B")).unwrap();
        doc.begin_gesture("2");
        assert_eq!(doc.undo_label(), Some("1"));
        doc.apply_in_gesture(rename("C")).unwrap();
        doc.end_gesture();
        doc.undo();
        assert_eq!(name(&doc), "B");
    }

    #[test]
    fn apply_in_gesture_without_gesture_is_apply() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply_in_gesture(rename("B")).unwrap();
        assert_eq!(doc.undo_label(), Some(""));
    }

    #[test]
    fn undo_and_redo_end_an_open_gesture() {
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("Ziehen");
        doc.apply_in_gesture(rename("B")).unwrap();
        assert!(doc.undo());
        assert!(!doc.in_gesture());
        assert_eq!(name(&doc), "A");

        doc.begin_gesture("Ziehen");
        doc.apply_in_gesture(rename("C")).unwrap();
        assert!(!doc.redo(), "the gesture's step clears the redo stack");
        assert_eq!(name(&doc), "C");
        assert_eq!(doc.undo_label(), Some("Ziehen"));
    }

    #[test]
    fn untracked_edits_survive_undo_and_redo() {
        let set_loop = |on: bool| {
            move |p: &Project| {
                let mut p = p.clone();
                p.loop_on = on;
                Ok(p)
            }
        };
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.apply("2", rename("C")).unwrap();
        doc.undo();
        let revision = doc.revision();
        doc.mark_saved();
        doc.apply_untracked(set_loop(true)).unwrap();
        assert!(doc.is_dirty());
        assert!(doc.revision() > revision);
        assert_eq!(doc.undo_label(), Some("1"));
        assert!(doc.project().loop_on);
        doc.undo();
        assert!(doc.project().loop_on && name(&doc) == "A");
        doc.redo();
        doc.redo();
        assert!(doc.project().loop_on && name(&doc) == "C");
    }

    #[test]
    fn untracked_errors_on_snapshots_are_ignored() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        // Fails for the snapshot named "A" only.
        let only_b = |p: &Project| {
            if p.name == "A" {
                return Err(EditError::Invalid("snapshot"));
            }
            let mut p = p.clone();
            p.metronome = true;
            Ok(p)
        };
        doc.apply_untracked(only_b).unwrap();
        assert!(doc.project().metronome);
        doc.undo();
        assert!(!doc.project().metronome);
    }

    #[test]
    fn untracked_edits_in_a_gesture() {
        let metronome = |p: &Project| {
            let mut p = p.clone();
            p.metronome = true;
            Ok(p)
        };
        // Without a tracked change the gesture stays empty.
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("Ziehen");
        doc.apply_untracked(metronome).unwrap();
        doc.end_gesture();
        assert_eq!(doc.undo_label(), None);
        assert!(doc.project().metronome);

        // With one, the gesture start gets the untracked value too.
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("Ziehen");
        doc.apply_in_gesture(rename("B")).unwrap();
        doc.apply_untracked(metronome).unwrap();
        doc.apply_in_gesture(rename("C")).unwrap();
        assert!(
            doc.project().metronome,
            "the next update starts from the rebased snapshot"
        );
        doc.end_gesture();
        doc.undo();
        assert!(doc.project().metronome && name(&doc) == "A");
    }

    #[test]
    fn history_is_limited() {
        let mut doc = Document::new(Project::new("0"));
        for n in 1..=UNDO_LIMIT + 3 {
            doc.apply(n.to_string(), rename(&n.to_string())).unwrap();
        }
        let mut undone = 0;
        while doc.undo() {
            undone += 1;
        }
        assert_eq!(undone, UNDO_LIMIT);
        assert_eq!(name(&doc), "3");
    }

    #[test]
    fn saving_clears_dirty() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        assert!(doc.is_dirty());
        doc.mark_saved();
        assert!(!doc.is_dirty());
        doc.undo();
        assert!(doc.is_dirty());
    }
}
