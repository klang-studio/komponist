//! `komponist`: a MIDI DAW for Linux.
//!
//! ```text
//! komponist [PROJECT.komponist] [--demo] [--null-audio] [--scale N] [--settings PATH]
//! ```
//!
//! Opens `PROJECT`, or shows the start screen (S09) with the recent projects
//! (`recent.json` next to the settings file). `--demo` opens the demo project "Nachtfahrt". The
//! audio output comes from the settings (`~/.config/komponist/settings.json`);
//! `--null-audio` (or `KOMPONIST_NULL_AUDIO=1`) runs the engine on the null backend without any
//! audio or MIDI device, for checks under Xvfb. `--scale` overrides the UI scale. With
//! `EGUI_INSPECTION=127.0.0.1:<port>` the window serves the inspection protocol for `egui-mcp`.
//!
//! Plugins: pisynth runs in process; klopfer and the CLAP/VST3 plugins run in
//! `komponist-bridge` processes next to this executable (`cargo build` builds both). At start a
//! background scan covers the default plugin folders plus those in the settings; devices whose
//! plugin it finds load when it ends. Closing the window shuts every bridge down.

use std::path::PathBuf;

use komponist_app::device_host::PluginHost;
use komponist_app::dialogs::new_project;
use komponist_app::engine_sync::{EngineSync, MidiSetup};
use komponist_app::panels::transport::WORDMARK;
use komponist_app::settings::{self, Settings};
use komponist_app::{Action, App, ProjectAction};
use komponist_audio::{HostKind, StreamConfig};
use komponist_model::{Document, fixtures};
use komponist_plugins::PluginManagerConfig;
use komponist_ui::RunOptions;

// Debug builds abort on any allocation inside the audio callback (ARCHITECTURE §6.1).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const USAGE: &str = "\
usage: komponist [PROJECT.komponist] [options]

  --demo           open the demo project Nachtfahrt instead of the start screen
  --null-audio     no audio or MIDI device: the engine runs on a timer (also KOMPONIST_NULL_AUDIO=1)
  --scale N        UI scale (pixels per point), e.g. 1 or 2
  --settings PATH  settings file (default ~/.config/komponist/settings.json)";

#[derive(Debug, Default)]
struct Args {
    project: Option<PathBuf>,
    demo: bool,
    null_audio: bool,
    scale: Option<f32>,
    settings: Option<PathBuf>,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        null_audio: std::env::var("KOMPONIST_NULL_AUDIO").is_ok_and(|v| v != "0" && !v.is_empty()),
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--null-audio" => args.null_audio = true,
            "--demo" => args.demo = true,
            "--scale" => {
                let v: f32 = value()?.parse().map_err(|e| format!("--scale: {e}"))?;
                if !(v.is_finite() && v > 0.0) {
                    return Err("--scale must be positive".to_owned());
                }
                args.scale = Some(v);
            }
            "--settings" => args.settings = Some(PathBuf::from(value()?)),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            path if args.project.is_none() => args.project = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(Some(args))
}

fn main() -> eframe::Result {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return Ok(());
        }
        Err(e) => {
            eprintln!("komponist: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    // An unreadable settings file is reported and left alone: this session runs on the
    // defaults and does not overwrite it.
    let path = args.settings.clone().or_else(settings::default_path);
    let (settings, settings_path) = match path.as_deref().map(Settings::load) {
        Some(Ok(s)) => (s, path),
        Some(Err(e)) => {
            eprintln!("komponist: settings not loaded, using defaults: {e}");
            (Settings::default(), None)
        }
        None => (Settings::default(), None),
    };

    let (config, midi) = if args.null_audio {
        let config = StreamConfig {
            host: HostKind::Null,
            device: None,
            device_id: None,
            ..settings.audio.clone()
        };
        (config, MidiSetup::None)
    } else {
        (settings.audio.clone(), MidiSetup::System)
    };
    let plugins = PluginManagerConfig::defaults();
    if !plugins.bridge_exe.exists() {
        eprintln!(
            "komponist: {} not found; plugins other than pisynth will not load \
             (cargo build -p komponist-bridge)",
            plugins.bridge_exe.display()
        );
    }
    let host = PluginHost::new(plugins);
    let (engine, started) = EngineSync::new(&config, midi, Box::new(host));
    if let Err(e) = started {
        eprintln!("komponist: audio output not available ({e}); running without device");
    }
    let first = if args.demo {
        fixtures::demo_project()
    } else {
        new_project::empty_project()
    };
    let title = komponist_app::app::window_title(&first.name);
    let mut options = RunOptions::new(WORDMARK, title);
    options.pixels_per_point = args.scale.or(settings.ui_scale);
    let mut app = App::new(Document::new(first), engine, settings, settings_path);
    app.scan_plugins();
    for name in app.settings().midi_disabled.clone() {
        if let Some(hub) = app.engine_mut().midi_mut() {
            let _ = hub.set_enabled(&name, false);
        }
    }
    if !args.demo {
        // The start screen; a project argument opens through it (recent list, S06 check), and
        // a file that cannot be read leaves the start screen with the reason.
        app.show_start_screen();
        if let Some(path) = args.project {
            let path = std::path::absolute(&path).unwrap_or(path);
            app.dispatch(Action::Project(ProjectAction::OpenPath(path)));
        }
    }
    komponist_ui::run(options, move |_cc| Shell(app))
}

/// The eframe side of the app.
struct Shell(App);

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.show(ui);
    }

    fn on_exit(&mut self) {
        self.0.shutdown();
    }
}
