//! #34: the master display of the transport bar (oscilloscope and spectrum of the master output,
//! master-scope proposal variant A „Fenster“, internal notes): click and keys switch it, the
//! settings remember it, AccessKit names it in German and English, and the snapshots show both
//! pictures with a known signal.

use std::path::PathBuf;
use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Key, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::SettingsAction;
use komponist_app::device_host::BuiltinHost;
use komponist_app::engine_sync::EngineSync;
use komponist_app::keymap::Keymap;
use komponist_app::master_scope::MasterDisplay;
use komponist_app::meters::Meters;
use komponist_app::metrics::transport as m;
use komponist_app::panels::transport::{self, TransportModel};
use komponist_app::settings::Settings;
use komponist_app::{Action, App};
use komponist_audio::AudioState;
use komponist_engine::{SampleTap, TransportState, tap};
use komponist_i18n::{Lang, de, en};
use komponist_model::fixtures::demo_project;
use komponist_model::{Document, Project};
use komponist_tokens::{size, space};

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_options(SnapshotOptions::new())
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(2);
    h
}

/// The demo on the null backend, saving its settings to `path`.
fn app_saving_to(path: &std::path::Path, settings: Settings) -> App {
    let engine = EngineSync::null(Box::new(BuiltinHost::new()));
    App::new(
        Document::new(demo_project()),
        engine,
        settings,
        Some(path.to_owned()),
    )
}

