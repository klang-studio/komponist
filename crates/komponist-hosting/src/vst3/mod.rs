//! VST3 hosting on the raw `vst3` 0.3 bindings (WP-32): [`load`] returns a [`Vst3Plugin`]
//! ([`HostedPlugin`]), whose `activate` returns a [`processor::Vst3Processor`]
//! ([`HostedProcessor`]). Built from the spike's `vst3_child.rs` (WP-15).
//!
//! Plugin interfaces used: `IPluginFactory` (2, 3), `IComponent`, `IAudioProcessor`,
//! `IEditController`, `IConnectionPoint`, `IMidiMapping`, `IUnitInfo`, `IPlugView`,
//! `IPlugViewContentScaleSupport`. Host interfaces offered (see [`host`]): `IHostApplication`,
//! `IPlugInterfaceSupport`, `IMessage` / `IAttributeList`, `IComponentHandler` (2),
//! `IPlugFrame` + `Linux::IRunLoop` (Linux and macOS), `IBStream` (+ `ISizeableStream`), `IParameterChanges` /
//! `IParamValueQueue`, `IEventList`.
//!
//! # Host duties (spike WP-15 §2 and §4: what truce-rack got wrong)
//!
//! - **Module lifetime:** `ModuleEntry` after `dlopen`, `ModuleExit` before `dlclose`, and the
//!   library stays mapped until every COM pointer into it is released ([`module`]; the plugin and
//!   its processor each hold the `Arc<Module>` as their last field).
//! - **A real host context:** `initialize` gets an [`host::HostApplication`], never NULL; the
//!   factory gets it through `IPluginFactory3::setHostContext`.
//! - **Component ↔ controller:** a separate controller is created from the component's controller
//!   class id, both are connected through `IConnectionPoint`, and the component state is mirrored
//!   into the controller (`setComponentState`) after initialisation and after every state load.
//! - **Buses:** stereo main buses are requested (`setBusArrangements`), main buses, the sidechain
//!   input (the first `kAux` audio input bus, #6) and the first event input are activated
//!   (`activateBus`) before `setActive(true)`, and instruments without audio inputs get
//!   `numInputs = 0`. Otherwise instruments stay silent.
//! - **Parameters reach the processor:** app values and GUI edits go in as
//!   `inputParameterChanges`; the controller learns the processor's output changes.
//! - **Unload:** GUI closed, deactivated, disconnected, controller and component terminated,
//!   every handler released, then the module.
//!
//! # Parameters
//!
//! VST3 parameters are normalized, and only the controller can convert to plain values (on the
//! main thread). komponist therefore exposes VST3 parameters **normalized**: [`ParamInfo`] has
//! `min = 0`, `max = 1`, `default` = the normalized default, and a stepped parameter has
//! `step_count` steps of `1 / step_count`. Values in [`HostedPlugin::param_value`],
//! [`HostedPlugin::set_param`], parameter events and [`HostNotification::ParamEdited`](crate::HostNotification::ParamEdited) are all
//! normalized; the plugin's text for a value comes from `getParamStringByValue`.
//!
//! GUI edits (`IComponentHandler` on the main thread) become [`HostNotification::ParamEdited`](crate::HostNotification::ParamEdited)
//! (plus `StateDirty`) for the app, and go to the processor, which applies them in its next block
//! and echoes them, with their `beginEdit` / `endEdit` gestures, as output events of that block.
//!
//! While inactive, `flush_params` and GUI edits update the controller at once and reach the
//! processor in the first block after the next activation (the VST3 way to hand a value to the
//! processor is a `process` call). JUCE and single-component plugins (nice-plug, clap-wrapper) share
//! one parameter set between controller and processor, so their state includes such a change at
//! once.
//!
//! # State
//!
//! Component and controller state in one blob, see [`stream`] for the format.

mod events;
mod host;
mod module;
mod processor;
pub(crate) mod stream;
mod strings;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
#[cfg(unix)]
use std::os::fd::RawFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use komponist_ipc::control::param_flags;
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{
    GuiSize, LogLevel, NoteName, ParamInfo, PluginDescriptor, RenderMode, WireEvent, WireEventKind,
};
use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
use vst3::Steinberg::Vst::BusInfo_::BusFlags_::kDefaultActive;
use vst3::Steinberg::Vst::BusTypes_::{kAux, kMain};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{
    kCanAutomate, kIsBypass, kIsHidden, kIsList, kIsReadOnly,
};
use vst3::Steinberg::Vst::ProcessModes_::{kOffline, kRealtime};
use vst3::Steinberg::Vst::SpeakerArr::kStereo;
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    BusInfo, IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentHandler, IComponentTrait,
    IConnectionPoint, IConnectionPointTrait, IEditController, IEditControllerTrait, IMidiMapping,
    IUnitInfo, IUnitInfoTrait, ParameterInfo, ProcessSetup, SpeakerArrangement, String128,
    UnitInfo, ViewType,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IPlugFrame, IPlugView, IPlugViewContentScaleSupport,
    IPlugViewContentScaleSupportTrait, IPlugViewTrait, IPluginBaseTrait, IPluginFactory,
    IPluginFactory3, IPluginFactory3Trait, IPluginFactoryTrait, TUID, ViewRect,
};
use vst3::Steinberg::{kResultOk, kResultTrue};
use vst3::{ComPtr, ComWrapper, Interface};

