//! The routing UI (WP-103): Strg G, folders, sends and the output list in the mixer, the dock
//! chip, the arrangement's group and bus rows, the inspector's „Routing“ section, the
//! new-track dialog's „Bus“ type. Snapshots next to the routing proposal's mockups (internal
//! notes).

use crate::fake_host;

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::views::mixer::model::names as mixer_names;
use komponist_app::views::routing::names;
use komponist_app::{Action, App, EditAction, UiAction};
use komponist_model::edits::{self, NewBus, SendProp};
use komponist_model::fixtures::demo_project;
use komponist_model::{Project, Tap, TrackId, TrackOutput};

use fake_host::FakeHost;

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn harness_at(app: App, ppp: f32) -> Harness<'static, App> {
    komponist_ui::snapshot::builder::<App>(ppp)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    h
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

fn id_of(p: &Project, name: &str) -> TrackId {
    p.tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no track {name}"))
        .id
}

/// The demo (Drums Bass Lead Pad Keys Arp) with two groups („Rhythmus“: Drums, Bass; „Melodie“:
/// Lead, Arp), two effect buses („Hall“ and „Delay“) and sends: Drums → Hall −10, Lead → Hall
/// −6 pre, Lead → Delay −16, Pad → Hall −6, Keys → Delay −12.
fn routed() -> Project {
    let p = demo_project();
    let t = |p: &Project, n: &str| id_of(p, n);
    let (p, _) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Rhythmus".into()),
            ..NewBus::group(vec![t(&p, "Drums"), t(&p, "Bass")])
        },
    )
    .unwrap();
    let (p, _) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Melodie".into()),
            ..NewBus::group(vec![t(&p, "Lead"), t(&p, "Arp")])
        },
    )
    .unwrap();
    let senders = |p: &Project, names: &[&str]| names.iter().map(|n| t(p, n)).collect::<Vec<_>>();
    let (p, hall) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Hall".into()),
            ..NewBus::effect(senders(&p, &["Drums", "Lead", "Pad"]))
        },
    )
    .unwrap();
    let (p, delay) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Delay".into()),
            ..NewBus::effect(senders(&p, &["Lead", "Keys"]))
        },
    )
    .unwrap();
    let send_of = |p: &Project, track: &str, bus: TrackId| {
        p.track(t(p, track)).unwrap().send_to(bus).unwrap().id
    };
    let mut p = p;
    for (track, bus, db, tap) in [
        ("Drums", hall, -10.0, Tap::Post),
        ("Lead", hall, -6.0, Tap::Pre),
        ("Lead", delay, -16.0, Tap::Post),
        ("Pad", hall, -6.0, Tap::Post),
        ("Keys", delay, -12.0, Tap::Post),
    ] {
        let id = send_of(&p, track, bus);
        p = edits::set_send(&p, id, SendProp::LevelDb(db)).unwrap();
        p = edits::set_send(&p, id, SendProp::Tap(tap)).unwrap();
    }
    p
}

fn app_with(p: Project) -> App {
    App::headless(p).with_device_host(Box::new(FakeHost::new()))
}

fn select(app: &mut App, t: TrackId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![t],
        mode: SelectMode::Replace,
    })));
}

#[test]
fn the_full_mixer_with_groups_buses_and_sends() {
    let mut app = app_with(routed());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    mask_cpu(&mut h);
    h.snapshot("routing-mixer-full-1x");
}

#[test]
fn the_arrangement_with_folder_and_busse_section() {
    let mut app = app_with(routed());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    let mut h = harness(app);
    mask_cpu(&mut h);
    h.snapshot("routing-arrangement-1x");
}