fn description(h: &Harness<'_, App>, name: &str) -> String {
    h.get_by_role_and_label(Role::Button, name)
        .accesskit_node()
        .description()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn click_enter_and_space_switch_the_display_and_the_settings_remember_it() {
    let dir = std::env::temp_dir().join(format!("komponist-master-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("settings.json");
    let mut h = harness(app_saving_to(&path, Settings::default()));
    // The default: the oscilloscope, its description says how to switch.
    assert_eq!(
        description(&h, de::TRANSPORT_SCOPE_OSCILLOSCOPE),
        de::TRANSPORT_SCOPE_OSCILLOSCOPE_TIP
    );
    // A click: the spectrum, saved.
    h.get_by_role_and_label(Role::Button, de::TRANSPORT_SCOPE_OSCILLOSCOPE)
        .click();
    h.run_steps(2);
    assert_eq!(
        description(&h, de::TRANSPORT_SCOPE_SPECTRUM),
        de::TRANSPORT_SCOPE_SPECTRUM_TIP
    );
    assert!(h.query_by_label(de::TRANSPORT_SCOPE_OSCILLOSCOPE).is_none());
    assert_eq!(h.state().settings().master_display, MasterDisplay::Spectrum);
    assert_eq!(
        Settings::load(&path).unwrap().master_display,
        MasterDisplay::Spectrum
    );
    // Reached with the keyboard: Enter and Space switch it; Space does not start playback.
    for (key, now) in [
        (Key::Enter, de::TRANSPORT_SCOPE_OSCILLOSCOPE),
        (Key::Space, de::TRANSPORT_SCOPE_SPECTRUM),
    ] {
        let before = if now == de::TRANSPORT_SCOPE_SPECTRUM {
            de::TRANSPORT_SCOPE_OSCILLOSCOPE
        } else {
            de::TRANSPORT_SCOPE_SPECTRUM
        };
        h.get_by_role_and_label(Role::Button, before).focus();
        h.run_steps(2);
        h.key_press(key);
        h.run_steps(2);
        h.get_by_role_and_label(Role::Button, now);
    }
    assert_eq!(h.state().engine().transport(), TransportState::Stopped);
    // The next start shows what was chosen last.
    let saved = Settings::load(&path).unwrap();
    assert_eq!(saved.master_display, MasterDisplay::Spectrum);
    let h = harness(app_saving_to(&path, saved));
    h.get_by_role_and_label(Role::Button, de::TRANSPORT_SCOPE_SPECTRUM);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_display_is_named_in_english() {
    let mut h = harness(App::headless(demo_project()).with_lang(Lang::En));
    assert_eq!(
        description(&h, en::TRANSPORT_SCOPE_OSCILLOSCOPE),
        en::TRANSPORT_SCOPE_OSCILLOSCOPE_TIP
    );
    h.state_mut()
        .dispatch(Action::Settings(SettingsAction::ToggleMasterDisplay));
    h.run_steps(2);
    assert_eq!(
        description(&h, en::TRANSPORT_SCOPE_SPECTRUM),
        en::TRANSPORT_SCOPE_SPECTRUM_TIP
    );
    assert!(h.query_by_label(de::TRANSPORT_SCOPE_SPECTRUM).is_none());
}

/// The transport bar alone, fed by a tap of its own (the engine's role in the app).
struct Bar {
    project: Arc<Project>,
    keymap: Keymap,
    audio: AudioState,
    meters: Meters,
    tap: Arc<SampleTap>,
    mode: MasterDisplay,
    /// Samples pushed so far (the sine's phase).
    pushed: usize,
}

const RATE: f32 = 48_000.0;
const BAR_WIDTH: f32 = 1280.0;

fn bar(mode: MasterDisplay, ppp: f32) -> Harness<'static, Bar> {
    let tap = Arc::new(SampleTap::new(tap::DEFAULT_CAPACITY));
    tap.set_rate(RATE);
    let mut meters = Meters::new();
    meters.master_scope.set_tap(&tap);
    let state = Bar {
        project: Arc::new(demo_project()),
        keymap: Keymap::default_bindings(),
        audio: AudioState::Stopped,
        meters,
        tap,
        mode,
        pushed: 0,
    };
    komponist_ui::snapshot::builder::<Bar>(ppp)
        .with_size(vec2(BAR_WIDTH, size::TRANSPORT))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, bar: &mut Bar| {
                let model = TransportModel {
                    project: &bar.project,
                    dirty: false,
                    state: TransportState::Playing,
                    playhead: komponist_model::Tick(0),
                    cpu: 0.07,
                    audio: &bar.audio,
                    keymap: &bar.keymap,
                    lang: Lang::De,
                    master_display: bar.mode,
                };
                let mut actions = Vec::new();
                transport::show(ui, ui.max_rect(), &model, &mut bar.meters, &mut actions);
            },
            state,
        )
}

/// Plays `hz` at `level` into the tap for `frames` UI frames, one frame's worth of samples
/// before each (as the audio thread would).
fn play(h: &mut Harness<'_, Bar>, hz: f32, level: f32, frames: usize) {
    for _ in 0..frames {
        let bar = h.state_mut();
        let n = (RATE / 60.0) as usize;
        let block: Vec<f32> = (bar.pushed..bar.pushed + n)
            .map(|i| level * (core::f32::consts::TAU * hz * i as f32 / RATE).sin())
            .collect();
        bar.pushed += n;
        bar.tap.push(&block);
        h.step();
    }
}

#[test]
fn snapshots_of_the_master_display_with_a_known_signal() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        // 200 Hz: six periods in the 30 ms window, standing still.
        let mut h = bar(MasterDisplay::Oscilloscope, ppp);
        play(&mut h, 200.0, 0.7, 12);
        let display = h
            .get_by_role_and_label(Role::Button, de::TRANSPORT_SCOPE_OSCILLOSCOPE)
            .rect();
        let cpu = h
            .get_by_role_and_label(Role::Status, de::TRANSPORT_CPU)
            .rect();
        assert_eq!(display.size(), vec2(m::SCOPE_WIDTH, size::CONTROL));
        assert_eq!(
            cpu.left() - display.right(),
            space::SPACE_4,
            "16 px to „CPU“"
        );
        assert!(h.state().tap.is_wanted(), "the display renews the lease");
        h.snapshot(format!("transport-scope-sine-{ppp}x"));
        results.extend_harness(&mut h);

        // 1 kHz at −6 dBFS: one peak, on the 1 kHz grid line.
        let mut h = bar(MasterDisplay::Spectrum, ppp);
        play(&mut h, 1000.0, 0.5, 12);
        h.get_by_role_and_label(Role::Button, de::TRANSPORT_SCOPE_SPECTRUM);
        h.snapshot(format!("transport-spectrum-sine-{ppp}x"));
        results.extend_harness(&mut h);
    }
    // Silence: the flat line on the floor under the grid.
    let mut h = bar(MasterDisplay::Spectrum, 1.0);
    h.run_steps(3);
    h.snapshot("transport-spectrum-silent-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
