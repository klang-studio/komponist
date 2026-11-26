//! The shipped demo's arrangement, authentic device state and generated assets. The packaged
//! acceptance suite calls these checks on both host routes. The level render also runs against
//! the verified development fixtures.

use std::path::PathBuf;
use std::sync::Arc;

use komponist_app::App;
use komponist_app::actions::{BitDepth, ExportRange};
use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::showcase::bundled_demo;
use komponist_ipc::device_assets::AssetStatus;
use komponist_model::automation::AutomationTarget;
use komponist_model::fixtures::demo_song;
use komponist_model::params::ParamId;
use komponist_model::{
    ClipContent, Device, DeviceId, DeviceRole, Project, Tap, Track, TrackOutput,
};
use komponist_plugins::{DeviceStatus, requires_bundled};

use crate::common::step_until;
use crate::export::{export, export_settings, peak_db, read, scratch};

const DEVICE_PREFIX: &str = "org.klang-studio.komponist.";

pub(super) fn assert_packaged_showcase(
    app: &mut App,
    project: &Project,
    factory: &PluginHost,
    repeat: &Project,
) {
    step_until(app, "all shipped demo devices ready", |app| {
        all_devices(project).all(|device| {
            app.engine()
                .host()
                .verified_bundled_kind(device.id)
                .is_some()
        })
    });
    let host = app.engine().host();
    check_device_catalog(project, host);
    check_factory_presets(project, host, factory);
    check_song(project);
    check_routing(project);
    check_audio_tracks(project);
    check_generated_audio(project, repeat);
    check_warp(project);
    check_automation(project, host);
    check_surf(project, host);
    check_sampler_assets(app, project);
}

fn device_id_is(device: &Device, slug: &str) -> bool {
    device.plugin.id.strip_prefix(DEVICE_PREFIX) == Some(slug)
}

fn authored_control(host: &dyn DeviceHost, device: DeviceId, name: &str) -> u32 {
    host.native_layout(device).unwrap().author_ids().unwrap()[name]
}

fn authored_target(host: &dyn DeviceHost, device: DeviceId, name: &str) -> AutomationTarget {
    AutomationTarget::Param {
        device,
        param: ParamId::plugin(authored_control(host, device, name)),
    }
}

fn all_devices(project: &Project) -> impl Iterator<Item = &Arc<Device>> {
    project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .chain(project.master.devices.iter())
}

fn track_named<'a>(project: &'a Project, name: &str) -> &'a Track {
    project
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no track {name}"))
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
            ClipContent::Step(_) | ClipContent::Audio(_) => Vec::new(),
        })
        .collect()
}

fn check_device_catalog(project: &Project, host: &dyn DeviceHost) {
    let mut count = 0;
    for device in all_devices(project) {
        count += 1;
        assert!(requires_bundled(&device.plugin), "{:?}", device.plugin);
        assert!(host.catalog().resolve(&device.plugin).is_some());
        assert!(host.bridge_pid(device.id).is_none());
        assert!(host.state_warning(device.id).is_none());
        assert!(device.state.is_some() || device_id_is(device, "sampler"));
    }
    // Drums 2, Bass 3, Lead 3, Pad 2, Keys 2, Arp 4, sampler demos 2, surf 1, Reverb bus 1,
    // master 1.
    assert_eq!(count, 2 + 3 + 3 + 2 + 2 + 4 + 2 + 1 + 1 + 1);
    // The Arp's MIDI effects come first, the Limiter is on the master.
    let arp = &track_named(project, "Arp").channel.devices;
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
    assert!(device_id_is(&project.master.devices[0], "limiter"));
    assert!(!track_named(project, "Arp").channel.mute);
}

