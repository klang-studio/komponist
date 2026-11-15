//! The app's demo project (`komponist --demo`): built-in devices only, same song as the design
//! fixture, and an offline render of it for the levels (`render_levels`, ignored).

use std::path::PathBuf;
use std::sync::Arc;

use komponist_app::actions::{BitDepth, ExportRange};
use komponist_app::showcase::showcase_project;
use komponist_builtin::Internal;
use komponist_model::fixtures::{demo_project, demo_song};
use komponist_model::{ClipContent, Device, DeviceRole, PluginFormat, Project};
use komponist_plugins::Catalog;

use crate::export::{app_in, export, export_settings, peak_db, read, scratch};

fn all_devices(project: &Project) -> impl Iterator<Item = &Arc<Device>> {
    project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .chain(project.master.devices.iter())
}

fn notes_of(project: &Project, track: usize) -> Vec<Vec<(i64, i64, u8, u8)>> {
    project.tracks[track]
        .clips
        .iter()
        .map(|c| match &c.content {
            ClipContent::Midi(m) => m.takes[m.active_take]
                .notes
                .iter()
                .map(|n| (n.start.0, n.length.0, n.pitch, n.velocity))
                .collect(),
            ClipContent::Step(_) => Vec::new(),
        })
        .collect()
}

#[test]
fn every_device_is_built_in_and_resolves_in_the_catalog() {
    let project = showcase_project();
    let catalog = Catalog::builtin();
    let mut count = 0;
    for device in all_devices(&project) {
        count += 1;
        assert_eq!(
            device.plugin.format,
            PluginFormat::Builtin,
            "{:?}",
            device.plugin
        );
        assert!(
            catalog.resolve(&device.plugin).is_some(),
            "{} is not in the built-in catalog",
            device.plugin.name
        );
        assert!(
            device.state.is_some()
                || Internal::from_plugin(&device.plugin) == Some(Internal::Sampler)
        );
    }
    assert_eq!(count, 2 + 2 + 4 + 3 + 2 + 4 + 1);
    // The Arp's MIDI effects come first, the Limiter is on the master.
    let arp = &project.tracks[5].channel.devices;
    let roles: Vec<_> = arp.iter().map(|d| d.role).collect();
    assert_eq!(
        roles,
        [
            DeviceRole::MidiEffect,
            DeviceRole::MidiEffect,
            DeviceRole::Instrument,
            DeviceRole::Effect
        ]
    );
    assert_eq!(
        Internal::from_plugin(&project.master.devices[0].plugin),
        Some(Internal::Limiter)
    );
    assert!(!project.tracks[5].channel.mute);
}

#[test]
fn internal_devices_show_their_preset_names() {
    let project = showcase_project();
    for device in all_devices(&project) {
        let Some(kind) = Internal::from_plugin(&device.plugin) else {
            continue;
        };
        if matches!(kind, Internal::Sampler | Internal::Limiter) {
            continue;
        }
        let (index, changed) = kind
            .loaded_preset(device.state.as_deref())
            .unwrap_or_else(|| panic!("{} has no loaded preset", kind.name()));
        assert!(!changed, "{} {}", kind.name(), kind.presets()[index].name);
    }
}

#[test]
fn the_song_is_the_design_fixtures_song() {
    let fixture = demo_project();
    let showcase = showcase_project();
    assert_eq!(showcase.name, fixture.name);
    assert_eq!(showcase.tempo, fixture.tempo);
    assert_eq!(showcase.loop_range, fixture.loop_range);
    assert_eq!(showcase.loop_on, fixture.loop_on);
    assert_eq!(showcase.tracks.len(), fixture.tracks.len());
    for (i, (a, b)) in showcase.tracks.iter().zip(&fixture.tracks).enumerate() {
        assert_eq!(
            (&a.name, a.hue, a.channel.pan),
            (&b.name, b.hue, b.channel.pan)
        );
        assert_eq!(a.clips.len(), b.clips.len(), "track {i}");
        for (ca, cb) in a.clips.iter().zip(&b.clips) {
            assert_eq!(
                (&ca.name, ca.start, ca.length),
                (&cb.name, cb.start, cb.length)
            );
        }
        if i != 5 {
            assert_eq!(notes_of(&showcase, i), notes_of(&fixture, i), "track {i}");
        }
    }
    // The fixture's own Arp clip is the song's.
    assert_eq!(notes_of(&demo_song(), 5), notes_of(&fixture, 5));
    // Unique ids across the project.
    let mut ids = Vec::new();
    for t in &showcase.tracks {
        ids.push(t.id.0);
        ids.extend(t.channel.devices.iter().map(|d| d.id.0));
        for c in &t.clips {
            ids.push(c.id.0);
            if let ClipContent::Midi(m) = &c.content {
                ids.extend(m.takes[0].notes.iter().map(|n| n.id.0));
            }
        }
    }
    ids.extend(showcase.master.devices.iter().map(|d| d.id.0));
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "ids are unique");
    assert!(ids.iter().all(|&i| i < showcase.next_id));
}

/// Levels of the demo: renders every track alone and the master with and without the Limiter,
/// offline (no audio device). `SHOWCASE_RENDER_DIR` names the folder for the WAV files.
/// `cargo test -p komponist-app --test it showcase::render_levels -- --ignored --nocapture`
#[test]
#[ignore = "slow analysis, run by hand"]
fn render_levels() {
    let out: PathBuf = std::env::var_os("SHOWCASE_RENDER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| scratch("levels"));
    std::fs::create_dir_all(&out).unwrap();
    let db = |x: f32| 20.0 * x.max(1e-9).log10();
    let measure = |project: Project, name: &str, depth: BitDepth| {
        let dir = scratch(name);
        let (mut app, _) = app_in(&dir, project);
        app.step();
        let mut settings = export_settings(&dir, ExportRange::Song);
        settings.normalize = false;
        settings.bit_depth = depth;
        settings.folder = out.clone();
        settings.file_name = format!("{name}.wav");
        export(&mut app, settings);
        let wav = read(&out.join(format!("{name}.wav")));
        let rms =
            (wav.samples.iter().map(|x| x * x).sum::<f32>() / wav.samples.len() as f32).sqrt();
        // The loudest second (stereo interleaved at 48 kHz).
        let loudest = wav
            .samples
            .chunks(2 * 48_000)
            .filter(|c| c.len() == 2 * 48_000)
            .map(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt())
            .fold(0.0_f32, f32::max);
        println!(
            "{name:>12}: peak {:6.1} dBFS  rms {:6.1} dBFS  loudest 1 s rms {:6.1} dBFS",
            peak_db(&wav.samples),
            db(rms),
            db(loudest)
        );
    };
    let base = komponist_app::samples::demo_with_kit(showcase_project(), &out.join("kit")).unwrap();
    for i in 0..base.tracks.len() {
        let mut p = base.clone();
        let keep = p.tracks[i].clone();
        p.tracks = vec![keep];
        Arc::make_mut(&mut p.master).devices.clear();
        let name = format!("track-{}", base.tracks[i].name.to_lowercase());
        measure(p, &name, BitDepth::Float32);
    }
    let mut no_limiter = base.clone();
    Arc::make_mut(&mut no_limiter.master).devices.clear();
    measure(no_limiter, "master-no-limiter", BitDepth::Float32);
    measure(base, "nachtfahrt", BitDepth::Int24);
}
