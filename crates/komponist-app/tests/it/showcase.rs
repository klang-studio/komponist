//! The app's demo project (`komponist --demo`): built-in devices only, same song as the design
//! fixture, and an offline render of it for the levels (`render_levels`, ignored).

use std::path::PathBuf;
use std::sync::Arc;

use komponist_app::actions::{BitDepth, ExportRange};
use komponist_app::showcase::showcase_project;
use komponist_builtin::Internal;
use komponist_model::fixtures::{demo_project, demo_song};
use komponist_model::{
    ClipContent, Device, DeviceRole, PluginFormat, Project, Tap, Track, TrackOutput,
};
use komponist_plugins::Catalog;

use crate::export::{app_in, export, export_settings, peak_db, read, scratch};

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
    // Drums 2, Bass 3, Lead 3, Pad 2, Keys 2, Arp 4, sampler demos 2, Reverb bus 1, master 1.
    assert_eq!(count, 2 + 3 + 3 + 2 + 2 + 4 + 2 + 1 + 1);
    // The Arp's MIDI effects come first, the Limiter is on the master.
    let arp = &track_named(&project, "Arp").channel.devices;
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
    assert!(!track_named(&project, "Arp").channel.mute);
}

#[test]
fn internal_devices_show_their_preset_names() {
    let project = showcase_project();
    for device in all_devices(&project) {
        let Some(kind) = Internal::from_plugin(&device.plugin) else {
            continue;
        };
        if matches!(
            kind,
            Internal::Sampler | Internal::GeneralSampler | Internal::Limiter
        ) {
            continue;
        }
        let (index, changed) = kind
            .loaded_preset(device.state.as_deref())
            .unwrap_or_else(|| panic!("{} has no loaded preset", kind.name()));
        // presse on the Bass is „Ducking“ with a gentler setting; every other device is a factory preset.
        assert!(
            !changed || kind == Internal::Presse,
            "{} {}",
            kind.name(),
            kind.presets()[index].name
        );
    }
}

#[test]
fn the_song_is_the_design_fixtures_song() {
    let fixture = demo_project();
    let showcase = showcase_project();
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
                && !matches!(t.name.as_str(), "Glass Pluck" | "Glass Slices")
        })
        .collect();
    assert_eq!(songs.len(), fixture.tracks.len());
    assert_eq!(showcase.tracks.len(), fixture.tracks.len() + 7);
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
            assert_eq!(notes(&showcase), notes(&fixture), "track {i}");
        }
    }
    // The fixture's own Arp clip is the song's.
    assert_eq!(notes_of(&demo_song(), 5), notes_of(&fixture, 5));
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

#[test]
fn the_demo_shows_group_effect_bus_and_sidechain() {
    let project = showcase_project();
    let rhythm = track_named(&project, "Rhythm");
    assert!(rhythm.is_bus() && rhythm.folder);
    assert_eq!(rhythm.output, TrackOutput::Master);
    let (drums, bass) = (
        track_named(&project, "Drums"),
        track_named(&project, "Bass"),
    );
    assert_eq!(drums.output, TrackOutput::Bus(rhythm.id));
    assert_eq!(bass.output, TrackOutput::Bus(rhythm.id));
    let members = komponist_model::routing::group_members(&project, rhythm.id);
    assert_eq!(
        members,
        [drums.id, bass.id, track_named(&project, "Shaker").id]
    );

    let hall = track_named(&project, "Reverb");
    assert!(hall.is_bus() && !hall.folder);
    assert_eq!(hall.channel.devices.len(), 1);
    assert_eq!(
        Internal::from_plugin(&hall.channel.devices[0].plugin),
        Some(Internal::Hall)
    );
    for name in ["Lead", "Pad"] {
        let track = track_named(&project, name);
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
                .all(|d| Internal::from_plugin(&d.plugin) != Some(Internal::Hall))
        );
    }
    // The Lead keeps its Delay as an insert.
    assert!(
        track_named(&project, "Lead")
            .channel
            .devices
            .iter()
            .any(|d| Internal::from_plugin(&d.plugin) == Some(Internal::Delay))
    );

    // presse on the Bass is keyed by the Drums, pre-fader; the Drums' own presse has no sidechain.
    let keyed: Vec<_> = all_devices(&project)
        .filter(|d| d.sidechain.is_some())
        .collect();
    assert_eq!(keyed.len(), 1);
    assert_eq!(
        Internal::from_plugin(&keyed[0].plugin),
        Some(Internal::Presse)
    );
    assert!(bass.channel.devices.iter().any(|d| d.id == keyed[0].id));
    let sc = keyed[0].sidechain.unwrap();
    assert_eq!((sc.source, sc.tap), (drums.id, Tap::Pre));
    assert_eq!(komponist_model::routing::check(&project), Ok(()));
}

