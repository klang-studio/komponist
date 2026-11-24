//! Installed third-party plugins through the plugin manager and the engine (`#[ignore]`):
//! klopfer from a scan of `~/.clap` (it is an ordinary CLAP plugin since the sampler, WP-87), Dexed
//! and Surge XT from a scan of `/usr/lib/clap`.
//!
//! ```text
//! cargo test -p komponist-plugins --test it installed:: -- --ignored --nocapture
//! ```

use crate::common;

use std::path::PathBuf;
use std::time::Instant;

use common::*;
use komponist_model::{DeviceId, DeviceRole};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManager, PluginManagerEvent};

fn installed(path: &str) -> Option<PathBuf> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => PathBuf::from(path),
    };
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {} is not installed", path.display());
        None
    }
}

/// Loads `plugin` as an instrument, plays `key` offline and returns the peak.
fn load_and_play(manager: PluginManager, plugin: komponist_model::PluginRef, key: u8) -> f32 {
    let name = plugin.name.clone();
    let t0 = Instant::now();
    let instrument = device(10, DeviceRole::Instrument, plugin, None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![instrument])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let loaded = t0.elapsed();
    rig.render(BLOCK);
    rig.note(1, key, 110);
    let range = rig.render(40 * BLOCK);
    rig.note(1, key, 0);
    rig.render(8 * BLOCK);
    let level = rig.peak(range);
    let params = rig.manager.params(DeviceId(10)).map_or(0, <[_]>::len);
    let states = rig.manager.snapshot_states(PATIENCE);
    eprintln!(
        "{name}: loaded in {loaded:.1?}, {params} parameters, peak {level:.3}, state {} bytes",
        states.first().map_or(0, |(_, s)| s.bytes.len())
    );
    assert_eq!(states.len(), 1);
    rig.manager.shutdown();
    level
}

#[test]
#[ignore = "installed third-party plugins; run with --ignored"]
fn klopfer_as_a_scanned_plugin() {
    let Some(folder) = installed("~/.clap") else {
        return;
    };
    let dir = scratch("installed-klopfer");
    let mut manager = PluginManager::new(config(&dir));
    manager.start_scan(&PluginDirs {
        clap: vec![folder],
        vst3: Vec::new(),
    });
    wait_for(&mut manager, "ScanFinished", |e| match e {
        PluginManagerEvent::ScanFinished(r) => Some(r.clone()),
        _ => None,
    });
    let Some(entry) = manager
        .catalog()
        .resolve(&clap_plugin("com.klopfer.drum-sampler"))
    else {
        eprintln!("skipped: Klopfer.clap not found in ~/.clap");
        return;
    };
    assert!(!entry.is_builtin());
    // A project saved with the old built-in entry finds the same plugin.
    assert_eq!(manager.catalog().resolve(&klopfer()), Some(entry));
    let plugin = entry.plugin.clone();
    // Pad 1 = note 36; the default kit may be empty, so only loading is required.
    load_and_play(manager, plugin, 36);
}

#[test]
#[ignore = "installed third-party plugins; run with --ignored"]
fn dexed_and_surge_xt_from_a_scan() {
    if installed("/usr/lib/clap").is_none() {
        return;
    }
    let dir = scratch("installed-scan");
    let scan = |dir: &std::path::Path| {
        let mut manager = PluginManager::new(config(dir));
        manager.start_scan(&PluginDirs {
            clap: vec!["/usr/lib/clap".into()],
            vst3: Vec::new(),
        });
        let (report, _) = wait_for(&mut manager, "ScanFinished", |e| match e {
            PluginManagerEvent::ScanFinished(r) => Some(r.clone()),
            _ => None,
        });
        (manager, report)
    };
    let t0 = Instant::now();
    let (manager, report) = scan(&dir);
    eprintln!("scan of /usr/lib/clap in {:.1?}: {report:?}", t0.elapsed());
    drop(manager);
    for (id, key) in [
        ("com.digital-suburban.dexed", 60),
        ("org.surge-synth-team.surge-xt", 60),
    ] {
        let (manager, _) = scan(&dir);
        let Some(entry) = manager.catalog().resolve(&clap_plugin(id)) else {
            eprintln!("skipped: {id} not found");
            continue;
        };
        let plugin = entry.plugin.clone();
        let level = load_and_play(manager, plugin, key);
        assert!(level > 0.01, "{id}: silent ({level})");
    }
}

/// The kind LaunchServices gives process `pid` (`lsappinfo`): "Foreground" means a Dock icon,
/// "UIElement" and "BackgroundOnly" none; empty when the process never checked in.
#[cfg(target_os = "macos")]
fn application_type(pid: u32) -> String {
    let out = std::process::Command::new("lsappinfo")
        .args(["info", "-only", "applicationtype"])
        .arg(pid.to_string())
        .output()
        .expect("lsappinfo");
    // `"ApplicationType"="UIElement"`, or a record with `type="…"` while it checks in.
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_once("type=")
        .or_else(|| text.split_once("ApplicationType\"="))
        .map(|(_, rest)| {
            rest.trim_start_matches('"')
                .split('"')
                .next()
                .unwrap_or("")
                .to_owned()
        })
        .unwrap_or_default()
}

