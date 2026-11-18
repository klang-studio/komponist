//! S07 "Einstellungen" (`Screens.dc.html` `ovSettings`, `setNav`, `buffers`, `sw`, `midiIns`,
//! `pluginDirs`; notes `S07`). Strg , opens it; Esc closes; changes apply at once.
//!
//! The navigation (Audio, MIDI, Plugins, Darstellung, Tastatur) scrolls the page to its section;
//! the page shows them all, as S07 shows "Audio", "MIDI-Eingänge" and "Plugin-Ordner" together.
//!
//! - **Audio:** driver (CoreAudio; WASAPI, ASIO; PipeWire, JACK, ALSA; unavailable ones disabled), output device, sample
//!   rate, buffer size with the latency next to it ([`buffer_limit`]: WASAPI runs at one size, its
//!   device period, so the segments are disabled and a hint names it; ASIO offers the sizes inside
//!   the driver's range). A change rebuilds the stream at once, the
//!   engine stays (`SettingsAction::SetAudio`). The device list shows one "Standardgerät" (the
//!   driver's default) instead of PipeWire's pseudo devices (`default_sink`, `default_output`),
//!   hides PipeWire application streams (named "unknown") and keeps the stored device id
//!   ([`device_entries`]). Without a device: red ring on the device field and the notice
//!   "Gerät nicht verbunden …" with "Erneut suchen". A hint says when the audio thread runs
//!   without real-time priority.
//! - **MIDI-Eingänge:** a switch per input, the activity dot lights on incoming messages; a lone
//!   " MIDI 1" suffix is cut ([`midi_label`]). An input another program holds (WinMM is exclusive)
//!   says "belegt …" instead of "aktiv".
//! - **Plugin-Ordner:** the folders per format with their plugin count, "+ Ordner", "Neu scannen"
//!   (background) and the summary of the last scan (folder rules: [`crate::settings`]).
//! - **Darstellung:** UI scale and language (System, Deutsch, English). **Tastatur:** computer keyboard MIDI on or off.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use egui::{Align, Color32, Context, Rect, Role, Sense, Ui, UiBuilder, pos2, vec2};
use komponist_audio::{AudioSystem, DeviceInfo, HostKind, JackDirection, JackPort, StreamConfig};
use komponist_i18n::Lang;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, typography};
use komponist_ui::widgets::button::{Button, Height, Kind};
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::switch::Switch;
use komponist_ui::widgets::tabs::Segmented;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{px, state};

use super::format;
use super::metrics::{dialog as dm, settings as m};
use super::{ActionButton, divider, dot, galley, header, label_node, modal, section_label, styled};
use crate::actions::{Action, SettingsAction, UiAction};
use crate::engine_sync::EngineProcess;
use crate::settings::Language;
use crate::ui_state::SettingsSection;

/// Buffer sizes of the segments (`buffers`).
pub const BUFFERS: [u32; 5] = [64, 128, 256, 512, 1024];

/// The segments of the language row, in order: System, Deutsch, English.
pub const LANGUAGES: [Language; 3] = [Language::System, Language::De, Language::En];

/// UI scales of the "Darstellung" segments; `None` follows the display.
pub const SCALES: [Option<f32>; 5] = [None, Some(1.0), Some(1.25), Some(1.5), Some(2.0)];

/// The label of a UI scale segment: "Automatisch", "100 %" ("Automatic", "100%").
pub fn scale_label(t: Lang, scale: Option<f32>) -> String {
    match scale {
        None => t.settings_scale_auto().to_owned(),
        Some(s) => t.settings_scale_percent((s * 100.0).round() as u32),
    }
}

/// The navigation entries in order (`setNav`).
pub const SECTIONS: [SettingsSection; 5] = [
    SettingsSection::Audio,
    SettingsSection::Midi,
    SettingsSection::Plugins,
    SettingsSection::Display,
    SettingsSection::Keyboard,
];

/// The name of a navigation entry ("Darstellung" / "Appearance").
pub fn section_name(t: Lang, section: SettingsSection) -> &'static str {
    match section {
        SettingsSection::Audio => t.settings_nav_audio(),
        SettingsSection::Midi => t.settings_nav_midi(),
        SettingsSection::Plugins => t.settings_nav_plugins(),
        SettingsSection::Display => t.settings_nav_display(),
        SettingsSection::Keyboard => t.settings_nav_keyboard(),
    }
}

/// The drivers of the "Treiber" segments (all systems; the dialog lists those of this system, see
/// [`drivers_here`]).
pub const DRIVERS: [(HostKind, &str); 6] = [
    (HostKind::CoreAudio, "CoreAudio"),
    (HostKind::Wasapi, "WASAPI"),
    (HostKind::Asio, "ASIO"),
    (HostKind::PipeWire, "PipeWire"),
    (HostKind::Jack, "JACK"),
    (HostKind::Alsa, "ALSA"),
];

/// The drivers this system has (CoreAudio on macOS; WASAPI, and ASIO in builds with the feature
/// `asio`, on Windows; PipeWire, JACK, ALSA on Linux), in segment order. Those not currently
/// available are shown disabled.
pub fn drivers_here() -> Vec<(HostKind, &'static str)> {
    DRIVERS
        .into_iter()
        .filter(|(host, _)| host.is_supported_here())
        .collect()
}

/// How long the MIDI activity dot stays lit after a message.
const ACTIVITY_HOLD: Duration = Duration::from_millis(150);
/// The available JACK ports can change without switching drivers or reopening the dialog.
const JACK_REFRESH: Duration = Duration::from_secs(1);

// ------------------------------------------------------------------------------------------------
// Device enumeration
// ------------------------------------------------------------------------------------------------

/// Where the dialog gets drivers and devices from: the system ([`SystemDevices`]) or a fixed
/// list (tests, `App::headless`).
pub trait AudioDevices: Send + Sync {
    /// Available drivers (`AudioSystem::available_hosts`).
    fn hosts(&self) -> Vec<HostKind>;
    /// Output devices of `host`; empty if it cannot be enumerated.
    fn devices(&self, host: HostKind) -> Vec<DeviceInfo>;
    /// Capture devices; providers used by headless tests expose none unless explicitly set.
    fn input_devices(&self, _host: HostKind) -> Vec<komponist_audio::InputDeviceInfo> {
        Vec::new()
    }
    /// JACK graph metadata; the fixed/headless provider never contacts a real JACK server.
    fn jack_ports(&self) -> Vec<JackPort> {
        Vec::new()
    }
    /// Whether enumeration may take long enough to stall a frame, so the dialog runs it on a
    /// worker thread and shows the previous list meanwhile (WP-61 measured 7–10 ms for the
    /// drivers, whose JACK probe opens a client, and 170–200 ms for ALSA's devices).
    fn is_slow(&self) -> bool {
        false
    }
}

/// The system's drivers and devices (cpal).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDevices;

impl AudioDevices for SystemDevices {
    fn hosts(&self) -> Vec<HostKind> {
        AudioSystem::<EngineProcess>::available_hosts()
    }

    fn devices(&self, host: HostKind) -> Vec<DeviceInfo> {
        AudioSystem::<EngineProcess>::devices(host).unwrap_or_default()
    }

    fn input_devices(&self, host: HostKind) -> Vec<komponist_audio::InputDeviceInfo> {
        AudioSystem::<EngineProcess>::input_devices(host).unwrap_or_default()
    }

    fn jack_ports(&self) -> Vec<JackPort> {
        komponist_audio::jack_ports().unwrap_or_default()
    }

    fn is_slow(&self) -> bool {
        true
    }
}

/// Runs `f` with `devices` on a worker thread if enumeration is slow, else at once; the result
/// comes through the receiver.
fn enumerate<T: Send + 'static>(
    devices: &Arc<dyn AudioDevices>,
    f: impl FnOnce(&dyn AudioDevices) -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    if devices.is_slow() {
        let devices = Arc::clone(devices);
        let spawned = std::thread::Builder::new()
            .name("komponist-audio-devices".into())
            .spawn(move || {
                let _ = tx.send(f(&*devices));
            });
        if spawned.is_err() {
            // No thread: the dialog keeps the list it has.
            return rx;
        }
    } else {
        let _ = tx.send(f(&**devices));
    }
    rx
}

/// A fixed list of drivers and devices.
#[derive(Debug, Default, Clone)]
pub struct FixedDevices {
    /// Available drivers.
    pub hosts: Vec<HostKind>,
    /// Devices of all drivers.
    pub devices: Vec<DeviceInfo>,
}

impl AudioDevices for FixedDevices {
    fn hosts(&self) -> Vec<HostKind> {
        self.hosts.clone()
    }

