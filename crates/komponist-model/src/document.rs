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
//! and the like go through [`Document::apply_untracked`]; sample paths rewritten for a save go
//! through [`Document::rewrite_untracked`] (every step from its own state), and a plugin's echo of
//! an undo through [`Document::apply_untracked_to_root`] (#86).
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
    /// The root at the last save, if undo or redo can still reach it (compared by pointer: undo
    /// and redo swap the very same `Arc`s). `None` once an edit after an undo dropped it from
    /// the history, or the undo limit did; the document is then dirty until the next save.
    saved: Option<Arc<Project>>,
    /// An untracked edit changed the root since the save: not undoable, so undo cannot clear it.
    untracked_dirty: bool,
    next_step: u64,
}

impl Document {
    /// A document with `project` as its root, empty history, not dirty.
    pub fn new(project: Project) -> Document {
        let current = Arc::new(project);
        Document {
            saved: Some(Arc::clone(&current)),
            current,
            undo: Vec::new(),
            redo: Vec::new(),
            gesture: None,
            revision: 0,
            untracked_dirty: false,
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
    /// (`label` is ignored). On error nothing changes. An edit that returns a project equal to the
    /// current root (compared by content, [`Project::same_content`], which skips shared `Arc`s) changes nothing either: no undo
    /// step, no redo clearing, no dirty mark, no revision bump.
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
        if next.same_content(&self.current) {
            // An edit that changes nothing leaves root, history, revision and dirty state alone.
            return Ok(value);
        }
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
    /// For one value that must not be undone and must not be reverted by undo, the same in every
    /// snapshot: transport settings (loop on/off, metronome, count-in), the loop range set from
    /// the ruler, the project name a new file gives. Never for a whole device state: it would
    /// overwrite what every step holds (#86); see [`Document::rewrite_untracked`] and
    /// [`Document::apply_untracked_to_root`]. Device parameters and a plugin's own edits are
    /// tracked, not untracked. An edit that changes the root marks the document dirty until the
    /// next save (undo cannot clear that); one that changes nothing leaves the dirty state alone.
    pub fn apply_untracked(
        &mut self,
        edit: impl Fn(&Project) -> Result<Project, EditError>,
    ) -> Result<(), EditError> {
        let edited = edit(&self.current)?;
        // An edit that changes nothing (a plugin state that merely echoes an undo) keeps the root
        // and the clean state.
        let changed = !edited.same_content(&self.current);
        let next = if changed {
            Arc::new(edited)
        } else {
            Arc::clone(&self.current)
        };
        // Replace a snapshot only when the edit changes it: an equal result keeps the very same
        // `Arc`, so the `saved` pointer stays reachable through undo and redo.
        let rebase = |snapshot: &mut Arc<Project>| {
            if let Ok(p) = edit(snapshot)
                && !p.same_content(snapshot)
            {
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
        self.untracked_dirty |= changed;
        self.revision += 1;
        Ok(())
    }

    /// Rewrites, outside the history, the current root and every snapshot (undo, redo, the open
    /// gesture's start), each from **its own** content: `rewrite` returns the new project, or
    /// `None` to leave that one as it is (the same `Arc`).
    ///
    /// For a transformation of what each state holds rather than one value for all: the sample
    /// paths rewritten for the file being saved, so every step keeps its own parameters and
    /// samples (#86). Returns whether the root changed; that marks the document dirty until the
    /// next save, like [`Document::apply_untracked`].
    pub fn rewrite_untracked(
        &mut self,
        mut rewrite: impl FnMut(&Project) -> Option<Project>,
    ) -> bool {
        let next = rewrite(&self.current).map(Arc::new);
        let mut rebase = |snapshot: &mut Arc<Project>| {
            if let Some(p) = rewrite(snapshot) {
                *snapshot = Arc::new(p);
            }
        };
        self.undo.iter_mut().for_each(|s| rebase(&mut s.project));
        self.redo.iter_mut().for_each(|s| rebase(&mut s.project));
        if let Some(gesture) = &mut self.gesture {
            if Arc::ptr_eq(&gesture.before, &self.current) {
                // Nothing tracked happened in the gesture yet; keep it that way for `end_gesture`.
                if let Some(next) = &next {
                    gesture.before = Arc::clone(next);
                }
            } else {
                rebase(&mut gesture.before);
            }
        }
        let Some(next) = next else {
            return false;
        };
        self.current = next;
        self.untracked_dirty = true;
        self.revision += 1;
        true
    }

    /// Applies `edit` to the current root only, outside the history; the undo and redo snapshots
    /// keep what they hold (its error is returned).
    ///
    /// For bytes that restate what the root already means, not an edit: a bridged plugin's echo of
    /// the state undo or redo handed it, the baseline a bridged device came up with, taken for the
    /// file. Undo does not revert it, and no other step takes it on (#86). An open gesture that
    /// has changed nothing yet starts from the new root. An edit that changes the root marks the
    /// document dirty until the next save, like [`Document::apply_untracked`]; one that changes
    /// nothing changes nothing.
    pub fn apply_untracked_to_root(
        &mut self,
        edit: impl FnOnce(&Project) -> Result<Project, EditError>,
    ) -> Result<(), EditError> {
        let edited = edit(&self.current)?;
        if edited.same_content(&self.current) {
            return Ok(());
        }
        let next = Arc::new(edited);
        if let Some(gesture) = &mut self.gesture
            && Arc::ptr_eq(&gesture.before, &self.current)
        {
            gesture.before = Arc::clone(&next);
        }
        self.current = next;
        self.untracked_dirty = true;
        self.revision += 1;
        Ok(())
    }

    /// Rewrites, outside the history, the current root and every snapshot (undo, redo, the open
    /// gesture's start), each from its own content like [`Document::rewrite_untracked`], for what
    /// a running device restates about itself on every load (the preset a plugin starts in). It
    /// does not make the document dirty: a saved root stays saved in its rewritten form. Returns
    /// whether the root changed.
    pub fn restate(&mut self, mut rewrite: impl FnMut(&Project) -> Option<Project>) -> bool {
        let saved = self.saved.take();
        let mut rebase = |snapshot: &mut Arc<Project>, saved_now: &mut Option<Arc<Project>>| {
            if let Some(p) = rewrite(snapshot) {
                let next = Arc::new(p);
                if saved
                    .as_ref()
                    .is_some_and(|saved| Arc::ptr_eq(saved, snapshot))
                {
                    *saved_now = Some(Arc::clone(&next));
                }
                *snapshot = next;
            }
        };
        let mut saved_now = None;
        let mut current = Arc::clone(&self.current);
        rebase(&mut current, &mut saved_now);
        let changed = !Arc::ptr_eq(&current, &self.current);
        for step in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            rebase(&mut step.project, &mut saved_now);
        }
        if let Some(gesture) = &mut self.gesture {
            if Arc::ptr_eq(&gesture.before, &self.current) {
                gesture.before = Arc::clone(&current);
            } else {
                rebase(&mut gesture.before, &mut saved_now);
            }
        }
        self.saved = saved_now.or(saved);
        if changed {
            self.current = current;
            self.revision += 1;
        }
        changed
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
        if next.same_content(&self.current) {
            return Ok(value);
        }
        // Back at the start of the gesture: take its very `Arc` so the gesture ends without a step.
        self.current = if next.same_content(&gesture.before) {
            Arc::clone(&gesture.before)
        } else {
            Arc::new(next)
        };
        self.revision += 1;
        Ok(value)
    }

    /// Whether a gesture is open.
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// Ends the open gesture: pushes one undo step with the root from `begin_gesture` if the root
    /// differs from it (cheap `Arc::ptr_eq` first, then [`Project::same_content`]), and clears the redo
    /// stack in that case. A gesture that ends where it started leaves no step. No-op without an
    /// open gesture.
    pub fn end_gesture(&mut self) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if !Arc::ptr_eq(&gesture.before, &self.current) {
            if gesture.before.same_content(&self.current) {
                // Equal content: keep the original `Arc` (the `saved` pointer may be it).
                self.current = gesture.before;
            } else {
                self.push_undo(gesture.label, gesture.before);
            }
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

    /// Whether the root differs from the last [`Document::mark_saved`] (or creation). Shown as the
    /// dot after the project name in the transport bar and asked about before closing. Undo or
    /// redo back to the saved root clears it; any other edit sets it, and an edit after undoing
    /// past the saved root makes that root unreachable until the next save.
    pub fn is_dirty(&self) -> bool {
        self.untracked_dirty
            || !self
                .saved
                .as_ref()
                .is_some_and(|saved| Arc::ptr_eq(saved, &self.current))
    }

    /// Marks the current root as saved.
    pub fn mark_saved(&mut self) {
        self.saved = Some(Arc::clone(&self.current));
        self.untracked_dirty = false;
    }

    /// Forgets the saved root: the file was written with more than any root of the history holds
    /// (a running take, which is not part of the document until it ends), so the document is
    /// dirty until the next [`Document::mark_saved`].
    pub fn forget_saved(&mut self) {
        self.saved = None;
        self.untracked_dirty = false;
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
    fn a_restatement_reaches_every_step_stays_clean_and_is_no_step() {
        use crate::edits::{ProjectProp, set_project_prop};
        let mut doc = Document::new(Project::new("Clean"));
        doc.mark_saved();
        doc.apply("rename", |p| {
            set_project_prop(p, ProjectProp::Name("Edited".into()))
        })
        .unwrap();
        // The restatement (here: the loop switch) reaches the saved root behind the edit too.
        let restated = |p: &Project| {
            (!p.loop_on).then(|| {
                set_project_prop(p, ProjectProp::LoopOn(true)).expect("a project property")
            })
        };
        assert!(doc.restate(restated));
        assert!(doc.project().loop_on);
        assert_eq!(doc.undo_label(), Some("rename"));
        assert!(doc.is_dirty(), "the edit still is one");
        assert!(doc.undo());
        assert!(doc.project().loop_on, "undo keeps the restatement");
        assert_eq!(doc.project().name, "Clean");
        assert!(!doc.is_dirty(), "undo returns to the restated saved root");
        assert!(!doc.restate(restated), "a restated root changes no more");
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
    fn amend_joins_the_newest_step_only_while_it_is_the_newest() {
        let mut doc = Document::new(Project::new("A"));
        assert_eq!(doc.top_step(), None);
        assert_eq!(doc.amend(1, rename("X")), Ok(false), "no step yet");
        doc.apply("1", rename("B")).unwrap();
        let first = doc.top_step().unwrap();
        // Amending replaces the root without a new step; undo goes back to before the step.
        assert_eq!(doc.amend(first, rename("C")), Ok(true));
        assert_eq!(doc.amend(first, rename("D")), Ok(true));
        assert_eq!(name(&doc), "D");
        assert_eq!(doc.top_step(), Some(first));
        // Another step makes the first one history; a gesture or a redo stack refuses too.
        doc.apply("2", rename("E")).unwrap();
        assert_eq!(doc.amend(first, rename("F")), Ok(false));
        let second = doc.top_step().unwrap();
        assert_ne!(first, second);
        doc.begin_gesture("g");
        assert_eq!(doc.amend(second, rename("G")), Ok(false));
        doc.end_gesture();
        doc.undo();
        assert_eq!(
            doc.amend(first, rename("H")),
            Ok(false),
            "redo is not empty"
        );
        doc.redo();
        assert_eq!(doc.amend(second, fail), Err(EditError::Invalid("test")));
        assert_eq!(name(&doc), "E");
        // One undo goes back to the root before the first step.
        doc.undo();
        doc.undo();
        assert_eq!(name(&doc), "A");
        assert!(!doc.undo());
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
    fn exhausted_identifiers_reject_edits_without_changing_the_document() {
        let mut project = Project::new("Exhausted");
        project.next_id = u64::MAX;
        assert_eq!(project.alloc_id(), Err(EditError::IdsExhausted));
        assert_eq!(project.next_id, u64::MAX);
        let mut doc = Document::new(project);
        let root = Arc::clone(doc.project());
        assert_eq!(
            doc.apply("Add track", |p| crate::edits::add_track(
                p,
                crate::edits::NewTrack {
                    name: None,
                    hue: None,
                    instrument: None,
                    index: None,
                    arm_exclusive: false
                }
            )
            .map(|(p, _)| p)),
            Err(EditError::IdsExhausted)
        );
        assert!(Arc::ptr_eq(&root, doc.project()));
        assert_eq!(doc.revision(), 0);
        assert_eq!(doc.undo_label(), None);
    }

    #[test]
    fn partial_identifier_allocation_does_not_commit_an_edit() {
        let mut project = Project::new("Nearly exhausted");
        project.next_id = u64::MAX - 2;
        let mut doc = Document::new(project);
        let root = Arc::clone(doc.project());
        let plugin = crate::PluginRef {
            format: crate::PluginFormat::Clap,
            id: "org.klang-studio.komponist.hexa".into(),
            name: crate::project::HEXA_NAME.into(),
            vendor: String::new(),
            version: String::new(),
            module: Box::default(),
        };
        assert_eq!(
            doc.apply("Add instrument", |p| crate::edits::add_track(
                p,
                crate::edits::NewTrack {
                    name: None,
                    hue: None,
                    instrument: Some(plugin),
                    index: None,
                    arm_exclusive: false,
                }
            )
            .map(|(p, _)| p)),
            Err(EditError::IdsExhausted)
        );
        assert!(Arc::ptr_eq(&root, doc.project()));
        assert_eq!(doc.project().next_id, u64::MAX - 2);
        assert_eq!(doc.revision(), 0);
    }

    #[test]
    fn apply_with_returns_the_value() {
        let mut doc = Document::new(Project::new("A"));
        let id = doc
            .apply_with("Spur", |p| {
                let mut p = p.clone();
                let id = p.alloc_id()?;
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
                    let id = p.alloc_id()?;
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

    #[test]
    fn undo_back_to_the_saved_state_is_clean_and_redo_is_dirty() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.mark_saved();
        doc.apply("2", rename("C")).unwrap();
        doc.apply("3", rename("D")).unwrap();
        assert!(doc.is_dirty());
        doc.undo();
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty(), "back at the saved root");
        doc.redo();
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
        // Before the saved root is dirty too, and redo to it is clean again.
        doc.undo();
        assert!(doc.is_dirty());
        doc.redo();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn an_edit_after_undoing_past_the_saved_state_makes_it_unreachable() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.mark_saved();
        doc.undo();
        doc.apply("2", rename("B")).unwrap();
        assert!(doc.is_dirty(), "an equal root is not the saved one");
        doc.undo();
        doc.redo();
        assert!(doc.is_dirty());
        doc.mark_saved();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn an_edit_on_the_saved_state_and_its_undo_cancel_out() {
        let mut doc = Document::new(Project::new("A"));
        doc.mark_saved();
        doc.apply("1", rename("B")).unwrap();
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn gestures_follow_the_saved_state() {
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("drag");
        doc.apply("", rename("B")).unwrap();
        assert!(doc.is_dirty());
        doc.cancel_gesture();
        assert!(!doc.is_dirty(), "cancel restores the saved root");
        doc.begin_gesture("drag");
        doc.apply("", rename("B")).unwrap();
        doc.end_gesture();
        doc.undo();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn an_edit_that_changes_nothing_leaves_history_and_dirty_flag_alone() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.undo();
        let root = Arc::clone(doc.project());
        let revision = doc.revision();
        doc.apply("noop", rename("A")).unwrap();
        assert!(Arc::ptr_eq(&root, doc.project()));
        assert_eq!(doc.revision(), revision);
        assert!(!doc.is_dirty());
        assert_eq!(doc.undo_label(), None);
        assert_eq!(doc.redo_label(), Some("1"), "redo survives a no-op");
        let value = doc.apply_with("noop", |p| Ok((p.clone(), 7))).unwrap();
        assert_eq!(value, 7, "the value still comes back");
        assert_eq!(doc.undo_label(), None);
    }

    #[test]
    fn a_gesture_that_ends_where_it_started_leaves_no_step() {
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("drag");
        doc.apply("", rename("A")).unwrap();
        doc.end_gesture();
        assert_eq!(doc.undo_label(), None);
        assert!(!doc.is_dirty());

        // Moved away and back again.
        doc.begin_gesture("drag");
        doc.apply_in_gesture(rename("B")).unwrap();
        assert!(doc.is_dirty());
        doc.apply_in_gesture(rename("A")).unwrap();
        doc.end_gesture();
        assert_eq!(doc.undo_label(), None);
        assert!(!doc.is_dirty());

        // A non-equal gesture still pushes.
        doc.begin_gesture("drag");
        doc.apply_in_gesture(rename("B")).unwrap();
        doc.end_gesture();
        assert_eq!(doc.undo_label(), Some("drag"));
    }

    #[test]
    fn an_untracked_edit_with_an_equal_result_keeps_the_saved_state_reachable() {
        let mut doc = Document::new(Project::new("A"));
        doc.mark_saved();
        doc.apply("1", rename("B")).unwrap();
        doc.apply_untracked(|p| Ok(p.clone())).unwrap();
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty(), "undo back to the saved root is clean");
        doc.redo();
        assert!(doc.is_dirty());
    }

    #[test]
    fn an_untracked_change_stays_dirty_through_undo_until_saved() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.mark_saved();
        doc.apply_untracked(|p| {
            let mut p = p.clone();
            p.loop_on = !p.loop_on;
            Ok(p)
        })
        .unwrap();
        assert!(doc.is_dirty());
        doc.undo();
        doc.redo();
        assert!(doc.is_dirty());
        doc.mark_saved();
        assert!(!doc.is_dirty());
        // An untracked edit that changes nothing does not dirty the document.
        doc.apply_untracked(|p| Ok(p.clone())).unwrap();
        assert!(!doc.is_dirty());
    }

    /// Appends `suffix` to the name of a project whose name starts with one of `prefixes`;
    /// others stay.
    fn suffix_if<'a>(
        prefixes: &'a [&'a str],
        suffix: &'a str,
    ) -> impl FnMut(&Project) -> Option<Project> + 'a {
        move |p| {
            prefixes.iter().any(|x| p.name.starts_with(x)).then(|| {
                let mut p = p.clone();
                p.name.push_str(suffix);
                p
            })
        }
    }

    #[test]
    fn a_rewrite_transforms_every_snapshot_from_its_own_content() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.apply("2", rename("C")).unwrap();
        doc.undo();
        doc.mark_saved();
        let a = Arc::clone(&doc.undo[0].project);
        // Rewrites B (the root) and C (redo), not A.
        assert!(doc.rewrite_untracked(suffix_if(&["B", "C"], "'")));
        assert_eq!(doc.project().name, "B'");
        assert!(doc.is_dirty());
        assert!(
            Arc::ptr_eq(&doc.undo[0].project, &a),
            "untouched: the same Arc"
        );
        doc.redo();
        assert_eq!(doc.project().name, "C'");
        doc.undo();
        doc.undo();
        assert_eq!(doc.project().name, "A");
        // A rewrite that leaves the root alone does not dirty a clean document.
        doc.mark_saved();
        let revision = doc.revision();
        assert!(!doc.rewrite_untracked(suffix_if(&["C"], "!")));
        assert!(!doc.is_dirty());
        assert_eq!(doc.revision(), revision);
        doc.redo();
        doc.redo();
        assert_eq!(doc.project().name, "C'!");
    }

    #[test]
    fn a_rewrite_keeps_an_untouched_gesture_untouched() {
        let mut doc = Document::new(Project::new("A"));
        doc.begin_gesture("drag");
        assert!(doc.rewrite_untracked(suffix_if(&["A"], "'")));
        doc.end_gesture();
        assert!(doc.undo_label().is_none(), "no step for the rewrite");
        doc.begin_gesture("drag");
        doc.apply("", rename("B")).unwrap();
        doc.rewrite_untracked(suffix_if(&["A"], "!"));
        doc.end_gesture();
        doc.undo();
        assert_eq!(
            doc.project().name,
            "A'!",
            "the gesture's start was rewritten"
        );
    }

    #[test]
    fn an_edit_of_the_root_alone_leaves_every_snapshot_as_it_was() {
        let mut doc = Document::new(Project::new("A"));
        doc.apply("1", rename("B")).unwrap();
        doc.apply("2", rename("C")).unwrap();
        doc.undo();
        doc.apply_untracked_to_root(rename("B'")).unwrap();
        assert_eq!(doc.project().name, "B'");
        assert!(doc.is_dirty());
        assert_eq!(doc.redo_label(), Some("2"), "the redo survives");
        doc.redo();
        assert_eq!(doc.project().name, "C");
        doc.undo();
        assert_eq!(doc.project().name, "B'");
        doc.undo();
        assert_eq!(doc.project().name, "A");
        // An open gesture without changes starts from the new root: no step.
        doc.mark_saved();
        doc.begin_gesture("drag");
        doc.apply_untracked_to_root(rename("A'")).unwrap();
        doc.end_gesture();
        assert!(doc.undo_label().is_none());
        assert_eq!(doc.project().name, "A'");
        // An equal root changes nothing; an error changes nothing either.
        doc.mark_saved();
        let revision = doc.revision();
        doc.apply_untracked_to_root(rename("A'")).unwrap();
        assert!(doc.apply_untracked_to_root(fail).is_err());
        assert!(!doc.is_dirty());
        assert_eq!(doc.revision(), revision);
    }

    #[test]
    fn a_forgotten_saved_state_stays_dirty_until_the_next_save() {
        let mut doc = Document::new(Project::new("A"));
        doc.forget_saved();
        assert!(doc.is_dirty());
        doc.apply("1", rename("B")).unwrap();
        doc.undo();
        assert!(doc.is_dirty(), "no root of the history is the saved one");
        doc.mark_saved();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn the_undo_limit_can_drop_the_saved_state() {
        let mut doc = Document::new(Project::new("A"));
        doc.mark_saved();
        for i in 0..=UNDO_LIMIT {
            doc.apply("e", rename(&format!("n{i}"))).unwrap();
        }
        while doc.undo() {}
        assert!(doc.is_dirty(), "the saved root fell off the history");
    }
}
