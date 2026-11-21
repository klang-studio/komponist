//! Runs against an authenticated release payload via
//! `scripts/measure-bundled-clap.py --historical-projects <unpacked app executable>`.

use std::path::Path;
use std::time::{Duration, Instant};

use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::project_io::{load_project, save_project};
use komponist_builtin::Internal;
use komponist_engine::Command;
use komponist_hexa::Macro;
use komponist_model::PluginFormat;
use komponist_model::params::ParamId;
use komponist_plugins::bundled_clap::BundledClap;
use komponist_plugins::legacy_bundled::{
    migrate_legacy_bundled_project, plan_legacy_bundled_device,
};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerEvent};

use crate::common::{PATIENCE, app_with, config, step_until};

#[test]
#[ignore = "requires a verified all-device package; run with scripts/measure-bundled-clap.py --historical-projects"]
fn historical_projects_migrate_and_play() {
    let launcher = std::env::var_os("KOMPONIST_HISTORICAL_BUNDLED_APP")
        .expect("set the unpacked app with scripts/measure-bundled-clap.py --historical-projects");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().expect("historical test workspace");
    let inventory = BundledClap::discover(scratch.path())
        .expect("authenticate the package manifest")
        .expect("runner must embed the release manifest digest");
    assert_eq!(inventory.bundle_entries().count(), 11);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    for (name, legacy_id, bundle_id) in [
        ("hexa-macros", "pisynth", "org.klang-studio.komponist.hexa"),
        ("knurr-project", "knurr", "org.klang-studio.komponist.volt"),
    ] {
        let fixture = fixtures.join(format!("{name}.komponist"));
        let original = std::fs::read(&fixture).unwrap();
        let source = load_project(&fixture).unwrap();
        let track = &source.tracks[0];
        let track_id = track.id;
        let device = track.channel.instrument().unwrap().1;
        let device_id = device.id;
        assert_eq!(device.plugin.format, PluginFormat::Builtin);
        assert_eq!(device.plugin.id, legacy_id);
        let plan = plan_legacy_bundled_device(&inventory, device, Some(&fixtures))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(plan.replacement().plugin.id, bundle_id);
        let (param, normalized) = if legacy_id == "pisynth" {
            let ratio = Macro::Ratio2;
            let patch = komponist_hexa::decode_state(device.state.as_deref()).unwrap();
            assert_eq!(ratio.get(&patch), 3.5, "historical nondefault patch");
            (
                ParamId::new(ratio.key()),
                ratio.param_info().to_normalized(ratio.get(&patch)),
            )
        } else {
            let cutoff = Internal::Volt
                .params()
                .iter()
                .find(|info| info.id.as_str() == "cutoff")
                .unwrap();
            (cutoff.id.clone(), cutoff.default_normalized())
        };
        let binding = plan.parameter(&param).expect("published author ID");
        let parameter = binding.id.plugin_id().unwrap();
        let expected = binding.clap_plain(normalized);
        let migration = migrate_legacy_bundled_project(&source, [plan]).unwrap();
        let path = scratch.path().join(format!("{name}-migrated.komponist"));
        save_project(&path, &migration.project).unwrap();
        let reopened = load_project(&path).unwrap();
        assert_eq!(reopened, migration.project);
        assert_eq!(std::fs::read(&fixture).unwrap(), original);
        let (_, _, replacement) = reopened.find_device(device_id).unwrap();
        assert_eq!(replacement.plugin.id, bundle_id);
        assert_eq!(
            &reopened.tracks[0].channel.devices[1..],
            &track.channel.devices[1..],
            "unrelated external inserts retain their saved identities and state",
        );

        let mut config = config(&scratch.path().join(name));
        config.bridge_exe = launcher.clone().into();
        let mut host = PluginHost::new(config);
        host.start_scan(&PluginDirs {
            clap: Vec::new(),
            vst3: Vec::new(),
        });
        let deadline = Instant::now() + PATIENCE;
        while !host
            .poll()
            .iter()
            .any(|event| matches!(event, PluginManagerEvent::ScanFinished(_)))
        {
            assert!(Instant::now() < deadline, "bundled CLAP scan timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut app = app_with(reopened, host);
        step_until(&mut app, "migrated instrument ready", |app| {
            app.engine().device_status(device_id) == DeviceStatus::Ok
                && app
                    .engine()
                    .host()
                    .param_base(device_id, parameter)
                    .is_some()
        });
        let host = app.engine().host();
        assert_eq!(host.bridge_pid(device_id).is_some(), force_bridge);
        assert!(host.state_warning(device_id).is_none());
        let actual = host.param_base(device_id, parameter).unwrap();
        assert!(
            (expected - actual).abs() < 1e-6,
            "{name}: {expected} != {actual}"
        );

        app.engine_mut().send(Command::LiveNote {
            track: track_id,
            key: 60,
            velocity: 100,
        });
        step_until(&mut app, "migrated instrument audio", |app| {
            app.engine().track_meter(track_id).is_some_and(|meter| {
                let (left, right) = meter.take();
                left.max(right) > 1e-4
            })
        });
        app.shutdown();
        eprintln!("{name}: state and live audio verified (forced bridge = {force_bridge})");
    }
}
