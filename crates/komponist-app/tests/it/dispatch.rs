//! Central dispatch without pixels: actions in, document / view state / engine out.

use std::time::{Duration, Instant};

use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::{DockTab, FocusArea, Panel, Tool};
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_engine::TransportState;
use komponist_model::edits::{ChannelProp, TrackProp};
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipContent, Tick};
use komponist_tokens::size;

fn app() -> App {
    App::headless(demo_project())
}

fn ui(a: UiAction) -> Action {
    Action::Ui(a)
}

fn wait_for(app: &mut App, mut done: impl FnMut(&App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        app.engine_mut().poll();
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn transport_toggles_are_not_undone() {
    let mut a = app();
    let loop_on = a.project().loop_on;
    a.dispatch(Action::Transport(TransportAction::ToggleLoop));
    a.dispatch(Action::Transport(TransportAction::ToggleMetronome));
    a.dispatch(Action::Transport(TransportAction::ToggleCountIn));
    assert_eq!(a.project().loop_on, !loop_on);
    assert!(a.project().metronome);
    assert_eq!(a.project().count_in_bars, 0);
    a.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(a.project().loop_on, !loop_on, "untracked");
    a.dispatch(Action::Transport(TransportAction::ToggleCountIn));
    assert_eq!(a.project().count_in_bars, 1);
}

#[test]
fn play_stop_and_go_to_start_drive_the_engine() {
    let mut a = app();
    a.dispatch(Action::Transport(TransportAction::TogglePlay));
    assert!(wait_for(&mut a, |a| a.engine().playhead() > Tick::ZERO));
    assert_eq!(a.transport(), TransportState::Playing);
    a.dispatch(Action::Transport(TransportAction::TogglePlay));
    assert!(wait_for(&mut a, |a| a.transport() == TransportState::Stopped));
    // Stop while stopped: to the loop start (loop on in the demo).
    a.dispatch(Action::Transport(TransportAction::Stop));
    let start = a.project().loop_range.start;
    assert!(wait_for(&mut a, |a| a.engine().playhead() == start));
    a.dispatch(Action::Transport(TransportAction::Locate(Tick(960))));
    assert!(wait_for(&mut a, |a| a.engine().playhead() == Tick(960)));
    a.dispatch(Action::Transport(TransportAction::GoToStart));
    assert!(wait_for(&mut a, |a| a.engine().playhead() == start));
}

/// Play after a change in the same frame plays the changed document: a loop range set just before
/// Play is in the engine before Play starts. (Before the fix the program went out only at the end
/// of the frame; playback started with the old loop, passed the new loop's end within a slow
/// frame, and never wrapped.) The engine is held, so time passes only in `run_held`.
#[test]
fn play_starts_with_a_loop_range_set_in_the_same_frame() {
    let mut a = app();
    a.engine_mut().hold_engine().expect("engine held");
    let start = Tick(8 * 3840 + 4 * 240);
    let end = Tick(start.0 + 240);
    for t in [
        TransportAction::SetLoopRange(komponist_model::TickRange::new(start, end)),
        TransportAction::Locate(start),
        TransportAction::TogglePlay,
    ] {
        a.dispatch(Action::Transport(t));
    }
    // Half a second at 124 bpm: about 990 ticks, four times the loop.
    assert!(a.engine_mut().run_held(24_000, &[], 0));
    assert_eq!(a.transport(), TransportState::Playing);
    let p = a.engine().playhead();
    assert!(
        p >= start && p < end,
        "{p:?} outside the loop {start:?}..{end:?}"
    );
    a.engine_mut().release_engine();
}

#[test]
fn panels_toggle_and_resize_within_their_ranges() {
    let mut a = app();
    a.dispatch(ui(UiAction::TogglePanel(Panel::Browser)));
    a.dispatch(ui(UiAction::TogglePanel(Panel::Inspector)));
    a.dispatch(ui(UiAction::TogglePanel(Panel::Dock)));
    let l = a.ui().layout;
    assert!(!l.browser_open && !l.inspector_open && !l.dock_open);
    a.dispatch(ui(UiAction::ResizePanel {
        panel: Panel::Browser,
        size: 9999.0,
    }));
    a.dispatch(ui(UiAction::ResizePanel {
        panel: Panel::Inspector,
        size: 1.0,
    }));
    a.dispatch(ui(UiAction::ResizePanel {
        panel: Panel::Dock,
        size: 450.0,
    }));
    let l = a.ui().layout;
    assert_eq!(l.browser_width, size::browser::MAX);
    assert_eq!(l.inspector_width, size::inspector::MIN);
    assert_eq!(l.dock_height, 450.0);
    a.dispatch(ui(UiAction::ResetPanelSize(Panel::Dock)));
    a.dispatch(ui(UiAction::ResetPanelSize(Panel::Browser)));
    let l = a.ui().layout;
    assert_eq!(l.dock_height, size::dock::DEFAULT);
    assert_eq!(l.browser_width, size::browser::DEFAULT);
}

#[test]
fn tab_maximises_the_mixer_and_restores_the_layout() {
    let mut a = app();
    a.dispatch(ui(UiAction::ToggleMixer));
    let l = a.ui().layout;
    assert_eq!(l.dock_tab, DockTab::Mixer);
    assert!(l.dock_maximized && l.dock_open);
    assert_eq!(a.ui().focus, FocusArea::Mixer);
    a.dispatch(ui(UiAction::ToggleMixer));
    let l = a.ui().layout;
    assert_eq!(l.dock_tab, DockTab::Editor);
    assert!(!l.dock_maximized);
    assert_eq!(a.ui().focus, FocusArea::Arrangement);

    // Choosing another tab leaves the mixer Tab maximised.
    a.dispatch(ui(UiAction::ToggleMixer));
    a.dispatch(ui(UiAction::SetDockTab(DockTab::Devices)));
    assert!(!a.ui().layout.dock_maximized);
    // Ctrl ↑ maximises on its own and stays.
    a.dispatch(ui(UiAction::ToggleDockMaximized));
    a.dispatch(ui(UiAction::SetDockTab(DockTab::Editor)));
    assert!(a.ui().layout.dock_maximized);
    // Collapsing the dock ends maximise.
    a.dispatch(ui(UiAction::TogglePanel(Panel::Dock)));
    assert!(!a.ui().layout.dock_open && !a.ui().layout.dock_maximized);
    // A tab click opens a collapsed dock.
    a.dispatch(ui(UiAction::SetDockTab(DockTab::Mixer)));
    assert!(a.ui().layout.dock_open);
}

#[test]
fn hidden_panels_hand_the_focus_back() {
    let mut a = app();
    a.dispatch(ui(UiAction::Focus(FocusArea::Inspector)));
    a.dispatch(ui(UiAction::TogglePanel(Panel::Inspector)));
    assert_eq!(a.ui().focus, FocusArea::Arrangement);
    a.dispatch(ui(UiAction::ToggleDockMaximized));
    assert_eq!(
        a.ui().focus,
        FocusArea::PianoRoll,
        "the arrangement is hidden"
    );
}

#[test]
fn opening_a_clip_fills_the_editor_tab() {
    let mut a = app();
    let lead = a.project().tracks[2].clone();
    let clip = lead.clips[0].id;
    a.dispatch(ui(UiAction::OpenClip(clip)));
    assert_eq!(a.ui().editor.clip, Some(clip));
    assert_eq!(a.ui().layout.dock_tab, DockTab::Editor);
    assert_eq!(a.ui().focus, FocusArea::PianoRoll);
    assert!(a.ui().selection.clips.contains(&clip));
    assert!(a.ui().selection.tracks.contains(&lead.id));
    let beat = a.project().tracks[0].clips[0].id;
    a.dispatch(ui(UiAction::OpenClip(beat)));
    assert_eq!(a.ui().focus, FocusArea::StepSequencer);
}

#[test]
fn ctrl_t_adds_an_armed_hexa_track() {
    let mut a = app();
    let n = a.project().tracks.len();
    a.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let p = a.project().clone();
    assert_eq!(p.tracks.len(), n + 1);
    let t = p.tracks.last().unwrap();
    assert!(t.arm && t.monitor);
    assert_eq!(t.channel.devices[0].plugin.name, "Hexa");
    assert!(p.tracks.iter().filter(|x| x.arm).count() == 1);
    assert_eq!(a.ui().selection.tracks.iter().next(), Some(&t.id));
    assert!(
        a.keyboard_midi_active(),
        "an armed track turns the keyboard into notes"
    );
    a.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(a.project().tracks.len(), n);
    a.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(a.project().tracks.len(), n + 1);
}

#[test]
fn clip_selection_edits() {
    let mut a = app();
    let bass = a.project().tracks[1].clone();
    let clip = bass.clips[0].id;
    a.dispatch(ui(UiAction::Select(SelectionChange::Clips {
        clips: vec![clip],
        mode: SelectMode::Replace,
    })));
    a.dispatch(Action::Edit(EditAction::DuplicateSelection));
    assert_eq!(a.project().tracks[1].clips.len(), bass.clips.len() + 1);
    assert!(
        !a.ui().selection.clips.contains(&clip),
        "the copy is selected"
    );
    a.dispatch(Action::Edit(EditAction::ToggleMuteSelection));
    let copy = *a.ui().selection.clips.iter().next().unwrap();
    assert!(a.project().find_clip(copy).unwrap().1.mute);
    a.dispatch(Action::Edit(EditAction::DeleteSelection));
    assert!(a.project().find_clip(copy).is_none());
    assert!(a.ui().selection.clips.is_empty());
}

#[test]
fn piano_roll_keys_edit_the_selected_notes() {
    let mut a = app();
    let lead = a.project().tracks[2].clone();
    let clip = lead.clips[0].id;
    a.dispatch(ui(UiAction::OpenClip(clip)));
    a.dispatch(ui(UiAction::SelectAll));
    let notes = |a: &App| match &a.project().find_clip(clip).unwrap().1.content {
        ClipContent::Midi(m) => m.active().notes.clone(),
        ClipContent::Step(_) | ClipContent::Audio(_) => unreachable!(),
    };
    let before = notes(&a);
    assert_eq!(a.ui().selection.notes.len(), before.len());
    a.dispatch(Action::Edit(EditAction::TransposeSelection(12)));
    let after = notes(&a);
    assert!(
        before
            .iter()
            .zip(after.iter())
            .all(|(b, x)| x.pitch == b.pitch + 12)
    );
    a.dispatch(Action::Edit(EditAction::NudgeSelection(1)));
    let moved = notes(&a);
    assert!(
        after
            .iter()
            .zip(moved.iter())
            .all(|(b, x)| x.start.0 == b.start.0 + 240)
    );
    a.dispatch(ui(UiAction::SetTool(Tool::Eraser)));
    assert_eq!(a.ui().piano_roll.tool, Tool::Eraser);
    a.dispatch(Action::Edit(EditAction::DeleteSelection));
    assert!(notes(&a).is_empty());
}

#[test]
fn zoom_and_grid_follow_the_focused_canvas() {
    let mut a = app();
    let px = a.ui().arrangement.px_per_bar;
    a.dispatch(ui(UiAction::Zoom {
        vertical: false,
        steps: 1,
    }));
    assert!((a.ui().arrangement.px_per_bar - px * 1.25).abs() < 1e-3);
    for _ in 0..20 {
        a.dispatch(ui(UiAction::Zoom {
            vertical: false,
            steps: 1,
        }));
    }
    assert_eq!(a.ui().arrangement.px_per_bar, 160.0);
    let grid = a.ui().arrangement.grid;
    a.dispatch(ui(UiAction::GridFiner));
    assert_eq!(a.ui().arrangement.grid, grid.finer());
    a.dispatch(ui(UiAction::Focus(FocusArea::PianoRoll)));
    let key = a.ui().piano_roll.key_height;
    a.dispatch(ui(UiAction::Zoom {
        vertical: true,
        steps: 1,
    }));
    assert!(a.ui().piano_roll.key_height > key);
    let pr = a.ui().piano_roll.grid;
    a.dispatch(ui(UiAction::GridCoarser));
    assert_eq!(a.ui().piano_roll.grid, pr.coarser());
}

#[test]
fn a_fader_gesture_is_one_undo_step() {
    let mut a = app();
    let track = a.project().tracks[1].id;
    a.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Lautstärke".into(),
    }));
    for db in [-5.0, -6.0, -7.5] {
        a.dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![track],
            prop: TrackProp::Channel(ChannelProp::VolumeDb(db)),
        }));
    }
    a.dispatch(Action::Edit(EditAction::EndGesture));
    assert_eq!(a.project().tracks[1].channel.volume_db, -7.5);
    a.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(a.project().tracks[1].channel.volume_db, -4.0);
}

