//! CLAP hosting on clack-host 0.2 (WP-26): [`load`] returns a [`ClapPlugin`] ([`HostedPlugin`]),
//! whose `activate` returns a [`processor::ClapProcessor`] ([`crate::HostedProcessor`]).
//!
//! Host extensions offered (see [`host`]): log, thread-check, gui, timer-support,
//! posix-fd-support (Linux and macOS), params, latency, state, audio-ports, note-ports, note-name.
//! Plugin extensions used: audio-ports, note-ports, params, state, latency, gui (x11, cocoa or
//! win32, embedded), timer-support, posix-fd-support (Linux and macOS), render, note-name.
//!
//! Windows (port W5): a `.clap` is a single DLL, loaded by clack-host and pinned by
//! `crate::library`.
//!
//! Threading follows the contract: the thread that calls [`load`] is the plugin's main thread
//! (CLAP thread-check reports it as such), processor methods mark their thread as the audio thread
//! while they run.

mod device_assets;
mod device_canvas;
mod events;
mod host;
mod native_layout;
mod presets;
mod processor;
mod sidechain_fallback;
mod signal_taps;
mod waveform_markers;

#[cfg(test)]
pub(crate) mod tests;

use std::cell::RefCell;
use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::os::fd::RawFd;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicU64};

use clack_extensions::audio_ports::PluginAudioPorts;
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize as ClapGuiSize, PluginGui, Window,
};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_name::{NoteNameBuffer, PluginNoteName};
use clack_extensions::note_ports::PluginNotePorts;
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
#[cfg(unix)]
use clack_extensions::posix_fd::PluginPosixFd;
use clack_extensions::preset_discovery::PluginPresetLoad;
use clack_extensions::render::{PluginRender, RenderMode as ClapRenderMode};
use clack_extensions::state::PluginState;
use clack_extensions::state_context::{PluginStateContext, StateContextType};
use clack_extensions::timer::{PluginTimer, TimerId as ClapTimerId};
use clack_host::events::Match;
use clack_host::prelude::*;
use komponist_ipc::clap_presets::{ClapPreset, ClapPresetLocation, PresetReport};
use komponist_ipc::control::param_flags;
use komponist_ipc::native_layout::LocalizedPlugin;
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{
    GuiSize, LogLevel, NoteName, ParamInfo, PluginDescriptor, PluginFormat, RenderMode, WireEvent,
    WireEventKind,
};

use self::events::{InputEventList, WireSink};
use self::host::{KpHost, MainThreadHost, SharedHost};
use self::processor::{ClapProcessor, NoteTarget, PortLayout};
#[cfg(unix)]
use crate::host::FdEvents;
use crate::host::{HostContext, TimerId};
use crate::plugin::{
    ActivateConfig, GUI_EMBEDDING, GuiInfo, GuiParent, HostedPlugin, HostedProcessor, HostingError,
};

/// Vendor reported to plugins in `clap_host.vendor`.
const HOST_VENDOR: &str = "klang-studio";

/// The embedded GUI API of this OS: X11 (Linux) or Cocoa (macOS, #52; no `set_scale` there, JUCE
/// plugins then double their size in points, spike #42 Q1).
#[cfg(x11_embed)]
const EMBEDDED: GuiConfiguration<'static> = GuiConfiguration {
    api_type: GuiApiType::X11,
    is_floating: false,
};
#[cfg(target_os = "macos")]
const EMBEDDED: GuiConfiguration<'static> = GuiConfiguration {
    api_type: GuiApiType::COCOA,
    is_floating: false,
};

/// Windows: `win32`, embedded into an `HWND` of the bridge's frame (W6), with `set_scale`.
#[cfg(windows)]
const EMBEDDED: GuiConfiguration<'static> = GuiConfiguration {
    api_type: GuiApiType::WIN32,
    is_floating: false,
};

#[cfg(not(any(x11_embed, target_os = "macos", windows)))]
compile_error!("komponist-hosting: no embedded CLAP GUI API (clap/mod.rs) for this OS");