/// The `IPlugView` platform type of this OS: an X11 window id (Linux) or an `NSView` (macOS, #52;
/// no `setContentScaleFactor` there, plugins refuse it, spike #42 Q1).
#[cfg(x11_embed)]
const PLATFORM_TYPE: FIDString = vst3::Steinberg::kPlatformTypeX11EmbedWindowID;
#[cfg(target_os = "macos")]
const PLATFORM_TYPE: FIDString = vst3::Steinberg::kPlatformTypeNSView;

/// Windows: an `HWND` of the bridge's frame (W6), with `setContentScaleFactor`.
#[cfg(windows)]
const PLATFORM_TYPE: FIDString = vst3::Steinberg::kPlatformTypeHWND;

#[cfg(not(any(x11_embed, target_os = "macos", windows)))]
compile_error!("komponist-hosting: no VST3 IPlugView platform type (vst3/mod.rs) for this OS");

/// The integer type of the VST3 enums (`MediaTypes`, `BusDirections`, `EventTypes`, flag sets, …): the
/// bindings make it `c_uint` on Linux and macOS and `c_int` on Windows (MSVC enums are signed).
/// Constants keep their bit patterns either way; [`enum_u32`] and [`enum_i32`] convert for the
/// `uint32` and `int32` fields and arguments they go into.
pub(crate) type VstEnum = vst3::Steinberg::Vst::MediaTypes;

/// A VST3 enum value (or flag set) as `uint32` (see [`VstEnum`]).
#[allow(clippy::unnecessary_cast)] // a no-op outside Windows
pub(crate) const fn enum_u32(value: VstEnum) -> u32 {
    value as u32
}

/// A VST3 enum value as `int32` (see [`VstEnum`]).
#[allow(clippy::unnecessary_cast)] // a no-op on Windows
pub(crate) const fn enum_i32(value: VstEnum) -> i32 {
    value as i32
}

use self::events::MidiMap;
use self::host::{
    ComponentHandler, EDIT_RING, Edit, Edits, HostApplication, ParamChange, PlugFrame, RunLoop,
};
use self::module::{Module, audio_classes, load_module, parse_cid};
use self::processor::{BusLayout, ProcessorSetup, TO_CONTROLLER_RING, Vst3Processor};
use self::strings::utf16;
#[cfg(unix)]
use crate::host::FdEvents;
use crate::host::{HostContext, TimerId};
use crate::plugin::{
    ActivateConfig, GUI_EMBEDDING, GuiInfo, GuiParent, HostedPlugin, HostedProcessor, HostingError,
};

/// The configuration of a parameter flush on a plugin that was never activated.
const FLUSH_CONFIG: ActivateConfig = ActivateConfig {
    sample_rate: 48_000.0,
    min_frames: 1,
    max_frames: 512,
};

/// Lists the audio module classes of a `.vst3` bundle. Each component is created and initialised
/// once (not activated, no controller) to read whether it has a sidechain bus (`crate::scan`).
pub(crate) fn scan(bundle: &Path) -> Result<Vec<PluginDescriptor>, HostingError> {
    let module = load_module(bundle)?;
    let factory = module.factory()?;
    let context = HostApplication::new("komponist");
    set_host_context(&factory, &context);
    let classes = audio_classes(&factory, bundle);
    let descriptors = classes
        .into_iter()
        .map(|(cid, descriptor)| PluginDescriptor {
            sidechain: probe_sidechain(&factory, &cid, &context),
            ..descriptor
        })
        .collect();
    drop(factory);
    Ok(descriptors)
}

/// Whether class `cid` has a sidechain input bus: creates its component, initialises it, reads
/// its audio input buses and terminates it again.
fn probe_sidechain(
    factory: &ComPtr<IPluginFactory>,
    cid: &TUID,
    context: &ComWrapper<HostApplication>,
) -> bool {
    let Some(component) = create::<IComponent>(factory, cid) else {
        return false;
    };
    let host = context
        .as_com_ref::<FUnknown>()
        .map_or(std::ptr::null_mut(), |c| c.as_ptr());
    // SAFETY: a fresh component on this (main) thread, with our live host context.
    if unsafe { component.initialize(host) } != kResultOk {
        return false;
    }
    let found = sidechain_bus(&bus_infos(&component, kAudio, kInput)).is_some();
    // SAFETY: initialised above; nothing else holds the component.
    unsafe { component.terminate() };
    found
}

/// The buses of `media` in direction `dir` (main thread, inactive); a bus whose info the plugin
/// refuses reads as 0 channels.
fn bus_infos(component: &ComPtr<IComponent>, media: VstEnum, dir: VstEnum) -> Vec<BusInfo> {
    // SAFETY: main thread, inactive; plain out-structs.
    let count = unsafe { component.getBusCount(enum_i32(media), enum_i32(dir)) };
    (0..count.max(0))
        .map(|index| {
            let mut info: BusInfo = unsafe { std::mem::zeroed() };
            let ok =
                unsafe { component.getBusInfo(enum_i32(media), enum_i32(dir), index, &mut info) }
                    == kResultOk;
            if !ok {
                info.channelCount = 0;
            }
            info
        })
        .collect()
}

/// The main bus: the first flagged `kMain`, else bus 0.
fn main_bus(infos: &[BusInfo]) -> Option<usize> {
    infos
        .iter()
        .position(|b| b.busType == enum_i32(kMain))
        .or((!infos.is_empty()).then_some(0))
}