    fn devices(&self, host: HostKind) -> Vec<DeviceInfo> {
        self.devices
            .iter()
            .filter(|d| d.host == host)
            .cloned()
            .collect()
    }
}

/// One entry of the "Ausgabegerät" dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    /// What the dropdown shows.
    pub label: String,
    /// `StreamConfig::device` when chosen.
    pub name: Option<String>,
    /// `StreamConfig::device_id` when chosen.
    pub id: Option<komponist_audio::DeviceId>,
}

/// One capture-device choice; None explicitly keeps recording input disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputEntry {
    pub label: String,
    pub config: Option<komponist_audio::InputConfig>,
}

pub fn input_entries(
    t: Lang,
    devices: &[komponist_audio::InputDeviceInfo],
    configured: Option<&komponist_audio::InputConfig>,
) -> (Vec<InputEntry>, usize) {
    let mut entries = vec![
        InputEntry {
            label: t.audio_input_none().to_owned(),
            config: None,
        },
        InputEntry {
            label: t.settings_default_device().to_owned(),
            config: Some(komponist_audio::InputConfig::default()),
        },
    ];
    entries.extend(devices.iter().map(|device| InputEntry {
        label: device.name.clone(),
        config: Some(komponist_audio::InputConfig {
            device: Some(device.name.clone()),
            device_id: device.id.clone(),
        }),
    }));
    let selected = entries
        .iter()
        .position(|entry| {
            entry.config.as_ref() == configured
                || configured.is_some_and(|config| {
                    config.device_id.as_ref().is_some_and(|id| {
                        entry.config.as_ref().and_then(|c| c.device_id.as_ref()) == Some(id)
                    })
                })
        })
        .unwrap_or_else(|| {
            let label = t.audio_input_unavailable(
                configured
                    .and_then(|c| c.device.as_deref())
                    .unwrap_or(t.settings_default_device()),
            );
            entries.push(InputEntry {
                label,
                config: configured.cloned(),
            });
            entries.len() - 1
        });
    (entries, selected)
}

/// Pseudo devices that stand for the default device (WP-13 notes: PipeWire lists
/// `default_sink` and `default_output`; ALSA's `default`) and application streams (PipeWire
/// entries named "unknown").
pub fn is_pseudo(device: &DeviceInfo) -> bool {
    const NAMES: [&str; 3] = ["default_sink", "default_output", "unknown"];
    const IDS: [&str; 3] = [
        "pipewire:sink_default",
        "pipewire:output_default",
        "alsa:default",
    ];
    NAMES.contains(&device.name.as_str())
        || device
            .id
            .as_ref()
            .is_some_and(|id| IDS.contains(&id.0.as_str()))
}

/// The dropdown entries for `host`'s `devices` and the index of `config`'s device: first
/// "Standardgerät", then the real devices; a configured device that is not present (unplugged)
/// is added at the end under its stored name, so the field shows what is missing.
pub fn device_entries(
    t: Lang,
    host: HostKind,
    devices: &[DeviceInfo],
    config: &StreamConfig,
) -> (Vec<DeviceEntry>, usize) {
    let mut entries = vec![DeviceEntry {
        label: t.settings_default_device().to_owned(),
        name: None,
        id: None,
    }];
    entries.extend(
        devices
            .iter()
            .filter(|d| !is_pseudo(d))
            .map(|d| DeviceEntry {
                label: d.name.clone(),
                name: Some(d.name.clone()),
                id: d.id.clone(),
            }),
    );
    if config.host != host || (config.device.is_none() && config.device_id.is_none()) {
        return (entries, 0);
    }
    let by_id = config
        .device_id
        .as_ref()
        .and_then(|id| entries.iter().position(|e| e.id.as_ref() == Some(id)));
    let by_name = || {
        config
            .device
            .as_ref()
            .and_then(|n| entries.iter().position(|e| e.name.as_ref() == Some(n)))
    };
    if let Some(i) = by_id.or_else(by_name) {
        return (entries, i);
    }
    let label = config
        .device
        .clone()
        .or_else(|| config.device_id.as_ref().map(|id| id.0.clone()))
        .unwrap_or_default();
    entries.push(DeviceEntry {
        label,
        name: config.device.clone(),
        id: config.device_id.clone(),
    });
    let last = entries.len() - 1;
    (entries, last)
}

/// The device `entry` stands for (the default device for "Standardgerät").
fn find_device<'a>(devices: &'a [DeviceInfo], entry: &DeviceEntry) -> Option<&'a DeviceInfo> {
    match (&entry.id, &entry.name) {
        (None, None) => devices.iter().find(|d| d.is_default),
        (Some(id), _) => devices.iter().find(|d| d.id.as_ref() == Some(id)),
        (None, Some(name)) => devices.iter().find(|d| d.name == *name),
    }
}

/// Which buffer sizes the driver of the stream allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferLimit {
    /// Any size of [`BUFFERS`] (the driver honours the request).
    Free,
    /// Only sizes inside `min ..= max` (ASIO: the driver's range).
    Range(u32, u32),
    /// One size, whatever is asked: WASAPI's device period (shared mode), or an ASIO driver with
    /// one size. The segments are disabled and the hint names this size.
    Fixed(u32),
}

/// The buffer sizes `host` allows on `entry`. `running` is the configuration the stream really
/// runs on (the engine's `AudioState::Running`): WASAPI's real size is the period it reports.
/// With JACK, the server supplies the actual period.
pub fn buffer_limit(
    host: HostKind,
    devices: &[DeviceInfo],
    entry: &DeviceEntry,
    running: Option<&StreamConfig>,
) -> BufferLimit {
    let range = find_device(devices, entry).and_then(|d| d.buffer_range);
    match host {
        HostKind::Wasapi => running
            .filter(|r| r.host == HostKind::Wasapi)
            .map(|r| r.buffer_frames)
            .or(range.map(|(min, _)| min))
            .map_or(BufferLimit::Free, BufferLimit::Fixed),
        HostKind::Jack => running
            .filter(|r| r.host == HostKind::Jack)
            .map_or(BufferLimit::Free, |r| BufferLimit::Fixed(r.buffer_frames)),
        HostKind::Asio => match range {
            Some((min, max)) if min == max => BufferLimit::Fixed(min),
            Some((min, max)) => BufferLimit::Range(min, max),
            None => BufferLimit::Free,
        },
        _ => BufferLimit::Free,
    }
}

/// The sample rates offered for `entry` on `host`: the device's own, the configured one, and on
/// PipeWire also 44.1, 48 and 96 kHz (PipeWire converts). Ascending. CoreAudio, like ALSA, offers
/// only what the device reports (it switches the device's rate and does not convert).
pub fn rates(
    host: HostKind,
    devices: &[DeviceInfo],
    entry: &DeviceEntry,
    config: &StreamConfig,
) -> Vec<u32> {
    let device = find_device(devices, entry);
    let mut rates: Vec<u32> = device.map(|d| d.sample_rates.clone()).unwrap_or_default();
    rates.push(config.sample_rate);
    if host == HostKind::PipeWire || rates.len() == 1 {
        rates.extend([44_100, 48_000, 96_000]);
    }
    rates.sort_unstable();
    rates.dedup();
    rates
}

/// A MIDI port name for display: a lone " MIDI 1" suffix is cut ("Arturia KeyStep 37 MIDI 1" →
/// "Arturia KeyStep 37"); it stays when the device has more ports (" MIDI 2").
pub fn midi_label(name: &str, all: &[&str]) -> String {
    if let Some(base) = name.strip_suffix(" MIDI 1") {
        let prefix = format!("{base} MIDI ");
        if !all.iter().any(|n| *n != name && n.starts_with(&prefix)) {
            return base.to_owned();
        }
    }
    name.to_owned()
}

// ------------------------------------------------------------------------------------------------
// Model
// ------------------------------------------------------------------------------------------------

/// One MIDI input row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiRow {
    /// Port name (for `SetMidiInput`).
    pub name: String,
    /// Display name ([`midi_label`]).
    pub label: String,
    /// Switched on.
    pub enabled: bool,
    /// Present on the system.
    pub connected: bool,
    /// A message arrived just now (the dot lights).
    pub active: bool,
    /// Switched on and present, but the port could not be opened: another program holds it
    /// (Windows: a WinMM input is exclusive). A message, not an error state.
    pub busy: bool,
}

impl MidiRow {
    /// The info text after the dot (`midiIns` `info`).
    pub fn info(&self, t: Lang) -> &'static str {
        match (self.enabled, self.connected) {
            (false, _) => t.settings_midi_off(),
            (true, true) if self.busy => t.settings_midi_busy(),
            (true, true) => t.settings_midi_active(),
            (true, false) => t.settings_midi_disconnected(),
        }
    }
}