/// Loads a bundle's entry (dlopen + `clap_entry.init`). clack caches entries per library, so
/// loading the same bundle twice in one process shares one initialised entry. The library stays
/// mapped after its last entry is gone (see `crate::library`).
///
/// Windows: clack-host loads with a plain `LoadLibraryExW(path, NULL, 0)`, which would look for the
/// plugin's dependencies next to the executable and on `PATH` rather than next to the plugin. So
/// the DLL is loaded with `crate::library`'s policy first (and pinned); clack's load then only
/// finds it loaded.
fn load_entry(bundle: &Path) -> Result<PluginEntry, HostingError> {
    // SAFETY: as below.
    #[cfg(windows)]
    let _policy_load = unsafe { crate::library::Library::open(bundle) }?;
    // SAFETY: loading a plugin library runs foreign code; the bridge process exists to contain it.
    let entry = unsafe { PluginEntry::load(bundle) }
        .map_err(|e| HostingError::Load(format!("{}: {e}", bundle.display())))?;
    // clack-host unloads the library with its last entry; plugin code must stay mapped
    // (`crate::library`, WP-32).
    crate::library::keep_mapped(&binary_path(bundle));
    Ok(entry)
}

/// The library clack-host loaded for `bundle`: the bundle itself on Linux and Windows (a `.clap` is the
/// shared library), the executable inside a `.clap` bundle directory on macOS (clack passes the
/// bundle's path to `clap_entry.init`).
fn binary_path(bundle: &Path) -> std::borrow::Cow<'_, Path> {
    #[cfg(target_os = "macos")]
    if let Some(executable) = crate::bundle::executable(bundle) {
        return executable.into();
    }
    bundle.into()
}

/// A NUL-terminated C string field as UTF-8 (lossy), cut at the first NUL.
fn c_bytes(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn c_str(s: Option<&CStr>) -> String {
    s.map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(crate) fn discover_presets(bundle: &Path) -> Result<Vec<ClapPreset>, HostingError> {
    presets::discover(bundle)
}

/// A CLAP factory descriptor as komponist's [`PluginDescriptor`].
fn descriptor_of(d: &clack_host::plugin::PluginDescriptor, bundle: &Path) -> PluginDescriptor {
    let features: Vec<String> = d
        .features()
        .map(|f| f.to_string_lossy().to_lowercase())
        .collect();
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: c_str(d.id()),
        name: c_str(d.name()),
        localized: d
            .description()
            .filter(|text| !text.to_bytes().is_empty())
            .map(|text| LocalizedPlugin {
                default_locale: "en".into(),
                names: Default::default(),
                descriptions: [("en".into(), text.to_string_lossy().into_owned())].into(),
            }),
        vendor: c_str(d.vendor()),
        version: c_str(d.version()),
        kind: crate::scan::kind_from_features(PluginFormat::Clap, &features),
        features,
        bundle: std::path::absolute(bundle).unwrap_or_else(|_| bundle.to_path_buf()),
        sidechain: false,
    }
}

/// Lists the plugins of a `.clap` bundle. Each one is instantiated without activation to probe
/// its sidechain input and optional localized names and descriptions.
pub(crate) fn scan(bundle: &Path) -> Result<Vec<PluginDescriptor>, HostingError> {
    let entry = load_entry(bundle)?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| HostingError::Load(format!("{}: no plugin factory", bundle.display())))?;
    let mut descriptors: Vec<PluginDescriptor> = factory
        .plugin_descriptors()
        .filter(|d| d.id().is_some())
        .map(|d| descriptor_of(d, bundle))
        .collect();
    for descriptor in &mut descriptors {
        if let Ok(mut plugin) =
            ClapPlugin::load(bundle, &descriptor.id, crate::scan::probe_context())
        {
            descriptor.sidechain = plugin.descriptor.sidechain;
            descriptor.localized = plugin.descriptor.localized.take();
        }
    }
    Ok(descriptors)
}

/// The plugin extensions komponist uses, queried once after instantiation.
#[derive(Clone, Copy)]
struct Extensions {
    audio_ports: Option<PluginAudioPorts>,
    sidechain_from_main: bool,
    signal_taps: Option<signal_taps::SignalTaps>,
    note_ports: Option<PluginNotePorts>,
    params: Option<PluginParams>,
    state: Option<PluginState>,
    preset_load: Option<PluginPresetLoad>,
    state_context: Option<PluginStateContext>,
    latency: Option<PluginLatency>,
    gui: Option<PluginGui>,
    timer: Option<PluginTimer>,
    #[cfg(unix)]
    posix_fd: Option<PluginPosixFd>,
    render: Option<PluginRender>,
    note_name: Option<PluginNoteName>,
}

impl Extensions {
    fn query(handle: &PluginMainThreadHandle) -> Extensions {
        Extensions {
            audio_ports: handle.get_extension(),
            sidechain_from_main: sidechain_fallback::configure(handle),
            signal_taps: signal_taps::SignalTaps::query(handle.as_raw()),
            note_ports: handle.get_extension(),
            params: handle.get_extension(),
            state: handle.get_extension(),
            preset_load: handle.get_extension(),
            state_context: handle.get_extension(),
            latency: handle.get_extension(),
            gui: handle.get_extension(),
            timer: handle.get_extension(),
            #[cfg(unix)]
            posix_fd: handle.get_extension(),
            render: handle.get_extension(),
            note_name: handle.get_extension(),
        }
    }
}

