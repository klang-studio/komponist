//! The sampler waveform, modes and modulation controls.

use std::path::PathBuf;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{EditAction, SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_builtin::general_sampler::read_settings;
use komponist_builtin::sampler::{PadStatus, SyncStatus};
use komponist_i18n::Lang;
use komponist_model::{DeviceId, params::ParamId};

use crate::common::{scratch, step_until};

fn harness(lang: Lang, name: &str, scenario: &str) -> (Harness<'static, App>, DeviceId) {
    let demo = komponist_app::showcase::showcase_project();
    let dir = scratch(&format!("general-sampler-ui-{scenario}-{}", lang.code()));
    let mut project = komponist_app::samples::demo_with_kit(demo, &dir).unwrap();
    let track = project
        .tracks
        .iter()
        .find(|track| track.name == name)
        .unwrap();
    let id = track.id;
    let device = track.channel.devices[0].id;
    // The sampler is the subject of these images. Other demo tracks load their own waveforms.
    project.tracks.retain(|track| track.id == id);
    let mut app = App::headless(project).with_lang(lang);
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![id],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    step_until(&mut app, "the demo sample is loaded", |app| {
        app.engine().host().sampler(device).is_some_and(|handle| {
            matches!(handle.view(0).status, PadStatus::Ready)
                && handle.waveform_peaks().is_some()
                && handle.sync_status() != Some(SyncStatus::Preparing)
        })
    });
    let mut harness = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_options(
            SnapshotOptions::new()
                .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
        )
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app| app.show(ui), app);
    harness.run_steps(3);
    (harness, device)
}

fn snapshot(harness: &mut Harness<'_, App>, lang: Lang, name: String) {
    let cpu = harness
        .get_by_role_and_label(Role::Status, lang.transport_cpu())
        .rect();
    harness.mask(cpu.expand(2.0));
    harness.snapshot(name);
}

fn click(harness: &mut Harness<'_, App>, at: Pos2) {
    harness.hover_at(at);
    harness.step();
    for pressed in [true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run_steps(3);
}

fn wheel(harness: &mut Harness<'_, App>, at: Pos2, delta: f32, modifiers: Modifiers) {
    harness.hover_at(at);
    harness.step();
    harness.event_modifiers(
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, delta),
            modifiers,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        },
        modifiers,
    );
    harness.run_steps(20);
}

fn page(harness: &mut Harness<'_, App>, lang: Lang, index: usize) {
    let page = [
        lang.devices_sampler_sample(),
        lang.devices_sampler_modulation(),
    ][index];
    let tab = harness
        .get_by_role_and_label(Role::RadioButton, page)
        .rect()
        .center();
    click(harness, tab);
    harness.hover_at(Pos2::ZERO);
    harness.run_steps(3);
}

#[test]
fn waveform_zoom_slice_editing_and_pages_are_accessible() {
    let mut snapshots = SnapshotResults::new();
    let set_fades = |harness: &mut Harness<'_, App>, device, fade_in, fade_out| {
        let sampler = komponist_builtin::Internal::GeneralSampler;
        for (name, value) in [("fade_in", fade_in), ("fade_out", fade_out)] {
            harness
                .state_mut()
                .dispatch(Action::Plugin(PluginAction::SetParam {
                    device,
                    id: sampler.wire(&ParamId::new(name)).unwrap(),
                    value,
                }));
        }
        harness.run_steps(3);
    };
    for (lang, suffix) in [(Lang::De, ""), (Lang::En, "-en")] {
        let (mut playback, device) = harness(lang, "Glass Pluck", "playback");
        set_fades(&mut playback, device, 125.0, 50.0);
        let zoom = playback
            .get_by_role_and_label(Role::Button, lang.audio_zoom_in())
            .rect()
            .center();
        click(&mut playback, zoom);
        assert!(
            !playback
                .get_by_role_and_label(Role::Button, lang.devices_sampler_fit())
                .accesskit_node()
                .is_disabled()
        );
        let earlier = playback
            .get_by_role_and_label(Role::Button, lang.devices_sampler_earlier())
            .rect()
            .center();
        click(&mut playback, earlier);
        playback.hover_at(Pos2::ZERO);
        playback.run_steps(3);
        snapshot(
            &mut playback,
            lang,
            format!("general-sampler-waveform-1x{suffix}"),
        );
        for _ in 0..2 {
            playback
                .state_mut()
                .dispatch(Action::Edit(EditAction::Undo));
        }
        page(&mut playback, lang, 1);
        playback.get_by_role_and_label(Role::Group, lang.devices_sampler_filter_envelope());
        snapshot(
            &mut playback,
            lang,
            format!("general-sampler-modulation-1x{suffix}"),
        );
        snapshots.extend_harness(&mut playback);

        let (mut slices, device) = harness(lang, "Glass Slices", "slices");
        set_fades(&mut slices, device, 160.0, 90.0);
        snapshot(
            &mut slices,
            lang,
            format!("general-sampler-slices-1x{suffix}"),
        );
        let add = slices
            .get_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
            .rect()
            .center();
        click(&mut slices, add);
        let state = slices
            .state()
            .project()
            .find_device(device)
            .unwrap()
            .2
            .state
            .as_deref();
        assert_eq!(read_settings(state).slices.len(), 6);
        snapshots.extend_harness(&mut slices);
    }
    snapshots.unwrap();
}