#[test]
fn the_demo_shows_repitch_gain_fades_and_mono_and_stereo_audio() {
    use komponist_model::{AudioInput, ClipTimeMode, Hue, MonitorMode, Tick};
    let project = showcase_project();
    assert_eq!(project.tempo.bpm, 124.0);
    let shaker = track_named(&project, "Shaker");
    let sweep = track_named(&project, "Sweep");
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

#[test]
fn loading_the_demo_synthesizes_deterministic_non_silent_audio_files() {
    let dir = scratch("demo-audio");
    let p = komponist_app::samples::demo_with_kit(showcase_project(), &dir.join("a")).unwrap();
    let second = komponist_app::samples::demo_with_kit(showcase_project(), &dir.join("b")).unwrap();
    for name in ["Shaker", "Sweep", "Pulse 90"] {
        let clip = &track_named(&p, name).clips[0];
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("audio clip");
        };
        let ClipContent::Audio(other) = &track_named(&second, name).clips[0].content else {
            panic!("audio clip");
        };
        assert!(audio.file.abs.starts_with(dir.join("a")));
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

#[test]
fn the_demo_plays_one_sample_chromatically_and_as_slices() {
    use komponist_general_sampler::read_settings;
    use komponist_model::params::ParamId;
    use komponist_sampler::links;

    let dir = scratch("demo-sampler");
    let project = komponist_app::samples::demo_with_kit(showcase_project(), &dir).unwrap();
    let mut files = Vec::new();
    for (name, slice_mode) in [("Glass Pluck", false), ("Glass Slices", true)] {
        let track = track_named(&project, name);
        let device = &track.channel.devices[0];
        let kind = Internal::GeneralSampler;
        assert_eq!(Internal::from_plugin(&device.plugin), Some(kind));
        let state = device.state.as_deref();
        let values = kind.decode_state(state).expect("sampler state");
        let value = |id| values[kind.wire(&ParamId::new(id)).unwrap() as usize];
        assert_eq!(value("root"), 48.0);
        assert_eq!(value("mode"), if slice_mode { 2.0 } else { 1.0 });
        assert_eq!(value("sync"), f64::from(u8::from(!slice_mode)));
        assert_eq!(
            read_settings(state).slices.len(),
            if slice_mode { 5 } else { 0 }
        );
        let links = links::read(state);
        let [(0, link)] = links.as_slice() else {
            panic!("one linked sample in slot zero");
        };
        files.push(link.abs.clone());
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
        assert!(rms > 1e-6, "{name} is silent");
        peak_db(&wav.samples)
    };
    let base = komponist_app::samples::demo_with_kit(showcase_project(), &out.join("kit")).unwrap();
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

#[test]
fn synthesized_pulse_loop_uses_opt_in_warp_without_changing_the_showcase_tempo() {
    let p = showcase_project();
    let pulse = track_named(&p, "Pulse 90");
    let c = &pulse.clips[0];
    let ClipContent::Audio(a) = &c.content else {
        panic!("audio")
    };
    assert_eq!(p.tempo.bpm, 124.0);
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
    assert_eq!(a.source_frames(c.length, p.tempo), 256000);
}

#[test]
fn the_demo_has_track_clip_and_master_automation() {
    use komponist_model::Tick;
    use komponist_model::automation::{AutomationTarget, effective_value_at};
    use komponist_model::params::ParamId;

    let project = showcase_project();
    let lead = track_named(&project, "Lead");
    let pad = track_named(&project, "Pad");
    let cutoff = AutomationTarget::Param {
        device: pad.channel.devices[0].id,
        param: ParamId::new("cutoff"),
    };
    let feedback = AutomationTarget::Param {
        device: lead.channel.devices[0].id,
        param: ParamId::new("feedback"),
    };
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
    let keys = track_named(&project, "Keys");
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
