//! Installed third-party plugins through the plugin manager and the engine (`#[ignore]`):
//! klopfer as the built-in "INTERN" entry, Dexed and Surge XT from a scan of `/usr/lib/clap`.
//!
//! ```text
//! cargo test -p komponist-plugins --test installed -- --ignored --nocapture
//! ```

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;

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