/// An open embedded GUI.
#[derive(Debug, Clone, Copy)]
struct OpenGui {
    resizable: bool,
}

/// Loads plugin `plugin_id` from a `.clap` bundle on the calling (main) thread.
pub(crate) fn load(
    bundle: &Path,
    plugin_id: &str,
    context: HostContext,
) -> Result<Box<dyn HostedPlugin>, HostingError> {
    Ok(Box::new(ClapPlugin::load(bundle, plugin_id, context)?))
}

/// One loaded CLAP plugin instance, main-thread half.
pub(crate) struct ClapPlugin {
    // Declared first: dropped (destroyed) before the entry reference below.
    instance: RefCell<PluginInstance<KpHost>>,
    _entry: PluginEntry,
    descriptor: PluginDescriptor,
    ext: Extensions,
    context: HostContext,
    has_gui: bool,
    native_layout: Option<(Vec<u8>, komponist_ipc::native_layout::NativeLayout)>,
    gui: Option<OpenGui>,
    render_mode: RenderMode,
}

impl ClapPlugin {
    pub(crate) fn load(
        bundle: &Path,
        plugin_id: &str,
        context: HostContext,
    ) -> Result<ClapPlugin, HostingError> {
        Self::load_from_entry(bundle, plugin_id, context, load_entry(bundle)?)
    }

    fn load_from_entry(
        bundle: &Path,
        plugin_id: &str,
        context: HostContext,
        entry: PluginEntry,
    ) -> Result<ClapPlugin, HostingError> {
        let factory = entry.get_plugin_factory().ok_or_else(|| {
            HostingError::Load(format!("{}: no plugin factory", bundle.display()))
        })?;
        let descriptor = factory
            .plugin_descriptors()
            .find(|d| {
                d.id()
                    .is_some_and(|id| id.to_bytes() == plugin_id.as_bytes())
            })
            .map(|d| descriptor_of(d, bundle))
            .ok_or_else(|| {
                HostingError::Load(format!("{}: no plugin {plugin_id:?}", bundle.display()))
            })?;
        let id = CString::new(plugin_id)
            .map_err(|_| HostingError::Load(format!("invalid plugin id {plugin_id:?}")))?;
        let host_info = HostInfo::new(context.host_name, HOST_VENDOR, "", context.host_version)
            .map_err(|e| HostingError::Load(format!("host info: {e}")))?;

        let main_thread = std::thread::current().id();
        let notify = context.notify.clone();
        let host_loop = context.host_loop.clone();
        let mut instance = PluginInstance::<KpHost>::new(
            |_| SharedHost::new(notify, main_thread),
            |shared| MainThreadHost::new(shared, host_loop),
            &entry,
            &id,
            &host_info,
        )
        .map_err(|e| HostingError::Load(format!("{plugin_id}: {e}")))?;
        // Created and initialised: later reports are changes unless the host starts the plugin
        // again (activation, a state restore).
        instance.access_handler(|host| host.set_starting(false));

        let handle = instance.plugin_handle();
        let ext = Extensions::query(&handle);
        let has_gui = GUI_EMBEDDING
            && ext
                .gui
                .is_some_and(|gui| gui.is_api_supported(&handle, EMBEDDED));
        let descriptor = PluginDescriptor {
            sidechain: PortLayout::query(ext.audio_ports, &handle, true)
                .sidechain
                .is_some(),
            ..descriptor
        };

        let mut plugin = ClapPlugin {
            instance: RefCell::new(instance),
            _entry: entry,
            descriptor,
            ext,
            context,
            has_gui,
            native_layout: None,
            gui: None,
            render_mode: RenderMode::Realtime,
        };
        let layout = {
            let mut instance = plugin.instance.borrow_mut();
            native_layout::read(instance.plugin_handle().as_raw())
        };
        match layout.and_then(|layout| {
            if let Some((_, descriptor)) = &layout {
                descriptor.validate_params(&plugin.params())?;
            }
            Ok(layout)
        }) {
            Ok(Some(layout)) => {
                if let Some(mut labels) = layout.1.localized_plugin() {
                    if let Some(fallback) = plugin.descriptor.localized.take()
                        && labels.descriptions.is_empty()
                        && let Some(description) = fallback.description(&labels.default_locale)
                    {
                        labels
                            .descriptions
                            .insert(labels.default_locale.clone(), description.into());
                    }
                    plugin.descriptor.localized = Some(labels);
                }
                plugin.native_layout = Some(layout);
            }
            Ok(None) => {}
            Err(error) => plugin.log(LogLevel::Warning, &error.to_string()),
        }
        Ok(plugin)
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.context.notify.log(level, message);
    }

