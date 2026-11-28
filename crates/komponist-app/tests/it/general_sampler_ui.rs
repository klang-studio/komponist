//! The sampler waveform, modes and modulation controls.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{EditAction, SelectMode, SelectionChange};
use komponist_app::device_host::PluginHost;
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_i18n::Lang;
use komponist_model::edits::{self, NewDevice};
use komponist_model::project::SavedPluginState;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, TrackId};
use komponist_plugin_api::device_assets::AssetStatus;
use komponist_plugin_api::waveform_markers::{Edit, Request, SliceOptions};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};

use crate::common::{
    FX, PATIENCE, app_with, clap_plugin, config, native_fixture, scratch, step_until,
};

const CLAP_ID: &str = "org.klang-studio.komponist.general-sampler";
const FIXTURE_BUNDLE: &str = "general-sampler.clap";

fn show_device(app: &mut App, track: TrackId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
}

fn ready_sampler(project: Project, host: PluginHost, lang: Lang) -> (App, DeviceId) {
    let track = project.tracks[0].id;
    let device = project.tracks[0].channel.devices[0].id;
    let mut app = app_with(project, host).with_lang(lang);
    show_device(&mut app, track);
    step_until(
        &mut app,
        "the external sampler asset and layout are ready",
        |app| {
            let host = app.engine().host();
            host.status(device) == DeviceStatus::Ok
                && host
                    .native_layout(device)
                    .is_some_and(|layout| layout.has_audio_asset_editor())
                && host.asset_slots(device).is_some_and(|assets| {
                    assets
                        .slots
                        .iter()
                        .any(|slot| slot.id == 0 && slot.status == AssetStatus::Ready)
                })
        },
    );
    let host = app.engine().host();
    assert!(host.verified_bundle(device).is_none());
    assert!(host.bridge_pid(device).is_some());
    (app, device)
}

/// Run the production bundle as an ordinary external CLAP on the null backend.
fn authored_harness(
    lang: Lang,
    name: &str,
    scenario: &str,
    fades_ms: Option<[f64; 2]>,
) -> Option<(Harness<'static, App>, DeviceId)> {
    let dir = scratch(&format!("general-sampler-ui-{scenario}-{}", lang.code()));
    let (host, plugin) = native_fixture(&dir, FIXTURE_BUNDLE, CLAP_ID)?;
    let fixture = komponist_app::showcase::general_sampler_fixture(name, plugin, &dir).unwrap();
    // Keep the populated master row from the showcase framing. Its effect is outside this test.
    let (project, master_effect) = edits::insert_device(
        &fixture.project,
        ChannelRef::Master,
        0,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: clap_plugin(FX),
            state: None,
            sidechain: None,
            preset: None,
        },
    )
    .unwrap();
    let project = edits::set_device_bypass(&project, master_effect, true).unwrap();
    let (mut app, device) = ready_sampler(project, host, lang);
    wait_for_sampler_state(&mut app, device);
    let host = app.engine_mut().host_mut();
    let layout = host.native_layout(device).cloned().unwrap();
    let ids = layout.author_ids().unwrap();
    let fades = fades_ms.map(|[fade_in, fade_out]| [("fade_in", fade_in), ("fade_out", fade_out)]);
    for (name, value) in fixture.params {
        if fades_ms.is_none() || !matches!(name, "fade_in" | "fade_out") {
            assert!(host.set_param(device, ids[name], value), "set {name}");
        }
    }
    for (name, milliseconds) in fades.into_iter().flatten() {
        assert!(host.set_param(device, ids[name], milliseconds / 10_000.0));
    }
    wait_for_sampler_state(&mut app, device);
    for (count, &seconds) in fixture.markers.iter().enumerate() {
        edit_markers(&mut app, device, count, Edit::Insert { seconds });
    }
    assert!(saved_state(&app, device).is_some());
    // The bridge authored the initial state. Load it into a clean document so fixture edits
    // are outside the test's undo history and the UI starts at its saved baseline.
    app.shutdown();
    let project = app.project().as_ref().clone();
    drop(app);
    let (mut app, device) = ready_sampler(project, PluginHost::new(config(&dir)), lang);
    for (name, milliseconds) in fades.into_iter().flatten() {
        let parameter = app.engine().host().param_value(device, ids[name]).unwrap();
        let reported_ms = parameter
            .text
            .strip_suffix(" ms")
            .and_then(|time| time.parse::<f64>().ok());
        assert!(
            reported_ms.is_some_and(|time| (time - milliseconds).abs() < 0.01),
            "{name} should report {milliseconds} ms, got {:?}",
            parameter.text
        );
    }
    if !fixture.markers.is_empty() {
        wait_for_editable_markers(&mut app, device, fixture.markers.len());
        assert_eq!(
            app.engine()
                .host()
                .waveform_markers(device, 0)
                .unwrap()
                .positions,
            fixture.markers
        );
    }
    app.clear_dispatched();
    assert!(!app.doc().is_dirty());

    let mut harness = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_options(
            SnapshotOptions::new()
                .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
        )
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    harness.run_steps(3);
    wait_for_ui(
        &mut harness,
        "the authored waveform to be ready",
        |harness| {
            harness
                .query_by_role_and_label(Role::Button, lang.audio_zoom_in())
                .is_some_and(|button| !button.accesskit_node().is_disabled())
        },
    );
    Some((harness, device))
}