/// The sidechain bus among audio inputs: the first `kAux` bus with channels that is not the main
/// bus (#6).
fn sidechain_bus(infos: &[BusInfo]) -> Option<usize> {
    let main = main_bus(infos);
    (0..infos.len()).find(|&index| {
        Some(index) != main
            && infos[index].busType == enum_i32(kAux)
            && infos[index].channelCount > 0
    })
}

/// Loads plugin `plugin_id` (a class id as 32 hex digits) from a `.vst3` bundle on the calling
/// (main) thread.
pub(crate) fn load(
    bundle: &Path,
    plugin_id: &str,
    context: HostContext,
) -> Result<Box<dyn HostedPlugin>, HostingError> {
    Ok(Box::new(Vst3Plugin::load(bundle, plugin_id, context)?))
}

/// `IPluginFactory3::setHostContext`, if the factory has it.
fn set_host_context(factory: &ComPtr<IPluginFactory>, context: &ComWrapper<HostApplication>) {
    if let (Some(f3), Some(unknown)) = (
        factory.cast::<IPluginFactory3>(),
        context.as_com_ref::<FUnknown>(),
    ) {
        // SAFETY: our live host object; the factory add-refs it if it keeps it.
        unsafe { f3.setHostContext(unknown.as_ptr()) };
    }
}

/// Creates class `cid` as interface `I`.
fn create<I: Interface>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> Option<ComPtr<I>> {
    let iid: TUID = I::IID.map(|b| b as std::ffi::c_char);
    let mut obj: *mut c_void = std::ptr::null_mut();
    // SAFETY: the factory's createInstance with valid ids and out-pointer.
    let result = unsafe { factory.createInstance(cid.as_ptr(), iid.as_ptr(), &mut obj) };
    if result != kResultOk {
        return None;
    }
    // SAFETY: an owned pointer of interface `I` (or NULL).
    unsafe { ComPtr::from_raw(obj.cast()) }
}

/// An open editor.
struct OpenView {
    view: ComPtr<IPlugView>,
    resizable: bool,
}

/// What exists while a processor does.
struct Active {
    /// Cleared when the processor is dropped.
    alive: Arc<AtomicBool>,
    /// The processor's output parameter changes, for the controller.
    from_processor: rtrb::Consumer<ParamChange>,
}

/// What the plugin told about one parameter, kept for value lookups and texts.
#[derive(Debug, Clone, Default)]
struct ParamExtra {
    units: String,
}

/// One loaded VST3 plugin instance, main-thread half.
pub(crate) struct Vst3Plugin {
    // Plugin objects. Field order is release order; `module` comes last.
    view: Option<OpenView>,
    midi_mapping: Option<ComPtr<IMidiMapping>>,
    unit_info: Option<ComPtr<IUnitInfo>>,
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    controller: Option<ComPtr<IEditController>>,
    audio: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    factory: ComPtr<IPluginFactory>,
    // Host objects.
    frame: ComWrapper<PlugFrame>,
    handler: ComWrapper<ComponentHandler>,
    host_app: ComWrapper<HostApplication>,
    run_loop: Rc<RunLoop>,
    edits: Arc<Edits>,
    midi_map: Arc<MidiMap>,
    midi_mapping_changed: Arc<AtomicBool>,
    // State.
    cid: TUID,
    descriptor: PluginDescriptor,
    context: HostContext,
    /// The controller is its own object (not the component).
    separate_controller: bool,
    /// Event input buses (notes).
    event_inputs: i32,
    has_gui: bool,
    params: RefCell<HashMap<u32, ParamExtra>>,
    render_mode: RenderMode,
    active: Option<Active>,
    /// The configuration of the last activation (for parameter flushes while inactive).
    last_config: Option<ActivateConfig>,
    /// `initialize` succeeded (terminate on drop).
    initialized: (bool, bool),
    module: Arc<Module>,
}

impl Vst3Plugin {
    pub(crate) fn load(
        bundle: &Path,
        plugin_id: &str,
        context: HostContext,
    ) -> Result<Vst3Plugin, HostingError> {
        let cid = parse_cid(plugin_id)
            .ok_or_else(|| HostingError::Load(format!("{plugin_id:?} is not a VST3 class id")))?;
        let module = load_module(bundle)?;
        let factory = module.factory()?;
        let descriptor = audio_classes(&factory, bundle)
            .into_iter()
            .find(|(c, _)| *c == cid)
            .map(|(_, d)| d)
            .ok_or_else(|| {
                HostingError::Load(format!("{}: no plugin {plugin_id}", bundle.display()))
            })?;
        let host_app = HostApplication::new(context.host_name);
        set_host_context(&factory, &host_app);
        let component: ComPtr<IComponent> = create(&factory, &cid).ok_or_else(|| {
            HostingError::Load(format!("{plugin_id}: createInstance(IComponent) failed"))
        })?;
        let audio: ComPtr<IAudioProcessor> = component.cast().ok_or_else(|| {
            HostingError::Load(format!("{plugin_id}: the component has no IAudioProcessor"))
        })?;

        let run_loop = Rc::new(RunLoop::new(
            context.host_loop.clone(),
            context.notify.clone(),
        ));
        let edits = Arc::new(Edits::default());
        let midi_mapping_changed = Arc::new(AtomicBool::new(false));
        let mut plugin = Vst3Plugin {
            view: None,
            midi_mapping: None,
            unit_info: None,
            connection: None,
            controller: None,
            audio,
            component,
            factory,
            frame: PlugFrame::new(run_loop.clone(), context.notify.clone()),
            handler: ComponentHandler::new(
                context.notify.clone(),
                edits.clone(),
                midi_mapping_changed.clone(),
            ),
            host_app,
            run_loop,
            edits,
            midi_map: Arc::new(MidiMap::new()),
            midi_mapping_changed,
            cid,
            descriptor,
            context,
            separate_controller: false,
            event_inputs: 0,
            has_gui: false,
            params: RefCell::new(HashMap::new()),
            render_mode: RenderMode::Realtime,
            active: None,
            last_config: None,
            initialized: (false, false),
            module,
        };
        // From here on, `Drop` cleans up whatever was set up.
        plugin.initialize(plugin_id)?;
        Ok(plugin)
    }