fn check_factory_presets(project: &Project, host: &dyn DeviceHost, factory: &PluginHost) {
    for (track, presets) in [
        ("Drums", &[(1, "Tight Drums")][..]),
        ("Bass", &[(0, "Fat Saw"), (1, "Warm Bass"), (2, "Ducking")]),
        (
            "Lead",
            &[(0, "Saw Lead"), (1, "Low Cut 80 Hz"), (2, "Dotted Eighths")],
        ),
        ("Pad", &[(0, "Wide Pad"), (1, "Low Cut 80 Hz")]),
        ("Keys", &[(0, "E-Piano"), (1, "Soft Tape")]),
        (
            "Arp",
            &[
                (0, "A Minor Pentatonic"),
                (1, "Sixteenths Up/Down"),
                (2, "Synth Pluck"),
                (3, "Eighth Ping-Pong"),
            ],
        ),
        ("Reverb", &[(0, "Large Hall")]),
        ("Vowel Pad", &[(0, "Vowel Pad")]),
    ] {
        for &(index, expected) in presets {
            let device = &track_named(project, track).channel.devices[index];
            let catalog = host.clap_presets(device);
            let (selected, _) = host
                .saved_preset(device, catalog)
                .unwrap_or_else(|| panic!("{track}: no preset for {}", device.plugin.id));
            assert!(catalog[selected].factory && catalog[selected].name == expected);
            let factory_state = factory
                .manager()
                .prepare_bundled_device(&device.plugin.id, Some(expected), &[], &[])
                .unwrap()
                .state
                .expect("factory preset state");
            // The Bass preset is deliberately gentler for the demo's pre-fader kick.
            assert_eq!(
                device.state.as_ref() == Some(&factory_state),
                track != "Bass" || expected != "Ducking",
                "{track}: {expected}"
            );
        }
    }
}

fn check_song(showcase: &Project) {
    let fixture = demo_song();
    assert_eq!(showcase.name, fixture.name);
    assert_eq!(showcase.tempo, fixture.tempo);
    assert!(!showcase.loop_on);
    assert_eq!(showcase.loop_range, Project::new("").loop_range);
    // Buses, audio tracks and the sampler demonstration are additions to the original song.
    let songs: Vec<&Arc<Track>> = showcase
        .tracks
        .iter()
        .filter(|t| {
            !t.is_bus()
                && !t.is_audio()
                && !matches!(
                    t.name.as_str(),
                    "Glass Pluck" | "Glass Slices" | komponist_app::showcase::surf::TRACK
                )
        })
        .collect();
    assert_eq!(songs.len(), fixture.tracks.len());
    assert_eq!(showcase.tracks.len(), fixture.tracks.len() + 8);
    for (i, (a, b)) in songs.iter().zip(&fixture.tracks).enumerate() {
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
        if b.name != "Arp" {
            let notes = |p: &Project| {
                notes_of(
                    p,
                    p.tracks
                        .iter()
                        .position(|t| t.id == a.id || t.name == b.name)
                        .unwrap(),
                )
            };
            assert_eq!(notes(showcase), notes(&fixture), "track {i}");
        }
    }
    // Unique ids across the project.
    let mut ids = Vec::new();
    for t in &showcase.tracks {
        ids.push(t.id.0);
        ids.extend(t.sends.iter().map(|s| s.id.0));
        ids.extend(t.channel.devices.iter().map(|d| d.id.0));
        for c in &t.clips {
            ids.push(c.id.0);
            if let ClipContent::Midi(m) = &c.content {
                ids.extend(m.takes[0].notes.iter().map(|n| n.id.0));
            }
        }
    }
    ids.extend(showcase.master.devices.iter().map(|d| d.id.0));
    ids.extend(
        showcase
            .master_automation
            .lanes
            .iter()
            .map(|lane| lane.id.0),
    );
    for track in &showcase.tracks {
        ids.extend(track.automation.lanes.iter().map(|lane| lane.id.0));
    }
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "ids are unique");
    assert!(ids.iter().all(|&i| i < showcase.next_id));
}