    fn owns_timer(&self, timer: TimerId) -> bool {
        self.instance
            .borrow()
            .access_handler(|mt| mt.owns_timer(timer))
    }

    #[cfg(unix)]
    fn owns_fd(&self, fd: RawFd) -> bool {
        self.instance.borrow().access_handler(|mt| mt.owns_fd(fd))
    }

    fn require_gui(&self) -> Result<(PluginGui, OpenGui), HostingError> {
        match (self.ext.gui, self.gui) {
            (Some(ext), Some(open)) => Ok((ext, open)),
            _ => Err(HostingError::WrongState("the GUI is not open")),
        }
    }
}

fn param_info(info: &clack_extensions::params::ParamInfo) -> ParamInfo {
    let f = info.flags;
    let mut flags = 0;
    let mut step_count = 0;
    if f.contains(ParamInfoFlags::IS_STEPPED) {
        flags |= param_flags::STEPPED;
        let steps = (info.max_value - info.min_value).round();
        step_count = if steps.is_finite() && steps > 0.0 {
            steps.min(f64::from(u32::MAX)) as u32
        } else {
            0
        };
    }
    for (clap, ours) in [
        (ParamInfoFlags::IS_BYPASS, param_flags::BYPASS),
        (ParamInfoFlags::IS_HIDDEN, param_flags::HIDDEN),
        (ParamInfoFlags::IS_READONLY, param_flags::READ_ONLY),
        (ParamInfoFlags::IS_AUTOMATABLE, param_flags::AUTOMATABLE),
        (ParamInfoFlags::IS_ENUM, param_flags::ENUM),
    ] {
        if f.contains(clap) {
            flags |= ours;
        }
    }
    ParamInfo {
        id: info.id.get(),
        name: c_bytes(info.name),
        module: c_bytes(info.module),
        min: info.min_value,
        max: info.max_value,
        default: info.default_value,
        step_count,
        flags,
    }
}

