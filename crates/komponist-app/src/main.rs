//! `komponist`: a MIDI DAW for Linux, macOS and Windows.
//!
//! ```text
//! komponist [PROJECT.komponist] [--demo] [--null-audio] [--scale N] [--settings PATH]
//!           [--open-plugin NAME] [--lang de|en] [--no-plugin-scan]
//! ```
//!
//! Opens `PROJECT`, or shows the start screen (S09) with the recent projects
//! (`recent.json` next to the settings file). On macOS, projects opened from Finder or dropped on
//! the Dock icon (also at launch) take the same way (`App::open_files`, #96). Recovery copies that
//! an abnormal end left behind are offered before any of that (`App::offer_recovery`, #227; the
//! folder is `komponist_app::recovery::default_root`). `--demo` opens the demo project "Night Drive". The
//! audio output comes from the settings (`~/.config/komponist/settings.json`, on macOS
//! `~/Library/Application Support/komponist/settings.json`, on Windows `%APPDATA%\komponist\settings.json`);
//! `--null-audio` (or `KOMPONIST_NULL_AUDIO=1`) runs the engine on the null backend without any
//! audio or MIDI device, for checks under Xvfb. `--scale` overrides the UI scale. `--lang` overrides
//! the UI language of the settings (`language`: `system`, `de`, `en`; `system` is German on a
//! German desktop and English otherwise). `--open-plugin`
//! opens the plugin window (S05 frame) of the first device whose plugin is called `NAME` in the
//! opened project (checks of the frame without clicking). With
//! `EGUI_INSPECTION=127.0.0.1:<port>` the window serves the inspection protocol for `egui-mcp`.
//!
//! The bundled devices are loaded from the installed, authenticated CLAP inventory. External
//! plugins run in bridge processes using this executable with `--bridge`. The startup scan
//! includes the bundled devices even when `--no-plugin-scan` skips external plugin folders.

// Windows release builds: no console window; `attach_parent_console` gives `--help` its output back (§8.8).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::dialogs::new_project;
use komponist_app::engine_sync::{EngineSync, MidiSetup};
use komponist_app::panels::transport::WORDMARK;
use komponist_app::settings::{self, Settings};
use komponist_app::{APP_ID, App};
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
  --settings PATH  settings file (default ~/.config/komponist/settings.json,
                   macOS: ~/Library/Application Support/komponist/settings.json,
                   Windows: %APPDATA%\\komponist\\settings.json)
  --open-plugin NAME  open the plugin window of the first device whose plugin is NAME
  --lang de|en     UI language (default: the setting; \"system\" is German on a German desktop, else English)
  --no-plugin-scan  scan only the bundled devices (also KOMPONIST_NO_PLUGIN_SCAN=1)";

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
    #[cfg(windows)]
    attach_parent_console();
    // Bridge mode first, before anything of the app starts (no settings, GUI, graphics or audio):
    // the app starts its plugin bridges as `komponist --bridge …` (WP-94).
    let mut argv = std::env::args_os().skip(1).peekable();
    if argv.peek().is_some_and(|a| a == BRIDGE_FLAG) {
        argv.next();
        return komponist_bridge::run(argv);
    }
    if argv.next_if(|arg| arg == "--verify-bundled-clap").is_some() {
        if argv.next().is_some() {
            eprintln!("usage: komponist --verify-bundled-clap");
            return ExitCode::from(2);
        }
        return match komponist_plugins::verify_bundled_clap(&settings::cache_dir_or_temp()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("bundled CLAP verification: {error}");
                ExitCode::FAILURE
            }
        };
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
    komponist_audiofile::FilePool::global()
        .set_cache_dir(settings::cache_dir().map(|dir| dir.join("peaks")));
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
    let mut host = PluginHost::new(PluginManagerConfig::defaults(&settings::cache_dir_or_temp()));
    let demo = if args.demo {
        let kit_dir = std::env::temp_dir().join("komponist-demo-kit");
        let project = komponist_app::showcase::bundled_demo(&host, &kit_dir)
            .map_err(|error| eframe::Error::AppCreation(std::io::Error::other(error).into()))?;
        Some(project)
    } else {
        None
    };
    if args.no_plugin_scan {
        host.start_bundled_scan();
    } else {
        host.start_scan(&settings.plugin_dirs());
    }
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
        if let Some(e) = &status.workgroup_error {
            eprintln!("komponist: worker threads did not join the audio workgroup ({e})");
        }
    }
    // The one place that reads the desktop's language; `App::new` stays German for the tests.
    let lang = komponist_app::settings::startup_lang(
        args.lang,
        settings.language,
        sys_locale::get_locale().as_deref(),
    );
    let first = demo.unwrap_or_else(|| new_project::empty_project(lang));
    let title = komponist_app::app::window_title(&first.name);
    let mut options = RunOptions::new(WORDMARK, title);
    options.app_id = APP_ID.to_owned();
    options.pixels_per_point = args.scale.or(settings.ui_scale);
    let mut app = App::new(Document::new(first), engine, settings, settings_path).with_lang(lang);
    // Crash recovery (#227): copies of unsaved changes in the per-user state folder, and a panic
    // hook that writes the last one when the UI thread panics.
    match komponist_app::recovery::default_root() {
        Some(root) => app = app.with_recovery(root),
        None => eprintln!("komponist: no home folder: recovery copies are off"),
    }
    if let Some(handle) = app.recovery_panic_handle() {
        komponist_app::recovery::install_panic_hook(handle);
    }
    if let Some(input) = app.settings().audio_input.clone()
        && let Err(error) = app.engine_mut().set_audio_input(Some(input))
    {
        app.report_audio_input_failed(&error);
    }
    for name in app.settings().midi_disabled.clone() {
        if let Some(hub) = app.engine_mut().midi_mut() {
            hub.remember_disabled(&name);
        }
    }
    for name in app.settings().midi_enabled.clone() {
        if let Some(hub) = app.engine_mut().midi_mut() {
            hub.remember_enabled(&name);
        }
    }
    if !args.demo {
        // The start screen; a project argument opens through it (recent list, S06 check), and
        // a file that cannot be read leaves the start screen with the reason.
        app.show_start_screen();
    }
    // Recovery copies of other sessions are asked for first; the project argument waits behind
    // them (with `--demo` the prompt shows over the demo, which a recovered copy replaces).
    app.offer_recovery(args.project.filter(|_| !args.demo));
    // `--open-plugin` looks in the document that opens: now, or after the recovery prompt.
    if let Some(name) = args.open_plugin.clone() {
        app.open_plugin_at_start(name);
    }
    // The Dock menu (#95) and documents from Finder (#96): methods on winit's application
    // delegate, added when the app launches.
    #[cfg(target_os = "macos")]
    komponist_app::macos_delegate::install();
    komponist_ui::run(options, move |cc| {
        // After winit's own default menu exists: this replaces it.
        #[cfg(target_os = "macos")]
        let menu = {
            use komponist_app::application_menu::{Backend, Snapshot};
            let snapshot = Snapshot::new(
                app.lang(),
                app.keymap(),
                Backend::AppKit,
                &app.plugin_window_entries(),
                true,
            );
            let menu = komponist_app::macos_menu::MacMenu::install(&cc.egui_ctx, &snapshot);
            if menu.is_some() {
                app.set_menu_backend(Backend::AppKit);
            }
            menu
        };
        #[cfg(target_os = "macos")]
        komponist_app::macos_delegate::set_waker(&cc.egui_ctx);
        // Only the macOS menu and delegate use the creation context.
        let _ = cc;
        Shell {
            app,
            #[cfg(target_os = "macos")]
            menu,
        }
    })
}