fn check_routing(project: &Project) {
    let rhythm = track_named(project, "Rhythm");
    assert!(rhythm.is_bus() && rhythm.folder);
    assert_eq!(rhythm.output, TrackOutput::Master);
    let (drums, bass) = (track_named(project, "Drums"), track_named(project, "Bass"));
    assert_eq!(drums.output, TrackOutput::Bus(rhythm.id));
    assert_eq!(bass.output, TrackOutput::Bus(rhythm.id));
    let members = komponist_model::routing::group_members(project, rhythm.id);
    assert_eq!(
        members,
        [drums.id, bass.id, track_named(project, "Shaker").id]
    );

    let hall = track_named(project, "Reverb");
    assert!(hall.is_bus() && !hall.folder);
    assert_eq!(hall.channel.devices.len(), 1);
    assert!(device_id_is(&hall.channel.devices[0], "hall"));
    for name in ["Lead", "Pad"] {
        let track = track_named(project, name);
        let send = track
            .send_to(hall.id)
            .unwrap_or_else(|| panic!("{name} sends to Reverb"));
        assert!(send.enabled && send.tap == Tap::Post);
        assert!(
            (-12.0..=-8.0).contains(&send.level_db),
            "{name} {}",
            send.level_db
        );
        // The reverb moved to the bus: no Hall insert is left on the track.
        assert!(
            track
                .channel
                .devices
                .iter()
                .all(|d| !device_id_is(d, "hall"))
        );
    }
    // The Lead keeps its Delay as an insert.
    assert!(
        track_named(project, "Lead")
            .channel
            .devices
            .iter()
            .any(|d| device_id_is(d, "delay"))
    );

    // presse on the Bass is keyed by the Drums, pre-fader; the Drums' own presse has no sidechain.
    let keyed: Vec<_> = all_devices(project)
        .filter(|d| d.sidechain.is_some())
        .collect();
    assert_eq!(keyed.len(), 1);
    assert!(device_id_is(keyed[0], "presse"));
    assert!(bass.channel.devices.iter().any(|d| d.id == keyed[0].id));
    let sc = keyed[0].sidechain.unwrap();
    assert_eq!((sc.source, sc.tap), (drums.id, Tap::Pre));
    assert_eq!(komponist_model::routing::check(project), Ok(()));
}

fn check_audio_tracks(project: &Project) {
    use komponist_model::{AudioInput, ClipTimeMode, Hue, MonitorMode, Tick};
    assert_eq!(project.tempo.bpm, 124.0);
    let shaker = track_named(project, "Shaker");
    let sweep = track_named(project, "Sweep");
    assert_eq!(shaker.hue, Hue::TEAL);
    assert_eq!(sweep.hue, Hue::ORANGE);
    for track in [shaker, sweep] {
        assert!(track.is_audio());
        assert_eq!(track.channel.volume_db, -12.0);
        assert_eq!(track.audio.input, AudioInput::None);
        assert_eq!(track.audio.monitor, MonitorMode::Off);
        assert!(!track.arm);
    }
    assert_eq!(sweep.output, TrackOutput::Master);
    assert_eq!(shaker.clips.len(), 3);
    for (clip, (bar, bars, gain, fade_in, fade_out)) in shaker.clips.iter().zip([
        (0, 8, 0.0, 0, 0),
        (8, 4, -3.0, 0, 3840),
        (12, 4, -3.0, 1920, 0),
    ]) {
        assert_eq!(
            (clip.start, clip.length),
            (Tick(bar * 3840), Tick(bars * 3840))
        );
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("audio clip");
        };
        assert_eq!(audio.time_mode, ClipTimeMode::RePitch);
        assert_eq!(audio.original_tempo.bpm, 120.0);
        assert_eq!(
            (audio.gain_db, audio.fade_in, audio.fade_out),
            (gain, Tick(fade_in), Tick(fade_out))
        );
        assert_eq!(audio.file.channels, 1);
    }
    let clip = &sweep.clips[0];
    assert_eq!((clip.start, clip.length), (Tick(6 * 3840), Tick(2 * 3840)));
    let ClipContent::Audio(audio) = &clip.content else {
        panic!("audio clip");
    };
    assert_eq!(audio.file.channels, 2);
    assert_eq!((audio.gain_db, audio.fade_in), (2.0, Tick(1920)));
    assert_eq!(audio.time_mode, ClipTimeMode::Fixed);
}