#[test]
fn dialogs_take_the_focus_and_give_it_back() {
    let mut a = app();
    a.dispatch(ui(UiAction::Focus(FocusArea::Inspector)));
    a.dispatch(ui(UiAction::OpenSettings(Default::default())));
    assert_eq!(a.ui().focus, FocusArea::Dialog);
    a.dispatch(ui(UiAction::Focus(FocusArea::Arrangement)));
    assert_eq!(
        a.ui().focus,
        FocusArea::Dialog,
        "clicks behind a dialog do not move focus"
    );
    a.dispatch(ui(UiAction::ClearSelection));
    assert!(a.ui().dialog.is_none());
    assert_eq!(a.ui().focus, FocusArea::Inspector);
    a.dispatch(Action::Project(
        komponist_app::ProjectAction::OpenExportDialog,
    ));
    match &a.ui().dialog {
        Some(komponist_app::ui_state::Dialog::Export(d)) => {
            assert_eq!(d.settings.file_name, "Night Drive.wav");
            assert_eq!(d.settings.sample_rate, 48_000);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn new_project_replaces_the_document() {
    let mut a = app();
    a.dispatch(Action::Project(komponist_app::ProjectAction::New));
    assert!(a.project().tracks.is_empty());
    assert_eq!(a.project().name, "Unbenannt");
    assert!(a.ui().selection.tracks.is_empty());
}

#[test]
fn replacing_and_closing_dialogs_twice_keeps_the_focus() {
    use komponist_app::ProjectAction;
    use komponist_app::ui_state::{Dialog, SettingsSection};
    let mut a = app();
    a.dispatch(ui(UiAction::Focus(FocusArea::Browser)));
    a.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    // S06 "Plugin-Ordner prüfen" style: one dialog replaces another.
    a.dispatch(Action::Project(ProjectAction::CheckPluginFolders));
    assert_eq!(
        a.ui().dialog,
        Some(Dialog::Settings(SettingsSection::Plugins))
    );
    a.dispatch(ui(UiAction::CloseDialog));
    a.dispatch(ui(UiAction::CloseDialog));
    assert_eq!(a.ui().focus, FocusArea::Browser);
}

/// WP-47a: grid and snap actions name their editor instead of following the focus.
#[test]
fn grid_and_snap_actions_name_their_editor() {
    use komponist_app::actions::GridTarget;
    use komponist_model::Grid;
    let mut a = app();
    a.dispatch(ui(UiAction::Focus(FocusArea::PianoRoll)));
    let (arr_grid, arr_snap) = (a.ui().arrangement.grid, a.ui().arrangement.snap);
    a.dispatch(ui(UiAction::SetGrid {
        target: GridTarget::Arrangement,
        grid: Grid::Quarter,
    }));
    assert_eq!(a.ui().arrangement.grid, Grid::Quarter);
    assert_ne!(arr_grid, Grid::Quarter);
    a.dispatch(ui(UiAction::ToggleSnap(GridTarget::Arrangement)));
    assert_eq!(a.ui().arrangement.snap, !arr_snap);
    let pr = (a.ui().piano_roll.grid, a.ui().piano_roll.snap);
    assert_eq!(
        pr,
        (Grid::Sixteenth, true),
        "the piano roll was not touched"
    );
    a.dispatch(ui(UiAction::SetGrid {
        target: GridTarget::PianoRoll,
        grid: Grid::Half,
    }));
    a.dispatch(ui(UiAction::ToggleSnap(GridTarget::PianoRoll)));
    assert_eq!(a.ui().piano_roll.grid, Grid::Half);
    assert!(!a.ui().piano_roll.snap);
}

/// WP-47a: views post transient status texts.
#[test]
fn a_view_can_post_a_status_message() {
    use komponist_app::ui_state::StatusKind;
    let mut a = app();
    a.dispatch(ui(UiAction::ShowStatus {
        text: "Ratchet ×3".to_owned(),
        kind: StatusKind::Info,
    }));
    let status = a.ui().status.clone().unwrap();
    assert_eq!(
        (status.text.as_str(), status.kind),
        ("Ratchet ×3", StatusKind::Info)
    );
}

#[test]
fn exhausted_object_ids_report_a_localized_error_without_changing_the_document() {
    for lang in [komponist_i18n::Lang::De, komponist_i18n::Lang::En] {
        let mut project = komponist_model::Project::new("Exhausted");
        project.next_id = u64::MAX;
        let mut app = App::headless(project).with_lang(lang);
        let before = app.project().clone();
        app.dispatch(Action::Edit(EditAction::NewMidiTrack));
        assert_eq!(
            app.last_error(),
            Some(&komponist_model::edits::EditError::IdsExhausted)
        );
        assert!(std::sync::Arc::ptr_eq(&before, app.project()));
        assert_eq!(
            app.ui().status.as_ref().unwrap().text,
            lang.status_id_space_exhausted()
        );
    }
}