    fn host_context(&self) -> *mut FUnknown {
        self.host_app
            .as_com_ref::<FUnknown>()
            .map_or(std::ptr::null_mut(), |c| c.as_ptr())
    }

    /// `initialize`, controller, connection, state sync, handler, GUI probe.
    fn initialize(&mut self, plugin_id: &str) -> Result<(), HostingError> {
        // SAFETY: a fresh component on the main thread, with our live host context.
        let result = unsafe { self.component.initialize(self.host_context()) };
        if result != kResultOk {
            return Err(HostingError::Load(format!(
                "{plugin_id}: IComponent::initialize returned {result}"
            )));
        }
        self.initialized.0 = true;

        let mut controller_cid: TUID = [0; 16];
        // SAFETY: valid out-pointer.
        let has_class = unsafe { self.component.getControllerClassId(&mut controller_cid) }
            == kResultTrue
            && controller_cid != [0; 16];
        let separate = has_class
            .then(|| create::<IEditController>(&self.factory, &controller_cid))
            .flatten();
        if let Some(controller) = separate {
            // SAFETY: a fresh controller on the main thread.
            let result = unsafe { controller.initialize(self.host_context()) };
            if result != kResultOk {
                return Err(HostingError::Load(format!(
                    "{plugin_id}: IEditController::initialize returned {result}"
                )));
            }
            self.initialized.1 = true;
            self.separate_controller = true;
            self.controller = Some(controller);
        } else {
            // A single-component plugin, or one without a controller at all.
            self.controller = self.component.cast::<IEditController>();
        }

        if self.separate_controller
            && let (Some(a), Some(b)) = (
                self.component.cast::<IConnectionPoint>(),
                self.controller
                    .as_ref()
                    .and_then(|c| c.cast::<IConnectionPoint>()),
            )
        {
            // SAFETY: both live; the SDK connects in both directions.
            unsafe {
                a.connect(b.as_ptr());
                b.connect(a.as_ptr());
            }
            self.connection = Some((a, b));
        }

        // Host duty: mirror the component state into the controller.
        if self.separate_controller {
            self.sync_controller_from_component();
        }

        if let Some(controller) = &self.controller {
            let handler = self
                .handler
                .as_com_ref::<IComponentHandler>()
                .map_or(std::ptr::null_mut(), |h| h.as_ptr());
            // SAFETY: our live handler; the controller add-refs it.
            unsafe { controller.setComponentHandler(handler) };
            self.midi_mapping = controller.cast::<IMidiMapping>();
            self.unit_info = controller.cast::<IUnitInfo>();
        }
        // SAFETY: plain getter on the main thread.
        self.event_inputs = unsafe {
            self.component
                .getBusCount(enum_i32(kEvent), enum_i32(kInput))
        };
        self.descriptor.sidechain = sidechain_bus(&self.bus_infos(kAudio, kInput)).is_some();
        // Not even probed where no GUI is embedded: JUCE builds its whole editor in `createView`.
        self.has_gui = GUI_EMBEDDING && self.probe_gui();
        self.refresh_params();
        Ok(())
    }

    /// Hands the component's state to the controller (`setComponentState`).
    fn sync_controller_from_component(&self) {
        let Some(controller) = &self.controller else {
            return;
        };
        // SAFETY: main thread; the stream lives for the call.
        let state = stream::write_with(|s| unsafe { self.component.getState(s) });
        match state {
            Ok(bytes) => {
                stream::read_with(&bytes, |s| unsafe { controller.setComponentState(s) });
            }
            Err(result) => self.log(
                LogLevel::Debug,
                &format!("IComponent::getState for the controller sync returned {result}"),
            ),
        }
    }

    /// Whether the controller creates an editor for [`PLATFORM_TYPE`]. The view is released again.
    fn probe_gui(&self) -> bool {
        let Some(view) = self.create_view() else {
            return false;
        };
        // SAFETY: a fresh view, never attached.
        unsafe { view.isPlatformTypeSupported(PLATFORM_TYPE) == kResultTrue }
    }

    fn create_view(&self) -> Option<ComPtr<IPlugView>> {
        let controller = self.controller.as_ref()?;
        // SAFETY: main thread; createView returns an owned view or NULL.
        unsafe { ComPtr::from_raw(controller.createView(ViewType::kEditor)) }
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.context.notify.log(level, message);
    }

