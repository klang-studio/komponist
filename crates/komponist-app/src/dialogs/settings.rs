//! S07 "Einstellungen" (`Screens.dc.html` `ovSettings`, `setNav`, `buffers`, `sw`, `midiIns`,
//! `pluginDirs`; notes `S07`). Strg , opens it; Esc closes; changes apply at once.
//!
//! The navigation (Audio, MIDI, Plugins, Darstellung, Tastatur) scrolls the page to its section;
//! the page shows them all, as S07 shows "Audio", "MIDI-Eingänge" and "Plugin-Ordner" together.
//!
//! - **Audio:** driver (PipeWire, JACK, ALSA; unavailable ones disabled), output device, sample
//!   rate, buffer size with the latency next to it. A change rebuilds the stream at once, the
//!   engine stays (`SettingsAction::SetAudio`). The device list shows one "Standardgerät" (the
//!   driver's default) instead of PipeWire's pseudo devices (`default_sink`, `default_output`),
//!   hides PipeWire application streams (named "unknown") and keeps the stored device id
//!   ([`device_entries`]). Without a device: red ring on the device field and the notice
//!   "Gerät nicht verbunden …" with "Erneut suchen". A hint says when the audio thread runs
//!   without real-time priority.
//! - **MIDI-Eingänge:** a switch per input, the activity dot lights on incoming messages; a lone
//!   " MIDI 1" suffix is cut ([`midi_label`]).
//! - **Plugin-Ordner:** the folders per format with their plugin count, "+ Ordner", "Neu scannen"
//!   (background) and the summary of the last scan (folder rules: [`crate::settings`]).
//! - **Darstellung:** UI scale. **Tastatur:** computer keyboard MIDI on or off.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use egui::{Align, Color32, Context, Rect, Role, Sense, Ui, UiBuilder, pos2, vec2};
use komponist_audio::{AudioSystem, DeviceInfo, HostKind, StreamConfig};
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
use crate::ui_state::SettingsSection;

/// Texts of S07.
pub mod names {
    /// Dialog title.
    pub const TITLE: &str = "Einstellungen";
    /// Section "Audio".
    pub const AUDIO: &str = "Audio";
    /// Section "MIDI-Eingänge".
    pub const MIDI_INPUTS: &str = "MIDI-Eingänge";
    /// Section "Plugin-Ordner".
    pub const PLUGIN_DIRS: &str = "Plugin-Ordner";
    /// Section "Darstellung".
    pub const DISPLAY: &str = "Darstellung";
    /// Section "Tastatur".
    pub const KEYBOARD: &str = "Tastatur";
    /// "Treiber".
    pub const DRIVER: &str = "Treiber";
    /// "Ausgabegerät".
    pub const DEVICE: &str = "Ausgabegerät";
    /// "Abtastrate".
    pub const RATE: &str = "Abtastrate";
    /// "Puffergröße".
    pub const BUFFER: &str = "Puffergröße";
    /// The driver's default device.
    pub const DEFAULT_DEVICE: &str = "Standardgerät";
    /// Notice without a device.
    pub const NO_DEVICE_NOTICE: &str = "Gerät nicht verbunden. Wiedergabe und Aufnahme sind \
                                        angehalten, das Projekt bleibt bearbeitbar.";
    /// "Erneut suchen".
    pub const RETRY: &str = "Erneut suchen";
    /// Hint when the audio thread got no real-time priority.
    pub const NO_REALTIME: &str = "Audio läuft ohne Echtzeit-Priorität; Aussetzer sind möglich. \
                                   RTKit oder die Gruppe „audio“ erlauben sie.";
    /// "+ Ordner".
    pub const ADD_FOLDER: &str = "+ Ordner";
    /// "Neu scannen".
    pub const RESCAN: &str = "Neu scannen";
    /// Remove an added plugin folder.
    pub const REMOVE_FOLDER: &str = "Ordner entfernen";
    /// No MIDI system.
    pub const NO_MIDI: &str = "Keine MIDI-Eingänge gefunden";
    /// "UI-Skalierung".
    pub const SCALE: &str = "UI-Skalierung";
    /// Computer keyboard MIDI switch.
    pub const KEYBOARD_MIDI: &str = "Computer-Tastatur spielt Noten";
    /// Its hint (prototype start screen "Tastatur").
    pub const KEYBOARD_HINT: &str = "A W S E D F T G Z H U J K spielen Noten, sobald eine Spur \
                                     scharf ist oder dieser Schalter an ist.";
}