fn wait_for_sampler_state(app: &mut App, device: DeviceId) {
    // A marker snapshot can become editable before the bridge confirms pending parameter writes.
    app.engine_mut().host_mut().request_state(device);
    step_until(
        app,
        "the current sampler parameter state is persisted",
        |app| {
            app.device_events().iter().any(|event| {
                matches!(event, PluginManagerEvent::StateSnapshot { device: updated, state }
                    if *updated == device && app.engine().host().is_current_param_snapshot(device, &state.bytes))
            })
        },
    );
}

fn wait_for_editable_markers(app: &mut App, device: DeviceId, count: usize) -> u64 {
    let deadline = Instant::now() + PATIENCE;
    loop {
        // Mode changes apply asynchronously on the bridge. Renew the same bounded request the
        // visible waveform uses until its snapshot describes the current playback mode.
        app.dispatch(Action::Plugin(PluginAction::RequestWaveformMarkers {
            device,
            slot: 0,
        }));
        app.step();
        let host = app.engine().host();
        let snapshot = host.waveform_markers(device, 0);
        if let Some(snapshot) =
            snapshot.filter(|snapshot| snapshot.editable && snapshot.positions.len() == count)
        {
            return snapshot.revision;
        }
        assert!(
            Instant::now() < deadline,
            "no editable marker snapshot with {count} positions; device status {:?}; snapshot {snapshot:?}; events {:?}",
            host.status(device),
            app.device_events()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn saved_state(app: &App, device: DeviceId) -> Option<SavedPluginState> {
    app.project().find_device(device).unwrap().2.saved_state()
}

fn edit_markers(app: &mut App, device: DeviceId, count: usize, operation: Edit) {
    let previous = saved_state(app, device);
    let revision = wait_for_editable_markers(app, device, count);
    app.dispatch(Action::Plugin(PluginAction::EditWaveformMarkers {
        device,
        slot: 0,
        request: Request {
            revision,
            operation,
        },
    }));
    step_until(app, "the sampler marker edit to persist", |app| {
        marker_edit_persisted(app, device) && saved_state(app, device) != previous
    });
}

fn marker_edit_persisted(app: &App, device: DeviceId) -> bool {
    use komponist_plugins::WaveformMarkerEditResult;
    app.device_events().iter().any(|event| {
        matches!(event, PluginManagerEvent::WaveformMarkersEdited {
            device: edited,
            result: WaveformMarkerEditResult::Applied { state, .. },
            ..
        } if *edited == device
            && saved_state(app, device).is_some_and(|recorded| recorded == *state))
    })
}

fn live_markers(
    app: &App,
    device: DeviceId,
) -> Option<&komponist_plugin_api::waveform_markers::Snapshot> {
    app.engine().host().waveform_markers(device, 0)
}

fn snapshot(harness: &mut Harness<'_, App>, lang: Lang, name: String) {
    // The bridge's scheduling notice depends on the test machine's realtime permissions.
    harness.state_mut().ui_mut().status = None;
    harness.step();
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
    for (lang, suffix) in [(Lang::De, ""), (Lang::En, "-en")] {
        let Some((mut playback, device)) =
            authored_harness(lang, "Glass Pluck", "playback", Some([125.0, 50.0]))
        else {
            return;
        };
        wait_for_waveform_image(&mut playback, device);
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
        page(&mut playback, lang, 1);
        playback.get_by_role_and_label(Role::Label, lang.devices_sampler_lfo());
        playback.get_by_role_and_label(Role::Label, lang.devices_sampler_velocity());
        playback.get_by_role_and_label(Role::Group, lang.devices_sampler_filter_envelope());
        wait_for_ui(
            &mut playback,
            "the live filter envelope to be drawn",
            |harness| {
                harness
                    .state()
                    .engine()
                    .host()
                    .device_display(device)
                    .is_some_and(|display| {
                        display
                            .frames
                            .iter()
                            .any(|frame| frame.key == "filter-envelope")
                    })
            },
        );
        playback.run_steps(2);
        snapshot(
            &mut playback,
            lang,
            format!("general-sampler-modulation-1x{suffix}"),
        );
        check_lfo_shapes(&mut playback, lang, suffix);
        snapshots.extend_harness(&mut playback);

        let (mut slices, device) =
            authored_harness(lang, "Glass Slices", "slices", Some([160.0, 90.0]))
                .expect("the already checked General Sampler fixture");
        wait_for_waveform_image(&mut slices, device);
        snapshot(
            &mut slices,
            lang,
            format!("general-sampler-slices-1x{suffix}"),
        );
        let add = slices
            .get_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
            .rect()
            .center();
        let initial = saved_state(slices.state(), device);
        click(&mut slices, add);
        wait_for_ui(&mut slices, "the added marker to persist", |harness| {
            saved_state(harness.state(), device) != initial
                && live_markers(harness.state(), device)
                    .is_some_and(|snapshot| snapshot.positions.len() == 6)
        });
        snapshots.extend_harness(&mut slices);
    }
    snapshots.unwrap();
}

/// The demo's Glass Pluck plays Smooth Random, the last of the LFO shapes (#365): the field shows
/// it, the open list names every shape.
fn check_lfo_shapes(harness: &mut Harness<'_, App>, lang: Lang, suffix: &str) {
    let field =
        harness.get_by_role_and_label(Role::ComboBox, lang.param_general_sampler_lfo_shape());
    assert_eq!(
        field.value().as_deref(),
        Some(lang.param_general_sampler_smooth_random())
    );
    let at = field.rect().center();
    click(harness, at);
    let shapes = [
        lang.param_general_sampler_sine(),
        lang.param_general_sampler_triangle(),
        lang.param_general_sampler_saw_up(),
        lang.param_general_sampler_saw_down(),
        lang.param_general_sampler_square(),
        lang.param_general_sampler_sample_hold(),
        lang.param_general_sampler_smooth_random(),
    ];
    for shape in &shapes {
        let option = harness.get_by_role_and_label(Role::ListBoxOption, shape);
        assert_eq!(
            option.accesskit_node().is_selected(),
            Some(shape == shapes.last().unwrap()),
            "{shape}"
        );
    }
    harness.hover_at(Pos2::ZERO);
    harness.run_steps(3);
    snapshot(
        harness,
        lang,
        format!("general-sampler-lfo-shapes-1x{suffix}"),
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(
        harness
            .query_by_role_and_label(Role::ListBoxOption, shapes[0])
            .is_none()
    );
}

fn wait_for_waveform_image(harness: &mut Harness<'_, App>, device: DeviceId) {
    use komponist_audiofile::{FilePool, PeakState};
    use komponist_plugin_api::waveform_markers::Preparation;

    let source = &harness
        .state()
        .project()
        .find_device(device)
        .unwrap()
        .2
        .asset_links[&0]
        .abs;
    let file = FilePool::global().open(source).unwrap();
    wait_for_ui(
        harness,
        "completed waveform peaks and sample preparation",
        |harness| {
            matches!(file.peaks_state(), PeakState::Ready(_))
                && harness
                    .state()
                    .engine()
                    .host()
                    .waveform_markers(device, 0)
                    .is_some_and(|snapshot| snapshot.preparation != Some(Preparation::Preparing))
        },
    );
    harness.run_steps(2);
}

#[test]
fn authored_waveform_trim_drag_follows_the_desktop_pointer_through_release() {
    let lang = Lang::De;
    let Some((mut harness, device)) =
        authored_harness(lang, "Glass Pluck", "authored-trim-drag", None)
    else {
        return;
    };
    check_waveform_trim_drag(&mut harness, lang, |app| {
        wait_for_sampler_state(app, device);
        let host = app.engine().host();
        let ids = host.native_layout(device).unwrap().author_ids().unwrap();
        ["start", "end"].map(|name| host.param_base(device, ids[name]).unwrap())
    });
}

fn check_waveform_trim_drag(
    harness: &mut Harness<'_, App>,
    lang: Lang,
    playback_range: impl FnOnce(&mut App) -> [f64; 2],
) {
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
    wait_for_ui(
        harness,
        "the trim handle to follow the desktop pointer",
        |harness| {
            let position = harness
                .get_by_role_and_label(Role::Slider, &name)
                .rect()
                .center()
                .x;
            (position - start.x - 49.0).abs() < 1.0
        },
    );

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
    let [trim_start, trim_end] = playback_range(harness.state_mut());
    let span = waveform.width() - 2.0 * komponist_tokens::space::SPACE_1_5;
    let final_distance = trim_start * f64::from(span);
    assert!((final_distance - 64.0).abs() < 1.0, "{final_distance}");
    assert!(trim_start < trim_end);
    assert!(
        !harness
            .state()
            .dispatched()
            .any(|action| matches!(action, Action::Plugin(PluginAction::PreviewWaveform { .. })))
    );
}

#[test]
fn authored_waveform_press_holds_the_note_until_mouse_release_outside_the_waveform() {
    let lang = Lang::De;
    let Some((mut harness, device)) =
        authored_harness(lang, "Glass Pluck", "authored-waveform-preview", None)
    else {
        return;
    };
    let end = harness
        .state()
        .engine()
        .host()
        .native_layout(device)
        .unwrap()
        .author_ids()
        .unwrap()["end"];
    harness
        .state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id: end,
            value: 1.0,
        }));
    wait_for_sampler_state(harness.state_mut(), device);
    harness.run_steps(3);
    check_held_waveform_preview(&mut harness, lang, |app| {
        let Some(tap) = app.engine().host().signal_tap(device, "playhead") else {
            return false;
        };
        tap.want();
        let mut position = [-1.0];
        tap.samples().read_latest(&mut position).is_some() && position[0] >= 0.0
    });
}

fn check_held_waveform_preview(
    harness: &mut Harness<'_, App>,
    lang: Lang,
    playing: impl Fn(&App) -> bool,
) {
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
    step_until(harness.state_mut(), "preview starts on press", &playing);

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
fn authored_zoomed_waveform_previews_the_slice_at_that_source_position() {
    let lang = Lang::De;
    let Some((mut harness, device)) =
        authored_harness(lang, "Glass Slices", "authored-slice-preview", None)
    else {
        return;
    };
    check_zoomed_preview(&mut harness, device, lang);
}

fn check_zoomed_preview(harness: &mut Harness<'_, App>, device: DeviceId, lang: Lang) {
    wait_for_ui(harness, "slice tools before preview", |harness| {
        harness
            .query_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
            .is_some()
    });
    for label in [lang.audio_zoom_in(), lang.devices_sampler_later()] {
        let button = harness
            .get_by_role_and_label(Role::Button, label)
            .rect()
            .center();
        click(harness, button);
    }
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    // The visible range is 2–4 s. Click just to the right of the marker at 3 s.
    click(harness, waveform.center() + vec2(36.0, 0.0));
    let position = preview_times(harness.state(), device).next().flatten();
    assert!(
        position.is_some_and(|time| (3.0..3.5).contains(&time)),
        "{position:?}"
    );
}

fn preview_times(app: &App, device: DeviceId) -> impl Iterator<Item = Option<f64>> + '_ {
    app.dispatched().filter_map(move |action| match action {
        Action::Plugin(PluginAction::PreviewWaveform {
            device: id,
            slice_time,
            ..
        }) if *id == device => Some(*slice_time),
        _ => None,
    })
}

#[test]
fn authored_waveform_wheel_zooms_at_the_pointer_and_pans_without_scrolling_the_device_chain() {
    let lang = Lang::De;
    let Some((mut harness, device)) =
        authored_harness(lang, "Glass Slices", "authored-waveform-wheel", None)
    else {
        return;
    };
    check_waveform_wheel_routing(&mut harness, device, lang);
}

fn check_waveform_wheel_routing(harness: &mut Harness<'_, App>, device: DeviceId, lang: Lang) {
    harness.set_size(vec2(1600.0, 1080.0));
    harness.run_steps(3);
    wait_for_ui(harness, "the slice waveform to be ready", |harness| {
        harness
            .query_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
            .is_some()
    });
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    let anchor = pos2(
        waveform.left() + waveform.width() * 0.55,
        waveform.center().y,
    );
    // A fully fitted waveform has no horizontal travel, so the chain gets Shift-wheel.
    wheel(harness, anchor, -60.0, Modifiers::SHIFT);
    assert!(harness.state().ui().devices.scroll_x > 0.0);
    harness.state_mut().ui_mut().devices.scroll_x = 0.0;
    harness.run_steps(3);
    let chain_scroll = harness.state().ui().devices.scroll_x;
    let probe = |harness: &mut Harness<'_, App>| {
        let positions = |app: &App| preview_times(app, device).flatten().collect::<Vec<_>>();
        let before = positions(harness.state()).len();
        click(harness, anchor);
        let values = positions(harness.state());
        assert_eq!(values.len(), before + 1);
        values[before]
    };
    let before = probe(harness);
    wheel(harness, anchor, 135.0, Modifiers::CTRL | Modifiers::COMMAND);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, lang.devices_sampler_fit())
            .accesskit_node()
            .is_disabled()
    );
    let zoomed = probe(harness);
    assert!((zoomed - before).abs() < 0.001);

    wheel(harness, anchor, -60.0, Modifiers::SHIFT);
    let shifted = probe(harness);
    assert!(shifted > zoomed);
    assert_eq!(harness.state().ui().devices.scroll_x, chain_scroll);

    wheel(harness, anchor, -60.0, Modifiers::NONE);
    assert!(probe(harness) > shifted);
}