    /// Re-reads parameter ids and units.
    fn refresh_params(&self) -> Vec<ParameterInfo> {
        let infos = self.parameter_infos();
        *self.params.borrow_mut() = infos
            .iter()
            .map(|i| {
                (
                    i.id,
                    ParamExtra {
                        units: utf16(&i.units),
                    },
                )
            })
            .collect();
        infos
    }

    fn parameter_infos(&self) -> Vec<ParameterInfo> {
        let Some(controller) = &self.controller else {
            return Vec::new();
        };
        // SAFETY: main thread; plain out-structs.
        let count = unsafe { controller.getParameterCount() };
        (0..count.max(0))
            .filter_map(|index| {
                let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
                (unsafe { controller.getParameterInfo(index, &mut info) } == kResultOk)
                    .then_some(info)
            })
            .collect()
    }

    /// Unit id → path ("Filter/Env"), from `IUnitInfo`; the root unit has no name in the path.
    fn unit_paths(&self) -> HashMap<i32, String> {
        let Some(units) = &self.unit_info else {
            return HashMap::new();
        };
        // SAFETY: main thread; plain out-structs.
        let count = unsafe { units.getUnitCount() };
        let infos: HashMap<i32, (i32, String)> = (0..count.max(0))
            .filter_map(|index| {
                let mut info: UnitInfo = unsafe { std::mem::zeroed() };
                (unsafe { units.getUnitInfo(index, &mut info) } == kResultOk)
                    .then(|| (info.id, (info.parentUnitId, utf16(&info.name))))
            })
            .collect();
        infos
            .keys()
            .map(|&id| {
                let mut parts = Vec::new();
                let mut at = id;
                // Units nest a few levels; the bound guards against cycles.
                for _ in 0..16 {
                    match infos.get(&at) {
                        Some((parent, name)) if at > 0 => {
                            parts.push(name.clone());
                            at = *parent;
                        }
                        _ => break,
                    }
                }
                parts.reverse();
                (id, parts.join("/"))
            })
            .collect()
    }

    /// Applies the processor's output parameter changes to the controller.
    fn drain_from_processor(&mut self) {
        let (Some(active), Some(controller)) = (self.active.as_mut(), self.controller.as_ref())
        else {
            return;
        };
        while let Ok((id, value)) = active.from_processor.pop() {
            self.edits.mark_controller_sync(id);
            // SAFETY: main thread.
            unsafe { controller.setParamNormalized(id, value) };
        }
    }

    fn bus_infos(&self, media: VstEnum, dir: VstEnum) -> Vec<BusInfo> {
        bus_infos(&self.component, media, dir)
    }

    /// Asks for stereo main buses (keeping the other buses' arrangements) and returns the audio
    /// layouts the plugin settled on.
    fn arrange_buses(&self) -> (BusLayout, BusLayout) {
        let arrangements = |dir: VstEnum, count: usize, main: Option<usize>| {
            (0..count)
                .map(|index| {
                    let mut arr: SpeakerArrangement = 0;
                    // SAFETY: main thread, inactive.
                    unsafe {
                        self.audio
                            .getBusArrangement(enum_i32(dir), index as i32, &mut arr)
                    };
                    if Some(index) == main { kStereo } else { arr }
                })
                .collect::<Vec<SpeakerArrangement>>()
        };
        let ins = self.bus_infos(kAudio, kInput);
        let outs = self.bus_infos(kAudio, kOutput);
        let mut in_arr = arrangements(kInput, ins.len(), main_bus(&ins));
        let mut out_arr = arrangements(kOutput, outs.len(), main_bus(&outs));
        // SAFETY: main thread, inactive; the arrays hold the counts given.
        let result = unsafe {
            self.audio.setBusArrangements(
                in_arr.as_mut_ptr(),
                in_arr.len() as i32,
                out_arr.as_mut_ptr(),
                out_arr.len() as i32,
            )
        };
        if result != kResultOk {
            self.log(
                LogLevel::Debug,
                "VST3: setBusArrangements(stereo) declined, adapting to the plugin's layout",
            );
        }
        let layout = |infos: Vec<BusInfo>, is_input: bool| BusLayout {
            main: main_bus(&infos),
            sidechain: if is_input {
                sidechain_bus(&infos)
            } else {
                None
            },
            channels: infos.iter().map(|b| b.channelCount.max(0) as u32).collect(),
        };
        (
            layout(self.bus_infos(kAudio, kInput), true),
            layout(self.bus_infos(kAudio, kOutput), false),
        )
    }

    /// Activates the main audio buses, the sidechain input, the first event input and every bus
    /// the plugin marks default-active.
    fn activate_buses(&self, inputs: &BusLayout, outputs: &BusLayout) {
        for (media, dir, main, sidechain) in [
            (kAudio, kInput, inputs.main, inputs.sidechain),
            (kAudio, kOutput, outputs.main, None),
            (kEvent, kInput, Some(0), None),
            (kEvent, kOutput, None, None),
        ] {
            for (index, info) in self.bus_infos(media, dir).iter().enumerate() {
                if Some(index) == main
                    || Some(index) == sidechain
                    || info.flags & enum_u32(kDefaultActive) != 0
                {
                    // SAFETY: main thread, inactive.
                    unsafe {
                        self.component
                            .activateBus(enum_i32(media), enum_i32(dir), index as i32, 1)
                    };
                }
            }
        }
    }