/// One plugin folder row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirRow {
    /// CLAP (else VST3).
    pub clap: bool,
    /// The folder.
    pub path: PathBuf,
    /// Shown path ("~/.clap").
    pub label: String,
    /// Plugins found in it by the last scan.
    pub count: Option<usize>,
    /// An added folder (it can be removed; default folders cannot).
    pub removable: bool,
}

/// Everything S07 shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// The section the navigation highlights.
    pub section: SettingsSection,
    /// Drivers that are available.
    pub hosts: Vec<HostKind>,
    /// The configured stream (settings).
    pub config: StreamConfig,
    /// Explicit capture-device choices (off, default, then named devices).
    pub input_devices: Vec<InputEntry>,
    /// Configured capture choice.
    pub input_device: usize,
    /// JACK audio ports, once the graph's first enumeration completed.
    pub jack_ports: Option<Vec<JackPort>>,
    /// WAV encoding for future recordings.
    pub capture_format: crate::settings::CaptureFormat,
    /// Device dropdown entries ([`device_entries`]).
    pub devices: Vec<DeviceEntry>,
    /// The configured device's entry.
    pub device: usize,
    /// Sample rates ([`rates`]).
    pub rates: Vec<u32>,
    /// Why there is no device, while there is none.
    pub no_device: Option<String>,
    /// Which buffer sizes the driver allows ([`buffer_limit`]).
    pub buffer: BufferLimit,
    /// "5.3 ms Latenz".
    pub latency: String,
    /// The audio thread got no real-time priority.
    pub realtime_denied: bool,
    /// MIDI inputs; `None` without MIDI system.
    pub midi: Option<Vec<MidiRow>>,
    /// Plugin folders.
    pub dirs: Vec<DirRow>,
    /// "214 Plugins gefunden · letzter Scan heute 09:12 · 1 Plugin übersprungen (…)".
    pub scan: String,
    /// A scan runs.
    pub scanning: bool,
    /// UI scale override.
    pub ui_scale: Option<f32>,
    /// The language setting (the chosen segment of "Sprache").
    pub language: Language,
    /// Computer keyboard MIDI switched on.
    pub keyboard_midi: bool,
}

/// The summary line under the plugin folders.
pub fn scan_summary(
    t: Lang,
    report: Option<&komponist_plugins::ScanReport>,
    scanning: bool,
    now: SystemTime,
) -> String {
    if scanning {
        return t.settings_scan_running().to_owned();
    }
    let Some(report) = report else {
        return t.settings_scan_none().to_owned();
    };
    let mut text = t.settings_scan_found(report.found);
    if let (Some(then), Some(now)) = (format::local(report.finished), format::local(now)) {
        text.push_str(" · ");
        text.push_str(&t.settings_scan_last(format::day_and_time(t, &then, &now)));
    }
    if !report.skipped.is_empty() {
        text.push_str(" · ");
        text.push_str(&t.settings_scan_skipped(report.skipped.len()));
    }
    text
}

/// The folder rows: per format the default folders that exist and every added folder, with the
/// counts of the last scan.
pub fn dir_rows(
    settings: &crate::settings::Settings,
    report: Option<&komponist_plugins::ScanReport>,
) -> Vec<DirRow> {
    let dirs = settings.plugin_dirs();
    let mut rows = Vec::new();
    for (clap, list, added) in [
        (true, &dirs.clap, &settings.clap_dirs),
        (false, &dirs.vst3, &settings.vst3_dirs),
    ] {
        for path in list {
            let is_default = crate::settings::Settings::is_default_plugin_dir(path);
            let removable = !is_default && added.contains(path);
            if !removable && !path.is_dir() {
                continue;
            }
            let count = report.and_then(|r| {
                r.per_dir
                    .iter()
                    .find(|c| c.path == *path)
                    .map(|c| c.plugins)
            });
            rows.push(DirRow {
                clap,
                path: path.clone(),
                label: format::tilde(path),
                count,
                removable,
            });
        }
    }
    rows
}

/// The format of a folder chosen with "+ Ordner": VST3 if it holds `.vst3` bundles and no `.clap`
/// ones, or (when it holds neither) if its name says "vst3"; else CLAP. A folder with both gives
/// both.
pub fn folder_formats(path: &Path) -> (bool, bool) {
    let (mut clap, mut vst3) = (false, false);
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            clap |= name.ends_with(".clap");
            vst3 |= name.ends_with(".vst3");
        }
    }
    if !clap && !vst3 {
        let named_vst3 = path.to_string_lossy().to_lowercase().contains("vst3");
        return (!named_vst3, named_vst3);
    }
    (clap, vst3)
}

/// The dialog's caches between frames (app-private).
#[derive(Debug, Default)]
pub(crate) struct Cache {
    /// The dialog was open in the last frame.
    pub(crate) open: bool,
    /// Available drivers.
    pub(crate) hosts: Vec<HostKind>,
    /// Devices of [`Cache::devices_host`].
    pub(crate) devices: Vec<DeviceInfo>,
    /// The driver the device list belongs to.
    pub(crate) devices_host: Option<HostKind>,
    /// A driver enumeration running on a worker thread.
    pending_hosts: Option<Receiver<Vec<HostKind>>>,
    /// A device enumeration running on a worker thread, and its driver.
    pending_devices: Option<(HostKind, Receiver<Vec<DeviceInfo>>)>,
    pending_inputs: Option<(HostKind, Receiver<Vec<komponist_audio::InputDeviceInfo>>)>,
    input_host: Option<HostKind>,
    inputs: Vec<komponist_audio::InputDeviceInfo>,
    jack_ports: Option<Vec<JackPort>>,
    pending_jack_ports: Option<Receiver<Vec<JackPort>>>,
    next_jack_refresh: Option<Instant>,
    /// When each MIDI port (by name) last had activity.
    pub(crate) lit: HashMap<String, Instant>,
    /// Scroll the page to this section in the next frame.
    pub(crate) scroll_to: Option<SettingsSection>,
}

impl Cache {
    pub(crate) fn invalidate_jack_ports(&mut self) {
        self.next_jack_refresh = None;
    }

    /// Whether an enumeration is still running (the dialog repaints until it arrives).
    pub(crate) fn enumerating(&self) -> bool {
        self.pending_hosts.is_some()
            || self.pending_devices.is_some()
            || self.pending_inputs.is_some()
            || self.pending_jack_ports.is_some()
    }