fn check_generated_audio(project: &Project, repeat: &Project) {
    for name in ["Shaker", "Sweep", "Pulse 90"] {
        let clip = &track_named(project, name).clips[0];
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("audio clip");
        };
        let ClipContent::Audio(other) = &track_named(repeat, name).clips[0].content else {
            panic!("audio clip");
        };
        assert_ne!(audio.file.abs, other.file.abs);
        assert_eq!(
            std::fs::read(&audio.file.abs).unwrap(),
            std::fs::read(&other.file.abs).unwrap()
        );
        let wav = read(&audio.file.abs);
        assert_eq!(wav.spec.channels, audio.file.channels);
        assert_eq!(wav.spec.sample_rate, 48_000);
        assert_eq!(wav.spec.bits_per_sample, 24);
        assert_eq!(wav.frames, audio.file.frames);
        assert!(peak_db(&wav.samples) > -24.0, "{name} is audible");
        if wav.spec.channels == 2 {
            assert!(
                wav.samples.as_chunks::<2>().0.iter().any(|s| s[0] != s[1]),
                "stereo noise"
            );
        }
    }
}

fn check_sampler_assets(app: &mut App, project: &Project) {
    for (track, count) in [("Drums", 8), ("Glass Pluck", 1), ("Glass Slices", 1)] {
        let sampler = track_named(project, track).channel.instrument().unwrap().1;
        assert_eq!(sampler.asset_links.len(), count);
        assert!(
            sampler
                .asset_links
                .values()
                .all(|link| link.resolve(None).is_some())
        );
        step_until(app, &format!("{track}: linked samples ready"), |app| {
            app.engine()
                .host()
                .asset_slots(sampler.id)
                .is_some_and(|assets| {
                    assets.slots.len() == count
                        && assets
                            .slots
                            .iter()
                            .all(|slot| slot.status == AssetStatus::Ready)
                })
        });
    }

    let mut files = Vec::new();
    for (name, slice_mode) in [("Glass Pluck", false), ("Glass Slices", true)] {
        let track = track_named(project, name);
        let device = &track.channel.devices[0];
        assert!(device_id_is(device, "general-sampler"));
        let host = app.engine().host();
        let value = |name| host.param_base(device.id, authored_control(host, device.id, name));
        assert_eq!(value("root"), Some(48.0));
        assert_eq!(value("mode"), Some(if slice_mode { 2.0 } else { 1.0 }));
        assert_eq!(value("sync"), Some(f64::from(u8::from(!slice_mode))));
        app.engine_mut()
            .host_mut()
            .request_waveform_markers(device.id, 0);
        step_until(app, &format!("{name}: live markers ready"), |app| {
            app.engine()
                .host()
                .waveform_markers(device.id, 0)
                .is_some_and(|markers| {
                    if slice_mode {
                        markers.editable
                    } else {
                        markers.default_options.is_some()
                    }
                })
        });
        let markers = app.engine().host().waveform_markers(device.id, 0).unwrap();
        let expected: &[f64] = if slice_mode {
            &[0.75, 1.5, 2.5, 3.0, 3.5]
        } else {
            &[]
        };
        assert_eq!(markers.positions, expected);
        files.push(device.asset_links[&0].resolve(None).unwrap());
        let ClipContent::Midi(midi) = &track.clips[0].content else {
            panic!("the sampler is played from MIDI");
        };
        assert_eq!(midi.takes[0].notes.len(), 4);
    }
    assert_eq!(files[0], files[1]);
    let wav = read(&files[0]);
    assert_eq!(wav.spec.channels, 1);
    assert_eq!(wav.spec.sample_rate, 48_000);
    assert_eq!(wav.frames, 4 * 48_000);
    assert!(peak_db(&wav.samples) > -12.0);
}

#[test]
fn headless_loads_a_shipped_device_from_the_verified_fixture_inventory() {
    let host = App::headless_plugin_host();
    let Some(hexa) = host.catalog().entries().iter().find(|entry| {
        entry.plugin.id == "org.klang-studio.komponist.hexa" && requires_bundled(&entry.plugin)
    }) else {
        assert!(
            !std::env::var_os("KOMPONIST_REQUIRE_FIXTURES").is_some_and(|value| value == "1"),
            "verified headless fixtures are missing"
        );
        return;
    };
    let (project, _, device) = crate::common::instrument_project("Headless", hexa.plugin.clone());
    let mut app = App::headless(project);
    step_until(&mut app, "verified headless Hexa", |app| {
        app.engine().host().status(device) == DeviceStatus::Ok
    });
    let host = app.engine().host();
    assert!(host.verified_bundled_kind(device).is_some());
    assert!(host.native_layout(device).is_some());
    assert!(host.bridge_pid(device).is_none());
}

