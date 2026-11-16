//! The bridge's own plugin frame window on macOS (#52): `OpenGui{OwnFrame}` opens an eframe window
//! with the header and the plugin's `NSView` below it, `CloseGui` closes both, blocks keep being
//! processed meanwhile, and the frame can open again in the same bridge (run-and-return).
//!
//! Every bridge runs with `KOMPONIST_FRAME_OFFSCREEN=1`: frames open at x/y = −20000 and the
//! process can never activate itself, so nothing shows on the desktop or takes the focus. The
//! installed-plugin tests are `#[ignore]` (large third-party plugins):
//! `cargo test -p komponist-bridge --test it frame_mac:: -- --ignored --test-threads=1`.

use crate::common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use komponist_ipc::frame::{FrameRect, HeaderModel, Mark, PluginView};
use komponist_ipc::{BlockStatus, BridgeMsg, ControlMsg, ErrorContext, GuiSize, GuiTarget};

/// Height of the frame header, points (`komponist_ui::plugin_frame::metrics::HEADER`).
const HEADER: f32 = 28.0;

/// Where off-screen frames open.
const OFFSCREEN: f32 = -20_000.0;

fn offscreen_bridge() -> Bridge {
    Bridge::spawn_with(SpawnOptions {
        env: vec![("KOMPONIST_FRAME_OFFSCREEN".into(), "1".into())],
        ..SpawnOptions::default()
    })
}

fn header(name: &str) -> HeaderModel {
    HeaderModel {
        name: name.into(),
        sub: "Pad · CLAP".into(),
        mark: Mark::Active,
        bypassed: false,
        presets: Vec::new(),
        preset: None,
        preset_changed: false,
        can_save: false,
        view: PluginView::Gui,
        has_gui: true,
        keep_on_top: false,
        close_shortcut: "Esc".into(),
    }
}

fn own_frame(name: &str, rect: Option<FrameRect>) -> ControlMsg {
    ControlMsg::OpenGui(GuiTarget::OwnFrame {
        header: header(name),
        lang: "de".into(),
        rect,
    })
}

/// `OpenGui{OwnFrame}` → `GuiOpened`: the plugin's size in points and whether it is resizable.
fn open(bridge: &mut Bridge, name: &str) -> (GuiSize, bool) {
    bridge.send(own_frame(name, None));
    bridge.expect("GuiOpened", |m| match m {
        BridgeMsg::GuiOpened { size, resizable } => Some((*size, *resizable)),
        _ => None,
    })
}

/// `CloseGui` → `GuiClosed` with the frame's last rect.
fn close(bridge: &mut Bridge) -> FrameRect {
    bridge.send(ControlMsg::CloseGui);
    bridge.expect("GuiClosed", |m| match m {
        BridgeMsg::GuiClosed { rect } => Some(rect.expect("the own frame reports its rect")),
        _ => None,
    })
}

/// A few blocks, all processed in time, while the frame is open.
fn process(bridge: &mut Bridge) {
    for i in 0..8 {
        let events = if i == 0 {
            vec![note_on(0, 60)]
        } else {
            Vec::new()
        };
        assert_eq!(
            bridge.block(&events).status,
            BlockStatus::Ok as u32,
            "block {i}"
        );
    }
}

fn synth_frame(bundle: &Path, id: &str) {
    let mut bridge = offscreen_bridge();
    let info = bridge.load(bundle, id);
    assert!(info.has_gui, "kp-test-synth has a Cocoa GUI");
    bridge.activate();

    let t0 = Instant::now();
    let (size, resizable) = open(&mut bridge, "kp-test-synth");
    eprintln!("first open: {:?}", t0.elapsed());
    // The editor's fixed 240 × 80 logical px are points here.
    assert_eq!((size.width, size.height, resizable), (240, 80, false));
    process(&mut bridge);
    bridge.sync();
    // A second open while one is open is refused.
    bridge.send(own_frame("kp-test-synth", None));
    assert!(
        bridge
            .expect_error(ErrorContext::Gui)
            .contains("already open")
    );
    // A fixed-size plugin keeps its size: the frame answers with it.
    bridge.send(ControlMsg::SetGuiSize(GuiSize {
        width: 500,
        height: 400,
    }));
    let kept = bridge.expect("GuiSizeRequest", |m| match m {
        BridgeMsg::GuiSizeRequest(s) => Some(*s),
        _ => None,
    });
    assert_eq!(kept, size);
    // Header changes and the activation request are taken while open (off-screen: no activation).
    let mut changed = header("kp-test-synth");
    changed.bypassed = true;
    changed.mark = Mark::Bypassed;
    changed.keep_on_top = true;
    bridge.send(ControlMsg::HeaderState {
        header: changed,
        lang: "en".into(),
    });
    bridge.send(ControlMsg::ActivateFrame);
    // komponist became active: the frame comes forward (off-screen: no activation either way).
    bridge.send(ControlMsg::RaiseFrame);
    bridge.sync();
    // The frame told its window number right after it opened.
    let window = bridge
        .seen
        .iter()
        .find_map(|m| match m {
            BridgeMsg::FrameWindow(n) => Some(*n),
            _ => None,
        })
        .expect("FrameWindow after GuiOpened");
    assert!(window > 0, "{window}");

    let rect = close(&mut bridge);
    assert_eq!((rect.x, rect.y), (OFFSCREEN, OFFSCREEN), "{rect:?}");
    // The plugin below the header; the frame at least as wide as the plugin (the header needs
    // more than 240 points, so the plugin is centred in a wider frame).
    assert_eq!(rect.height, 80.0 + HEADER, "{rect:?}");
    assert!(rect.width >= 240.0, "{rect:?}");

    // Again in the same bridge: eframe's event loop is reused.
    let t1 = Instant::now();
    assert_eq!(open(&mut bridge, "kp-test-synth").0, size);
    eprintln!("reopen: {:?}", t1.elapsed());
    process(&mut bridge);
    let again = close(&mut bridge);
    assert_eq!(again.height, rect.height);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn the_test_synth_opens_in_an_own_frame_clap() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    synth_frame(&bundle, SYNTH);
}

