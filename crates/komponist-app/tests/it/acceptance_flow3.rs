//! Acceptance flow 3 (HANDOFF §9): build a drum beat in the step sequencer and play it, with the
//! sampler and its synthesized test kit (WP-87; it replaced klopfer as the drum instrument).
//!
//! The app runs on the null backend in real time. A double-click into the empty lane makes the
//! step clip whose rows are the sampler's pads (named after the kit's files), clicks on the steps
//! build the beat, Space plays: the document holds the pattern, the engine's transport runs and
//! the track's meter shows signal. The sampler runs in process, so no bridge or installed plugin
//! is needed.

use crate::acceptance_support;
use crate::common;

use std::sync::Arc;
use std::time::Duration;

use acceptance_support::{click_at, double_click, harness, key};
use common::{PATIENCE, app_with, config, scratch, step_for, step_until};
use egui::Key;
use egui::accesskit::Role;
use egui::pos2;
use egui_kittest::kittest::Queryable as _;
use komponist_app::device_host::PluginHost;
use komponist_builtin::Internal;
use komponist_builtin::sampler::{SampleLink, links, testkit};
use komponist_engine::TransportState;
use komponist_i18n::de;
use komponist_model::edits::{self, NewTrack};
use komponist_model::project::DEFAULT_PROJECT_NAME;
use komponist_model::{ClipContent, Project, Tick};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};

const BAR: i64 = 3840;

/// `(row, step)` hits: kick, snare, closed hat.
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

/// An empty project with one sampler track whose pads hold the synthesized kit (links into
/// `dir`).
fn project_with_kit(dir: &std::path::Path) -> Project {
    let spec = NewTrack {
        name: None,
        hue: None,
        instrument: Some(Internal::Sampler.plugin_ref()),
        index: None,
        arm_exclusive: false,
    };
    let (mut p, _) = edits::add_track(&Project::new(DEFAULT_PROJECT_NAME), spec).unwrap();
    let kit = testkit::write_kit(dir).unwrap();
    let mut state: Option<Vec<u8>> = None;
    for (pad, path) in kit.iter().enumerate() {
        state = Some(links::write(
            state.as_deref(),
            pad,
            Some(&SampleLink::new(path, None)),
        ));
    }
    let track = Arc::make_mut(&mut p.tracks[0]);
    let device = Arc::make_mut(&mut track.channel.devices[0]);
    device.state = state.map(Arc::from);
    p
}

fn x_of(h: &egui_kittest::Harness<'_, komponist_app::App>, tick: Tick) -> f32 {
    let ruler = h
        .get_all_by_role_and_label(Role::Slider, de::A11Y_RULER)
        .map(|n| n.rect())
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("ruler");
    let v = h.state().ui().arrangement;
    ruler.left() + ((tick.0 as f64 - v.scroll_tick) as f32) * (v.px_per_bar / BAR as f32)
}

#[test]
fn a_drum_beat_built_in_the_step_sequencer_plays() {
    let dir = scratch("flow3");
    let project = project_with_kit(&dir);
    let track = project.tracks[0].id;
    let device = project.tracks[0].channel.devices[0].id;
    let host = PluginHost::new(config(&dir));
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
    let at = pos2(x_of(&h, Tick(BAR / 3)), header.center().y);
    double_click(&mut h, at);
    let clip = h.state().project().tracks[0].clips[0].id;
    assert_eq!(h.state().ui().editor.clip, Some(clip));
    let rows = match &h.state().project().find_clip(clip).unwrap().1.content {
        ClipContent::Step(s) => s.rows.clone(),
        ClipContent::Midi(_) => panic!("not a step clip"),
    };
    // The pads of the kit are the rows, named after its files.
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, testkit::NAMES);
    assert_eq!(
        rows.iter().map(|r| r.note).collect::<Vec<_>>(),
        (36..44).collect::<Vec<u8>>()
    );

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

    // The kit loads in the background; give it a moment, then Space plays.
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
