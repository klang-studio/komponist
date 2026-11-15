//! `komponist`: a MIDI DAW for Linux.
//!
//! ```text
//! komponist [PROJECT.komponist] [--demo] [--null-audio] [--scale N] [--settings PATH]
//!           [--open-plugin NAME] [--lang de|en] [--no-plugin-scan]
//! ```
//!
//! Opens `PROJECT`, or shows the start screen (S09) with the recent projects
//! (`recent.json` next to the settings file). `--demo` opens the demo project "Night Drive". The
//! audio output comes from the settings (`~/.config/komponist/settings.json`);
//! `--null-audio` (or `KOMPONIST_NULL_AUDIO=1`) runs the engine on the null backend without any
//! audio or MIDI device, for checks under Xvfb. `--scale` overrides the UI scale. `--lang` overrides
//! the UI language of the settings (`language`: `system`, `de`, `en`; `system` is German on a
//! German desktop and English otherwise). `--open-plugin`
//! opens the plugin window (S05 frame) of the first device whose plugin is called `NAME` in the
//! opened project (checks of the frame without clicking). With
//! `EGUI_INSPECTION=127.0.0.1:<port>` the window serves the inspection protocol for `egui-mcp`.
//!
//! Plugins: hexa, the Sampler and the internal devices run in process; the CLAP/VST3 plugins run in
//! bridge processes of their own: this executable started as `komponist --bridge …` (one
//! executable, nothing to build or keep in step; the bridge process initialises no GUI or audio).
//! At start a
//! background scan covers the default plugin folders plus those in the settings; devices whose
//! plugin it finds load when it ends. `--no-plugin-scan` (or `KOMPONIST_NO_PLUGIN_SCAN=1`) skips that
//! scan, so the browser lists only the built-in devices (screenshots, checks). Closing the window shuts
//! every bridge down.

use std::path::PathBuf;
use std::process::ExitCode;

use komponist_app::device_host::PluginHost;
use komponist_app::dialogs::new_project;
use komponist_app::engine_sync::{EngineSync, MidiSetup};
use komponist_app::panels::transport::WORDMARK;
use komponist_app::settings::{self, Settings};
use komponist_app::{APP_ID, Action, App, ProjectAction};
use komponist_audio::{HostKind, StreamConfig};
use komponist_i18n::Lang;
use komponist_model::Document;
use komponist_plugins::{BRIDGE_FLAG, PluginManagerConfig};
use komponist_ui::RunOptions;

// Debug builds abort on any allocation inside the audio callback (ARCHITECTURE §6.1).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const USAGE: &str = "\
usage: komponist [PROJECT.komponist] [options]

  --demo           open the demo project Night Drive instead of the start screen
  --null-audio     no audio or MIDI device: the engine runs on a timer (also KOMPONIST_NULL_AUDIO=1)
  --scale N        UI scale (pixels per point), e.g. 1 or 2
  --settings PATH  settings file (default ~/.config/komponist/settings.json)
  --open-plugin NAME  open the plugin window of the first device whose plugin is NAME
  --lang de|en     UI language (default: the setting; \"system\" is German on a German desktop, else English)
  --no-plugin-scan  skip the plugin scan: only the built-in devices (also KOMPONIST_NO_PLUGIN_SCAN=1)";

#[derive(Debug, Default)]
struct Args {
    project: Option<PathBuf>,
    demo: bool,
    null_audio: bool,
    scale: Option<f32>,
    settings: Option<PathBuf>,
    open_plugin: Option<String>,
    lang: Option<Lang>,
    no_plugin_scan: bool,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        null_audio: std::env::var("KOMPONIST_NULL_AUDIO").is_ok_and(|v| v != "0" && !v.is_empty()),
        no_plugin_scan: std::env::var("KOMPONIST_NO_PLUGIN_SCAN")
            .is_ok_and(|v| v != "0" && !v.is_empty()),
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--null-audio" => args.null_audio = true,
            "--no-plugin-scan" => args.no_plugin_scan = true,
            "--demo" => args.demo = true,
            "--scale" => {
                let v: f32 = value()?.parse().map_err(|e| format!("--scale: {e}"))?;
                if !(v.is_finite() && v > 0.0) {
                    return Err("--scale must be positive".to_owned());
                }
                args.scale = Some(v);
            }
            "--settings" => args.settings = Some(PathBuf::from(value()?)),
            "--open-plugin" => args.open_plugin = Some(value()?),
            "--lang" => {
                let v = value()?;
                args.lang = Some(Lang::from_code(&v).ok_or_else(|| {
                    let codes: Vec<_> = Lang::ALL.iter().map(|l| l.code()).collect();
                    format!(
                        "--lang: unknown language \"{v}\" (one of {})",
                        codes.join(", ")
                    )
                })?);
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            path if args.project.is_none() => args.project = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(Some(args))
}

fn main() -> ExitCode {
    // Bridge mode first, before anything of the app starts (no settings, GUI, graphics or audio):
    // the app starts its plugin bridges as `komponist --bridge …` (WP-94).
    let mut argv = std::env::args_os().skip(1).peekable();
    if argv.peek().is_some_and(|a| a == BRIDGE_FLAG) {
        argv.next();
        return komponist_bridge::run(argv);
    }
    match run_app() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

fn run_app() -> eframe::Result {
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
    // The plugin bridge is this executable with `--bridge` (see `main`).
    let host = PluginHost::new(PluginManagerConfig::defaults());
    let (mut engine, started) = EngineSync::new(&config, midi, Box::new(host));
    if let Err(e) = started {
        eprintln!("komponist: audio output not available ({e}); running without device");
    }
    if settings.worker_threads.is_some()
        && let Err(e) = engine.set_workers(settings.worker_threads)
    {
        eprintln!("komponist: worker threads not changed ({e})");
    }
    if let Some(status) = engine.worker_status() {
        if let Some(e) = &status.spawn_error {
            eprintln!("komponist: {} worker threads started ({e})", status.threads);
        }
        if let Some(e) = &status.realtime_error {
            eprintln!("komponist: worker threads run without realtime priority ({e})");
        }
    }
    // The one place that reads the desktop's language; `App::new` stays German for the tests.
    let lang = komponist_app::settings::startup_lang(
        args.lang,
        settings.language,
        sys_locale::get_locale().as_deref(),
    );
    let first = if args.demo {
        // The demo's Sampler plays the synthesized test kit (written to the temporary folder).
        let demo = komponist_app::showcase::showcase_project();
        let kit_dir = std::env::temp_dir().join("komponist-demo-kit");
        komponist_app::samples::demo_with_kit(demo.clone(), &kit_dir).unwrap_or_else(|e| {
            eprintln!("komponist: demo kit not written ({e}); the Sampler stays empty");
            demo
        })
    } else {
        new_project::empty_project(lang)
    };
    let title = komponist_app::app::window_title(&first.name);
    let mut options = RunOptions::new(WORDMARK, title);
    options.app_id = APP_ID.to_owned();
    options.pixels_per_point = args.scale.or(settings.ui_scale);
    let mut app = App::new(Document::new(first), engine, settings, settings_path).with_lang(lang);
    if !args.no_plugin_scan {
        app.scan_plugins();
    }
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
    if let Some(name) = &args.open_plugin
        && !app.open_plugin_named(name)
    {
        eprintln!("komponist: --open-plugin: no device with plugin \"{name}\"");
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