impl HostedPlugin for ClapPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn native_layout(&self) -> Option<(&[u8], &komponist_ipc::native_layout::NativeLayout)> {
        self.native_layout
            .as_ref()
            .map(|(bytes, layout)| (bytes.as_slice(), layout))
    }

    fn signal_taps(&self) -> &[komponist_ipc::signal_taps::Descriptor] {
        self.ext
            .signal_taps
            .as_ref()
            .map_or(&[], signal_taps::SignalTaps::inventory)
    }

    fn supports_asset_slots(&self) -> bool {
        device_assets::supports_slots(self.instance.borrow_mut().plugin_handle().as_raw())
    }

    fn asset_slots(&self) -> Option<komponist_ipc::device_assets::Snapshot> {
        device_assets::snapshot(self.instance.borrow_mut().plugin_handle().as_raw())
    }

    fn set_asset_bindings(&mut self, bindings: &komponist_ipc::device_assets::Bindings) -> bool {
        device_assets::set_bindings(
            self.instance.borrow_mut().plugin_handle().as_raw(),
            bindings,
        )
    }

    fn supports_asset_edit_state(&self) -> bool {
        device_assets::supports_edit_state(self.instance.borrow_mut().plugin_handle().as_raw())
    }

    fn prepare_asset_edit_state(
        &self,
        edits: &komponist_ipc::device_assets::edit_state::Edits,
        state: Option<&[u8]>,
    ) -> komponist_ipc::device_assets::edit_state::PreparedChange {
        device_assets::prepare_edit_state(
            self.instance.borrow_mut().plugin_handle().as_raw(),
            edits,
            state,
        )
    }

    fn supports_offline_preparation(&self) -> bool {
        device_assets::supports_offline_preparation(
            self.instance.borrow_mut().plugin_handle().as_raw(),
        )
    }

    fn prepare_offline(
        &mut self,
        request: Option<komponist_ipc::device_assets::OfflineRequest>,
    ) -> Option<komponist_ipc::device_assets::OfflineStatus> {
        device_assets::prepare_offline(self.instance.borrow_mut().plugin_handle().as_raw(), request)
    }

    fn has_gui(&self) -> bool {
        self.has_gui
    }

    fn activate(
        &mut self,
        config: ActivateConfig,
    ) -> Result<Box<dyn HostedProcessor>, HostingError> {
        if self.is_active() {
            return Err(HostingError::WrongState("already active"));
        }
        if !(config.sample_rate.is_finite() && config.sample_rate > 0.0)
            || config.min_frames == 0
            || config.min_frames > config.max_frames
            || config.max_frames as usize > MAX_BLOCK
        {
            return Err(HostingError::Activate(format!(
                "invalid configuration {config:?}"
            )));
        }
        let ext = self.ext;
        let mut instance = self.instance.borrow_mut();
        let (inputs, outputs, notes, note_output) = {
            let handle = instance.plugin_handle();
            if let Some(render) = ext.render {
                let mode = match self.render_mode {
                    RenderMode::Realtime => ClapRenderMode::Realtime,
                    RenderMode::Offline => ClapRenderMode::Offline,
                };
                if render.set(&handle, mode).is_err() {
                    self.context.notify.log(
                        LogLevel::Info,
                        "render: the plugin declined the render mode",
                    );
                }
            }
            let mut inputs = PortLayout::query(ext.audio_ports, &handle, true);
            inputs.sidechain_from_main = ext.sidechain_from_main;
            (
                inputs,
                PortLayout::query(ext.audio_ports, &handle, false),
                NoteTarget::query(ext.note_ports, &handle),
                self.descriptor
                    .features
                    .iter()
                    .any(|feature| feature == "note-effect")
                    && ext
                        .note_ports
                        .is_some_and(|ports| ports.count(&handle, false) != 0),
            )
        };
        instance.access_handler(|host| host.set_starting(true));
        let activated = instance.activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: config.sample_rate,
                min_frames_count: config.min_frames,
                max_frames_count: config.max_frames,
            },
        );
        instance.access_handler(|host| host.set_starting(false));
        let stopped = activated.map_err(|e| HostingError::Activate(e.to_string()))?;
        let mut processor = ClapProcessor::new(
            stopped,
            &inputs,
            &outputs,
            notes,
            note_output,
            config.max_frames,
        );
        processor.set_signal_taps(ext.signal_taps);
        Ok(Box::new(processor))
    }

    fn deactivate(&mut self, processor: Box<dyn HostedProcessor>) {
        // The processor holds this instance's audio half; dropping it (it stops processing if
        // still started) leaves the instance as the only owner, which `try_deactivate` needs.
        drop(processor);
        let result = self.instance.borrow_mut().try_deactivate();
        if let Err(e) = result {
            self.log(LogLevel::Error, &format!("deactivate failed: {e}"));
        }
    }

    fn is_active(&self) -> bool {
        self.instance.borrow().is_active()
    }

    fn set_render_mode(&mut self, mode: RenderMode) -> Result<(), HostingError> {
        if self.is_active() {
            return Err(HostingError::WrongState(
                "render mode changes need an inactive plugin",
            ));
        }
        self.render_mode = mode;
        Ok(())
    }

    fn params(&self) -> Vec<ParamInfo> {
        let Some(params) = self.ext.params else {
            return Vec::new();
        };
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let mut buffer = ParamInfoBuffer::new();
        (0..params.count(&handle))
            .filter_map(|index| {
                params
                    .get_info(&handle, index, &mut buffer)
                    .map(|i| param_info(&i))
            })
            .collect()
    }

    fn device_display(&self) -> Option<komponist_ipc::device_canvas::DisplayList> {
        device_canvas::draw(self.instance.borrow_mut().plugin_handle().as_raw())
    }

    fn waveform_markers(&self, slot: u32) -> Option<komponist_ipc::waveform_markers::Snapshot> {
        waveform_markers::snapshot(self.instance.borrow_mut().plugin_handle().as_raw(), slot)
    }

    fn edit_waveform_markers(
        &mut self,
        slot: u32,
        request: komponist_ipc::waveform_markers::Request,
    ) -> Option<komponist_ipc::waveform_markers::EditStatus> {
        waveform_markers::edit(
            self.instance.borrow_mut().plugin_handle().as_raw(),
            slot,
            request,
        )
    }

    fn preview_waveform(&self, slot: u32, time: Option<f64>) {
        use komponist_ipc::waveform_preview::{EXTENSION_ID, Extension, encode_time};

        let Some(time) = encode_time(time) else {
            return;
        };
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let plugin = handle.as_raw();
        // SAFETY: The live instance is borrowed on its main thread. Its extension table
        // is immutable for the lifetime of the instance and queried by its exact ID.
        unsafe {
            if let Some(preview) = plugin
                .get_extension
                .and_then(|query| {
                    query(plugin, EXTENSION_ID.as_ptr())
                        .cast::<Extension>()
                        .as_ref()
                })
                .and_then(|ext| ext.preview)
            {
                preview(std::ptr::from_ref(plugin).cast(), slot, time);
            }
        }
    }

    fn param_value(&self, id: u32) -> Option<f64> {
        let params = self.ext.params?;
        let id = ClapId::from_raw(id)?;
        let mut instance = self.instance.borrow_mut();
        params.get_value(&instance.plugin_handle(), id)
    }

    fn param_text(&self, id: u32, value: f64) -> Option<String> {
        let params = self.ext.params?;
        let id = ClapId::from_raw(id)?;
        let mut instance = self.instance.borrow_mut();
        let mut buffer = [0u8; 256];
        let text = params
            .value_to_text(&instance.plugin_handle(), id, value, &mut buffer)
            .ok()?;
        Some(c_bytes(text))
    }

    fn parse_param_text(&self, id: u32, text: &str) -> Option<f64> {
        let params = self.ext.params?;
        let id = ClapId::from_raw(id)?;
        let text = std::ffi::CString::new(text).ok()?;
        let mut instance = self.instance.borrow_mut();
        let value = params.text_to_value(&instance.plugin_handle(), id, &text)?;
        value.is_finite().then_some(value)
    }

    fn set_param(&mut self, _id: u32, _value: f64) {
        // CLAP has no separate main-thread parameter view: the value reaches the plugin as an
        // event in the next block or through `flush_params`.
    }

    fn flush_params(&mut self, input: &[WireEvent], output: &mut Vec<WireEvent>) {
        let Some(params) = self.ext.params else {
            return;
        };
        let mut list = InputEventList::with_capacity(input.len());
        for wire in input {
            if let Some(WireEventKind::ParamValue { id, value }) = wire.decode()
                && let Some(event) = events::param_value_event(0, id, value)
            {
                list.push(event);
            }
        }
        let mut instance = self.instance.borrow_mut();
        // While active, CLAP allows `flush` only on the audio thread; the bridge then delivers
        // parameter values as events of the next block instead.
        let Some(mut handle) = instance.inactive_plugin_handle() else {
            return;
        };
        let mut sink = WireSink::flush(|event| {
            output.push(event);
            true
        });
        params.flush(
            &mut handle,
            &InputEvents::from_buffer(&list),
            &mut OutputEvents::from_buffer(&mut sink),
        );
    }

    fn save_state(&mut self) -> Result<Vec<u8>, HostingError> {
        let state = self.ext.state.ok_or(HostingError::Unsupported(
            "the plugin has no state extension",
        ))?;
        let mut instance = self.instance.borrow_mut();
        let mut blob = Vec::new();
        state
            .save(&instance.plugin_handle(), &mut blob)
            .map_err(|e| HostingError::State(e.to_string()))?;
        Ok(blob)
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), HostingError> {
        let ext = self.ext.state.ok_or(HostingError::Unsupported(
            "the plugin has no state extension",
        ))?;
        let mut instance = self.instance.borrow_mut();
        let mut reader = state;
        // A preset the plugin names while it restores a state is the one it starts in.
        instance.access_handler(|host| host.set_starting(true));
        let loaded = ext.load(&instance.plugin_handle(), &mut reader);
        instance.access_handler(|host| host.set_starting(false));
        loaded.map_err(|e| HostingError::State(e.to_string()))
    }

    fn load_clap_preset(&mut self, preset: &ClapPreset) -> Result<(), HostingError> {
        if preset.plugin_id != self.descriptor.id {
            return Err(HostingError::State(
                "preset belongs to a different plug-in".into(),
            ));
        }
        let ext = self.ext.preset_load.ok_or(HostingError::Unsupported(
            "the plugin has no preset-load extension",
        ))?;
        let path = match &preset.source.location {
            ClapPresetLocation::Plugin => {
                if preset.source.load_key.as_ref().is_none_or(Vec::is_empty) {
                    return Err(HostingError::State(
                        "the embedded preset has no load key".into(),
                    ));
                }
                None
            }
            ClapPresetLocation::File(path) if Path::new(path).is_absolute() => {
                Some(path.as_bytes())
            }
            ClapPresetLocation::File(_) => {
                return Err(HostingError::State("the preset path is relative".into()));
            }
        };
        let to_c_string = |value: Option<&[u8]>| {
            value
                .map(CString::new)
                .transpose()
                .map_err(|error| HostingError::State(error.to_string()))
        };
        let path = to_c_string(path)?;
        let key = to_c_string(preset.source.load_key.as_deref())?;
        let mut instance = self.instance.borrow_mut();
        let mut earlier = instance.access_handler(|host| host.take_preset_reports());
        let result = ext.load_from_location(
            &instance.plugin_handle(),
            presets::location_arg(path.as_deref()),
            key.as_deref(),
        );
        if result.is_ok() {
            // Only a successful load supersedes a preset the plugin reported earlier.
            earlier.retain(|report| matches!(report, PresetReport::Failed { .. }));
        }
        // What the plugin reports during the call describes this load: the result covers it.
        let during = instance.access_handler(|host| {
            let during = host.take_preset_reports();
            host.restore_preset_reports(earlier);
            during
        });
        result.map_err(|error| {
            let reason = during.into_iter().rev().find_map(|report| match report {
                PresetReport::Failed { message, .. } if !message.is_empty() => Some(message),
                _ => None,
            });
            HostingError::State(
                reason.map_or_else(|| error.to_string(), |reason| format!("{error}: {reason}")),
            )
        })
    }

    fn preset_load_epoch(&self) -> Option<Arc<AtomicU64>> {
        Some(
            self.instance
                .borrow()
                .access_shared_handler(|host| Arc::clone(&host.preset_load_epoch)),
        )
    }

    fn take_preset_reports(&mut self) -> Vec<PresetReport> {
        self.instance
            .borrow()
            .access_handler(|host| host.take_preset_reports())
    }

    fn save_preset_state(&mut self) -> Result<Vec<u8>, HostingError> {
        let Some(ext) = self.ext.state_context else {
            return self.save_state();
        };
        let mut instance = self.instance.borrow_mut();
        let mut blob = Vec::new();
        ext.save(
            &instance.plugin_handle(),
            &mut blob,
            StateContextType::ForPreset,
        )
        .map_err(|error| HostingError::State(error.to_string()))?;
        Ok(blob)
    }

    fn load_preset_state(&mut self, state: &[u8]) -> Result<(), HostingError> {
        let Some(ext) = self.ext.state_context else {
            return self.load_state(state);
        };
        let mut instance = self.instance.borrow_mut();
        let mut reader = state;
        ext.load(
            &instance.plugin_handle(),
            &mut reader,
            StateContextType::ForPreset,
        )
        .map_err(|error| HostingError::State(error.to_string()))
    }

    fn latency(&self) -> u32 {
        let Some(latency) = self.ext.latency else {
            return 0;
        };
        let mut instance = self.instance.borrow_mut();
        if !instance.is_active() {
            return 0;
        }
        latency.get(&instance.plugin_handle())
    }

    fn note_names(&self) -> Vec<NoteName> {
        let Some(ext) = self.ext.note_name else {
            return Vec::new();
        };
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let mut buffer = NoteNameBuffer::new();
        let count = ext.count(&handle) as u32;
        (0..count)
            .filter_map(|index| {
                let name = ext.get(&handle, index, &mut buffer)?;
                if matches!(name.port, Match::Specific(p) if p != 0) {
                    return None;
                }
                let key = match name.key {
                    Match::Specific(k) if k < 128 => k as u8,
                    _ => return None,
                };
                let channel = match name.channel {
                    Match::Specific(c) if c < 16 => Some(c as u8),
                    Match::Specific(_) => return None,
                    Match::All => None,
                };
                Some(NoteName {
                    key,
                    channel,
                    name: c_bytes(name.name),
                })
            })
            .collect()
    }

    fn gui_open(&mut self, parent: GuiParent) -> Result<GuiInfo, HostingError> {
        if self.gui.is_some() {
            return Err(HostingError::WrongState("the GUI is already open"));
        }
        let gui = self
            .ext
            .gui
            .filter(|_| self.has_gui)
            .ok_or_else(|| HostingError::Gui("the plugin has no embeddable GUI".into()))?;
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        gui.create(&handle, EMBEDDED)
            .map_err(|e| HostingError::Gui(format!("create: {e}")))?;
        let opened = (|| {
            let window = match parent {
                #[cfg(x11_embed)]
                GuiParent::X11 { xid, scale } => {
                    // Allowed to fail on X11 (Shortcircuit XT picks its own scale).
                    if gui.set_scale(&handle, scale).is_err() {
                        self.context
                            .notify
                            .log(LogLevel::Debug, &format!("gui.set_scale({scale}) declined"));
                    }
                    Window::from_x11_handle(xid as _)
                }
                #[cfg(target_os = "macos")]
                // SAFETY: see `set_parent` below; the view outlives the GUI.
                GuiParent::Cocoa(view) => unsafe { Window::from_cocoa_nsview(view) },
                #[cfg(windows)]
                GuiParent::Win32 { hwnd, scale } => {
                    // Allowed to fail: a plugin may follow the window's DPI itself.
                    if gui.set_scale(&handle, scale).is_err() {
                        self.context
                            .notify
                            .log(LogLevel::Debug, &format!("gui.set_scale({scale}) declined"));
                    }
                    // SAFETY: see `set_parent` below; the window outlives the GUI.
                    unsafe { Window::from_win32_hwnd(hwnd) }
                }
                _ => {
                    return Err(HostingError::Gui(
                        "the parent window is of another platform".into(),
                    ));
                }
            };
            let size = gui
                .get_size(&handle)
                .ok_or_else(|| HostingError::Gui("get_size failed".into()))?;
            let resizable = gui.can_resize(&handle);
            // SAFETY: the parent is a live X11 window of the app's frame, or an `NSView` / `HWND`
            // of the bridge's frame; either is kept until after `gui_close` (or the bridge's
            // death).
            unsafe { gui.set_parent(&handle, window) }
                .map_err(|e| HostingError::Gui(format!("set_parent: {e}")))?;
            gui.show(&handle)
                .map_err(|e| HostingError::Gui(format!("show: {e}")))?;
            Ok(GuiInfo {
                size: GuiSize {
                    width: size.width,
                    height: size.height,
                },
                resizable,
            })
        })();
        match opened {
            Ok(info) => {
                self.gui = Some(OpenGui {
                    resizable: info.resizable,
                });
                Ok(info)
            }
            Err(e) => {
                gui.destroy(&handle);
                Err(e)
            }
        }
    }

    fn gui_set_size(&mut self, size: GuiSize) -> Result<GuiSize, HostingError> {
        let (gui, open) = self.require_gui()?;
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let current = |handle: &PluginMainThreadHandle| {
            gui.get_size(handle)
                .map(|s| GuiSize {
                    width: s.width,
                    height: s.height,
                })
                .ok_or_else(|| HostingError::Gui("get_size failed".into()))
        };
        if !open.resizable {
            // A fixed-size GUI keeps its size. Calling `set_size` anyway makes JUCE log
            // HOST_MISBEHAVING (spike WP-15 §1.4).
            return current(&handle);
        }
        let wanted = ClapGuiSize {
            width: size.width,
            height: size.height,
        };
        let adjusted = gui.adjust_size(&handle, wanted).unwrap_or(wanted);
        gui.set_size(&handle, adjusted)
            .map_err(|e| HostingError::Gui(format!("set_size: {e}")))?;
        current(&handle).or(Ok(GuiSize {
            width: adjusted.width,
            height: adjusted.height,
        }))
    }

    fn gui_set_scale(&mut self, scale: f64) -> bool {
        let Ok((gui, _)) = self.require_gui() else {
            return false;
        };
        let mut instance = self.instance.borrow_mut();
        gui.set_scale(&instance.plugin_handle(), scale).is_ok()
    }

    fn gui_close(&mut self) {
        let Some(gui) = self.ext.gui else {
            return;
        };
        if self.gui.take().is_none() {
            return;
        }
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let _ = gui.hide(&handle);
        gui.destroy(&handle);
    }

    fn on_main_thread(&mut self) {
        self.instance.borrow_mut().call_on_main_thread_callback();
    }

    fn on_timer(&mut self, timer: TimerId) {
        let Some(ext) = self.ext.timer else {
            return;
        };
        if !self.owns_timer(timer) {
            return;
        }
        let mut instance = self.instance.borrow_mut();
        ext.on_timer(&instance.plugin_handle(), ClapTimerId(timer.0));
    }

    #[cfg(unix)]
    fn on_fd(&mut self, fd: RawFd, events: FdEvents) {
        let Some(ext) = self.ext.posix_fd else {
            return;
        };
        if !self.owns_fd(fd) {
            return;
        }
        let mut instance = self.instance.borrow_mut();
        ext.on_fd(&instance.plugin_handle(), fd, host::fd_flags(events));
    }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        self.gui_close();
        let mut instance = self.instance.borrow_mut();
        if instance.is_active() && instance.try_deactivate().is_err() {
            // The processor still lives somewhere; clack destroys the instance only after it.
            self.context.notify.log(
                LogLevel::Error,
                "plugin dropped while its processor is still alive",
            );
        }
    }
}