    /// Takes finished enumerations.
    fn collect(&mut self) {
        if let Some(rx) = &self.pending_jack_ports {
            match rx.try_recv() {
                Ok(ports) => {
                    self.jack_ports = Some(ports);
                    self.pending_jack_ports = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending_jack_ports = None,
            }
        }
        if let Some((host, rx)) = &self.pending_inputs {
            match rx.try_recv() {
                Ok(inputs) => {
                    self.inputs = inputs;
                    self.input_host = Some(*host);
                    self.pending_inputs = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending_inputs = None,
            }
        }
        if let Some(rx) = &self.pending_hosts {
            match rx.try_recv() {
                Ok(hosts) => {
                    self.hosts = hosts;
                    self.pending_hosts = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending_hosts = None,
            }
        }
        if let Some((host, rx)) = &self.pending_devices {
            match rx.try_recv() {
                Ok(devices) => {
                    self.devices = devices;
                    self.devices_host = Some(*host);
                    self.pending_devices = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending_devices = None,
            }
        }
    }

    /// Marks the MIDI port `name` active (`now`) or reports whether it still glows.
    pub(crate) fn activity(&mut self, name: &str, active: bool, now: Instant) -> bool {
        if active {
            self.lit.insert(name.to_owned(), now);
        }
        self.lit
            .get(name)
            .is_some_and(|t| now.saturating_duration_since(*t) < ACTIVITY_HOLD)
    }
}

impl crate::App {
    /// The S07 model from the settings, the engine, the MIDI inputs and the device host. Opening
    /// the dialog reads the drivers and refreshes the MIDI inputs; the device list is read again
    /// when the driver changes or "Erneut suchen" is pressed. System enumeration runs on a worker
    /// thread ([`AudioDevices::is_slow`]); until it arrives the dialog shows the lists it has.
    pub(crate) fn settings_model(&mut self, section: SettingsSection) -> Model {
        let config = self.settings.audio.clone();
        let opening = !self.dialogs.settings.open;
        if opening {
            self.dialogs.settings.pending_hosts =
                Some(enumerate(&self.audio_devices, |d| d.hosts()));
            self.dialogs.settings.devices_host = None;
            self.dialogs.settings.input_host = None;
            self.dialogs.settings.next_jack_refresh = None;
            self.dialogs.settings.scroll_to.get_or_insert(section);
            self.engine.scan_midi();
        }
        self.dialogs.settings.open = true;
        let cache = &mut self.dialogs.settings;
        let requested = cache.pending_devices.as_ref().map(|(host, _)| *host);
        if cache.devices_host != Some(config.host) && requested != Some(config.host) {
            let host = config.host;
            let rx = enumerate(&self.audio_devices, move |d| d.devices(host));
            cache.pending_devices = Some((host, rx));
        }
        if cache.input_host != Some(config.host)
            && cache.pending_inputs.as_ref().map(|(host, _)| *host) != Some(config.host)
        {
            let host = config.host;
            cache.pending_inputs = Some((
                host,
                enumerate(&self.audio_devices, move |provider| {
                    provider.input_devices(host)
                }),
            ));
        }
        if config.host == HostKind::Jack
            && cache.pending_jack_ports.is_none()
            && cache
                .next_jack_refresh
                .is_none_or(|next| Instant::now() >= next)
        {
            cache.pending_jack_ports = Some(enumerate(&self.audio_devices, |d| d.jack_ports()));
            cache.next_jack_refresh = Some(Instant::now() + JACK_REFRESH);
        }
        cache.collect();
        let t = self.lang;
        let (devices, device) = device_entries(t, config.host, &cache.devices, &config);
        let running = match self.engine.audio_state() {
            komponist_audio::AudioState::Running { config, .. } => Some(config.clone()),
            _ => None,
        };
        let rates = running
            .as_ref()
            .filter(|r| config.host == HostKind::Jack && r.host == HostKind::Jack)
            .map_or_else(
                || rates(config.host, &cache.devices, &devices[device], &config),
                |r| vec![r.sample_rate],
            );
        let buffer = buffer_limit(
            config.host,
            &cache.devices,
            &devices[device],
            running.as_ref(),
        );
        let (no_device, latency) = match self.engine.audio_state() {
            komponist_audio::AudioState::NoDevice { reason, .. } => (
                Some(reason.clone()),
                format::latency(t, config.buffer_frames, config.sample_rate, Duration::ZERO),
            ),
            komponist_audio::AudioState::Running {
                config: running,
                latency,
            } if running.host == config.host => (
                None,
                if running.host == HostKind::Jack
                    && (!running.jack_routes.is_automatic() || self.settings.audio_input.is_some())
                {
                    // Native JACK's playback range already includes the graph's device periods.
                    format::latency(t, 0, running.sample_rate, *latency)
                } else {
                    format::latency(t, running.buffer_frames, running.sample_rate, *latency)
                },
            ),
            _ => (
                None,
                format::latency(t, config.buffer_frames, config.sample_rate, Duration::ZERO),
            ),
        };
        let now = Instant::now();
        let midi = self.engine.midi().map(|hub| {
            let ports = hub.ports();
            let all: Vec<&str> = ports.iter().map(|p| p.name.as_str()).collect();
            ports
                .iter()
                .map(|p| MidiRow {
                    name: p.name.clone(),
                    label: midi_label(&p.name, &all),
                    enabled: p.enabled,
                    connected: p.connected,
                    active: cache.activity(&p.name, hub.take_activity(p.id), now),
                    busy: hub.port_busy(&p.name),
                })
                .collect()
        });
        let host = self.engine.host();
        let (input_devices, input_device) =
            input_entries(t, &cache.inputs, self.settings.audio_input.as_ref());
        Model {
            input_devices,
            input_device,
            jack_ports: cache.jack_ports.clone(),
            capture_format: self.settings.capture_format,
            section,
            hosts: cache.hosts.clone(),
            config: config.clone(),
            devices,
            device,
            rates,
            no_device,
            buffer,
            latency,
            realtime_denied: self.engine.stats().realtime_denied(),
            midi,
            dirs: dir_rows(&self.settings, host.last_scan()),
            scan: scan_summary(t, host.last_scan(), host.is_scanning(), SystemTime::now()),
            scanning: host.is_scanning(),
            ui_scale: self.settings.ui_scale,
            language: self.settings.language,
            keyboard_midi: self.ui.keyboard_midi,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// View
// ------------------------------------------------------------------------------------------------

/// What the user did.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// × , Esc or a click beside the dialog.
    Close,
    /// A navigation entry: highlight and scroll to it.
    Section(SettingsSection),
    /// A settings or UI action to dispatch.
    Act(Action),
    /// "+ Ordner": choose a folder.
    AddFolder,
}

/// Draws S07; `scroll_to` scrolls the page to a section in this frame.
pub fn show(ctx: &Context, model: &Model, scroll_to: Option<SettingsSection>) -> Vec<Output> {
    let t = komponist_ui::lang(ctx);
    let mut out = Vec::new();
    let size = super::fit(ctx, vec2(m::SIZE[0], m::SIZE[1]));
    let response = modal(ctx, "settings", color32(color::SURFACE_2), |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        if header(ui, size.x, t.settings_title()) {
            out.push(Output::Close);
        }
        let body = Rect::from_min_size(ui.cursor().min, vec2(size.x, size.y - dm::HEADER_HEIGHT));
        ui.allocate_rect(body, Sense::hover());
        let nav = Rect::from_min_size(body.min, vec2(m::NAV_WIDTH, body.height()));
        let content = Rect::from_min_max(pos2(nav.right() + 1.0, body.top()), body.max);
        // The content's bottom corners follow the dialog's radius (`overflow:hidden`).
        let r = radius::L as u8;
        ui.painter().rect_filled(
            content,
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: 0,
                se: r,
            },
            color32(color::SURFACE_1),
        );
        px::vline(
            ui.painter(),
            nav.right(),
            nav.y_range(),
            color32(color::SURFACE_0),
        );
        let mut nav_ui = ui.new_child(UiBuilder::new().max_rect(nav).id_salt("settings_nav"));
        navigation(&mut nav_ui, t, model.section, &mut out);
        let mut page = ui.new_child(
            UiBuilder::new()
                .max_rect(content)
                .id_salt("settings_page")
                .layout(egui::Layout::top_down(Align::Min)),
        );
        page.set_clip_rect(content);
        egui::ScrollArea::vertical()
            .id_salt("komponist_settings_scroll")
            .auto_shrink([false, false])
            .show(&mut page, |ui| {
                let [px_, py] = m::CONTENT_PADDING;
                ui.add_space(py);
                ui.horizontal_top(|ui| {
                    ui.add_space(px_);
                    ui.vertical(|ui| {
                        let width = content.width() - 2.0 * px_;
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        sections(ui, t, model, width, scroll_to, &mut out);
                    });
                });
                ui.add_space(py);
            });
    });
    if !out.contains(&Output::Close) && response.should_close() {
        out.push(Output::Close);
    }
    out
}

/// The navigation column (`setNav`): 28 px items, the active one on `accent.surface`.
fn navigation(ui: &mut Ui, t: Lang, active: SettingsSection, out: &mut Vec<Output>) {
    let [pad_x, pad_y] = m::NAV_PADDING;
    let rect = ui.max_rect();
    let mut y = rect.top() + pad_y;
    for section in SECTIONS {
        let label = section_name(t, section);
        let item = Rect::from_min_size(
            pos2(rect.left() + pad_x, y),
            vec2(rect.width() - 2.0 * pad_x, m::NAV_ITEM_HEIGHT),
        );
        y += m::NAV_ITEM_HEIGHT + m::NAV_GAP;
        let id = ui.scope_id().with(("komponist_settings_nav", label));
        let response = ui.interact(item, id, Sense::click());
        let on = section == active;
        response.widget_info(|| egui::WidgetInfo::selected(Role::Tab, true, on, label));
        let st = state::State::of(ui, &response);
        let fill = if on {
            Some(color32(color::ACCENT_SURFACE))
        } else if st.hovered {
            Some(state::surface_for(2, st))
        } else {
            None
        };
        if let Some(fill) = fill {
            px::fill(ui.painter(), item, radius::M, fill);
        }
        let font = if on {
            w::font_at(&typography::UI, wm::MEDIUM)
        } else {
            komponist_ui::fonts::font_id(&typography::UI)
        };
        let ink = if on { color::TEXT_1 } else { color::TEXT_2 };
        let g = galley(
            ui,
            label,
            font,
            typography::UI.line_height,
            color32(ink),
            None,
        );
        let pos = w::text_pos(
            item,
            &g,
            item.left() + m::NAV_ITEM_PADDING_X,
            ui.pixels_per_point(),
        );
        ui.painter().galley(pos, g, Color32::PLACEHOLDER);
        if st.focused {
            komponist_ui::focus::paint_ring(ui.painter(), item, radius::M);
        }
        if response.clicked() {
            out.push(Output::Section(section));
        }
    }
}

/// All sections of the page.
fn sections(
    ui: &mut Ui,
    t: Lang,
    model: &Model,
    width: f32,
    scroll_to: Option<SettingsSection>,
    out: &mut Vec<Output>,
) {
    let anchor = |ui: &mut Ui, section: SettingsSection| {
        if scroll_to == Some(section) {
            let top = Rect::from_min_size(ui.cursor().min, vec2(width, 1.0));
            ui.scroll_to_rect(
                top.expand2(vec2(0.0, m::CONTENT_PADDING[1])),
                Some(Align::TOP),
            );
        }
    };
    anchor(ui, SettingsSection::Audio);
    audio(ui, t, model, width, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Midi);
    midi(ui, t, model, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Plugins);
    plugins(ui, t, model, width, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Display);
    display(ui, t, model, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Keyboard);
    keyboard(ui, t, model, width, out);
}

fn gap_and_divider(ui: &mut Ui, width: f32) {
    ui.add_space(m::SECTION_GAP);
    divider(ui, width);
    ui.add_space(m::SECTION_GAP);
}

/// A grid row: label column (140, `type.ui`, `text.2`), then `add`.
fn grid_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = m::GRID_GAP[0];
            let g = styled(ui, label, &typography::UI, color32(color::TEXT_2), None);
            let (rect, _) =
                ui.allocate_exact_size(vec2(m::LABEL_COLUMN, size::CONTROL), Sense::hover());
            let pos = w::text_pos(rect, &g, rect.left(), ui.pixels_per_point());
            ui.painter().galley(pos, g, Color32::PLACEHOLDER);
            if !label.is_empty() {
                label_node(ui, rect, label, Role::Label);
            }
            add(ui)
        })
        .inner;
    ui.add_space(m::GRID_GAP[1]);
    r
}

/// The "ASIO Compatible" logo (white on transparent: the app has only the dark theme), as
/// Steinberg's ASIO usage guidelines require in every dialog that enables or configures ASIO.
/// The vendored SVG itself, untouched (`vendor/asio-sdk/VENDORED.md`).
const ASIO_LOGO_SVG: &[u8] = include_bytes!(
    "../../../../vendor/asio-sdk/Steinberg ASIO Logo Artwork/ASIO-compatible-logo-Steinberg-®-white-transparent-RGB.svg"
);

/// Draws the ASIO logo at the right edge of the rest of the driver row.
fn asio_logo(ui: &mut Ui, t: Lang) {
    let height = m::ASIO_LOGO_HEIGHT;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add(
            egui::Image::from_bytes("bytes://asio-compatible-logo.svg", ASIO_LOGO_SVG)
                .fit_to_exact_size(vec2(height * m::ASIO_LOGO_ASPECT, height))
                .alt_text(t.settings_asio_logo()),
        );
    });
}