/// The eframe side of the app.
struct Shell {
    app: App,
    /// The native app menu (macOS): its commands are dispatched at the start of a frame.
    #[cfg(target_os = "macos")]
    menu: Option<komponist_app::macos_menu::MacMenu>,
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        if let Some(menu) = &mut self.menu {
            menu.update(&self.app.menu_snapshot());
            for command in menu.drain() {
                if self.app.menu_command_enabled(command) {
                    if command == komponist_app::application_menu::Command::About {
                        menu.show_about();
                    } else {
                        self.app.dispatch_menu(command);
                    }
                }
            }
        }
        // Documents from Finder or the Dock icon (#96), also those of the launch.
        #[cfg(target_os = "macos")]
        {
            let files = komponist_app::macos_delegate::take_opened_files();
            if !files.is_empty() {
                self.app.open_files(&files);
            }
        }
        self.app.show(ui);
    }

    fn on_exit(&mut self) {
        self.app.shutdown();
    }
}

/// Windows release builds use the `windows` subsystem (no console window), so a start from a terminal has no
/// stdout or stderr: `komponist --help` would print nothing. When a standard handle is missing, attach to the
/// console of the parent process (if there is one) and point the missing handles at it. Handles the parent
/// passes (pipes or files, e.g. the bridge's, or a redirect in a script) are left alone. Without a parent
/// console (Explorer, the Start menu) nothing changes and the app has no console, as intended.
#[cfg(windows)]
fn attach_parent_console() {
    use std::fs::OpenOptions;
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };

    let missing = |which| {
        // SAFETY: `GetStdHandle` only reads the process' standard handle slot.
        let handle = unsafe { GetStdHandle(which) };
        handle.is_null() || handle == INVALID_HANDLE_VALUE
    };
    if !missing(STD_OUTPUT_HANDLE) && !missing(STD_ERROR_HANDLE) {
        return;
    }
    // SAFETY: no pointers; fails (returns 0) when the parent has no console.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        if !missing(which) {
            continue;
        }
        if let Ok(conout) = OpenOptions::new().write(true).open("CONOUT$") {
            // The handle stays open for the life of the process, like the standard handles themselves.
            // SAFETY: `into_raw_handle` hands over a valid, owned console handle.
            unsafe { SetStdHandle(which, conout.into_raw_handle()) };
        }
    }
}
