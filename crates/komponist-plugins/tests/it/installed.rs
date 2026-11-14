//! Installed third-party plugins through the plugin manager and the engine (`#[ignore]`):
//! klopfer as the built-in "INTERN" entry, Dexed and Surge XT from a scan of `/usr/lib/clap`.
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
        states.first().map_or(0, |(_, s)| s.len())
    );
    assert_eq!(states.len(), 1);
    rig.manager.shutdown();
    level
}

#[test]
#[ignore = "installed third-party plugins; run with --ignored"]
fn klopfer_as_builtin() {
    let Some(bundle) = installed("~/.clap/Klopfer.clap") else {
        return;
    };
    let dir = scratch("installed-klopfer");
    let mut config = config(&dir);
    config.klopfer_bundle = Some(bundle);
    let manager = PluginManager::new(config);
    let entry = manager.catalog().resolve(&klopfer()).unwrap();
    assert!(entry.is_builtin());
    // Pad 1 = note 36; the default kit may be empty, so only loading is required.
    load_and_play(manager, klopfer(), 36);
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

/// klopfer's cold start (WP-54 note, WP-61): how long after the bridge reports it active (warm-up
/// switched off) a pad first sounds, over fresh bridges. A note goes in every 2 ms of wall-clock
/// time; the first block with output marks the readiness. `crate::WARM_UP` must cover it.
///
/// ```text
/// cargo test --release -p komponist-plugins --test it installed::klopfer_cold_start -- --ignored --nocapture
/// ```
#[test]
#[ignore = "installed klopfer and its test kit; run with --ignored"]
fn klopfer_cold_start() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let bundle = manifest.join("../../../klopfer/target/bundled/Klopfer.clap");
    let kit = manifest.join("../../../klopfer/testkit-out/Testkit.klopfer");
    let (Ok(bundle), Ok(kit)) = (bundle.canonicalize(), kit.canonicalize()) else {
        eprintln!("skipped: klopfer bundle or test kit missing (scripts/build-klopfer.sh)");
        return;
    };
    let state = komponist_plugins::builtin::klopfer_kit_state(&kit).expect("kit");
    for offline in [false, true] {
        cold_start(&bundle, &state, offline);
    }
}

fn cold_start(bundle: &std::path::Path, state: &[u8], offline: bool) {
    let mut times = Vec::new();
    for run in 0..8 {
        let dir = scratch(&format!("installed-klopfer-cold-{run}"));
        let mut config = config(&dir);
        config.klopfer_bundle = Some(bundle.to_owned());
        let mut manager = PluginManager::new(config);
        manager.set_warm_up(std::time::Duration::ZERO);
        if offline {
            manager.set_render_mode(komponist_ipc::RenderMode::Offline);
        }
        let instrument = device(10, DeviceRole::Instrument, klopfer(), Some(state));
        let mut rig = Rig::new(manager, project(vec![track(1, vec![instrument])]));
        rig.wait_status(10, DeviceStatus::Ok);
        let active = Instant::now();
        let ready = loop {
            rig.note(1, 36, 110);
            let range = if offline {
                rig.render(BLOCK)
            } else {
                rig.process(BLOCK)
            };
            rig.note(1, 36, 0);
            if rig.peak(range) > 1e-4 {
                break active.elapsed();
            }
            assert!(active.elapsed() < PATIENCE, "klopfer never sounded");
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        rig.manager.shutdown();
        times.push(ready);
    }
    times.sort();
    eprintln!(
        "klopfer cold start ({}): first hit {:.1?} … {:.1?} after `Activated` (median {:.1?}); WARM_UP {:?}",
        if offline { "offline" } else { "realtime" },
        times[0],
        times[times.len() - 1],
        times[times.len() / 2],
        komponist_plugins::WARM_UP
    );
    assert!(times[times.len() - 1] < komponist_plugins::WARM_UP);
}