fn capture_device(ui: &mut Ui, t: Lang, model: &Model, width: f32, out: &mut Vec<Output>) {
    let mut selected = model.input_device;
    let items: Vec<_> = model
        .input_devices
        .iter()
        .map(|entry| Item::new(&entry.label))
        .collect();
    grid_row(ui, t.settings_input_device(), |ui| {
        if ui
            .add(Dropdown::new(t.settings_input_device(), &mut selected, &items).width(width))
            .changed()
        {
            out.push(Output::Act(Action::Settings(
                SettingsAction::SetAudioInput(model.input_devices[selected].config.clone()),
            )));
        }
    });
}

fn capture_format(ui: &mut Ui, t: Lang, model: &Model, out: &mut Vec<Output>) {
    grid_row(ui, t.settings_capture_format(), |ui| {
        let items = [
            komponist_ui::widgets::tabs::Item::new(t.settings_capture_pcm24()),
            komponist_ui::widgets::tabs::Item::new(t.settings_capture_float32()),
        ];
        let mut selected =
            usize::from(model.capture_format == crate::settings::CaptureFormat::Float32);
        if ui
            .add(Segmented::new(
                t.settings_capture_format(),
                &mut selected,
                &items,
            ))
            .changed()
        {
            out.push(Output::Act(Action::Settings(
                SettingsAction::SetCaptureFormat(if selected == 0 {
                    crate::settings::CaptureFormat::Pcm24
                } else {
                    crate::settings::CaptureFormat::Float32
                }),
            )));
        }
    });
}

fn audio(ui: &mut Ui, t: Lang, model: &Model, width: f32, out: &mut Vec<Output>) {
    section_label(ui, t.settings_audio());
    ui.add_space(m::SECTION_INNER_GAP);
    let field_width = width - m::LABEL_COLUMN - m::GRID_GAP[0];
    let config = &model.config;

    let here = drivers_here();
    let drivers: Vec<Item<'_>> = here
        .iter()
        .map(|(host, label)| {
            let item = Item::new(label);
            if model.hosts.contains(host) {
                item
            } else {
                item.disabled()
            }
        })
        .collect();
    let mut driver = here
        .iter()
        .position(|(h, _)| *h == config.host)
        .unwrap_or(here.len());
    grid_row(ui, t.settings_driver(), |ui| {
        if ui
            .add(Segmented::new(t.settings_driver(), &mut driver, &drivers))
            .changed()
            && let Some((host, _)) = here.get(driver)
        {
            out.push(set_audio(StreamConfig {
                host: *host,
                device: None,
                device_id: None,
                ..config.clone()
            }));
        }
        if here.iter().any(|(h, _)| *h == HostKind::Asio) {
            asio_logo(ui, t);
        }
    });

    if config.host == HostKind::Jack {
        jack_playback(ui, t, model, field_width, out);
    } else {
        output_device(ui, t, model, field_width, out);
    }
    if model.no_device.is_some() {
        grid_row(ui, "", |ui| notice(ui, t, field_width, out));
    }
    if config.host != HostKind::Jack || config.jack_routes.capture.is_none() {
        capture_device(ui, t, model, field_width, out);
    }
    if config.host == HostKind::Jack {
        ui.ctx().request_repaint_after(JACK_REFRESH);
        jack_capture(ui, t, model, field_width, out);
    }
    capture_format(ui, t, model, out);

    let labels: Vec<String> = model.rates.iter().map(|r| format!("{r} Hz")).collect();
    let rate_items: Vec<Item<'_>> = labels.iter().map(|l| Item::new(l)).collect();
    let mut rate = model
        .rates
        .iter()
        .position(|r| *r == config.sample_rate)
        .unwrap_or(0);
    grid_row(ui, t.settings_rate(), |ui| {
        let r =
            ui.add(Dropdown::new(t.settings_rate(), &mut rate, &rate_items).width(m::RATE_WIDTH));
        if r.changed()
            && let Some(&sample_rate) = model.rates.get(rate)
        {
            out.push(set_audio(StreamConfig {
                sample_rate,
                ..config.clone()
            }));
        }
    });

    let buffer_labels: Vec<String> = BUFFERS.iter().map(u32::to_string).collect();
    let allowed = |size: u32| match model.buffer {
        BufferLimit::Free => true,
        BufferLimit::Range(min, max) => (min..=max).contains(&size),
        BufferLimit::Fixed(_) => false,
    };
    let buffer_items: Vec<Item<'_>> = buffer_labels
        .iter()
        .zip(BUFFERS)
        .map(|(l, size)| {
            let item = Item::new(l);
            if allowed(size) { item } else { item.disabled() }
        })
        .collect();
    // With one fixed size no segment is the stream's: the hint below names the real one.
    let mut buffer = BUFFERS
        .iter()
        .position(|b| *b == config.buffer_frames && !matches!(model.buffer, BufferLimit::Fixed(_)))
        .unwrap_or(BUFFERS.len());
    grid_row(ui, t.settings_buffer(), |ui| {
        ui.spacing_mut().item_spacing.x = m::LATENCY_GAP;
        if ui
            .add(Segmented::new(
                t.settings_buffer(),
                &mut buffer,
                &buffer_items,
            ))
            .changed()
            && let Some(&buffer_frames) = BUFFERS.get(buffer)
        {
            out.push(set_audio(StreamConfig {
                buffer_frames,
                ..config.clone()
            }));
        }
        let g = styled(
            ui,
            &model.latency,
            &super::metrics::MONO_SMALL,
            color32(color::TEXT_3),
            None,
        );
        super::paint_text(ui, g, &model.latency);
    });
    if let BufferLimit::Fixed(frames) = model.buffer {
        grid_row(ui, "", |ui| {
            let text = t.settings_buffer_fixed(frames);
            let g = styled(
                ui,
                &text,
                &typography::SMALL,
                color32(color::TEXT_3),
                Some(field_width),
            );
            super::paint_text(ui, g, &text);
        });
    }
    if model.realtime_denied {
        grid_row(ui, "", |ui| {
            let g = styled(
                ui,
                t.settings_no_realtime(),
                &typography::SMALL,
                color32(color::METER_LOUD),
                Some(field_width),
            );
            super::paint_text(ui, g, t.settings_no_realtime());
        });
    }
}

