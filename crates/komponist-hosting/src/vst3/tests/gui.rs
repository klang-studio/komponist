//! Embedded VST3 editors under Xvfb: `IPlugView::attached` into an x11rb parent window, the
//! plugin's fds and timers served through `IRunLoop` → [`PollLoop`], a check that the editor is a
//! child of the parent and draws, a GUI edit (`IComponentHandler::performEdit`), resize, close.
//!
//! `#[ignore]` and guarded by `KP_GUI_TESTS=1`: run only under Xvfb (see `super`), never on a
//! desktop. `KP_GUI_SHOTS=<dir>` additionally saves a screenshot per plugin with ImageMagick's
//! `import`. The parent window and helpers come from the CLAP GUI tests.

use std::time::{Duration, Instant};

use komponist_ipc::{GuiSize, WireEventKind};
use x11rb::protocol::xproto::{BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, MOTION_NOTIFY_EVENT};

use super::installed::{DECENT_SAMPLER, DEXED, KLOPFER, SHORTCIRCUIT_XT, SURGE_XT, SURGE_XT_FX};
use super::*;
use crate::clap::tests::gui::{Parent, cpu_seconds, gui_allowed};
use crate::host::FdEvents;
use crate::host_loop::notification_bits as bits;

/// What [`exercise_gui`] expects of an editor.
struct Expect {
    /// The editor draws into its window (false only for the known-blank fixture, see
    /// [`gui_kp_test_synth`]).
    drawn: bool,
    /// A mouse drag (from, to) in editor coordinates over a control: the edit must arrive as
    /// `performEdit` (a `ParamEdited` notification) and in the controller.
    drag: Option<((i16, i16), (i16, i16))>,
}

/// Drags the pointer while serving the loop; returns the `ParamEdited` count before and after and
/// the last edit.
fn drag(
    parent: &Parent,
    host_loop: &PollLoop,
    plugin: &mut dyn HostedPlugin,
    (from, to): ((i16, i16), (i16, i16)),
) -> (u32, u32, Option<(u32, f64)>) {
    let serve = |plugin: &mut dyn HostedPlugin, ms| {
        host_loop.run_for(plugin, Duration::from_millis(ms));
    };
    let (before, _) = host_loop.notify().param_edits();
    parent.pointer(MOTION_NOTIFY_EVENT, from);
    serve(plugin, 100);
    parent.pointer(BUTTON_PRESS_EVENT, from);
    serve(plugin, 100);
    for step in 1..=10i16 {
        let at = (
            from.0 + (to.0 - from.0) * step / 10,
            from.1 + (to.1 - from.1) * step / 10,
        );
        parent.pointer(MOTION_NOTIFY_EVENT, at);
        serve(plugin, 30);
    }
    parent.pointer(BUTTON_RELEASE_EVENT, to);
    serve(plugin, 300);
    let (after, last) = host_loop.notify().param_edits();
    (before, after, last)
}