#[test]
fn the_test_synth_opens_in_an_own_frame_vst3() {
    let Some(bundle) = fixture_in("vst3", "kp-test-synth") else {
        return;
    };
    synth_frame(&bundle, SYNTH_VST3);
}

#[test]
fn shutdown_closes_an_open_frame() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut bridge = offscreen_bridge();
    bridge.load(&bundle, SYNTH);
    bridge.activate();
    open(&mut bridge, "kp-test-synth");
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn x11_targets_and_plugins_without_gui_are_refused() {
    let Some(bundle) = fixture("kp-test-fx") else {
        return;
    };
    let mut bridge = offscreen_bridge();
    let info = bridge.load(&bundle, FX);
    assert!(!info.has_gui);
    bridge.send(own_frame("kp-test-fx", None));
    bridge.expect_error(ErrorContext::Gui);
    bridge.send(ControlMsg::OpenGui(GuiTarget::X11Child {
        xid: 1,
        scale: 1.0,
    }));
    bridge.expect_error(ErrorContext::Gui);
    // Raising without a frame does nothing.
    bridge.send(ControlMsg::RaiseFrame);
    bridge.sync();
    // Closing what never opened still answers.
    bridge.send(ControlMsg::CloseGui);
    bridge.expect("GuiClosed", |m| {
        matches!(m, BridgeMsg::GuiClosed { .. }).then_some(())
    });
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

// ------------------------------------------------------------------------------------------------
// Installed plugins
// ------------------------------------------------------------------------------------------------

/// Opens `path`'s frame twice; for a resizable plugin also a user resize to 900 × 600 points, which
/// the plugin may adjust: the frame then reports the adjusted body.
fn installed_frame(path: &str, plugin_id: &str) {
    let Some(bundle) = installed(path) else {
        return;
    };
    let mut bridge = offscreen_bridge();
    let info = bridge.load(&bundle, plugin_id);
    assert!(info.has_gui, "{path}: no Cocoa GUI");
    bridge.activate();
    let t0 = Instant::now();
    let (size, resizable) = open(&mut bridge, &info.descriptor.name);
    eprintln!(
        "{}: {}×{} pt, resizable {resizable}, opened in {:?}",
        info.descriptor.name,
        size.width,
        size.height,
        t0.elapsed()
    );
    assert!(size.width > 100 && size.height > 100, "{size:?}");
    process(&mut bridge);
    if resizable {
        bridge.send(ControlMsg::SetGuiSize(GuiSize {
            width: 900,
            height: 600,
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        let moved = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match bridge.recv_timeout(left) {
                Some(BridgeMsg::FrameMoved(rect)) => break rect,
                Some(BridgeMsg::Error { message, .. }) => panic!("{message}"),
                Some(other) => bridge.seen.push(other),
                None => panic!("no FrameMoved after the resize"),
            }
        };
        eprintln!("{}: resized to {moved:?}", info.descriptor.name);
        let body = (moved.width, moved.height - HEADER);
        // Surge snaps to its zoom steps (CLAP) or keeps its aspect ratio (VST3): near 900 × 600.
        assert!((body.0 - 900.0).abs() <= 100.0, "{moved:?}");
        assert!((body.1 - 600.0).abs() <= 100.0, "{moved:?}");
    }
    let rect = close(&mut bridge);
    assert_eq!((rect.x, rect.y), (OFFSCREEN, OFFSCREEN), "{rect:?}");
    let (again, _) = open(&mut bridge, &info.descriptor.name);
    assert!(again.width > 100, "{again:?}");
    close(&mut bridge);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn surge_xt_frame() {
    installed_frame(
        "/Library/Audio/Plug-Ins/CLAP/Surge XT.clap",
        "org.surge-synth-team.surge-xt",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn surge_xt_vst3_frame() {
    installed_frame(
        "/Library/Audio/Plug-Ins/VST3/Surge XT.vst3",
        "ABCDEF019182FAEB566D624153675854",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn dexed_frame() {
    installed_frame(
        "/Library/Audio/Plug-Ins/CLAP/Dexed.clap",
        "com.digital-suburban.dexed",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn dexed_vst3_frame() {
    installed_frame(
        "/Library/Audio/Plug-Ins/VST3/Dexed.vst3",
        "ABCDEF019182FAEB4447534244657864",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn klopfer_frame() {
    installed_frame(
        "~/Library/Audio/Plug-Ins/CLAP/Klopfer.clap",
        "com.klopfer.drum-sampler",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn klopfer_vst3_frame() {
    installed_frame(
        "~/Library/Audio/Plug-Ins/VST3/Klopfer.vst3",
        "4B6C6F706665724472756D536D706C72",
    );
}