fn output_device(ui: &mut Ui, t: Lang, model: &Model, field_width: f32, out: &mut Vec<Output>) {
    let config = &model.config;
    let items: Vec<Item<'_>> = model.devices.iter().map(|d| Item::new(&d.label)).collect();
    let mut device = model.device;
    grid_row(ui, t.settings_device(), |ui| {
        let r = ui.add(Dropdown::new(t.settings_device(), &mut device, &items).width(field_width));
        if model.no_device.is_some() {
            // `box-shadow: inset 0 0 0 1px #ff5a5f` (`state.error`).
            px::inner_border(ui.painter(), r.rect, color32(color::STATE_ERROR));
        }
        if r.changed()
            && let Some(entry) = model.devices.get(device)
        {
            out.push(set_audio(StreamConfig {
                device: entry.name.clone(),
                device_id: entry.id.clone(),
                ..config.clone()
            }));
        }
    });
}

fn jack_playback(ui: &mut Ui, t: Lang, model: &Model, width: f32, out: &mut Vec<Output>) {
    let config = &model.config;
    let playback = &config.jack_routes.playback;
    let ports = model.jack_ports.as_deref();
    for (channel, label) in [
        t.settings_jack_playback_left(),
        t.settings_jack_playback_right(),
    ]
    .into_iter()
    .enumerate()
    {
        let other = playback[1 - channel].as_deref();
        let empty = if other.is_none() {
            t.settings_jack_automatic()
        } else {
            t.settings_jack_disconnected()
        };
        let (entries, selected) = jack_entries(
            t,
            ports,
            JackDirection::Playback,
            playback[channel].as_deref(),
            empty,
            |name| other != Some(name),
        );
        if let Some(port) = jack_dropdown(ui, label, width, &entries, selected) {
            let mut next = config.clone();
            next.jack_routes.playback[channel] = port;
            out.push(set_audio(next));
        }
    }
}

fn jack_capture(ui: &mut Ui, t: Lang, model: &Model, width: f32, out: &mut Vec<Output>) {
    let config = &model.config;
    let capture = config.jack_routes.capture.as_deref().unwrap_or_default();
    let ports = model.jack_ports.as_deref();
    for (channel, name) in capture.iter().enumerate() {
        let label = t.settings_jack_capture_channel(channel as u32 + 1);
        let (entries, selected) = jack_entries(
            t,
            ports,
            JackDirection::Capture,
            Some(name),
            t.settings_jack_remove_capture(),
            |_| true,
        );
        if let Some(port) = jack_dropdown(ui, &label, width, &entries, selected) {
            let mut next = config.clone();
            let list = next.jack_routes.capture.as_mut().expect("explicit capture");
            if let Some(port) = port {
                if let Some(other) = list.iter().position(|name| *name == port) {
                    list.swap(channel, other);
                } else {
                    list[channel] = port;
                }
            } else {
                list.remove(channel);
            }
            out.push(set_audio(next));
        }
    }
    if capture.len() < komponist_audio::input::MAX_INPUT_CHANNELS {
        let (entries, selected) = jack_entries(
            t,
            ports,
            JackDirection::Capture,
            None,
            t.settings_jack_add_capture(),
            |name| !capture.iter().any(|chosen| chosen == name),
        );
        if let Some(Some(port)) =
            jack_dropdown(ui, t.settings_jack_inputs(), width, &entries, selected)
        {
            let mut next = config.clone();
            next.jack_routes.capture.get_or_insert_default().push(port);
            out.push(set_audio(next));
        }
    }
    if config.jack_routes.capture.is_some() {
        grid_row(ui, "", |ui| {
            if ActionButton::dialog(t.settings_jack_auto_capture(), Kind::Secondary, 4)
                .show(ui)
                .clicked()
            {
                out.push(Output::Act(Action::Settings(
                    SettingsAction::SetJackCaptureAutomatic,
                )));
            }
        });
    }
}

struct JackPortEntry {
    name: Option<String>,
    label: String,
    available: bool,
}

fn jack_entries(
    t: Lang,
    ports: Option<&[JackPort]>,
    direction: JackDirection,
    saved: Option<&str>,
    unassigned_label: &str,
    include: impl Fn(&str) -> bool,
) -> (Vec<JackPortEntry>, usize) {
    let mut entries = vec![JackPortEntry {
        name: None,
        label: unassigned_label.to_owned(),
        available: true,
    }];
    entries.extend(
        ports
            .unwrap_or_default()
            .iter()
            .filter(|port| port.direction == direction && include(&port.name))
            .map(|port| JackPortEntry {
                name: Some(port.name.clone()),
                label: port.label(),
                available: true,
            }),
    );
    let selected = entries
        .iter()
        .position(|entry| entry.name.as_deref() == saved)
        .unwrap_or_else(|| {
            let saved = saved.expect("unassigned entry exists");
            entries.push(JackPortEntry {
                name: Some(saved.to_owned()),
                label: if ports.is_some() {
                    t.settings_jack_port_missing(saved)
                } else {
                    saved.to_owned()
                },
                available: ports.is_none(),
            });
            entries.len() - 1
        });
    (entries, selected)
}

fn jack_dropdown(
    ui: &mut Ui,
    label: &str,
    width: f32,
    entries: &[JackPortEntry],
    mut selected: usize,
) -> Option<Option<String>> {
    let items: Vec<_> = entries
        .iter()
        .map(|port| Item {
            label: &port.label,
            enabled: port.available,
        })
        .collect();
    let mut changed = None;
    grid_row(ui, label, |ui| {
        let response = ui.add(Dropdown::new(label, &mut selected, &items).width(width));
        if !entries[selected].available {
            px::inner_border(ui.painter(), response.rect, color32(color::STATE_ERROR));
        }
        if response.changed() {
            changed = Some(entries[selected].name.clone());
        }
    });
    changed
}

fn set_audio(config: StreamConfig) -> Output {
    Output::Act(Action::Settings(SettingsAction::SetAudio(config)))
}

/// "Gerät nicht verbunden …" with "Erneut suchen" (`#2c1c1e` box, red dot).
fn notice(ui: &mut Ui, t: Lang, width: f32, out: &mut Vec<Output>) {
    let [pad_x, pad_y] = m::NOTICE_PADDING;
    let button = ActionButton {
        height: Height::Control,
        ..ActionButton::dialog(t.settings_retry(), Kind::Secondary, 4)
    };
    // The button's width, to wrap the text beside it.
    let button_text = galley(
        ui,
        t.settings_retry(),
        komponist_ui::fonts::emphasis_font_id(&typography::UI),
        typography::UI.line_height,
        Color32::PLACEHOLDER,
        None,
    );
    let button_width = button_text.size().x.ceil() + 2.0 * wm::BUTTON_PADDING_X;
    let text_width = width - 2.0 * pad_x - m::DOT - 2.0 * m::NOTICE_GAP - button_width;
    let text = galley(
        ui,
        t.settings_no_device_notice(),
        komponist_ui::fonts::font_id(&typography::UI),
        m::NOTICE_LINE_HEIGHT,
        color32(color::TEXT_1),
        Some(text_width),
    );
    let height = text.size().y.max(size::CONTROL) + 2.0 * pad_y;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    px::fill(ui.painter(), rect, radius::M, super::error_surface());
    dot(
        ui,
        pos2(rect.left() + pad_x + m::DOT / 2.0, rect.center().y),
        m::DOT,
        color32(color::STATE_ERROR),
    );
    let text_pos = pos2(
        rect.left() + pad_x + m::DOT + m::NOTICE_GAP,
        rect.center().y - text.size().y / 2.0,
    );
    let text_rect = Rect::from_min_size(text_pos, text.size());
    ui.painter().galley(
        px::snap_rect(text_rect, ui.pixels_per_point()).min,
        text,
        Color32::PLACEHOLDER,
    );
    label_node(ui, text_rect, t.settings_no_device_notice(), Role::Label);
    let b = Rect::from_min_size(
        pos2(
            rect.right() - pad_x - button_width,
            rect.center().y - size::CONTROL / 2.0,
        ),
        vec2(button_width, size::CONTROL),
    );
    let mut child = ui.new_child(UiBuilder::new().max_rect(b));
    if button.show(&mut child).clicked() {
        out.push(Output::Act(Action::Settings(SettingsAction::RetryAudio)));
    }
}