#[test]
fn authored_undo_keeps_slice_selection_and_cancels_an_in_progress_marker_drag() {
    let lang = Lang::De;
    let Some((mut harness, device)) =
        authored_harness(lang, "Glass Slices", "authored-slice-undo", None)
    else {
        return;
    };
    check_slice_undo(&mut harness, device, lang);
}

fn check_slice_undo(harness: &mut Harness<'_, App>, device: DeviceId, lang: Lang) {
    let initial = saved_state(harness.state(), device);
    let markers = live_markers(harness.state(), device).unwrap().clone();
    let count = markers.positions.len();
    edit_markers(
        harness.state_mut(),
        device,
        count,
        Edit::Insert { seconds: 0.4 },
    );
    harness.run_steps(3);
    wait_for_ui(harness, "slice controls after the insertion", |harness| {
        harness
            .query_by_role_and_label(Role::ComboBox, lang.devices_sampler_midi_assignment())
            .is_some()
    });
    let wave = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    click(
        harness,
        pos2(wave.left() + wave.width() * 0.44, wave.center().y),
    );
    harness.state_mut().dispatch(Action::Edit(EditAction::Undo));
    harness.run_steps(3);
    wait_for_ui(
        harness,
        "the original selected slice to survive undo",
        |harness| {
            harness
                .query_by_role_and_label(Role::ComboBox, lang.devices_sampler_midi_assignment())
                .and_then(|control| control.accesskit_node().value())
                .is_some_and(|value| value.contains("1500"))
        },
    );

    assert_eq!(saved_state(harness.state(), device), initial);
    edit_markers(
        harness.state_mut(),
        device,
        count,
        Edit::SetSliceOptions {
            slice: 2,
            options: SliceOptions {
                gate: true,
                ..Default::default()
            },
        },
    );
    harness.run_steps(3);
    let label = format!(
        "{} {}",
        komponist_model::format::note_name(50),
        lang.ui_graph_position()
    );
    wait_for_ui(
        harness,
        "the marker to return after the options edit",
        |harness| {
            harness
                .query_by_role_and_label(Role::Slider, &label)
                .is_some()
        },
    );
    let marker = harness
        .get_by_role_and_label(Role::Slider, &label)
        .rect()
        .center();
    harness.hover_at(marker);
    harness.step();
    harness.drag_at(marker);
    harness.step();
    let moved = marker + vec2(30.0, 0.0);
    harness.hover_at(moved);
    harness.step();
    harness.state_mut().dispatch(Action::Edit(EditAction::Undo));
    harness.hover_at(moved + vec2(6.0, 0.0));
    harness.step();
    harness.event(Event::PointerButton {
        pos: moved,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(3);
    wait_for_ui(
        harness,
        "the host to finish restoring the marker options",
        |harness| {
            live_markers(harness.state(), device).is_some_and(|snapshot| {
                snapshot.editable
                    && snapshot.positions == markers.positions
                    && snapshot.slice_options == markers.slice_options
            })
        },
    );
    assert!(!harness.state().dispatched().any(|action| matches!(action,
        Action::Plugin(PluginAction::EditWaveformMarkers { device: edited, request: Request { operation: Edit::Move { .. }, .. }, .. }) if *edited == device
    )), "undo must discard the draft move");
    assert_eq!(saved_state(harness.state(), device), initial);
}

#[test]
fn authored_slice_click_shows_and_edits_only_that_slices_options() {
    let lang = Lang::De;
    let Some((mut harness, device)) =
        authored_harness(lang, "Glass Slices", "authored-slice-options", None)
    else {
        return;
    };
    check_slice_options(&mut harness, device, lang);
}

fn check_slice_options(harness: &mut Harness<'_, App>, device: DeviceId, lang: Lang) {
    wait_for_ui(harness, "slice tools before editing options", |harness| {
        harness
            .query_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
            .is_some()
    });
    let waveform = harness
        .get_by_role_and_label(Role::Group, lang.ui_graph_waveform())
        .rect();
    let point = |fraction| {
        pos2(
            waveform.left() + waveform.width() * fraction,
            waveform.center().y,
        )
    };
    let initial = live_markers(harness.state(), device)
        .unwrap()
        .slice_options
        .clone()
        .unwrap();
    let toggle = harness
        .get_by_role_and_label(Role::Button, lang.devices_sampler_click_preview())
        .rect()
        .center();
    let previews = |app: &App| preview_times(app, device).count();

    // The second slice occupies 0.75–1.5 s of this four-second sample.
    click(harness, toggle);
    click(harness, point(0.25));
    assert_eq!(previews(harness.state()), 0);
    let control_ready = |harness: &Harness<'_, App>, role, label| {
        harness
            .query_by_role_and_label(role, label)
            .is_some_and(|control| !control.accesskit_node().is_disabled())
    };
    let choose = |harness: &mut Harness<'_, App>, field, option| {
        // An accepted edit can reach the project before the bridge re-enables the controls.
        wait_for_ui(harness, field, |harness| {
            control_ready(harness, Role::ComboBox, field)
        });
        harness
            .get_by_role_and_label(Role::ComboBox, field)
            .click_accesskit();
        harness.run_steps(2);
        harness
            .get_by_role_and_label(Role::ListBoxOption, option)
            .click_accesskit();
        harness.run_steps(2);
    };
    let previous = saved_state(harness.state(), device);
    choose(
        harness,
        lang.devices_sampler_slice_trigger(),
        lang.choice_sampler_mode_gate(),
    );
    wait_for_slice_option(
        harness,
        device,
        "the edited trigger mode",
        previous,
        |options| options.gate,
    );
    let previous = saved_state(harness.state(), device);
    choose(
        harness,
        lang.param_general_sampler_direction(),
        lang.param_general_sampler_forward(),
    );
    wait_for_slice_option(
        harness,
        device,
        "the edited slice direction",
        previous,
        |options| options.reverse == Some(false),
    );
    let fade = |harness: &mut Harness<'_, App>, label| {
        wait_for_ui(harness, label, |harness| {
            control_ready(harness, Role::SpinButton, label)
        });
        let field = harness
            .get_by_role_and_label(Role::SpinButton, label)
            .rect()
            .center();
        wheel(harness, field, 72.0, Modifiers::NONE);
    };
    let previous = saved_state(harness.state(), device);
    fade(harness, lang.param_general_sampler_fade_in());
    wait_for_slice_option(
        harness,
        device,
        "the edited attack fade",
        previous,
        |options| options.fade_in_ms.is_some(),
    );
    let previous = saved_state(harness.state(), device);
    fade(harness, lang.param_general_sampler_fade_out());
    wait_for_slice_option(
        harness,
        device,
        "the edited release fade",
        previous,
        |options| options.fade_out_ms.is_some(),
    );

    let edited = live_markers(harness.state(), device)
        .unwrap()
        .slice_options
        .as_ref()
        .unwrap();
    assert_eq!(edited[0], initial[0]);
    assert_eq!(edited[2..], initial[2..]);
    let options = edited[1];
    assert!(options.gate);
    assert_eq!(options.reverse, Some(false));
    assert!(options.fade_in_ms.is_some() && options.fade_out_ms.is_some());

    click(harness, point(0.07));
    let gate = harness.get_by_role_and_label(Role::ComboBox, lang.devices_sampler_slice_trigger());
    assert_eq!(
        gate.accesskit_node().value().as_deref(),
        Some(lang.choice_sampler_mode_one_shot())
    );
    click(harness, point(0.25));
    let gate = harness.get_by_role_and_label(Role::ComboBox, lang.devices_sampler_slice_trigger());
    assert_eq!(
        gate.accesskit_node().value().as_deref(),
        Some(lang.choice_sampler_mode_gate())
    );
    assert_eq!(previews(harness.state()), 0);
    click(harness, toggle);
    click(harness, point(0.25));
    assert_eq!(previews(harness.state()), 1);
}