    /// Activates and returns the processor (the body of [`HostedPlugin::activate`]).
    fn start(&mut self, config: ActivateConfig) -> Result<Vst3Processor, HostingError> {
        if self.active.is_some() {
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
        let process_mode = enum_i32(match self.render_mode {
            RenderMode::Realtime => kRealtime,
            RenderMode::Offline => kOffline,
        });
        let (inputs, outputs) = self.arrange_buses();
        // SAFETY: main thread, inactive.
        if unsafe { self.audio.canProcessSampleSize(enum_i32(kSample32)) } != kResultOk {
            return Err(HostingError::Activate(
                "the plugin cannot process 32-bit samples".into(),
            ));
        }
        let mut setup = ProcessSetup {
            processMode: process_mode,
            symbolicSampleSize: enum_i32(kSample32),
            maxSamplesPerBlock: config.max_frames as i32,
            sampleRate: config.sample_rate,
        };
        // SAFETY: main thread, inactive.
        let result = unsafe { self.audio.setupProcessing(&mut setup) };
        if result != kResultOk {
            return Err(HostingError::Activate(format!(
                "setupProcessing returned {result}"
            )));
        }
        self.activate_buses(&inputs, &outputs);
        self.midi_map.refresh(self.midi_mapping.as_ref());
        // SAFETY: main thread; set up as the SDK's workflow requires.
        let result = unsafe { self.component.setActive(1) };
        if result != kResultOk {
            return Err(HostingError::Activate(format!(
                "setActive(true) returned {result}"
            )));
        }

        let (to_processor, from_main) = rtrb::RingBuffer::new(EDIT_RING);
        let (to_controller, from_processor) = rtrb::RingBuffer::new(TO_CONTROLLER_RING);
        let (initial_generation, initial) = self.edits.connect(to_processor);
        let alive = Arc::new(AtomicBool::new(true));
        let processor = Vst3Processor::new(ProcessorSetup {
            audio: self.audio.clone(),
            module: self.module.clone(),
            inputs,
            outputs,
            notes: self.event_inputs > 0,
            process_mode,
            sample_rate: config.sample_rate,
            max_frames: config.max_frames,
            notify: self.context.notify.clone(),
            midi_map: self.midi_map.clone(),
            edits: self.edits.clone(),
            from_main,
            initial_generation,
            initial,
            to_controller,
            alive: alive.clone(),
        });
        self.active = Some(Active {
            alive,
            from_processor,
        });
        self.last_config = Some(config);
        Ok(processor)
    }

    /// `setActive(false)` once the processor is gone (the rest of [`HostedPlugin::deactivate`]).
    fn finish_deactivate(&mut self) {
        self.drain_from_processor();
        let Some(active) = self.active.take() else {
            return;
        };
        if active.alive.load(Ordering::Acquire) {
            self.log(
                LogLevel::Error,
                "deactivate: another plugin's processor was handed back",
            );
            self.active = Some(active);
            return;
        }
        // SAFETY: main thread; no processor exists any more.
        unsafe { self.component.setActive(0) };
    }

    /// While inactive: hands changes made since the last processing (`flush_params`, GUI edits)
    /// to the component through a parameter flush (a `process` call without audio, VST3 FAQ), so
    /// that its state includes them. Activates briefly at the last configuration.
    fn flush_pending(&mut self) {
        if self.active.is_some() || !self.edits.has_pending() {
            return;
        }
        let config = self.last_config.unwrap_or(FLUSH_CONFIG);
        match self.start(config) {
            Ok(mut processor) => {
                processor.flush();
                drop(processor);
                self.finish_deactivate();
            }
            Err(e) => self.log(
                LogLevel::Warning,
                &format!("VST3: activation for a parameter flush failed: {e}"),
            ),
        }
    }

    fn require_view(&self) -> Result<&OpenView, HostingError> {
        self.view
            .as_ref()
            .ok_or(HostingError::WrongState("the GUI is not open"))
    }

    fn view_size(view: &ComPtr<IPlugView>) -> Result<GuiSize, HostingError> {
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: an open view on the main thread.
        if unsafe { view.getSize(&mut rect) } != kResultOk {
            return Err(HostingError::Gui("getSize failed".into()));
        }
        Ok(GuiSize {
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
        })
    }
}

fn param_info(info: &ParameterInfo, module: String) -> ParamInfo {
    let f = info.flags;
    let mut flags = 0;
    let step_count = info.stepCount.max(0) as u32;
    if step_count > 0 {
        flags |= param_flags::STEPPED;
    }
    for (vst3, ours) in [
        (kIsBypass, param_flags::BYPASS),
        (kIsHidden, param_flags::HIDDEN),
        (kIsReadOnly, param_flags::READ_ONLY),
        (kCanAutomate, param_flags::AUTOMATABLE),
        (kIsList, param_flags::ENUM),
    ] {
        if f & vst3 != 0 {
            flags |= ours;
        }
    }
    ParamInfo {
        id: info.id,
        name: utf16(&info.title),
        module,
        min: 0.0,
        max: 1.0,
        default: info.defaultNormalizedValue.clamp(0.0, 1.0),
        step_count,
        flags,
    }
}

impl HostedPlugin for Vst3Plugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn has_gui(&self) -> bool {
        self.has_gui
    }

    fn activate(
        &mut self,
        config: ActivateConfig,
    ) -> Result<Box<dyn HostedProcessor>, HostingError> {
        Ok(Box::new(self.start(config)?))
    }