fn midi(ui: &mut Ui, t: Lang, model: &Model, out: &mut Vec<Output>) {
    section_label(ui, t.settings_midi_inputs());
    ui.add_space(m::SECTION_INNER_GAP);
    let Some(rows) = &model.midi else {
        let g = styled(
            ui,
            t.settings_no_midi(),
            &typography::UI,
            color32(color::TEXT_3),
            None,
        );
        super::paint_text(ui, g, t.settings_no_midi());
        return;
    };
    if rows.is_empty() {
        let g = styled(
            ui,
            t.settings_no_midi(),
            &typography::UI,
            color32(color::TEXT_3),
            None,
        );
        super::paint_text(ui, g, t.settings_no_midi());
    }
    for row in rows {
        ui.horizontal(|ui| {
            ui.set_height(m::MIDI_ROW);
            ui.spacing_mut().item_spacing.x = m::MIDI_GAP;
            let mut on = row.enabled;
            if ui.add(Switch::new(&mut on, &row.label)).changed() {
                out.push(Output::Act(Action::Settings(
                    SettingsAction::SetMidiInput {
                        name: row.name.clone(),
                        enabled: on,
                    },
                )));
            }
            let ink = if row.enabled {
                color::TEXT_1
            } else {
                color::TEXT_2
            };
            let g = styled(ui, &row.label, &typography::UI, color32(ink), None);
            let (rect, _) =
                ui.allocate_exact_size(vec2(m::MIDI_NAME_WIDTH, m::MIDI_ROW), Sense::hover());
            let pos = w::text_pos(rect, &g, rect.left(), ui.pixels_per_point());
            ui.painter()
                .with_clip_rect(rect)
                .galley(pos, g, Color32::PLACEHOLDER);
            let (dot_rect, _) = ui.allocate_exact_size(vec2(m::DOT, m::MIDI_ROW), Sense::hover());
            let lit = if row.active {
                color::STATE_PLAY
            } else {
                color::SURFACE_4
            };
            dot(ui, dot_rect.center(), m::DOT, color32(lit));
            let info = row.info(t);
            let g = styled(ui, info, &typography::VALUE_S, color32(color::TEXT_3), None);
            super::paint_text(ui, g, info);
        });
    }
}

fn plugins(ui: &mut Ui, t: Lang, model: &Model, width: f32, out: &mut Vec<Output>) {
    ui.horizontal(|ui| {
        ui.set_width(width);
        section_label(ui, t.settings_plugin_dirs());
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = m::DIR_BUTTON_GAP;
            if ui.add(Button::new(t.settings_rescan())).clicked() {
                out.push(Output::Act(Action::Settings(SettingsAction::RescanPlugins)));
            }
            if ui.add(Button::new(t.settings_add_folder())).clicked() {
                out.push(Output::AddFolder);
            }
        });
    });
    ui.add_space(m::SECTION_INNER_GAP);
    for row in &model.dirs {
        dir_row(ui, t, width, row, out);
        ui.add_space(super::metrics::missing::LIST_GAP);
    }
    ui.add_space(m::SECTION_INNER_GAP - super::metrics::missing::LIST_GAP);
    let g = styled(
        ui,
        &model.scan,
        &typography::SMALL,
        color32(color::TEXT_3),
        Some(width),
    );
    super::paint_text(ui, g, &model.scan);
}

/// A folder row (`pluginDirs`): format, path, count; added folders also "×".
fn dir_row(ui: &mut Ui, t: Lang, width: f32, row: &DirRow, out: &mut Vec<Output>) {
    let ppp = ui.pixels_per_point();
    let (rect, _) = ui.allocate_exact_size(vec2(width, m::DIR_ROW), Sense::hover());
    px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_2));
    let inner = rect.shrink2(vec2(m::DIR_PADDING_X, 0.0));
    let format = if row.clap { "CLAP" } else { "VST3" };
    let g = styled(
        ui,
        format,
        &typography::VALUE_S,
        color32(color::TEXT_3),
        None,
    );
    ui.painter().galley(
        w::text_pos(inner, &g, inner.left(), ppp),
        g,
        Color32::PLACEHOLDER,
    );
    let mut right = inner.right();
    if row.removable {
        let b = Rect::from_min_size(
            pos2(
                right - size::CONTROL_COMPACT,
                rect.center().y - size::CONTROL_COMPACT / 2.0,
            ),
            vec2(size::CONTROL_COMPACT, size::CONTROL_COMPACT),
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(b));
        let remove = Button::icon(Icon::X, t.settings_remove_folder()).frameless(2);
        if child.add_sized(b.size(), remove).clicked() {
            out.push(Output::Act(Action::Settings(
                SettingsAction::RemovePluginDir {
                    clap: row.clap,
                    path: row.path.clone(),
                },
            )));
        }
        right = b.left() - m::DIR_GAP;
    }
    let count = row.count.map_or_else(|| "–".to_owned(), |n| n.to_string());
    let g = styled(
        ui,
        &count,
        &super::metrics::MONO_SMALL,
        color32(color::TEXT_3),
        None,
    );
    let count_x = right - g.size().x;
    ui.painter().galley(
        w::text_pos(inner, &g, count_x, ppp),
        g,
        Color32::PLACEHOLDER,
    );
    let path_x = inner.left() + m::DIR_FORMAT_COLUMN + m::DIR_GAP;
    let g = super::styled_line(
        ui,
        &row.label,
        &typography::VALUE_M,
        color32(color::TEXT_1),
        count_x - m::DIR_GAP - path_x,
    );
    let clip = Rect::from_x_y_ranges(path_x..=count_x - m::DIR_GAP, rect.y_range());
    ui.painter().with_clip_rect(clip).galley(
        w::text_pos(inner, &g, path_x, ppp),
        g,
        Color32::PLACEHOLDER,
    );
    let name = format!("{format} {} · {count}", row.label);
    label_node(ui, rect, &name, Role::ListItem);
}

fn display(ui: &mut Ui, t: Lang, model: &Model, out: &mut Vec<Output>) {
    section_label(ui, t.settings_display());
    ui.add_space(m::SECTION_INNER_GAP);
    let labels: Vec<String> = SCALES.iter().map(|s| scale_label(t, *s)).collect();
    let items: Vec<Item<'_>> = labels.iter().map(|l| Item::new(l)).collect();
    let mut scale = SCALES
        .iter()
        .position(|s| *s == model.ui_scale)
        .unwrap_or(0);
    grid_row(ui, t.settings_scale(), |ui| {
        if ui
            .add(Segmented::new(t.settings_scale(), &mut scale, &items))
            .changed()
            && let Some(value) = SCALES.get(scale)
        {
            out.push(Output::Act(Action::Settings(SettingsAction::SetUiScale(
                *value,
            ))));
        }
    });
    language_row(ui, t, model.language, out);
}

/// The UI language: System / Deutsch / English, applied at once.
fn language_row(ui: &mut Ui, t: Lang, current: Language, out: &mut Vec<Output>) {
    let labels = [
        t.settings_language_system(),
        t.settings_language_de(),
        t.settings_language_en(),
    ];
    let items: Vec<Item<'_>> = labels.iter().map(|l| Item::new(l)).collect();
    let mut selected = LANGUAGES.iter().position(|l| *l == current).unwrap_or(0);
    grid_row(ui, t.settings_language(), |ui| {
        if ui
            .add(Segmented::new(t.settings_language(), &mut selected, &items))
            .changed()
            && let Some(language) = LANGUAGES.get(selected)
        {
            out.push(Output::Act(Action::Settings(SettingsAction::SetLanguage(
                *language,
            ))));
        }
    });
}

