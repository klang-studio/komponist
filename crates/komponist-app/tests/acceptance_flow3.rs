//! Acceptance flow 3 (HANDOFF §9): build a drum beat in the step sequencer and play it. With the
//! real klopfer bundle and its test kit; without them kp-test-synth (WP-27) plays; without that
//! the test skips with a note.
//!
//! The app runs on the null backend in real time with the real bridge. A double-click into the
//! empty lane makes the step clip, clicks on the steps build the beat, Space plays: the document
//! holds the pattern, the engine's transport runs and the track's meter shows signal.

mod acceptance_support;
mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use acceptance_support::{click_at, double_click, harness, key};
use common::{
    PATIENCE, SYNTH, app_with, clap_plugin, config, fixture_dirs, scratch, step_for, step_until,
};
use egui::Key;
use egui::accesskit::Role;
use egui::pos2;
use egui_kittest::kittest::Queryable as _;
use komponist_app::device_host::{DeviceHost as _, PluginHost};
use komponist_app::step_rows;
use komponist_app::views::arrangement::names as arrangement_names;
use komponist_app::{Action, EditAction, UiAction};
use komponist_engine::TransportState;
use komponist_model::edits::{self, NewClip, NewClipContent, NewTrack};
use komponist_model::project::{BUILTIN_KLOPFER, DEFAULT_PROJECT_NAME};
use komponist_model::{ClipContent, PluginFormat, PluginRef, Project, Tick};
use komponist_plugins::builtin::klopfer_kit_state;
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerConfig, PluginManagerEvent};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const BAR: i64 = 3840;

/// `(row, step)` hits: kick, snare, closed hat (rows 0, 1, 4 exist for klopfer's eight pads and for
/// a generic instrument's drum rows alike).
const BEAT: [(usize, usize); 8] = [
    (0, 0),
    (4, 2),
    (1, 4),
    (4, 6),
    (0, 8),
    (4, 10),
    (1, 12),
    (4, 14),
];

/// What plays the beat.
struct Sound {
    plugin: PluginRef,
    /// klopfer's kit as plugin state.
    state: Option<Vec<u8>>,
    config: PluginManagerConfig,
    scan: Option<PluginDirs>,
    label: &'static str,
}

fn klopfer_sound(dir: &Path) -> Option<Sound> {
    let bundle = PluginManagerConfig::defaults().klopfer_bundle?;
    let kit = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../klopfer/testkit-out/Testkit.klopfer")
        .canonicalize()
        .ok()?;
    Some(Sound {
        plugin: PluginRef {
            format: PluginFormat::Builtin,
            id: BUILTIN_KLOPFER.into(),
            name: "klopfer".into(),
            vendor: "Klopfer".into(),
            version: "0.1.0".into(),
        },
        state: Some(klopfer_kit_state(&kit).expect("kit state")),
        config: PluginManagerConfig {
            klopfer_bundle: Some(bundle),
            ..config(dir)
        },
        scan: None,
        label: "klopfer",
    })
}

fn synth_sound(dir: &Path) -> Option<Sound> {
    let scan = fixture_dirs()?;
    Some(Sound {
        plugin: clap_plugin(SYNTH),
        state: None,
        config: config(dir),
        scan: Some(scan),
        label: "kp-test-synth",
    })
}

fn project_with(sound: &Sound) -> Project {
    let spec = NewTrack {
        name: None,
        hue: None,
        instrument: Some(sound.plugin.clone()),
        index: None,
        arm_exclusive: false,
    };
    let (mut p, _) = edits::add_track(&Project::new(DEFAULT_PROJECT_NAME), spec).unwrap();
    if let Some(state) = &sound.state {
        let track = Arc::make_mut(&mut p.tracks[0]);
        let device = Arc::make_mut(&mut track.channel.devices[0]);
        device.state = Some(Arc::from(state.as_slice()));
    }
    p
}

fn x_of(h: &egui_kittest::Harness<'_, komponist_app::App>, tick: Tick) -> f32 {
    let ruler = h
        .get_all_by_role_and_label(Role::Slider, arrangement_names::RULER)
        .map(|n| n.rect())
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("ruler");
    let v = h.state().ui().arrangement;
    ruler.left() + ((tick.0 as f64 - v.scroll_tick) as f32) * (v.px_per_bar / BAR as f32)
}