    fn deactivate(&mut self, processor: Box<dyn HostedProcessor>) {
        // Dropping the processor stops it if needed and hands unapplied edits back.
        drop(processor);
        self.finish_deactivate();
    }

    fn is_active(&self) -> bool {
        self.active.is_some()
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
        let infos = self.refresh_params();
        let units = self.unit_paths();
        infos
            .iter()
            .map(|i| param_info(i, units.get(&i.unitId).cloned().unwrap_or_default()))
            .collect()
    }

    fn param_value(&self, id: u32) -> Option<f64> {
        let controller = self.controller.as_ref()?;
        if !self.params.borrow().contains_key(&id) {
            return None;
        }
        // SAFETY: main thread.
        Some(unsafe { controller.getParamNormalized(id) })
    }

    fn param_text(&self, id: u32, value: f64) -> Option<String> {
        let controller = self.controller.as_ref()?;
        let units = self.params.borrow().get(&id)?.units.clone();
        let mut text: String128 = [0; 128];
        // SAFETY: main thread; a String128 to fill.
        if unsafe { controller.getParamStringByValue(id, value.clamp(0.0, 1.0), &mut text) }
            != kResultOk
        {
            return None;
        }
        let text = utf16(&text);
        let units = units.trim();
        // nice-plug puts the unit into the text already, JUCE plugins do not.
        Some(if units.is_empty() || text.trim_end().ends_with(units) {
            text
        } else {
            format!("{} {units}", text.trim_end())
        })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(controller) = &self.controller {
            // SAFETY: main thread.
            unsafe { controller.setParamNormalized(id, value.clamp(0.0, 1.0)) };
        }
    }

    fn sync_controller_param(&mut self, id: u32, value: f64) {
        self.edits.mark_controller_sync(id);
        self.set_param(id, value);
    }

