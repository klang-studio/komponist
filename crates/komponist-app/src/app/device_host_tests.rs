use std::time::Duration;

use crate::app::App;
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{DeviceRole, PluginRef, Project};
use komponist_plugins::{DeviceStatus, ProcessorSetup};
use komponist_session::device_host::{
    ParameterHost, PluginCatalogHost, PluginHost, PluginWindowHost, ProcessorHost,
};

const SETUP: ProcessorSetup = ProcessorSetup {
    sample_rate: 48_000.0,
    max_block: komponist_engine::MAX_BLOCK,
};

fn shipped(host: &PluginHost, name: &str) -> Option<PluginRef> {
    let id = format!("org.klang-studio.komponist.{name}");
    let plugin = host
        .catalog()
        .entries()
        .iter()
        .find(|entry| entry.plugin.id == id && komponist_plugins::requires_bundled(&entry.plugin))
        .map(|entry| entry.plugin.clone());
    if plugin.is_none() {
        assert!(
            !std::env::var_os("KOMPONIST_REQUIRE_FIXTURES").is_some_and(|value| value == "1"),
            "required bundled fixture {id} is missing from the catalog"
        );
        eprintln!("skipping: build {id} with scripts/build-test-plugins.sh");
    }
    plugin
}

#[test]
fn the_fixture_catalog_resolves_shipped_devices_and_reports_unavailable_plugins() {
    let mut host = PluginHost::headless_fixture();
    let Some(hexa) = shipped(&host, "hexa") else {
        return;
    };
    assert_eq!(host.catalog().entries().len(), 12);
    assert_eq!(host.role(&hexa), Some(DeviceRole::Instrument));
    let demo = komponist_model::fixtures::demo_project();
    for track in &demo.tracks[..3] {
        let (_, instrument) = track.channel.instrument().unwrap();
        assert_eq!(
            host.role(&instrument.plugin),
            Some(DeviceRole::Instrument),
            "{} ({}) must resolve in the staged package",
            instrument.plugin.id,
            instrument.plugin.module
        );
    }
    let mut unavailable = hexa.clone();
    unavailable.id = "de.klang-studio.test.unavailable".into();
    unavailable.module = "not-installed.clap".into();
    let (project, _) = komponist_model::edits::add_track(
        &Project::new("Fixture inventory"),
        komponist_model::edits::NewTrack {
            name: None,
            hue: None,
            instrument: Some(unavailable.clone()),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    assert_eq!(host.role(&unavailable), None);
    let missing = host.missing_plugins(&project);
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].plugin, unavailable);
    assert_eq!(missing[0].role, DeviceRole::Instrument);
    let device = &project.tracks[0].channel.devices[0];
    assert!(host.create_processor(device, SETUP).is_none());
    assert_eq!(host.status(device.id), DeviceStatus::Missing);
    assert!(host.params(device.id).is_none());
    assert!(!host.has_gui(device.id));
    host.release(device.id);
    assert_eq!(host.status(device.id), DeviceStatus::Ok);
}

#[test]
fn the_shipped_sampler_loads_linked_audio_into_its_authored_pad_bank() {
    use komponist_plugin_api::device_assets::AssetStatus;
    use std::time::Instant;

    let host = PluginHost::headless_fixture();
    let Some(sampler) = shipped(&host, "sampler") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let kit = komponist_audiofile::fixtures::write_kit(dir.path()).unwrap();
    let (project, _) = komponist_model::edits::add_track(
        &Project::new("Sampler assets"),
        komponist_model::edits::NewTrack {
            name: None,
            hue: None,
            instrument: Some(sampler),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let device = project.tracks[0].channel.devices[0].id;
    let links = kit
        .iter()
        .take(2)
        .enumerate()
        .map(|(pad, path)| (pad as u32, AssetLink::new(path, None)))
        .collect();
    let project = komponist_model::edits::set_device_asset_links(&project, device, links).unwrap();
    let mut app = App::headless(project);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        app.step();
        let host = app.engine().host();
        if host.status(device) == DeviceStatus::Ok
            && host.asset_slots(device).is_some_and(|assets| {
                assets
                    .slots
                    .iter()
                    .filter(|slot| slot.status == AssetStatus::Ready)
                    .count()
                    == 2
            })
        {
            let layout = host.native_layout(device).unwrap();
            let bank = layout.pad_bank.as_ref().unwrap();
            assert_eq!(
                bank.pads
                    .iter()
                    .take(2)
                    .map(|pad| pad.note)
                    .collect::<Vec<_>>(),
                [36, 37]
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the sampler fixture did not load its kit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