#[test]
fn dragging_the_waveform_start_follows_the_desktop_pointer_through_release() {
    let lang = Lang::De;
    let (mut harness, device) = harness(lang, "Glass Pluck", "trim-drag");
    let sampler = komponist_builtin::Internal::GeneralSampler;
    let wire = |name| sampler.wire(&ParamId::new(name)).unwrap();
    harness
        .state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id: wire("mode"),
            value: 1.0,
        }));
    harness.run_steps(3);
    let name = format!(
        "{} {}",
        lang.ui_graph_sample_start(),
        lang.ui_graph_position()
    );
    let start = harness
        .get_by_role_and_label(Role::Slider, name.as_str())
        .rect()
        .center();
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    harness.hover_at(start);
    harness.step();
    harness.event(Event::PointerButton {
        pos: start,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    harness.hover_at(start + vec2(12.0, 0.0));
    harness.step();
    // Desktop pointer movement includes acceleration. Raw motion reports a much smaller delta.
    harness.event(Event::MouseMoved(vec2(3.0, 0.0)));
    harness.hover_at(start + vec2(49.0, 0.0));
    harness.run_steps(2);
    let position = harness
        .get_by_role_and_label(Role::Slider, &name)
        .rect()
        .center()
        .x;
    let drag_distance = position - start.x;
    assert!((drag_distance - 49.0).abs() < 1.0, "{drag_distance}");

    let end = start + vec2(64.0, 0.0);
    harness.event(Event::MouseMoved(vec2(1.0, 0.0)));
    harness.hover_at(end);
    harness.event(Event::PointerButton {
        pos: end,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(3);
    let state = harness
        .state()
        .project()
        .find_device(device)
        .unwrap()
        .2
        .state
        .as_deref();
    let values = sampler.decode_state(state).unwrap();
    let span = waveform.width() - 2.0 * komponist_tokens::space::SPACE_1_5;
    let final_distance = values[wire("start") as usize] * f64::from(span);
    assert!((final_distance - 64.0).abs() < 1.0, "{final_distance}");
    assert!(values[wire("start") as usize] < values[wire("end") as usize]);
    assert!(
        !harness
            .state()
            .dispatched()
            .any(|action| matches!(action, Action::Plugin(PluginAction::PreviewSampler { .. })))
    );
}

#[test]
fn waveform_press_holds_the_note_until_mouse_release_outside_the_waveform() {
    let lang = Lang::De;
    let (mut harness, device) = harness(lang, "Glass Pluck", "waveform-preview");
    let sampler = komponist_builtin::Internal::GeneralSampler;
    let end = sampler.wire(&ParamId::new("end")).unwrap();
    harness
        .state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id: end,
            value: 1.0,
        }));
    harness.run_steps(3);
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    let start = waveform.center();
    harness.hover_at(start);
    harness.step();
    harness.event(Event::PointerButton {
        pos: start,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    let playing = |app: &App| {
        app.engine()
            .host()
            .sampler(device)
            .is_some_and(|handle| handle.playhead(0).is_some())
    };
    step_until(harness.state_mut(), "preview starts on press", playing);

    // Hold longer than the former timed audition and its release envelope.
    std::thread::sleep(Duration::from_millis(650));
    let outside = waveform.left_bottom() + vec2(-10.0, 10.0);
    harness.hover_at(outside);
    harness.step();
    assert!(playing(harness.state()), "the note is still held");

    harness.event(Event::PointerButton {
        pos: outside,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    step_until(harness.state_mut(), "preview releases outside", |app| {
        !playing(app)
    });
}

#[test]
fn clicking_the_zoomed_waveform_previews_the_slice_at_that_source_position() {
    let lang = Lang::De;
    let (mut harness, device) = harness(lang, "Glass Slices", "slice-preview");
    for label in [lang.audio_zoom_in(), lang.devices_sampler_later()] {
        let button = harness
            .get_by_role_and_label(Role::Button, label)
            .rect()
            .center();
        click(&mut harness, button);
    }
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    // The visible range is 2–4 s. Click just to the right of the marker at 3 s.
    click(&mut harness, waveform.center() + vec2(36.0, 0.0));
    let position = harness
        .state()
        .dispatched()
        .find_map(|action| match action {
            Action::Plugin(PluginAction::PreviewSampler {
                device: id,
                slice_time,
            }) if *id == device => *slice_time,
            _ => None,
        });
    assert!(
        position.is_some_and(|time| (3.0..3.5).contains(&time)),
        "{position:?}"
    );
}

#[test]
fn waveform_wheel_zooms_at_the_pointer_and_pans_without_scrolling_the_device_chain() {
    let lang = Lang::De;
    let (mut harness, device) = harness(lang, "Glass Slices", "waveform-wheel");
    harness.set_size(vec2(1600.0, 1080.0));
    harness.run_steps(3);
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    let anchor = pos2(
        waveform.left() + waveform.width() * 0.55,
        waveform.center().y,
    );
    let chain_scroll = harness.state().ui().devices.scroll_x;
    let probe = |harness: &mut Harness<'_, App>| {
        let positions = |app: &App| {
            app.dispatched()
                .filter_map(|action| match action {
                    Action::Plugin(PluginAction::PreviewSampler {
                        device: id,
                        slice_time,
                    }) if *id == device => *slice_time,
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let before = positions(harness.state()).len();
        click(harness, anchor);
        let values = positions(harness.state());
        assert_eq!(values.len(), before + 1);
        values[before]
    };
    let before = probe(&mut harness);
    wheel(
        &mut harness,
        anchor,
        135.0,
        Modifiers::CTRL | Modifiers::COMMAND,
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, lang.devices_sampler_fit())
            .accesskit_node()
            .is_disabled()
    );
    let zoomed = probe(&mut harness);
    assert!((zoomed - before).abs() < 0.001);

    wheel(&mut harness, anchor, -60.0, Modifiers::SHIFT);
    let shifted = probe(&mut harness);
    assert!(shifted > zoomed);
    assert_eq!(harness.state().ui().devices.scroll_x, chain_scroll);

    wheel(&mut harness, anchor, -60.0, Modifiers::NONE);
    assert!(probe(&mut harness) > shifted);
}

#[test]
fn clicking_a_slice_shows_and_edits_only_that_slices_options() {
    let lang = Lang::De;
    let (mut harness, device) = harness(lang, "Glass Slices", "slice-options");
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    let point = |fraction| {
        pos2(
            waveform.left() + waveform.width() * fraction,
            waveform.center().y,
        )
    };
    let state = |app: &App| {
        read_settings(
            app.project()
                .find_device(device)
                .unwrap()
                .2
                .state
                .as_deref(),
        )
    };
    let initial = state(harness.state());
    let toggle = harness
        .get_by_role_and_label(Role::Button, lang.devices_sampler_click_preview())
        .rect()
        .center();
    let previews = |app: &App| {
        app.dispatched()
            .filter(|action| matches!(action, Action::Plugin(PluginAction::PreviewSampler { .. })))
            .count()
    };

    // The second slice occupies 0.75–1.5 s of this four-second sample.
    click(&mut harness, toggle);
    click(&mut harness, point(0.25));
    assert_eq!(previews(harness.state()), 0);
    let choose = |harness: &mut Harness<'_, App>, field, option| {
        let combo = harness
            .get_by_role_and_label(Role::ComboBox, field)
            .rect()
            .center();
        click(harness, combo);
        let row = harness
            .get_by_role_and_label(Role::ListBoxOption, option)
            .rect()
            .center();
        click(harness, row);
    };
    choose(
        &mut harness,
        lang.devices_sampler_slice_trigger(),
        lang.choice_sampler_mode_gate(),
    );
    choose(
        &mut harness,
        lang.param_general_sampler_direction(),
        lang.param_general_sampler_forward(),
    );
    let fade = |harness: &mut Harness<'_, App>, label| {
        let field = harness
            .get_by_role_and_label(Role::SpinButton, label)
            .rect()
            .center();
        wheel(harness, field, 72.0, Modifiers::NONE);
    };
    fade(&mut harness, lang.param_general_sampler_fade_in());
    fade(&mut harness, lang.param_general_sampler_fade_out());

    let edited = state(harness.state());
    assert_eq!(edited.first, initial.first);
    assert_eq!(edited.slices[1..], initial.slices[1..]);
    let options = edited.slices[0].options;
    assert!(options.gate);
    assert_eq!(options.reverse, Some(false));
    assert!(options.fade_in_ms.is_some() && options.fade_out_ms.is_some());

    click(&mut harness, point(0.07));
    let gate = harness.get_by_role_and_label(Role::ComboBox, lang.devices_sampler_slice_trigger());
    assert_eq!(
        gate.accesskit_node().value().as_deref(),
        Some(lang.choice_sampler_mode_one_shot())
    );
    click(&mut harness, point(0.25));
    let gate = harness.get_by_role_and_label(Role::ComboBox, lang.devices_sampler_slice_trigger());
    assert_eq!(
        gate.accesskit_node().value().as_deref(),
        Some(lang.choice_sampler_mode_gate())
    );
    assert_eq!(previews(harness.state()), 0);
    click(&mut harness, toggle);
    click(&mut harness, point(0.25));
    assert_eq!(previews(harness.state()), 1);
}

#[test]
fn slice_tools_and_markers_follow_the_mode_on_the_sample_page() {
    let lang = Lang::De;
    let (mut harness, device) = harness(lang, "Glass Slices", "slice-modes");
    let slice = format!(
        "{} {}",
        komponist_model::format::note_name(49),
        lang.ui_graph_position()
    );
    harness.get_by_role_and_label(Role::Slider, &slice);
    assert!(
        harness
            .query_by_role_and_label(Role::RadioButton, "Schnitte")
            .is_none()
    );
    let mode = |name| format!("{}: {}", lang.param_general_sampler_mode(), name);
    for name in [
        lang.choice_sampler_mode_one_shot(),
        lang.choice_sampler_mode_gate(),
    ] {
        let button = harness
            .get_by_role_and_label(Role::RadioButton, &mode(name))
            .rect()
            .center();
        click(&mut harness, button);
        assert!(
            harness
                .query_by_role_and_label(Role::Slider, &slice)
                .is_none()
        );
        assert!(
            harness
                .query_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
                .is_none()
        );
        for edge in [lang.ui_graph_sample_start(), lang.ui_graph_sample_end()] {
            let name = format!("{} {}", edge, lang.ui_graph_position());
            harness.get_by_role_and_label(Role::Slider, &name);
        }
    }

    let button = harness
        .get_by_role_and_label(
            Role::RadioButton,
            &mode(lang.param_general_sampler_slices()),
        )
        .rect()
        .center();
    click(&mut harness, button);
    let marker = harness
        .get_by_role_and_label(Role::Slider, &slice)
        .rect()
        .center();
    harness.get_by_role_and_label(Role::Button, lang.devices_sampler_add_slice());
    let markers = |app: &App| {
        let state = app
            .project()
            .find_device(device)
            .unwrap()
            .2
            .state
            .as_deref();
        read_settings(state).slices
    };
    let original = markers(harness.state());
    assert_eq!(original.len(), 5);
    harness.hover_at(marker);
    harness.step();
    harness.event(Event::PointerButton {
        pos: marker,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    let moved = marker + vec2(28.0, 0.0);
    harness.hover_at(moved);
    harness.step();
    harness.event(Event::PointerButton {
        pos: moved,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(3);
    let edited = markers(harness.state());
    assert!(edited[0].time > original[0].time);
    assert_eq!(edited[1..], original[1..]);
}
