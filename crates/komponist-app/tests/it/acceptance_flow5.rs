//! Acceptance flow 5 (HANDOFF §9): export as WAV through "⋯ › WAV exportieren …" and through
//! Strg Shift E; the files are read back (length = song + tail, normalised peak, music in the body).
//!
//! The song is the demo's first bar on hexa only (debug builds render slowly). Files and
//! settings go to a scratch folder under `CARGO_TARGET_TMPDIR`, never to the real home.

use crate::acceptance_support;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use acceptance_support::*;
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use komponist_app::settings::{self, Settings};
use komponist_app::ui_state::Dialog;
use komponist_app::{App, engine_sync::EngineSync};
use komponist_engine::export::{ExportJob, NORMALIZE_TARGET_DB};
use komponist_i18n::de;
use komponist_model::fixtures::demo_project;
use komponist_model::{DeviceRole, Document, Project, Tick, TickRange};

const BAR: i64 = 3840;
const PATIENCE: Duration = Duration::from_secs(120);

/// The demo's first bar, hexa for every instrument, no effects.
fn one_bar_song(instrument: &komponist_model::PluginRef) -> Project {
    let mut p = demo_project();
    for track in &mut p.tracks {
        let track = Arc::make_mut(track);
        track
            .channel
            .devices
            .retain(|d| d.role == DeviceRole::Instrument);
        for device in &mut track.channel.devices {
            let device = Arc::make_mut(device);
            device.plugin = instrument.clone();
            device.state = None;
        }
        track.clips.retain(|c| c.start == Tick::ZERO);
        for clip in &mut track.clips {
            Arc::make_mut(clip).length = Tick(BAR);
        }
    }
    p
}

fn app_in(dir: &Path) -> App {
    let mut settings = Settings::default();
    settings.audio.host = komponist_audio::HostKind::Null;
    let host = App::headless_plugin_host();
    let instrument = super::common::shipped_plugin(&host, "org.klang-studio.komponist.hexa")
        .expect("build the Hexa fixture with scripts/build-test-plugins.sh");
    let engine = EngineSync::null(Box::new(host));
    App::new(
        Document::new(one_bar_song(&instrument)),
        engine,
        settings,
        Some(dir.join("config").join(settings::FILE_NAME)),
    )
}

/// Points the open export dialog at `folder` and `file`.
fn target(h: &mut Harness<'_, App>, folder: &Path, file: &str) {
    let Some(Dialog::Export(d)) = &mut h.state_mut().ui_mut().dialog else {
        panic!("the export dialog is not open");
    };
    d.settings.folder = folder.to_path_buf();
    d.settings.file_name = file.into();
    h.run_steps(2);
}

fn wait_for_export(h: &mut Harness<'_, App>) {
    let until = Instant::now() + PATIENCE;
    h.run_steps(2);
    while h.state().is_exporting() {
        assert!(Instant::now() < until, "the export does not end");
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(2);
}

/// Length, peak and body of the WAV: bar 1 plus the default 2 s tail at 48 kHz, 24 bit stereo,
/// normalised to −1 dBFS, music in the bar and a quiet end.
fn check_wav(app: &App, path: &Path) {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    assert_eq!(
        (spec.sample_rate, spec.bits_per_sample, spec.channels),
        (48_000, 24, 2)
    );
    let full = (1_i64 << 23) as f32;
    let samples: Vec<f32> = reader
        .samples::<i32>()
        .map(|s| s.unwrap() as f32 / full)
        .collect();
    let program = app.engine().program().unwrap();
    let song = ExportJob::frames_of(
        &program.tempo_map,
        TickRange::new(Tick::ZERO, Tick(BAR)),
        48_000,
    );
    let song = (song.end - song.start) as usize;
    assert_eq!(samples.len() / 2, song + 96_000, "bar 1 plus 2 s");
    let peak = 20.0 * samples.iter().fold(0.0_f32, |m, s| m.max(s.abs())).log10();
    assert!(
        (peak - NORMALIZE_TARGET_DB).abs() < 0.01,
        "peak {peak} dBFS"
    );
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let body = rms(&samples[..2 * song]);
    let end = rms(&samples[samples.len() - 9_600..]);
    assert!(body > 0.05 && end < body / 10.0, "rms {body}, tail {end}");
}

#[test]
fn the_song_exports_as_wav_through_the_menu_and_through_ctrl_shift_e() {
    let dir = scratch("flow5");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let mut h = harness(app_in(&dir));

    // ⋯ › "WAV exportieren …": S08 opens, "Exportieren" starts it.
    click(&mut h, Role::Button, de::TRANSPORT_MENU);
    h.run_steps(60);
    h.get_by_label_contains("WAV exportieren").click();
    h.run_steps(3);
    h.get_by_label(de::EXPORT_TITLE);
    target(&mut h, &out, "Menue.wav");
    click(&mut h, Role::Button, de::EXPORT_EXPORT);
    wait_for_export(&mut h);
    assert!(h.state().ui().dialog.is_none());
    check_wav(h.state(), &out.join("Menue.wav"));

    // Strg Shift E: the same dialog with the last settings; Enter starts it.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    h.run_steps(3);
    h.get_by_label(de::EXPORT_TITLE);
    target(&mut h, &out, "Tastatur.wav");
    key(&mut h, Key::Enter);
    wait_for_export(&mut h);
    check_wav(h.state(), &out.join("Tastatur.wav"));

    assert_eq!(
        h.state().exported(),
        [out.join("Menue.wav"), out.join("Tastatur.wav")]
    );
    let mut files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["Menue.wav", "Tastatur.wav"], "no part files left");
}