/// A bridge started from inside an app bundle gets no Dock icon, even after a plugin initialised
/// AppKit (#100: in `komponist.app` the bridge is the bundle's executable, so LaunchServices took it
/// for a second komponist, and JUCE's `NSApplication` gave it a bouncing Dock tile). The bundle is
/// `KOMPONIST_BUNDLE` (a `komponist.app` from `scripts/bundle-macos.sh`; its executable runs with
/// `--bridge`) or, without it, a minimal app bundle with the real `Info.plist` around
/// `komponist-bridge`. Needs Surge XT in `/Library/Audio/Plug-Ins/CLAP`. With
/// `KOMPONIST_FRAME_OFFSCREEN=1` it also opens Surge's frame, off screen (never on the desktop),
/// and samples the process while the frame opens.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "installed third-party plugins; run with --ignored"]
fn a_bridge_inside_an_app_bundle_has_no_dock_icon() {
    let Some(folder) = installed("/Library/Audio/Plug-Ins/CLAP/Surge XT.clap") else {
        return;
    };
    let dir = scratch("bundle-dock");
    let mut config = config(&dir);
    if let Some(bundle) = std::env::var_os("KOMPONIST_BUNDLE") {
        config.bridge_exe = PathBuf::from(bundle).join("Contents/MacOS/komponist");
        config.bridge_args = vec!["--bridge".into()];
    } else {
        // The bundle's real Info.plist, with `komponist-bridge` as its executable and its own id.
        let contents = dir.join("Bridge Test.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let plist = std::fs::read_to_string(root.join("assets/macos/Info.plist.in"))
            .unwrap()
            .replace("@VERSION@", "0.0.0")
            .replace(
                "<key>CFBundleExecutable</key>\n    <string>komponist</string>",
                "<key>CFBundleExecutable</key>\n    <string>komponist-bridge</string>",
            )
            .replace(
                "<string>io.github.klang-studio.komponist</string>",
                "<string>io.github.klang-studio.komponist.bridge-test</string>",
            );
        // Without the document and type declarations: LaunchServices must not take this
        // temporary bundle for an owner of komponist projects.
        let cut = plist.find("    <key>CFBundleDocumentTypes</key>").unwrap();
        let plist = format!("{}</dict>\n</plist>\n", &plist[..cut]);
        assert!(plist.contains("<string>komponist-bridge</string>"));
        assert!(plist.contains("<key>LSUIElement</key>"));
        std::fs::write(contents.join("Info.plist"), plist).unwrap();
        let exe = contents.join("MacOS/komponist-bridge");
        std::fs::copy(&config.bridge_exe, &exe).unwrap();
        config.bridge_exe = exe;
    }
    let mut manager = PluginManager::new(config);
    manager.start_scan(&PluginDirs {
        clap: vec![folder.parent().unwrap().to_path_buf()],
        vst3: Vec::new(),
    });
    wait_for(&mut manager, "ScanFinished", |e| {
        matches!(e, PluginManagerEvent::ScanFinished(_)).then_some(())
    });
    let Some(entry) = manager
        .catalog()
        .resolve(&clap_plugin("org.surge-synth-team.surge-xt"))
    else {
        eprintln!("skipped: Surge XT not found by the scan");
        return;
    };
    let plugin = entry.plugin.clone();
    let instrument = device(10, DeviceRole::Instrument, plugin, None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![instrument])]));
    rig.wait_status(10, DeviceStatus::Ok);
    rig.render(4 * BLOCK);
    let pid = rig.manager.bridge_pid(DeviceId(10)).expect("bridge pid");
    // LaunchServices checks a process in asynchronously.
    let settle = || std::thread::sleep(std::time::Duration::from_secs(2));
    settle();
    let mut kinds = vec![("loaded", application_type(pid))];
    // The frame, off screen: the bridge inherits `KOMPONIST_FRAME_OFFSCREEN=1` (never on screen).
    if std::env::var_os("KOMPONIST_FRAME_OFFSCREEN").is_some_and(|v| v == "1") {
        let frame = komponist_ipc::GuiTarget::OwnFrame {
            header: komponist_ipc::frame::HeaderModel {
                name: "Surge XT".into(),
                sub: "Pad · CLAP".into(),
                mark: komponist_ipc::frame::Mark::Active,
                bypassed: false,
                presets: Vec::new(),
                preset: None,
                preset_changed: false,
                can_save: false,
                view: komponist_ipc::frame::PluginView::Gui,
                has_gui: true,
                keep_on_top: false,
                close_shortcut: "Esc".into(),
                mapping_chip: None,
            },
            lang: "de".into(),
            rect: None,
        };
        // Sampled while the frame opens: the bridge must not be "Foreground" even briefly.
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sampler = {
            let done = std::sync::Arc::clone(&done);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                while !done.load(std::sync::atomic::Ordering::Acquire) {
                    let kind = application_type(pid);
                    if seen.last() != Some(&kind) {
                        seen.push(kind);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                seen
            })
        };
        rig.manager.open_gui(DeviceId(10), frame).expect("open_gui");
        wait_for(&mut rig.manager, "GuiOpened", |e| {
            matches!(e, PluginManagerEvent::GuiOpened { .. }).then_some(())
        });
        settle();
        done.store(true, std::sync::atomic::Ordering::Release);
        for kind in sampler.join().unwrap() {
            kinds.push(("frame opening", kind));
        }
        kinds.push(("frame open", application_type(pid)));
        rig.manager.close_gui(DeviceId(10));
        wait_for(&mut rig.manager, "GuiClosed", |e| {
            matches!(e, PluginManagerEvent::GuiClosed { .. }).then_some(())
        });
        settle();
        kinds.push(("frame closed", application_type(pid)));
    } else {
        eprintln!("frame not opened: run with KOMPONIST_FRAME_OFFSCREEN=1");
    }
    eprintln!("bridge {pid}: {kinds:?}");
    rig.manager.shutdown();
    for (when, kind) in kinds {
        assert!(
            !kind.contains("Foreground"),
            "{when}: the bridge has a Dock icon: {kind}"
        );
    }
}