/// Levels of the demo: renders every track alone and the master with and without the Limiter,
/// offline (no audio device). `SHOWCASE_RENDER_DIR` names the folder for the WAV files.
/// `cargo test -p komponist-app --test it showcase::render_levels -- --ignored --nocapture`
/// The test uses the locally built verified fixtures, or the verified package named by
/// `KOMPONIST_CURRENT_BUNDLED_APP`.
#[test]
#[ignore = "slow analysis, run by hand"]
fn render_levels() {
    let out: PathBuf = std::env::var_os("SHOWCASE_RENDER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| scratch("levels"));
    std::fs::create_dir_all(&out).unwrap();
    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP").map(PathBuf::from);
    let host = || {
        if let Some(launcher) = &launcher {
            super::bundled_current::scan(launcher, &out)
        } else {
            App::headless_plugin_host()
        }
    };
    let db = |x: f32| 20.0 * x.max(1e-9).log10();
    let measure = |project: Project, name: &str, depth: BitDepth| {
        let dir = scratch(name);
        let mut app = crate::common::app_with(project, host());
        crate::common::step_until(&mut app, "demo plugins", |app| {
            all_devices(app.project()).all(|device| {
                let host = app.engine().host();
                host.status(device.id) == DeviceStatus::Ok
                    && host.verified_bundled_kind(device.id).is_some()
                    && host.state_warning(device.id).is_none()
            })
        });
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
        assert!(rms > 1e-6, "{name} is silent");
        peak_db(&wav.samples)
    };
    let kit = out.join("kit");
    let base = bundled_demo(&host(), &kit).unwrap();
    // Each track alone with its routing (the buses stay, they are silent without input), so the
    // level of Lead and Pad includes their Hall send. A sidechain whose source is gone is cleared.
    for keep in base.tracks.iter().filter(|t| !t.is_bus()) {
        let mut p = base.clone();
        p.tracks.retain(|t| t.is_bus() || t.id == keep.id);
        let ids: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
        for t in &mut p.tracks {
            for d in &mut Arc::make_mut(t).channel.devices {
                if d.sidechain.is_some_and(|sc| !ids.contains(&sc.source)) {
                    Arc::make_mut(d).sidechain = None;
                }
            }
        }
        Arc::make_mut(&mut p.master).devices.clear();
        let name = format!("track-{}", keep.name.to_lowercase());
        measure(p, &name, BitDepth::Float32);
    }
    // The Bass keyed by the Drums, whose fader is pulled down (the tap is pre-fader): the Bass as
    // it sits in the mix, ducked by the kick, without the kick itself.
    let mut keyed = base.clone();
    let drums = track_named(&keyed, "Drums").id;
    keyed
        .tracks
        .retain(|t| t.is_bus() || t.name == "Bass" || t.id == drums);
    for t in &mut keyed.tracks {
        if t.id == drums {
            Arc::make_mut(t).channel.volume_db = -90.0;
        }
    }
    Arc::make_mut(&mut keyed.master).devices.clear();
    measure(keyed, "track-bass-keyed", BitDepth::Float32);
    let mut no_limiter = base.clone();
    Arc::make_mut(&mut no_limiter.master).devices.clear();
    let input_peak = measure(no_limiter, "master-no-limiter", BitDepth::Float32);
    let master_peak = measure(base, "night-drive", BitDepth::Int24);
    assert!(master_peak <= -0.29, "master peak {master_peak} dBFS");
    // The master's Limiter has 3 dB of input gain; keep its peak reduction around 2 dB or less.
    let reduction = input_peak + 3.0 - master_peak;
    assert!(reduction <= 2.0, "limiter peak reduction {reduction} dB");
}

fn check_warp(project: &Project) {
    let pulse = track_named(project, "Pulse 90");
    let c = &pulse.clips[0];
    let ClipContent::Audio(a) = &c.content else {
        panic!("audio")
    };
    assert_eq!(project.tempo.bpm, 124.0);
    assert_eq!(
        (c.start, c.length),
        (
            komponist_model::Tick(4 * 3840),
            komponist_model::Tick(2 * 3840)
        )
    );
    assert!(a.warp_enabled());
    assert_eq!(a.original_tempo.bpm, 90.0);
    assert_eq!(
        (a.file.frames, a.file.sample_rate, a.file.channels),
        (256000, 48000, 2)
    );
    assert_eq!(a.source_frames(c.length, project.tempo), 256000);
}

fn check_automation(project: &Project, host: &dyn DeviceHost) {
    use komponist_model::Tick;
    use komponist_model::automation::effective_value_at;

    let lead = track_named(project, "Lead");
    let pad = track_named(project, "Pad");
    let cutoff = authored_target(host, pad.channel.devices[0].id, "cutoff");
    let feedback = authored_target(host, lead.channel.devices[0].id, "feedback");
    let delay = lead.channel.devices.last().unwrap().id;
    assert_eq!(
        lead.macros.0[0].mappings[0].target,
        authored_target(host, delay, "mix")
    );
    let clip = lead
        .clips
        .iter()
        .find(|clip| clip.name == "Lead B")
        .unwrap();
    let loop_length = clip.loop_length.expect("Lead B has two repetitions");
    assert!(lead.automation.shown);
    assert!(lead.automation.lane(&feedback).is_some());
    assert_eq!(clip.automation[0].target, feedback);
    let points = clip.automation[0].points.as_slice();
    assert!(points[1].value - points[0].value >= 0.15);
    assert_eq!(
        effective_value_at(lead, &feedback, clip.start + Tick(480)),
        effective_value_at(lead, &feedback, clip.start + loop_length + Tick(480))
    );
    let cutoff_points = pad.automation.lane(&cutoff).unwrap().points.as_slice();
    assert!(cutoff_points[0].value < cutoff_points[1].value);
    let macro_target = AutomationTarget::Macro { index: 0 };
    let macro_lane = pad.automation.lane(&macro_target).unwrap();
    assert!(macro_lane.points.as_slice()[0].value > macro_lane.points.as_slice()[1].value);
    assert!(
        clip.automation
            .iter()
            .any(|envelope| envelope.target == macro_target)
    );
    assert_eq!(
        effective_value_at(lead, &macro_target, clip.start),
        Some(0.25)
    );
    assert_eq!(
        effective_value_at(pad, &macro_target, Tick::ZERO),
        Some(1.0)
    );
    let keys = track_named(project, "Keys");
    assert!(keys.automation.lane(&AutomationTarget::Volume).is_some());
    let master = project
        .master_automation
        .lane(&AutomationTarget::Volume)
        .unwrap();
    assert_eq!(master.points.as_slice().last().unwrap().value, 0.0);
    assert!(
        project
            .tracks
            .iter()
            .flat_map(|t| t.automation.lanes.iter())
            .chain(project.master_automation.lanes.iter())
            .flat_map(|lane| lane.points.as_slice())
            .all(|point| (0.0..=1.0).contains(&point.value))
    );
}

fn check_surf(project: &Project, host: &dyn DeviceHost) {
    use komponist_model::Tick;
    use komponist_model::automation::effective_value_at;

    let track = track_named(project, komponist_app::showcase::surf::TRACK);
    let device = &track.channel.devices[0];
    assert!(device_id_is(device, "surf"));
    let index = project
        .tracks
        .iter()
        .position(|t| t.id == track.id)
        .unwrap();
    let notes = notes_of(project, index);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].len(), 8);
    let target = authored_target(host, device.id, "osc_b_pos");
    let at = |bar: i64| effective_value_at(track, &target, Tick::from_steps(bar * 16)).unwrap();
    assert!(at(8) < 0.2 && at(16) > 0.8 && at(12) > at(9));
    // Position A runs through the whole table and back.
    let a = authored_target(host, device.id, "osc_a_pos");
    let at = |bar: i64| effective_value_at(track, &a, Tick::from_steps(bar * 16)).unwrap();
    assert_eq!((at(8), at(12)), (0.0, 1.0));
    assert!(at(10) > 0.2 && at(10) < 0.8 && at(16) < 0.5);
}