#[test]
fn a_drum_beat_built_in_the_step_sequencer_plays() {
    let dir = scratch("flow3");
    // KOMPONIST_ACCEPTANCE_SYNTH=1 forces the fallback (to try it where klopfer is built).
    let forced = std::env::var_os("KOMPONIST_ACCEPTANCE_SYNTH").is_some();
    let klopfer = if forced { None } else { klopfer_sound(&dir) };
    let Some(sound) = klopfer.or_else(|| synth_sound(&dir)) else {
        eprintln!("skipped: neither the klopfer bundle with its test kit nor kp-test-synth exists");
        return;
    };
    eprintln!("flow 3 plays {}", sound.label);
    let project = project_with(&sound);
    let track = project.tracks[0].id;
    let device = project.tracks[0].channel.devices[0].id;
    let mut host = PluginHost::new(sound.config.clone());
    if let Some(scan) = &sound.scan {
        host.start_scan(scan);
    }
    let mut app = app_with(project, host);
    step_until(&mut app, "the instrument is loaded", |a| {
        a.engine().device_status(device) == DeviceStatus::Ok
    });
    let mut h = harness(app);

    // A double-click into the empty lane: a one-bar step clip, opened in the sequencer.
    let header = h
        .get_all_by_role(Role::Row)
        .find(|n| {
            egui_kittest::kittest::NodeT::accesskit_node(n)
                .label()
                .is_some_and(|l| l.starts_with("Spur "))
        })
        .expect("track header")
        .rect();
    if sound.state.is_some() {
        let at = pos2(x_of(&h, Tick(BAR / 3)), header.center().y);
        double_click(&mut h, at);
    } else {
        // kp-test-synth is no drum instrument: its lane makes MIDI clips, so the step clip (the
        // General MIDI rows) is added by the action the double-click would send for klopfer.
        let rows = step_rows::rows_for(None, &[]);
        h.state_mut().dispatch(Action::Edit(EditAction::AddClip {
            track,
            clip: NewClip {
                start: Tick::ZERO,
                length: Tick(BAR),
                name: "Pattern 1".into(),
                content: NewClipContent::Step { rows },
            },
        }));
        h.run_steps(2);
        let clip = h.state().project().tracks[0].clips[0].id;
        h.state_mut().dispatch(Action::Ui(UiAction::OpenClip(clip)));
        h.run_steps(3);
    }
    let clip = h.state().project().tracks[0].clips[0].id;
    assert_eq!(h.state().ui().editor.clip, Some(clip));
    let rows = match &h.state().project().find_clip(clip).unwrap().1.content {
        ClipContent::Step(s) => s.rows.clone(),
        ClipContent::Midi(_) => panic!("not a step clip"),
    };
    assert!(rows.len() >= 5);

    // The beat, one click per step.
    for (row, step) in BEAT {
        let label = format!("Step {}, {}, ", step + 1, rows[row].name);
        let pos = h.get_by_label_contains(&label).rect().center();
        click_at(&mut h, pos);
    }
    let on = |h: &egui_kittest::Harness<'_, komponist_app::App>| -> Vec<(usize, usize)> {
        let ClipContent::Step(s) = &h.state().project().find_clip(clip).unwrap().1.content else {
            panic!()
        };
        let mut on = Vec::new();
        for (r, row) in s.rows.iter().enumerate() {
            for (i, step) in row.steps.iter().enumerate() {
                if step.on {
                    on.push((r, i));
                }
            }
        }
        on.sort_unstable();
        on
    };
    let mut expected = BEAT.to_vec();
    expected.sort_unstable();
    assert_eq!(on(&h), expected);

    // The instrument gets a moment after activation before it sounds (klopfer loads its pads in the
    // background), then Space plays.
    step_for(h.state_mut(), Duration::from_secs(1));
    h.state().engine().track_meter(track).unwrap().take();
    key(&mut h, Key::Space);
    let until = std::time::Instant::now() + PATIENCE;
    while h.state().transport() != TransportState::Playing {
        assert!(std::time::Instant::now() < until, "not playing");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    // One and a half bars (3 s at 120 bpm); the beat's hits fall in the first.
    let mut peak = 0.0f32;
    let end = std::time::Instant::now() + Duration::from_millis(1500);
    while std::time::Instant::now() < end {
        h.step();
        let (l, r) = h.state().engine().track_meter(track).unwrap().take();
        peak = peak.max(l).max(r);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        h.state().engine().playhead() > Tick(BAR / 2),
        "the playhead runs"
    );
    assert!(peak > 0.01, "the drums sound (peak {peak})");
    key(&mut h, Key::Space);
    let until = std::time::Instant::now() + PATIENCE;
    while h.state().transport() != TransportState::Stopped {
        assert!(std::time::Instant::now() < until, "not stopped");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    let events: Vec<PluginManagerEvent> = h.state().device_events().to_vec();
    assert!(
        !events.iter().any(|e| format!("{e:?}").contains("Crash")),
        "no bridge crashed: {events:?}"
    );
}