fn wait_for_slice_option(
    harness: &mut Harness<'_, App>,
    device: DeviceId,
    what: &str,
    previous: Option<SavedPluginState>,
    matches: impl Fn(SliceOptions) -> bool,
) {
    wait_for_ui(harness, what, |harness| {
        let app = harness.state();
        saved_state(app, device) != previous
            && live_markers(app, device)
                .filter(|snapshot| snapshot.editable)
                .and_then(|snapshot| snapshot.slice_options.as_ref())
                .and_then(|options| options.get(1).copied())
                .is_some_and(&matches)
    });
}

#[test]
fn authored_slice_tools_and_markers_follow_the_mode_on_the_sample_page() {
    let lang = Lang::De;
    let Some((mut harness, device)) =
        authored_harness(lang, "Glass Slices", "authored-slice-modes", None)
    else {
        return;
    };
    check_slice_modes(&mut harness, lang, device);
}

fn wait_for_ui(
    harness: &mut Harness<'_, App>,
    what: &str,
    ready: impl Fn(&Harness<'_, App>) -> bool,
) {
    let deadline = Instant::now() + PATIENCE;
    while !ready(harness) {
        harness.run_steps(1);
        assert!(Instant::now() < deadline, "no {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn check_slice_modes(harness: &mut Harness<'_, App>, lang: Lang, device: DeviceId) {
    let slice = format!(
        "{} {}",
        komponist_model::format::note_name(49),
        lang.ui_graph_position()
    );
    wait_for_ui(harness, "the first slice marker", |harness| {
        harness
            .query_by_role_and_label(Role::Slider, &slice)
            .is_some()
    });
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
        click(harness, button);
        wait_for_ui(harness, "the slice controls to hide", |harness| {
            harness
                .query_by_role_and_label(Role::Slider, &slice)
                .is_none()
                && harness
                    .query_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
                    .is_none()
        });
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
    click(harness, button);
    wait_for_ui(harness, "the slice marker and tools to return", |harness| {
        harness
            .query_by_role_and_label(Role::Slider, &slice)
            .is_some()
            && harness
                .query_by_role_and_label(Role::Button, lang.devices_sampler_add_slice())
                .is_some()
    });
    wait_for_sampler_state(harness.state_mut(), device);
    wait_for_editable_markers(harness.state_mut(), device, 5);
    harness.run_steps(1);
    let marker = harness
        .get_by_role_and_label(Role::Slider, &slice)
        .rect()
        .center();
    let original = live_markers(harness.state(), device)
        .unwrap()
        .positions
        .clone();
    assert_eq!(original.len(), 5);
    let previous = saved_state(harness.state(), device);
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
    harness.run_steps(1);
    let operations: Vec<_> = harness
        .state()
        .dispatched()
        .filter_map(|action| match action {
            Action::Plugin(PluginAction::EditWaveformMarkers {
                device: edited,
                slot: 0,
                request,
            }) if *edited == device => Some(request.operation),
            _ => None,
        })
        .collect();
    assert!(
        matches!(operations.as_slice(), [Edit::Move { index: 0, seconds }] if *seconds > original[0]),
        "the marker drag should emit one move on release: {operations:?}"
    );
    wait_for_ui(harness, "the dragged marker to be persisted", |harness| {
        let app = harness.state();
        saved_state(app, device) != previous
            && live_markers(app, device).is_some_and(|snapshot| {
                snapshot.positions[0] > original[0] && snapshot.positions[1..] == original[1..]
            })
    });
}