/// Opens the editor of the first plugin in `bundle`, serves it, resizes and closes it.
fn exercise_gui(bundle: &Path, name: &str, expect: Expect) {
    // The parent outlives the plugin even when an assertion fails (locals drop in reverse).
    let parent = Parent::new(1600, 1000);
    let mut rig = load_first(bundle);
    let host_loop = rig.host_loop.clone();
    let plugin = rig.plugin.as_mut();
    assert!(plugin.has_gui());

    let opened = Instant::now();
    let info = plugin.gui_open(u64::from(parent.window), 1.0).unwrap();
    let open_time = opened.elapsed();
    assert!(info.size.width > 0 && info.size.height > 0, "{info:?}");
    assert!(matches!(
        plugin.gui_open(u64::from(parent.window), 1.0),
        Err(crate::HostingError::WrongState(_))
    ));

    // Serve the run loop while the editor comes up, and measure what the loop costs.
    host_loop.run_for(plugin, Duration::from_millis(1500));
    let cpu_start = cpu_seconds();
    let stats_start = host_loop.stats();
    let measure = Duration::from_secs(2);
    host_loop.run_for(plugin, measure);
    let cpu = (cpu_seconds() - cpu_start) / measure.as_secs_f64();
    let stats = host_loop.stats();
    let ticks = stats.ticks - stats_start.ticks;
    let (timers, fds) = host_loop.registrations();
    let observed = host_loop.take_observed();

    assert!(
        !parent.children().is_empty(),
        "no plugin window in the parent"
    );
    let colors = parent.distinct_colors(info.size);
    parent.screenshot(&format!("vst3-{name}-open"));
    // IRunLoop fds are polled for reading only: no spinning.
    assert!(ticks < 1500, "{ticks} loop ticks in 2 s");
    for fd in 0..1024 {
        if let Some(interest) = host_loop.fd_interest(fd) {
            assert_eq!(interest, FdEvents::READ, "fd {fd}");
        }
    }

    let mut edit = None;
    if let Some(path) = expect.drag {
        let (before, after, last) = drag(&parent, &host_loop, plugin, path);
        assert!(after > before, "no performEdit from the GUI drag");
        let (id, value) = last.unwrap();
        assert!(plugin.params().iter().any(|p| p.id == id));
        let now = plugin.param_value(id).unwrap();
        assert!(
            (now - value).abs() < 1e-6,
            "param {id}: {now} after edit {value}"
        );
        let observed = host_loop.take_observed();
        assert!(observed.has(bits::PARAM_EDITED) && observed.has(bits::STATE_DIRTY));
        assert!(observed.flushed.is_empty(), "VST3 edits need no flush");
        edit = Some((after - before, id, value));
    }

    // Resize: a resizable editor adopts (or snaps) a new size, a fixed one keeps its size.
    let errors_before = host_loop.notify().log_counts()[3];
    let asked = GuiSize {
        width: info.size.width + 160,
        height: info.size.height + 120,
    };
    let adopted = plugin.gui_set_size(asked).unwrap();
    host_loop.run_for(plugin, Duration::from_millis(500));
    if info.resizable {
        assert!(adopted.width > 0 && adopted.height > 0);
        parent.screenshot(&format!("vst3-{name}-resized"));
    } else {
        assert_eq!(adopted, info.size);
    }
    let resize_errors = host_loop.notify().log_counts()[3] - errors_before;
    let resize_observed = host_loop.take_observed();

    plugin.gui_close();
    plugin.gui_close();
    host_loop.run_for(plugin, Duration::from_millis(200));
    let after_close = host_loop.registrations();

    // A second open after a close works too.
    let again = plugin.gui_open(u64::from(parent.window), 1.0).unwrap();
    host_loop.run_for(plugin, Duration::from_millis(300));
    plugin.gui_close();
    assert_eq!(again.size.width > 0, info.size.width > 0);

    eprintln!(
        "VST3 GUI {name}: opened in {open_time:?}, size {}x{}, resizable {}, asked {}x{} -> {}x{}, \
         {colors} colours, {timers} timers, {fds} fds, {ticks} ticks / {} timer calls / {} fd \
         calls in 2 s, CPU {:.0} %, resize requests {:?} then {:?}, error logs after resize \
         {resize_errors}, registrations after close {after_close:?}, GUI edit (edits, id, \
         value) {edit:?}",
        info.size.width,
        info.size.height,
        info.resizable,
        asked.width,
        asked.height,
        adopted.width,
        adopted.height,
        stats.timer_calls - stats_start.timer_calls,
        stats.fd_calls - stats_start.fd_calls,
        cpu * 100.0,
        observed
            .has(bits::GUI_RESIZE_REQUESTED)
            .then_some(observed.gui_size),
        resize_observed
            .has(bits::GUI_RESIZE_REQUESTED)
            .then_some(resize_observed.gui_size),
    );
    if expect.drawn {
        assert!(colors > 1, "the editor area is blank");
    }

    drop(rig);
    assert_eq!(host_loop.registrations(), (0, 0), "leftover timers or fds");
    drop(parent);
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_kp_test_synth() {
    if !gui_allowed() {
        return;
    }
    if let Some(bundle) = fixture("kp-test-synth") {
        // Known (as for CLAP): the fixture's crates.io baseview stays black under Xvfb (no X11
        // Present/MapNotify fix, spike WP-15 §1.8). Attach, size, close and cleanup are checked.
        exercise_gui(
            &bundle,
            "kp-test-synth",
            Expect {
                drawn: false,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_klopfer() {
    if gui_allowed()
        && let Some(bundle) = installed(KLOPFER)
    {
        exercise_gui(
            &bundle,
            "klopfer",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_surge_xt() {
    if gui_allowed()
        && let Some(bundle) = installed(SURGE_XT)
    {
        exercise_gui(
            &bundle,
            "surge-xt",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_surge_xt_effects() {
    if gui_allowed()
        && let Some(bundle) = installed(SURGE_XT_FX)
    {
        exercise_gui(
            &bundle,
            "surge-xt-effects",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_dexed() {
    if gui_allowed()
        && let Some(bundle) = installed(DEXED)
    {
        exercise_gui(
            &bundle,
            "dexed",
            Expect {
                drawn: true,
                // The Cutoff knob (default: fully open), dragged down.
                drag: Some(((250, 463), (250, 523))),
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_decent_sampler() {
    if gui_allowed()
        && let Some(bundle) = installed(DECENT_SAMPLER)
    {
        exercise_gui(
            &bundle,
            "decent-sampler",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_shortcircuit_xt() {
    if gui_allowed()
        && let Some(bundle) = installed(SHORTCIRCUIT_XT)
    {
        exercise_gui(
            &bundle,
            "shortcircuit-xt",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

/// The bridge's real situation: the RT thread processes while the main thread serves the editor.
/// A GUI edit arrives as `performEdit` on the main thread, reaches the controller, and goes to the
/// processor through the parameter ring (no flush while active), which echoes it with its
/// gestures as output events.
#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_dexed_edit_while_processing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    if !gui_allowed() {
        return;
    }
    let Some(bundle) = installed(DEXED) else {
        return;
    };
    let parent = Parent::new(1600, 1000);
    let mut rig = load_first(&bundle);
    let host_loop = rig.host_loop.clone();
    let plugin = rig.plugin.as_mut();
    let processor = plugin.activate(CONFIG).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let rt = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let mut processor = processor;
            processor.start_processing().unwrap();
            let mut blocks = Blocks::new();
            let mut events = Vec::new();
            let mut peak = 0.0f32;
            let note = |on| {
                komponist_ipc::WireEvent::new(
                    0,
                    if on {
                        WireEventKind::NoteOn {
                            channel: 0,
                            key: 60,
                            velocity: 100,
                            note_id: u32::MAX,
                        }
                    } else {
                        WireEventKind::NoteOff {
                            channel: 0,
                            key: 60,
                            note_id: u32::MAX,
                        }
                    },
                )
            };
            blocks.run(processor.as_mut(), &[note(true)]);
            while !stop.load(Ordering::Acquire) {
                blocks.run(processor.as_mut(), &[]);
                peak = peak.max(blocks.peak(0..BLOCK));
                events.extend_from_slice(&blocks.events_out[..blocks.out_count]);
                std::thread::sleep(Duration::from_millis(5));
            }
            blocks.run(processor.as_mut(), &[note(false)]);
            processor.stop_processing();
            (processor, events, peak)
        }
    });

    plugin.gui_open(u64::from(parent.window), 1.0).unwrap();
    host_loop.run_for(plugin, Duration::from_millis(1000));
    // The Cutoff knob again, dragged down.
    let (before, after, last) = drag(&parent, &host_loop, plugin, ((250, 463), (250, 523)));
    host_loop.run_for(plugin, Duration::from_millis(200));
    stop.store(true, Ordering::Release);
    let (processor, events, peak) = rt.join().unwrap();
    plugin.gui_close();
    plugin.deactivate(processor);

    let observed = host_loop.take_observed();
    assert!(observed.flushed.is_empty(), "flushed while active");
    assert!(after > before, "no performEdit while processing");
    let (id, value) = last.unwrap();
    assert!((plugin.param_value(id).unwrap() - value).abs() < 1e-6);
    // The processor echoed the edit with its gestures as output events.
    let kinds: Vec<_> = events
        .iter()
        .filter_map(komponist_ipc::WireEvent::decode)
        .collect();
    assert!(
        kinds.contains(&WireEventKind::ParamGesture { id, begin: true })
            && kinds.contains(&WireEventKind::ParamGesture { id, begin: false }),
        "no gestures in {kinds:?}"
    );
    assert!(
        kinds.contains(&WireEventKind::ParamValue { id, value }),
        "the last value is missing in {kinds:?}"
    );
    assert!(
        peak > 0.01,
        "the note played while the editor was open: {peak}"
    );
    eprintln!(
        "VST3 GUI dexed while processing: {} edits, last {id} = {value}, {} output events, \
         peak {peak:.3}",
        after - before,
        events.len()
    );
    drop(rig);
    drop(parent);
}