/// Buffer sizes of the segments (`buffers`).
pub const BUFFERS: [u32; 5] = [64, 128, 256, 512, 1024];

/// UI scales of the "Darstellung" segments; `None` follows the display.
pub const SCALES: [(Option<f32>, &str); 5] = [
    (None, "Automatisch"),
    (Some(1.0), "100 %"),
    (Some(1.25), "125 %"),
    (Some(1.5), "150 %"),
    (Some(2.0), "200 %"),
];

/// The navigation entries in order (`setNav`).
pub const SECTIONS: [(SettingsSection, &str); 5] = [
    (SettingsSection::Audio, "Audio"),
    (SettingsSection::Midi, "MIDI"),
    (SettingsSection::Plugins, "Plugins"),
    (SettingsSection::Display, "Darstellung"),
    (SettingsSection::Keyboard, "Tastatur"),
];

/// The drivers of the "Treiber" segments.
pub const DRIVERS: [(HostKind, &str); 3] = [
    (HostKind::PipeWire, "PipeWire"),
    (HostKind::Jack, "JACK"),
    (HostKind::Alsa, "ALSA"),
];

/// How long the MIDI activity dot stays lit after a message.
const ACTIVITY_HOLD: Duration = Duration::from_millis(150);

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
    host: HostKind,
    devices: &[DeviceInfo],
    config: &StreamConfig,
) -> (Vec<DeviceEntry>, usize) {
    let mut entries = vec![DeviceEntry {
        label: names::DEFAULT_DEVICE.to_owned(),
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

/// The sample rates offered for `entry` on `host`: the device's own, the configured one, and on
/// PipeWire also 44.1, 48 and 96 kHz (PipeWire converts). Ascending.
pub fn rates(
    host: HostKind,
    devices: &[DeviceInfo],
    entry: &DeviceEntry,
    config: &StreamConfig,
) -> Vec<u32> {
    let device = match (&entry.id, &entry.name) {
        (None, None) => devices.iter().find(|d| d.is_default),
        (Some(id), _) => devices.iter().find(|d| d.id.as_ref() == Some(id)),
        (None, Some(name)) => devices.iter().find(|d| d.name == *name),
    };
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
}

impl MidiRow {
    /// The info text after the dot (`midiIns` `info`).
    pub fn info(&self) -> &'static str {
        match (self.enabled, self.connected) {
            (false, _) => "aus",
            (true, true) => "aktiv · Kanal 1–16",
            (true, false) => "nicht verbunden",
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
    /// Device dropdown entries ([`device_entries`]).
    pub devices: Vec<DeviceEntry>,
    /// The configured device's entry.
    pub device: usize,
    /// Sample rates ([`rates`]).
    pub rates: Vec<u32>,
    /// Why there is no device, while there is none.
    pub no_device: Option<String>,
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
    /// Computer keyboard MIDI switched on.
    pub keyboard_midi: bool,
}

/// The summary line under the plugin folders.
pub fn scan_summary(
    report: Option<&komponist_plugins::ScanReport>,
    scanning: bool,
    now: SystemTime,
) -> String {
    if scanning {
        return "Scan läuft …".to_owned();
    }
    let Some(report) = report else {
        return "Noch kein Scan".to_owned();
    };
    let mut text = format::plural(
        report.found as i64,
        "1 Plugin gefunden",
        "{} Plugins gefunden",
    );
    if let (Some(then), Some(now)) = (format::local(report.finished), format::local(now)) {
        text.push_str(&format!(
            " · letzter Scan {}",
            format::day_and_time(&then, &now)
        ));
    }
    if !report.skipped.is_empty() {
        text.push_str(" · ");
        text.push_str(&format::plural(
            report.skipped.len() as i64,
            "1 Plugin übersprungen (lässt sich nicht laden)",
            "{} Plugins übersprungen (lassen sich nicht laden)",
        ));
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
    /// When each MIDI port (by name) last had activity.
    pub(crate) lit: HashMap<String, Instant>,
    /// Scroll the page to this section in the next frame.
    pub(crate) scroll_to: Option<SettingsSection>,
}

impl Cache {
    /// Whether an enumeration is still running (the dialog repaints until it arrives).
    pub(crate) fn enumerating(&self) -> bool {
        self.pending_hosts.is_some() || self.pending_devices.is_some()
    }

    /// Takes finished enumerations.
    fn collect(&mut self) {
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
        cache.collect();
        let (devices, device) = device_entries(config.host, &cache.devices, &config);
        let rates = rates(config.host, &cache.devices, &devices[device], &config);
        let (no_device, latency) = match self.engine.audio_state() {
            komponist_audio::AudioState::NoDevice { reason, .. } => (
                Some(reason.clone()),
                format::latency(config.buffer_frames, config.sample_rate, Duration::ZERO),
            ),
            komponist_audio::AudioState::Running {
                config: running,
                latency,
            } if running.host == config.host => (
                None,
                format::latency(running.buffer_frames, running.sample_rate, *latency),
            ),
            _ => (
                None,
                format::latency(config.buffer_frames, config.sample_rate, Duration::ZERO),
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
                })
                .collect()
        });
        let host = self.engine.host();
        Model {
            section,
            hosts: cache.hosts.clone(),
            config: config.clone(),
            devices,
            device,
            rates,
            no_device,
            latency,
            realtime_denied: self.engine.stats().realtime_denied(),
            midi,
            dirs: dir_rows(&self.settings, host.last_scan()),
            scan: scan_summary(host.last_scan(), host.is_scanning(), SystemTime::now()),
            scanning: host.is_scanning(),
            ui_scale: self.settings.ui_scale,
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
    let mut out = Vec::new();
    let size = super::fit(ctx, vec2(m::SIZE[0], m::SIZE[1]));
    let response = modal(ctx, "settings", color32(color::SURFACE_2), |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        if header(ui, size.x, names::TITLE) {
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
        navigation(&mut nav_ui, model.section, &mut out);
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
                        sections(ui, model, width, scroll_to, &mut out);
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
fn navigation(ui: &mut Ui, active: SettingsSection, out: &mut Vec<Output>) {
    let [pad_x, pad_y] = m::NAV_PADDING;
    let rect = ui.max_rect();
    let mut y = rect.top() + pad_y;
    for (section, label) in SECTIONS {
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
    audio(ui, model, width, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Midi);
    midi(ui, model, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Plugins);
    plugins(ui, model, width, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Display);
    display(ui, model, out);
    gap_and_divider(ui, width);
    anchor(ui, SettingsSection::Keyboard);
    keyboard(ui, model, width, out);
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

fn audio(ui: &mut Ui, model: &Model, width: f32, out: &mut Vec<Output>) {
    section_label(ui, names::AUDIO);
    ui.add_space(m::SECTION_INNER_GAP);
    let field_width = width - m::LABEL_COLUMN - m::GRID_GAP[0];
    let config = &model.config;

    let drivers: Vec<Item<'_>> = DRIVERS
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
    let mut driver = DRIVERS
        .iter()
        .position(|(h, _)| *h == config.host)
        .unwrap_or(DRIVERS.len());
    grid_row(ui, names::DRIVER, |ui| {
        if ui
            .add(Segmented::new(names::DRIVER, &mut driver, &drivers))
            .changed()
            && let Some((host, _)) = DRIVERS.get(driver)
        {
            out.push(set_audio(StreamConfig {
                host: *host,
                device: None,
                device_id: None,
                ..config.clone()
            }));
        }
    });

    let items: Vec<Item<'_>> = model.devices.iter().map(|d| Item::new(&d.label)).collect();
    let mut device = model.device;
    grid_row(ui, names::DEVICE, |ui| {
        let r = ui.add(Dropdown::new(names::DEVICE, &mut device, &items).width(field_width));
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
    if model.no_device.is_some() {
        grid_row(ui, "", |ui| notice(ui, field_width, out));
    }

    let labels: Vec<String> = model.rates.iter().map(|r| format!("{r} Hz")).collect();
    let rate_items: Vec<Item<'_>> = labels.iter().map(|l| Item::new(l)).collect();
    let mut rate = model
        .rates
        .iter()
        .position(|r| *r == config.sample_rate)
        .unwrap_or(0);
    grid_row(ui, names::RATE, |ui| {
        let r = ui.add(Dropdown::new(names::RATE, &mut rate, &rate_items).width(m::RATE_WIDTH));
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
    let buffer_items: Vec<Item<'_>> = buffer_labels.iter().map(|l| Item::new(l)).collect();
    let mut buffer = BUFFERS
        .iter()
        .position(|b| *b == config.buffer_frames)
        .unwrap_or(BUFFERS.len());
    grid_row(ui, names::BUFFER, |ui| {
        ui.spacing_mut().item_spacing.x = m::LATENCY_GAP;
        if ui
            .add(Segmented::new(names::BUFFER, &mut buffer, &buffer_items))
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
    if model.realtime_denied {
        grid_row(ui, "", |ui| {
            let g = styled(
                ui,
                names::NO_REALTIME,
                &typography::SMALL,
                color32(color::METER_LOUD),
                Some(field_width),
            );
            super::paint_text(ui, g, names::NO_REALTIME);
        });
    }
}

fn set_audio(config: StreamConfig) -> Output {
    Output::Act(Action::Settings(SettingsAction::SetAudio(config)))
}

/// "Gerät nicht verbunden …" with "Erneut suchen" (`#2c1c1e` box, red dot).
fn notice(ui: &mut Ui, width: f32, out: &mut Vec<Output>) {
    let [pad_x, pad_y] = m::NOTICE_PADDING;
    let button = ActionButton {
        height: Height::Control,
        ..ActionButton::dialog(names::RETRY, Kind::Secondary, 4)
    };
    // The button's width, to wrap the text beside it.
    let button_text = galley(
        ui,
        names::RETRY,
        komponist_ui::fonts::emphasis_font_id(&typography::UI),
        typography::UI.line_height,
        Color32::PLACEHOLDER,
        None,
    );
    let button_width = button_text.size().x.ceil() + 2.0 * wm::BUTTON_PADDING_X;
    let text_width = width - 2.0 * pad_x - m::DOT - 2.0 * m::NOTICE_GAP - button_width;
    let text = galley(
        ui,
        names::NO_DEVICE_NOTICE,
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
    label_node(ui, text_rect, names::NO_DEVICE_NOTICE, Role::Label);
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

fn midi(ui: &mut Ui, model: &Model, out: &mut Vec<Output>) {
    section_label(ui, names::MIDI_INPUTS);
    ui.add_space(m::SECTION_INNER_GAP);
    let Some(rows) = &model.midi else {
        let g = styled(
            ui,
            names::NO_MIDI,
            &typography::UI,
            color32(color::TEXT_3),
            None,
        );
        super::paint_text(ui, g, names::NO_MIDI);
        return;
    };
    if rows.is_empty() {
        let g = styled(
            ui,
            names::NO_MIDI,
            &typography::UI,
            color32(color::TEXT_3),
            None,
        );
        super::paint_text(ui, g, names::NO_MIDI);
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
            let info = row.info();
            let g = styled(ui, info, &typography::VALUE_S, color32(color::TEXT_3), None);
            super::paint_text(ui, g, info);
        });
    }
}

fn plugins(ui: &mut Ui, model: &Model, width: f32, out: &mut Vec<Output>) {
    ui.horizontal(|ui| {
        ui.set_width(width);
        section_label(ui, names::PLUGIN_DIRS);
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = m::DIR_BUTTON_GAP;
            if ui.add(Button::new(names::RESCAN)).clicked() {
                out.push(Output::Act(Action::Settings(SettingsAction::RescanPlugins)));
            }
            if ui.add(Button::new(names::ADD_FOLDER)).clicked() {
                out.push(Output::AddFolder);
            }
        });
    });
    ui.add_space(m::SECTION_INNER_GAP);
    for row in &model.dirs {
        dir_row(ui, width, row, out);
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
fn dir_row(ui: &mut Ui, width: f32, row: &DirRow, out: &mut Vec<Output>) {
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
        let remove = Button::icon(Icon::X, names::REMOVE_FOLDER).frameless(2);
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

fn display(ui: &mut Ui, model: &Model, out: &mut Vec<Output>) {
    section_label(ui, names::DISPLAY);
    ui.add_space(m::SECTION_INNER_GAP);
    let items: Vec<Item<'_>> = SCALES.iter().map(|(_, l)| Item::new(l)).collect();
    let mut scale = SCALES
        .iter()
        .position(|(s, _)| *s == model.ui_scale)
        .unwrap_or(0);
    grid_row(ui, names::SCALE, |ui| {
        if ui
            .add(Segmented::new(names::SCALE, &mut scale, &items))
            .changed()
            && let Some((value, _)) = SCALES.get(scale)
        {
            out.push(Output::Act(Action::Settings(SettingsAction::SetUiScale(
                *value,
            ))));
        }
    });
}

fn keyboard(ui: &mut Ui, model: &Model, width: f32, out: &mut Vec<Output>) {
    section_label(ui, names::KEYBOARD);
    ui.add_space(m::SECTION_INNER_GAP);
    ui.horizontal(|ui| {
        ui.set_height(m::MIDI_ROW);
        ui.spacing_mut().item_spacing.x = m::MIDI_GAP;
        let mut on = model.keyboard_midi;
        if ui.add(Switch::new(&mut on, names::KEYBOARD_MIDI)).changed() {
            out.push(Output::Act(Action::Ui(UiAction::ToggleKeyboardMidi)));
        }
        let g = styled(
            ui,
            names::KEYBOARD_MIDI,
            &typography::UI,
            color32(color::TEXT_1),
            None,
        );
        super::paint_text(ui, g, names::KEYBOARD_MIDI);
    });
    let g = styled(
        ui,
        names::KEYBOARD_HINT,
        &typography::SMALL,
        color32(color::TEXT_3),
        Some(width),
    );
    super::paint_text(ui, g, names::KEYBOARD_HINT);
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
        let (entries, selected) = device_entries(HostKind::PipeWire, &pipewire(), &config);
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
            device: Some("Alter Name".into()),
            device_id: Some(DeviceId(starship.into())),
            ..StreamConfig::default()
        };
        let (entries, i) = device_entries(HostKind::PipeWire, &pipewire(), &config);
        assert_eq!(entries[i].id.as_ref().unwrap().0, starship);
        // Unplugged: shown under its stored name, selected, with its id kept.
        let config = StreamConfig {
            device: Some("Focusrite Scarlett 2i2".into()),
            device_id: Some(DeviceId("pipewire:alsa_output.usb-Focusrite".into())),
            ..StreamConfig::default()
        };
        let (entries, i) = device_entries(HostKind::PipeWire, &pipewire(), &config);
        assert_eq!(i, entries.len() - 1);
        assert_eq!(entries[i].label, "Focusrite Scarlett 2i2");
        assert_eq!(
            entries[i].id.as_ref().unwrap().0,
            "pipewire:alsa_output.usb-Focusrite"
        );
        // Another driver's list starts at the default.
        assert_eq!(device_entries(HostKind::Alsa, &[], &config).1, 0);
    }

    #[test]
    fn rates_include_the_configured_one_and_pipewire_conversions() {
        let devices = pipewire();
        let (entries, _) = device_entries(HostKind::PipeWire, &devices, &StreamConfig::default());
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
        };
        assert_eq!(row.info(), "aktiv · Kanal 1–16");
        assert_eq!(
            MidiRow {
                enabled: false,
                ..row.clone()
            }
            .info(),
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
        let text = scan_summary(Some(&report), false, now);
        assert!(
            text.starts_with("214 Plugins gefunden · letzter Scan heute "),
            "{text}"
        );
        assert!(text.ends_with(" · 1 Plugin übersprungen (lässt sich nicht laden)"));
        assert_eq!(scan_summary(Some(&report), true, now), "Scan läuft …");
        assert_eq!(scan_summary(None, false, now), "Noch kein Scan");
    }
}