fn keyboard(ui: &mut Ui, t: Lang, model: &Model, width: f32, out: &mut Vec<Output>) {
    section_label(ui, t.settings_keyboard());
    ui.add_space(m::SECTION_INNER_GAP);
    ui.horizontal(|ui| {
        ui.set_height(m::MIDI_ROW);
        ui.spacing_mut().item_spacing.x = m::MIDI_GAP;
        let mut on = model.keyboard_midi;
        if ui
            .add(Switch::new(&mut on, t.settings_keyboard_midi()))
            .changed()
        {
            out.push(Output::Act(Action::Ui(UiAction::ToggleKeyboardMidi)));
        }
        let g = styled(
            ui,
            t.settings_keyboard_midi(),
            &typography::UI,
            color32(color::TEXT_1),
            None,
        );
        super::paint_text(ui, g, t.settings_keyboard_midi());
    });
    let g = styled(
        ui,
        t.settings_keyboard_hint(),
        &typography::SMALL,
        color32(color::TEXT_3),
        Some(width),
    );
    super::paint_text(ui, g, t.settings_keyboard_hint());
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_audio::DeviceId;

    fn device(host: HostKind, name: &str, id: &str, default: bool) -> DeviceInfo {
        DeviceInfo {
            host,
            id: Some(DeviceId(id.to_owned())),
            name: name.to_owned(),
            is_default: default,
            channels: 2,
            sample_rates: vec![48_000],
            buffer_range: Some((64, 1024)),
        }
    }

    /// What this machine's PipeWire listed in WP-13 and WP-52.
    fn pipewire() -> Vec<DeviceInfo> {
        vec![
            device(
                HostKind::PipeWire,
                "default_sink",
                "pipewire:sink_default",
                false,
            ),
            device(
                HostKind::PipeWire,
                "default_output",
                "pipewire:output_default",
                true,
            ),
            device(
                HostKind::PipeWire,
                "Starship/Matisse HD Audio Controller Analoges Stereo",
                "pipewire:alsa_output.pci-0000_09_00.4.analog-stereo",
                false,
            ),
            device(HostKind::PipeWire, "unknown", "pipewire:Firefox", false),
        ]
    }

    #[test]
    fn pseudo_devices_become_one_default_entry() {
        let config = StreamConfig::default();
        let (entries, selected) =
            device_entries(Lang::De, HostKind::PipeWire, &pipewire(), &config);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Standardgerät",
                "Starship/Matisse HD Audio Controller Analoges Stereo"
            ]
        );
        assert_eq!(selected, 0);
        assert_eq!(
            (entries[0].name.as_ref(), entries[0].id.as_ref()),
            (None, None)
        );
    }

    #[test]
    fn the_stored_device_is_found_by_id_then_name_or_shown_missing() {
        let starship = "pipewire:alsa_output.pci-0000_09_00.4.analog-stereo";
        let config = StreamConfig {
            host: HostKind::PipeWire,
            device: Some("Alter Name".into()),
            device_id: Some(DeviceId(starship.into())),
            ..StreamConfig::default()
        };
        let (entries, i) = device_entries(Lang::De, HostKind::PipeWire, &pipewire(), &config);
        assert_eq!(entries[i].id.as_ref().unwrap().0, starship);
        // Unplugged: shown under its stored name, selected, with its id kept.
        let config = StreamConfig {
            host: HostKind::PipeWire,
            device: Some("Focusrite Scarlett 2i2".into()),
            device_id: Some(DeviceId("pipewire:alsa_output.usb-Focusrite".into())),
            ..StreamConfig::default()
        };
        let (entries, i) = device_entries(Lang::De, HostKind::PipeWire, &pipewire(), &config);
        assert_eq!(i, entries.len() - 1);
        assert_eq!(entries[i].label, "Focusrite Scarlett 2i2");
        assert_eq!(
            entries[i].id.as_ref().unwrap().0,
            "pipewire:alsa_output.usb-Focusrite"
        );
        // Another driver's list starts at the default.
        assert_eq!(device_entries(Lang::De, HostKind::Alsa, &[], &config).1, 0);
    }

    #[test]
    fn rates_include_the_configured_one_and_pipewire_conversions() {
        let devices = pipewire();
        let (entries, _) = device_entries(
            Lang::De,
            HostKind::PipeWire,
            &devices,
            &StreamConfig::default(),
        );
        let config = StreamConfig {
            sample_rate: 88_200,
            ..StreamConfig::default()
        };
        assert_eq!(
            rates(HostKind::PipeWire, &devices, &entries[0], &config),
            [44_100, 48_000, 88_200, 96_000]
        );
        let alsa = vec![DeviceInfo {
            sample_rates: vec![44_100, 48_000, 96_000, 192_000],
            ..device(HostKind::Alsa, "HDA", "alsa:hw:0", false)
        }];
        let entry = DeviceEntry {
            label: "HDA".into(),
            name: Some("HDA".into()),
            id: Some(DeviceId("alsa:hw:0".into())),
        };
        assert_eq!(
            rates(HostKind::Alsa, &alsa, &entry, &StreamConfig::default()),
            [44_100, 48_000, 96_000, 192_000]
        );
    }

    #[test]
    fn coreaudio_offers_only_the_rates_of_the_device() {
        let devices = vec![DeviceInfo {
            sample_rates: vec![44_100, 48_000, 96_000],
            ..device(HostKind::CoreAudio, "Speakers", "coreaudio:spk", true)
        }];
        let entry = DeviceEntry {
            label: "Speakers".into(),
            name: Some("Speakers".into()),
            id: Some(DeviceId("coreaudio:spk".into())),
        };
        let config = StreamConfig {
            host: HostKind::CoreAudio,
            ..StreamConfig::default()
        };
        assert_eq!(
            rates(HostKind::CoreAudio, &devices, &entry, &config),
            [44_100, 48_000, 96_000]
        );
    }

    #[test]
    fn the_driver_list_has_only_drivers_of_this_system() {
        let here = drivers_here();
        assert!(here.iter().all(|(h, _)| h.is_supported_here()));
        assert!(here.contains(&(
            HostKind::platform_default(),
            if cfg!(target_os = "macos") {
                "CoreAudio"
            } else if cfg!(windows) {
                "WASAPI"
            } else {
                "PipeWire"
            }
        )));
        // ASIO is a driver of Windows builds with the feature `asio` only.
        assert_eq!(
            here.iter().any(|(h, _)| *h == HostKind::Asio),
            HostKind::Asio.is_supported_here()
        );
        assert!(DRIVERS.iter().all(|(h, _)| *h != HostKind::Null));
    }

    #[test]
    fn a_lone_midi_1_suffix_is_cut() {
        let all = ["Arturia KeyStep 37 MIDI 1", "Midi Through Port-0"];
        assert_eq!(midi_label(all[0], &all), "Arturia KeyStep 37");
        assert_eq!(midi_label(all[1], &all), "Midi Through Port-0");
        let two = ["Launchkey MIDI 1", "Launchkey MIDI 2"];
        assert_eq!(midi_label(two[0], &two), "Launchkey MIDI 1");
        let row = MidiRow {
            name: all[0].into(),
            label: "Arturia KeyStep 37".into(),
            enabled: true,
            connected: true,
            active: false,
            busy: false,
        };
        assert_eq!(row.info(Lang::De), "aktiv · Kanal 1–16");
        assert_eq!(
            MidiRow {
                busy: true,
                ..row.clone()
            }
            .info(Lang::De),
            "belegt · ein anderes Programm nutzt den Eingang"
        );
        assert_eq!(
            MidiRow {
                enabled: false,
                ..row.clone()
            }
            .info(Lang::De),
            "aus"
        );
    }

    #[test]
    fn folders_get_their_format_from_their_bundles() {
        let dir = std::env::temp_dir().join(format!("komponist-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (sub, bundles) in [
            ("a", &["x.clap"][..]),
            ("b", &["y.vst3"][..]),
            ("c", &["x.clap", "y.vst3"][..]),
            ("my-vst3", &[][..]),
            ("leer", &[][..]),
        ] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            for b in bundles {
                std::fs::create_dir_all(dir.join(sub).join(b)).unwrap();
            }
        }
        assert_eq!(folder_formats(&dir.join("a")), (true, false));
        assert_eq!(folder_formats(&dir.join("b")), (false, true));
        assert_eq!(folder_formats(&dir.join("c")), (true, true));
        assert_eq!(folder_formats(&dir.join("my-vst3")), (false, true));
        assert_eq!(folder_formats(&dir.join("leer")), (true, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_summary_reads_like_s07() {
        use komponist_plugins::ScanReport;
        use komponist_plugins::catalog::SkippedBundle;
        let now = SystemTime::now();
        let report = ScanReport {
            found: 214,
            per_dir: Vec::new(),
            skipped: vec![SkippedBundle {
                path: "/x.clap".into(),
                reason: "crash".into(),
            }],
            finished: now,
        };
        let text = scan_summary(Lang::De, Some(&report), false, now);
        assert!(
            text.starts_with("214 Plugins gefunden · letzter Scan heute "),
            "{text}"
        );
        assert!(text.ends_with(" · 1 Plugin übersprungen (lässt sich nicht laden)"));
        assert_eq!(
            scan_summary(Lang::De, Some(&report), true, now),
            "Scan läuft …"
        );
        assert_eq!(scan_summary(Lang::De, None, false, now), "Noch kein Scan");
    }
}