    fn flush_params(&mut self, input: &[WireEvent], _output: &mut Vec<WireEvent>) {
        // VST3 has no flush with output: the controller takes the values now, the processor in
        // the next processed block (see the module docs).
        for wire in input {
            if let Some(WireEventKind::ParamValue { id, value }) = wire.decode() {
                let value = value.clamp(0.0, 1.0);
                self.set_param(id, value);
                if !self.edits.push(Edit::Set(id, value)) {
                    self.log(
                        LogLevel::Warning,
                        "VST3: parameter ring full, a flushed value was dropped",
                    );
                }
            }
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>, HostingError> {
        self.flush_pending();
        // SAFETY: main thread; the stream lives for the call.
        let component = stream::write_with(|s| unsafe { self.component.getState(s) })
            .map_err(|r| HostingError::State(format!("IComponent::getState returned {r}")))?;
        let controller = match (&self.controller, self.separate_controller) {
            (Some(controller), true) => {
                // SAFETY: as above.
                stream::write_with(|s| unsafe { controller.getState(s) }).unwrap_or_else(|r| {
                    self.log(
                        LogLevel::Debug,
                        &format!("IEditController::getState returned {r}; saved without"),
                    );
                    Vec::new()
                })
            }
            _ => Vec::new(),
        };
        Ok(stream::encode_state(&self.cid, &component, &controller))
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), HostingError> {
        let parts = stream::decode_state(state).map_err(HostingError::State)?;
        if parts.cid != self.cid {
            return Err(HostingError::State(format!(
                "the state belongs to plugin {}, not {}",
                module::cid_hex(&parts.cid),
                self.descriptor.id
            )));
        }
        // SAFETY: main thread (allowed while active, VST3 threading model).
        let result = stream::read_with(parts.component, |s| unsafe { self.component.setState(s) });
        if result != kResultOk {
            return Err(HostingError::State(format!(
                "IComponent::setState returned {result}"
            )));
        }
        // A loaded state supersedes queued edits, whether the processor is active or not.
        self.edits.clear_pending();
        if self.separate_controller
            && let Some(controller) = &self.controller
        {
            // Host duty: the controller follows the component, then gets its own state.
            stream::read_with(parts.component, |s| unsafe {
                controller.setComponentState(s)
            });
            if !parts.controller.is_empty() {
                let result =
                    stream::read_with(parts.controller, |s| unsafe { controller.setState(s) });
                if result != kResultOk {
                    self.log(
                        LogLevel::Warning,
                        &format!("IEditController::setState returned {result}"),
                    );
                }
            }
        }
        Ok(())
    }

    fn latency(&self) -> u32 {
        if self.active.is_none() {
            return 0;
        }
        // SAFETY: main thread, active (VST3: UI thread, any state after setup).
        unsafe { self.audio.getLatencySamples() }
    }

    fn note_names(&self) -> Vec<NoteName> {
        // VST3 names notes only through IUnitInfo program pitch names; none of komponist's
        // targets use them.
        Vec::new()
    }

    fn gui_open(&mut self, parent: GuiParent) -> Result<GuiInfo, HostingError> {
        if self.view.is_some() {
            return Err(HostingError::WrongState("the GUI is already open"));
        }
        if !self.has_gui {
            return Err(HostingError::Gui("the plugin has no embeddable GUI".into()));
        }
        let (window, scale): (*mut c_void, Option<f64>) = match parent {
            #[cfg(x11_embed)]
            GuiParent::X11 { xid, scale } => (xid as *mut c_void, Some(scale)),
            #[cfg(target_os = "macos")]
            GuiParent::Cocoa(view) => (view, None),
            #[cfg(windows)]
            GuiParent::Win32 { hwnd, scale } => (hwnd, Some(scale)),
            _ => {
                return Err(HostingError::Gui(
                    "the parent window is of another platform".into(),
                ));
            }
        };
        let view = self
            .create_view()
            .ok_or_else(|| HostingError::Gui("createView returned no view".into()))?;
        // SAFETY: a fresh view on the main thread; the frame outlives it (we own both).
        unsafe {
            if view.isPlatformTypeSupported(PLATFORM_TYPE) != kResultTrue {
                return Err(HostingError::Gui("embedding not supported".into()));
            }
            let frame = self
                .frame
                .as_com_ref::<IPlugFrame>()
                .map_or(std::ptr::null_mut(), |f| f.as_ptr());
            view.setFrame(frame);
            if let Some(scale) = scale
                && let Some(scaling) = view.cast::<IPlugViewContentScaleSupport>()
            {
                let result = scaling.setContentScaleFactor(scale as f32);
                if result != kResultOk {
                    self.log(
                        LogLevel::Debug,
                        &format!("setContentScaleFactor({scale}) returned {result}"),
                    );
                }
            }
            // The parent is a live X11 window of the app's frame, or an `NSView` / `HWND` of the
            // bridge's frame; either is kept until after `gui_close` (or the bridge's death).
            let result = view.attached(window, PLATFORM_TYPE);
            if result != kResultOk {
                view.setFrame(std::ptr::null_mut());
                return Err(HostingError::Gui(format!("attached returned {result}")));
            }
        }
        let size = match Self::view_size(&view) {
            Ok(size) => size,
            Err(e) => {
                // SAFETY: attached above.
                unsafe {
                    view.removed();
                    view.setFrame(std::ptr::null_mut());
                }
                return Err(e);
            }
        };
        // SAFETY: main thread.
        let resizable = unsafe { view.canResize() } == kResultTrue;
        self.view = Some(OpenView { view, resizable });
        Ok(GuiInfo { size, resizable })
    }

    fn gui_set_size(&mut self, size: GuiSize) -> Result<GuiSize, HostingError> {
        let open = self.require_view()?;
        if !open.resizable {
            // A fixed-size editor keeps its size.
            return Self::view_size(&open.view);
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: size.width.min(i32::MAX as u32) as i32,
            bottom: size.height.min(i32::MAX as u32) as i32,
        };
        // SAFETY: an open view on the main thread; `rect` lives for the calls.
        unsafe {
            open.view.checkSizeConstraint(&mut rect);
            let result = open.view.onSize(&mut rect);
            if result != kResultOk {
                return Err(HostingError::Gui(format!("onSize returned {result}")));
            }
        }
        Self::view_size(&open.view)
    }

    fn gui_set_scale(&mut self, scale: f64) -> bool {
        let Ok(open) = self.require_view() else {
            return false;
        };
        // SAFETY: an open view on the main thread.
        unsafe {
            open.view
                .cast::<IPlugViewContentScaleSupport>()
                .is_some_and(|scaling| scaling.setContentScaleFactor(scale as f32) == kResultOk)
        }
    }

    fn gui_close(&mut self) {
        let Some(open) = self.view.take() else {
            return;
        };
        // SAFETY: an attached view on the main thread.
        unsafe {
            open.view.removed();
            open.view.setFrame(std::ptr::null_mut());
        }
    }

    fn on_main_thread(&mut self) {
        self.drain_from_processor();
        if self.midi_mapping_changed.swap(false, Ordering::AcqRel) {
            self.midi_map.refresh(self.midi_mapping.as_ref());
        }
    }

    fn on_timer(&mut self, timer: TimerId) {
        if self.run_loop.owns_timer(timer) {
            self.run_loop.dispatch_timer(timer);
        }
    }

    #[cfg(unix)]
    fn on_fd(&mut self, fd: RawFd, events: FdEvents) {
        if self.run_loop.owns_fd(fd) {
            self.run_loop.dispatch_fd(fd, events);
        }
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.gui_close();
        let processor_alive = self
            .active
            .as_ref()
            .is_some_and(|a| a.alive.load(Ordering::Acquire));
        if processor_alive {
            // The processor still holds the component; it is released (without terminate) when
            // the processor drops, and the module stays loaded until then.
            self.log(
                LogLevel::Error,
                "plugin dropped while its processor is still alive",
            );
        } else {
            if self.active.take().is_some() {
                self.drain_from_processor();
                // SAFETY: main thread; no processor exists.
                unsafe { self.component.setActive(0) };
            }
            if let Some((a, b)) = self.connection.take() {
                // SAFETY: connected in `initialize`.
                unsafe {
                    a.disconnect(b.as_ptr());
                    b.disconnect(a.as_ptr());
                }
            }
            // SAFETY: main thread; the SDK terminates the controller first.
            unsafe {
                if self.initialized.1
                    && let Some(controller) = &self.controller
                {
                    controller.terminate();
                }
                if self.initialized.0 {
                    self.component.terminate();
                }
            }
        }
        // Handlers point into the plugin library: release them before the module goes.
        self.run_loop.clear();
        // The remaining COM pointers drop with the fields, `module` last.
    }
}
