//! The host side of a VST3 instance: the COM objects komponist hands to plugins.
//!
//! - [`HostApplication`] (`IHostApplication`, `IPlugInterfaceSupport`): the context for
//!   `initialize` and `IPluginFactory3::setHostContext`. `createInstance` makes [`HostMessage`] and
//!   [`HostAttributeList`] objects, which plugins use to talk between component and controller
//!   through `IConnectionPoint`. Thread-safe.
//! - [`ComponentHandler`] (`IComponentHandler`, `IComponentHandler2`): GUI edits and restart
//!   requests from the controller. Edits go to the processor through [`Edits`] and to the bridge
//!   as [`HostNotification::ParamEdited`]. Thread-safe (plugins should call it on the main thread).
//! - [`PlugFrame`] (`IPlugFrame`, `Linux::IRunLoop` on one object, as the spike did): resize
//!   requests of an open view and the plugin's fds and timers, served by the bridge's
//!   [`HostLoop`] through [`RunLoop`]. Main thread only. Windows offers no `IRunLoop` (port W5:
//!   plugins there pump their own window messages and timers), so `RunLoop` stays empty there.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_void};
#[cfg(unix)]
use std::os::fd::{BorrowedFd, RawFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use komponist_ipc::{GuiSize, LogLevel};
#[cfg(unix)]
use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, IRunLoop, IRunLoopTrait, TimerInterval,
};
use vst3::Steinberg::Linux::{ITimerHandler, ITimerHandlerTrait};
use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::RestartFlags_::{
    kIoChanged, kLatencyChanged, kMidiCCAssignmentChanged, kParamIDMappingChanged,
    kParamTitlesChanged, kParamValuesChanged, kReloadComponent,
};
use vst3::Steinberg::Vst::{
    IAttributeList, IAttributeListTrait, IAudioProcessor, IComponent, IComponentHandler,
    IComponentHandler2, IComponentHandler2Trait, IComponentHandlerTrait, IConnectionPoint,
    IEditController, IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait,
    IMidiMapping, IPlugInterfaceSupport, IPlugInterfaceSupportTrait, IUnitInfo, ParamID,
    ParamValue, String128, TChar,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewContentScaleSupport,
    IPlugViewTrait, TBool, TUID, ViewRect, int32, int64, tresult, uint32,
};
use vst3::Steinberg::{kInvalidArgument, kNoInterface, kResultFalse, kResultOk, kResultTrue};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

use super::strings::{utf16_ptr, write_utf16};
#[cfg(unix)]
use crate::host::FdEvents;
use crate::host::{HostLoop, HostNotification, HostNotify, TimerId};

/// Capacity of the main-thread → processor parameter ring.
pub(crate) const EDIT_RING: usize = 1024;

/// A parameter change: id and normalized value.
pub(crate) type ParamChange = (u32, f64);

/// A main-thread change on its way to the processor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Edit {
    /// A value from the app (`set_param` / `flush_params`): applied, not echoed back.
    Set(u32, f64),
    /// A GUI edit (`performEdit`): applied, and echoed as a `ParamValue` output event.
    Perform(u32, f64),
    /// A GUI gesture (`beginEdit` = true / `endEdit` = false), echoed as a `ParamGesture` output
    /// event.
    Gesture(u32, bool),
}

/// An edit is valid only until the next successful component state load.
pub(crate) struct QueuedEdit {
    pub(crate) generation: u64,
    pub(crate) edit: Edit,
}

/// Whether the interface id behind `iid` is `I`'s.
///
/// # Safety
/// `iid` is NULL or points to 16 bytes.
unsafe fn is_iid<I: Interface>(iid: *const TUID) -> bool {
    // SAFETY: caller's contract; a TUID and a Guid are both 16 bytes.
    !iid.is_null() && unsafe { *iid.cast::<[u8; 16]>() } == I::IID
}

/// `queryInterface` on an object we created, for an out-parameter.
fn query_into<C: Class>(
    object: &ComWrapper<C>,
    iid: *const TUID,
    obj: *mut *mut c_void,
) -> tresult {
    let Some(unknown) = object.as_com_ref::<FUnknown>() else {
        return kNoInterface;
    };
    let ptr = unknown.as_ptr();
    // SAFETY: a live object of ours; queryInterface add-refs what it returns.
    unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid, obj) }
}

// ------------------------------------------------------------------------------------------------
// IHostApplication
// ------------------------------------------------------------------------------------------------

/// The host context for `initialize` and the factory.
pub(crate) struct HostApplication {
    name: String,
}

impl Class for HostApplication {
    type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

impl HostApplication {
    pub(crate) fn new(name: &str) -> ComWrapper<HostApplication> {
        ComWrapper::new(HostApplication {
            name: name.to_owned(),
        })
    }
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes a String128 to fill.
        write_utf16(&self.name, unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: valid out-pointer (checked for NULL).
        unsafe { *obj = std::ptr::null_mut() };
        // SAFETY: the plugin passes TUIDs (or NULL).
        if unsafe { is_iid::<IMessage>(cid) } {
            query_into(&ComWrapper::new(HostMessage::default()), iid, obj)
        } else if unsafe { is_iid::<IAttributeList>(cid) } {
            query_into(&ComWrapper::new(HostAttributeList::default()), iid, obj)
        } else {
            kNoInterface
        }
    }
}

impl IPlugInterfaceSupportTrait for HostApplication {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        // SAFETY: the plugin passes a TUID (or NULL).
        let supported = unsafe {
            is_iid::<IComponent>(iid)
                || is_iid::<IAudioProcessor>(iid)
                || is_iid::<IEditController>(iid)
                || is_iid::<IConnectionPoint>(iid)
                || is_iid::<IUnitInfo>(iid)
                || is_iid::<IMidiMapping>(iid)
                || is_iid::<IPlugView>(iid)
                || is_iid::<IPlugViewContentScaleSupport>(iid)
        };
        if supported { kResultTrue } else { kResultFalse }
    }
}

// ------------------------------------------------------------------------------------------------
// IMessage, IAttributeList
// ------------------------------------------------------------------------------------------------

/// A message for `IConnectionPoint::notify`, created through `IHostApplication::createInstance`.
pub(crate) struct HostMessage {
    id: Mutex<Option<CString>>,
    attributes: ComWrapper<HostAttributeList>,
}

impl Default for HostMessage {
    fn default() -> Self {
        HostMessage {
            id: Mutex::new(None),
            attributes: ComWrapper::new(HostAttributeList::default()),
        }
    }
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        let id = self.id.lock().unwrap_or_else(PoisonError::into_inner);
        // The pointer stays valid until the next `setMessageID` (as in the SDK).
        id.as_ref().map_or(std::ptr::null(), |id| id.as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let new = (!id.is_null()).then(|| {
            // SAFETY: a NUL-terminated C string from the plugin.
            unsafe { CStr::from_ptr(id) }.to_owned()
        });
        *self.id.lock().unwrap_or_else(PoisonError::into_inner) = new;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Like the SDK's HostMessage: no reference is added; the list lives as long as the message.
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |list| list.as_ptr())
    }
}

/// A value in a [`HostAttributeList`].
#[derive(Debug, Clone, PartialEq)]
enum AttrValue {
    Int(i64),
    Float(f64),
    /// UTF-16 without the terminating NUL.
    String(Vec<u16>),
    Binary(Vec<u8>),
}

/// Attributes of a [`HostMessage`] (or a standalone list).
#[derive(Default)]
pub(crate) struct HostAttributeList {
    values: Mutex<HashMap<Vec<u8>, AttrValue>>,
}

impl Class for HostAttributeList {
    type Interfaces = (IAttributeList,);
}

impl HostAttributeList {
    fn key(id: AttrID) -> Option<Vec<u8>> {
        // SAFETY: attribute ids are NUL-terminated C strings.
        (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_bytes().to_vec())
    }

    fn set(&self, id: AttrID, value: AttrValue) -> tresult {
        let Some(key) = Self::key(id) else {
            return kInvalidArgument;
        };
        self.values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, value);
        kResultOk
    }

    fn with<R>(&self, id: AttrID, read: impl FnOnce(&AttrValue) -> Option<R>) -> Option<R> {
        let key = Self::key(id)?;
        let values = self.values.lock().unwrap_or_else(PoisonError::into_inner);
        values.get(&key).and_then(read)
    }
}

impl IAttributeListTrait for HostAttributeList {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        self.set(id, AttrValue::Int(value))
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        match self.with(id, |v| match v {
            AttrValue::Int(i) => Some(*i),
            _ => None,
        }) {
            Some(i) if !value.is_null() => {
                // SAFETY: valid out-pointer (checked for NULL).
                unsafe { *value = i };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        self.set(id, AttrValue::Float(value))
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        match self.with(id, |v| match v {
            AttrValue::Float(f) => Some(*f),
            _ => None,
        }) {
            Some(f) if !value.is_null() => {
                // SAFETY: valid out-pointer (checked for NULL).
                unsafe { *value = f };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        // SAFETY: a NUL-terminated UTF-16 string from the plugin.
        let units = unsafe { utf16_ptr(string, 1 << 20) };
        self.set(id, AttrValue::String(units))
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        let room = size_in_bytes as usize / size_of::<TChar>();
        if string.is_null() || room == 0 {
            return kInvalidArgument;
        }
        let copied = self.with(id, |v| match v {
            AttrValue::String(units) => {
                let n = units.len().min(room - 1);
                // SAFETY: the plugin's buffer holds `room` units.
                unsafe {
                    std::ptr::copy_nonoverlapping(units.as_ptr(), string, n);
                    *string.add(n) = 0;
                }
                Some(())
            }
            _ => None,
        });
        if copied.is_some() {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: uint32) -> tresult {
        let bytes = if data.is_null() || size_in_bytes == 0 {
            Vec::new()
        } else {
            // SAFETY: the plugin's buffer holds `size_in_bytes` bytes.
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size_in_bytes as usize) }
                .to_vec()
        };
        self.set(id, AttrValue::Binary(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        // The pointer stays valid until the attribute is replaced or the list is released (the
        // vector's heap buffer does not move when the map rehashes).
        match self.with(id, |v| match v {
            AttrValue::Binary(bytes) => Some((bytes.as_ptr(), bytes.len())),
            _ => None,
        }) {
            Some((ptr, len)) => {
                // SAFETY: valid out-pointers (checked for NULL).
                unsafe {
                    *data = ptr.cast();
                    *size_in_bytes = len as uint32;
                }
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Parameter edits towards the processor
// ------------------------------------------------------------------------------------------------

/// Where parameter changes made on the main thread (GUI edits, `flush_params`) go: into the
/// active processor's ring, or, while inactive, into a pending list of values that the next
/// processor applies in its first block (gestures are dropped while inactive).
#[derive(Default)]
pub(crate) struct Edits {
    inner: Mutex<EditsInner>,
    generation: AtomicU64,
}

#[derive(Default)]
struct EditsInner {
    to_processor: Option<rtrb::Producer<QueuedEdit>>,
    pending: Vec<ParamChange>,
    gestures: HashSet<u32>,
    controller_sync: HashMap<u32, Instant>,
}

impl EditsInner {
    fn keep_pending(&mut self, (id, value): ParamChange) {
        match self.pending.iter_mut().find(|(p, _)| *p == id) {
            Some(slot) => slot.1 = value,
            None => self.pending.push((id, value)),
        }
    }
}

impl Edits {
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn mark_controller_sync(&self, id: u32) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .controller_sync
            .insert(id, Instant::now());
    }

    /// A controller may echo a host value through performEdit without opening a gesture.
    fn controller_echo(&self, id: u32) -> bool {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        !inner.gestures.contains(&id)
            && inner
                .controller_sync
                .get(&id)
                .is_some_and(|sent| sent.elapsed() < Duration::from_millis(50))
    }

    /// Queues a change; `false` if the processor's ring is full (the change is lost).
    pub(crate) fn push(&self, edit: Edit) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Edit::Gesture(id, begin) = edit {
            if begin {
                inner.gestures.insert(id);
            } else {
                inner.gestures.remove(&id);
            }
        }
        match (inner.to_processor.as_mut(), edit) {
            (Some(ring), _) => ring
                .push(QueuedEdit {
                    generation: self.generation.load(Ordering::Relaxed),
                    edit,
                })
                .is_ok(),
            (None, Edit::Set(id, value) | Edit::Perform(id, value)) => {
                inner.keep_pending((id, value));
                true
            }
            (None, Edit::Gesture(..)) => true,
        }
    }

    /// Activation: changes go to `ring` from now on; returns the pending ones.
    pub(crate) fn connect(&self, ring: rtrb::Producer<QueuedEdit>) -> (u64, Vec<ParamChange>) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.to_processor = Some(ring);
        (
            self.generation.load(Ordering::Relaxed),
            std::mem::take(&mut inner.pending),
        )
    }

    /// Deactivation: changes the processor did not apply become pending again.
    pub(crate) fn disconnect(&self, leftover: impl IntoIterator<Item = ParamChange>) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.to_processor = None;
        let newer = std::mem::take(&mut inner.pending);
        for change in leftover.into_iter().chain(newer) {
            inner.keep_pending(change);
        }
    }

    /// Whether changes wait for the next processor.
    pub(crate) fn has_pending(&self) -> bool {
        !self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .is_empty()
    }

    /// Forgets pending changes (a state load supersedes them).
    pub(crate) fn clear_pending(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.pending.clear();
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Pending changes, for tests.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> Vec<ParamChange> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .clone()
    }
}

// ------------------------------------------------------------------------------------------------
// IComponentHandler
// ------------------------------------------------------------------------------------------------

/// Receives the controller's edits and restart requests.
pub(crate) struct ComponentHandler {
    notify: Arc<dyn HostNotify>,
    edits: Arc<Edits>,
    /// Set by `restartComponent(kMidiCCAssignmentChanged)`; the plugin re-reads its MIDI mapping in
    /// the main-thread callback this requests.
    midi_mapping_changed: Arc<AtomicBool>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl ComponentHandler {
    pub(crate) fn new(
        notify: Arc<dyn HostNotify>,
        edits: Arc<Edits>,
        midi_mapping_changed: Arc<AtomicBool>,
    ) -> ComWrapper<ComponentHandler> {
        ComWrapper::new(ComponentHandler {
            notify,
            edits,
            midi_mapping_changed,
        })
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.edits.push(Edit::Gesture(id, true));
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        if self.edits.controller_echo(id) {
            return kResultOk;
        }
        let value = value.clamp(0.0, 1.0);
        if !self.edits.push(Edit::Perform(id, value)) {
            self.notify.log(
                LogLevel::Warning,
                "VST3: parameter ring full, a GUI edit did not reach the processor",
            );
        }
        self.notify
            .notify(HostNotification::ParamEdited { id, value });
        self.notify.notify(HostNotification::StateDirty);
        kResultOk
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.edits.push(Edit::Gesture(id, false));
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        for notification in restart_notifications(flags) {
            self.notify.notify(notification);
        }
        if flags & kMidiCCAssignmentChanged != 0 {
            self.midi_mapping_changed.store(true, Ordering::Release);
            self.notify.notify(HostNotification::MainThreadCallback);
        }
        kResultOk
    }
}

/// What a `restartComponent(flags)` means for the bridge.
pub(crate) fn restart_notifications(flags: int32) -> Vec<HostNotification> {
    let mut out = Vec::new();
    if flags & (kReloadComponent | kIoChanged) != 0 {
        out.push(HostNotification::RestartRequested);
    }
    if flags & kLatencyChanged != 0 {
        // The host re-reads `getLatencySamples` (VST3 FAQ, Processing).
        out.push(HostNotification::LatencyChanged);
    }
    if flags & (kParamValuesChanged | kParamTitlesChanged | kParamIDMappingChanged) != 0 {
        out.push(HostNotification::ParamsChanged);
    }
    if flags & kParamValuesChanged != 0 {
        out.push(HostNotification::StateDirty);
    }
    out
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0 {
            self.notify.notify(HostNotification::StateDirty);
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        // The app opens editors; a plugin cannot.
        kResultFalse
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

// ------------------------------------------------------------------------------------------------
// Linux IRunLoop
// ------------------------------------------------------------------------------------------------

/// The plugin's fds and timers, registered with the bridge's [`HostLoop`]. Main thread only.
///
/// Callbacks run outside every borrow: a handler may (un)register handlers re-entrantly, and
/// releasing a handler may run plugin code that does so.
pub(crate) struct RunLoop {
    host_loop: Rc<dyn HostLoop>,
    #[cfg(unix)]
    notify: Arc<dyn HostNotify>,
    #[cfg(unix)]
    fds: RefCell<Vec<(RawFd, ComPtr<IEventHandler>)>>,
    timers: RefCell<Vec<(TimerId, ComPtr<ITimerHandler>)>>,
}

impl RunLoop {
    pub(crate) fn new(host_loop: Rc<dyn HostLoop>, notify: Arc<dyn HostNotify>) -> RunLoop {
        #[cfg(windows)]
        let _ = notify;
        RunLoop {
            host_loop,
            #[cfg(unix)]
            notify,
            #[cfg(unix)]
            fds: RefCell::new(Vec::new()),
            timers: RefCell::new(Vec::new()),
        }
    }

    #[cfg(unix)]
    fn warn(&self, message: &str) {
        self.notify.log(LogLevel::Warning, message);
    }

    #[cfg(unix)]
    fn register_event_handler(&self, handler: *mut IEventHandler, fd: FileDescriptor) -> tresult {
        // SAFETY: the plugin passes a live handler (or NULL).
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        if fd < 0 {
            return kInvalidArgument;
        }
        let mut fds = self.fds.borrow_mut();
        if fds
            .iter()
            .any(|(f, h)| *f == fd && h.as_ptr() == handler.as_ptr())
        {
            return kResultOk;
        }
        // Several handlers may share one fd; the loop watches it once.
        if !fds.iter().any(|(f, _)| *f == fd)
            && let Err(e) = self.host_loop.register_fd(fd, FdEvents::READ)
        {
            drop(fds);
            self.warn(&format!("IRunLoop: register fd {fd} failed: {e}"));
            return kResultFalse;
        }
        fds.push((fd, handler.to_com_ptr()));
        kResultOk
    }

    #[cfg(unix)]
    fn unregister_event_handler(&self, handler: *mut IEventHandler) -> tresult {
        if handler.is_null() {
            return kInvalidArgument;
        }
        let removed: Vec<(RawFd, ComPtr<IEventHandler>)> = {
            let mut fds = self.fds.borrow_mut();
            let (removed, kept) = std::mem::take(&mut *fds)
                .into_iter()
                .partition(|(_, h)| h.as_ptr() == handler);
            *fds = kept;
            removed
        };
        for (fd, _) in &removed {
            if !self.fds.borrow().iter().any(|(f, _)| f == fd) {
                let _ = self.host_loop.unregister_fd(*fd);
            }
        }
        let found = !removed.is_empty();
        // Released here, outside the borrow.
        drop(removed);
        if found { kResultOk } else { kInvalidArgument }
    }

    #[cfg(unix)]
    fn register_timer(&self, handler: *mut ITimerHandler, ms: TimerInterval) -> tresult {
        // SAFETY: the plugin passes a live handler (or NULL).
        let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        let period = Duration::from_millis(ms.clamp(1, u64::from(u32::MAX)));
        match self.host_loop.register_timer(period) {
            Ok(id) => {
                self.timers.borrow_mut().push((id, handler.to_com_ptr()));
                kResultOk
            }
            Err(e) => {
                self.warn(&format!("IRunLoop: register timer failed: {e}"));
                kResultFalse
            }
        }
    }

    #[cfg(unix)]
    fn unregister_timer(&self, handler: *mut ITimerHandler) -> tresult {
        if handler.is_null() {
            return kInvalidArgument;
        }
        let removed: Vec<(TimerId, ComPtr<ITimerHandler>)> = {
            let mut timers = self.timers.borrow_mut();
            let (removed, kept) = std::mem::take(&mut *timers)
                .into_iter()
                .partition(|(_, h)| h.as_ptr() == handler);
            *timers = kept;
            removed
        };
        for (id, _) in &removed {
            let _ = self.host_loop.unregister_timer(*id);
        }
        let found = !removed.is_empty();
        drop(removed);
        if found { kResultOk } else { kInvalidArgument }
    }

    /// Whether a handler is registered for `fd`.
    #[cfg(unix)]
    pub(crate) fn owns_fd(&self, fd: RawFd) -> bool {
        self.fds.borrow().iter().any(|(f, _)| *f == fd)
    }

    /// Whether `timer` belongs to a handler.
    pub(crate) fn owns_timer(&self, timer: TimerId) -> bool {
        self.timers.borrow().iter().any(|(t, _)| *t == timer)
    }

    /// Calls the handlers of a ready `fd`.
    #[cfg(unix)]
    pub(crate) fn dispatch_fd(&self, fd: RawFd, events: FdEvents) {
        let handlers: Vec<ComPtr<IEventHandler>> = self
            .fds
            .borrow()
            .iter()
            .filter(|(f, _)| *f == fd)
            .map(|(_, h)| h.clone())
            .collect();
        if handlers.is_empty() {
            return;
        }
        // An earlier handler of this tick may have drained the fd already (JUCE's X11 and
        // message-queue handlers share state); never call a handler for an fd that is no longer
        // readable, it might block in read() (spike WP-15, vst3_child).
        if !events.contains(FdEvents::ERROR) && !readable_now(fd) {
            return;
        }
        for handler in handlers {
            let registered = self
                .fds
                .borrow()
                .iter()
                .any(|(f, h)| *f == fd && h.as_ptr() == handler.as_ptr());
            if registered {
                // SAFETY: a live handler of the plugin, on the main thread.
                unsafe { handler.onFDIsSet(fd) };
            }
        }
    }

    /// Calls the handler of a due timer.
    pub(crate) fn dispatch_timer(&self, timer: TimerId) {
        let handler = self
            .timers
            .borrow()
            .iter()
            .find(|(t, _)| *t == timer)
            .map(|(_, h)| h.clone());
        if let Some(handler) = handler {
            // SAFETY: a live handler of the plugin, on the main thread.
            unsafe { handler.onTimer() };
        }
    }

    /// Unregisters everything and releases the handlers; before the plugin's library goes away.
    pub(crate) fn clear(&self) {
        #[cfg(unix)]
        let fds = std::mem::take(&mut *self.fds.borrow_mut());
        let timers = std::mem::take(&mut *self.timers.borrow_mut());
        #[cfg(unix)]
        {
            let mut unique: Vec<RawFd> = fds.iter().map(|(fd, _)| *fd).collect();
            unique.dedup();
            for fd in unique {
                let _ = self.host_loop.unregister_fd(fd);
            }
        }
        for (id, _) in &timers {
            let _ = self.host_loop.unregister_timer(*id);
        }
        #[cfg(unix)]
        drop(fds);
        drop(timers);
    }

    /// Registered (fds, timers), for tests.
    #[cfg(all(test, unix))]
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.fds.borrow().len(), self.timers.borrow().len())
    }
}

/// Whether `fd` is readable right now (zero-timeout `poll`).
#[cfg(unix)]
fn readable_now(fd: RawFd) -> bool {
    // SAFETY: the plugin keeps a registered fd open while it is registered.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut pollfd = [rustix::event::PollFd::new(
        &borrowed,
        rustix::event::PollFlags::IN,
    )];
    let zero = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match rustix::event::poll(&mut pollfd, Some(&zero)) {
        Ok(n) => n > 0 && !pollfd[0].revents().is_empty(),
        // Let the handler find out.
        Err(_) => true,
    }
}

// ------------------------------------------------------------------------------------------------
// IPlugFrame (+ IRunLoop)
// ------------------------------------------------------------------------------------------------

/// The frame of an open view: resize requests, and the run loop plugins query from it.
pub(crate) struct PlugFrame {
    #[cfg(unix)]
    run_loop: Rc<RunLoop>,
    notify: Arc<dyn HostNotify>,
    /// Inside `resizeView` (guards against a plugin that answers `onSize` with another request).
    resizing: Cell<bool>,
}

#[cfg(unix)]
impl Class for PlugFrame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

/// Windows: no `IRunLoop` (plugins there use window messages and `SetTimer`).
#[cfg(windows)]
impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl PlugFrame {
    pub(crate) fn new(run_loop: Rc<RunLoop>, notify: Arc<dyn HostNotify>) -> ComWrapper<PlugFrame> {
        #[cfg(windows)]
        let _ = run_loop;
        ComWrapper::new(PlugFrame {
            #[cfg(unix)]
            run_loop,
            notify,
            resizing: Cell::new(false),
        })
    }
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes a valid rect.
        let rect = unsafe { *new_size };
        let size = GuiSize {
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
        };
        self.notify
            .notify(HostNotification::GuiResizeRequested(size));
        // Accept at once and answer with `onSize`, as the SDK's editorhost does (spike WP-15 §1.4);
        // the app resizes the frame window after the notification.
        if !self.resizing.replace(true) {
            // SAFETY: the plugin passes its own live view.
            if let Some(view) = unsafe { ComRef::from_raw(view) } {
                unsafe { view.onSize(new_size) };
            }
            self.resizing.set(false);
        }
        kResultOk
    }
}

#[cfg(unix)]
impl IRunLoopTrait for PlugFrame {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        self.run_loop.register_event_handler(handler, fd)
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        self.run_loop.unregister_event_handler(handler)
    }

    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, ms: TimerInterval) -> tresult {
        self.run_loop.register_timer(handler, ms)
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        self.run_loop.unregister_timer(handler)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_echo_guard_allows_an_explicit_user_gesture() {
        let edits = Edits::default();
        assert!(!edits.controller_echo(2));
        edits.mark_controller_sync(2);
        assert!(edits.controller_echo(2));
        assert!(!edits.controller_echo(3));
        edits.push(Edit::Gesture(2, true));
        assert!(!edits.controller_echo(2));
        edits.push(Edit::Gesture(2, false));
        assert!(edits.controller_echo(2));
    }

    use crate::host_loop::{PollLoop, notification_bits as bits};
    #[cfg(unix)]
    use std::sync::atomic::AtomicU32;
    use vst3::Steinberg::Vst::IMessage;

    #[test]
    fn host_application_names_and_creates_messages() {
        let app = HostApplication::new("komponist");
        let host: ComPtr<IHostApplication> = app.to_com_ptr().unwrap();
        let mut name: String128 = [0; 128];
        unsafe {
            assert_eq!(host.getName(&mut name), kResultOk);
            assert_eq!(super::super::strings::utf16(&name), "komponist");
        }
        // createInstance(IMessage) → a message with an attribute list.
        let mut cid: TUID = IMessage::IID.map(|b| b as std::ffi::c_char);
        let mut iid = cid;
        let mut obj = std::ptr::null_mut();
        assert_eq!(
            unsafe { host.createInstance(&mut cid, &mut iid, &mut obj) },
            kResultOk
        );
        let message = unsafe { ComPtr::<IMessage>::from_raw(obj.cast()) }.unwrap();
        unsafe {
            assert!(message.getMessageID().is_null());
            message.setMessageID(c"JuceVST3EditController".as_ptr());
            assert_eq!(
                CStr::from_ptr(message.getMessageID()),
                c"JuceVST3EditController"
            );
            let attrs = ComRef::from_raw(message.getAttributes()).unwrap();
            assert_eq!(attrs.setInt(c"ptr".as_ptr(), 42), kResultOk);
            let mut i = 0;
            assert_eq!(attrs.getInt(c"ptr".as_ptr(), &mut i), kResultOk);
            assert_eq!(i, 42);
            let mut f = 0.0;
            assert_eq!(attrs.getFloat(c"ptr".as_ptr(), &mut f), kResultFalse);
            attrs.setFloat(c"f".as_ptr(), 0.5);
            attrs.getFloat(c"f".as_ptr(), &mut f);
            assert_eq!(f, 0.5);
            let text: Vec<u16> = "Hallo\0".encode_utf16().collect();
            attrs.setString(c"s".as_ptr(), text.as_ptr());
            let mut out = [0xFFFFu16; 4];
            assert_eq!(
                attrs.getString(c"s".as_ptr(), out.as_mut_ptr(), 8),
                kResultOk
            );
            assert_eq!(super::super::strings::utf16(&out), "Hal");
            let blob = [1u8, 2, 3];
            attrs.setBinary(c"b".as_ptr(), blob.as_ptr().cast(), 3);
            let (mut ptr, mut size) = (std::ptr::null(), 0);
            assert_eq!(
                attrs.getBinary(c"b".as_ptr(), &mut ptr, &mut size),
                kResultOk
            );
            assert_eq!(
                std::slice::from_raw_parts(ptr.cast::<u8>(), size as usize),
                blob
            );
            assert_eq!(
                attrs.getBinary(c"missing".as_ptr(), &mut ptr, &mut size),
                kResultFalse
            );
        }
        // Unknown classes are refused.
        let mut other: TUID = IComponent::IID.map(|b| b as std::ffi::c_char);
        assert_eq!(
            unsafe { host.createInstance(&mut other, &mut iid, &mut obj) },
            kNoInterface
        );
        assert!(obj.is_null());
        let support: ComPtr<IPlugInterfaceSupport> = host.cast().unwrap();
        let midi: TUID = IMidiMapping::IID.map(|b| b as std::ffi::c_char);
        let frame: TUID = IPlugFrame::IID.map(|b| b as std::ffi::c_char);
        unsafe {
            assert_eq!(support.isPlugInterfaceSupported(&midi), kResultTrue);
            assert_eq!(support.isPlugInterfaceSupported(&frame), kResultFalse);
        }
    }

    #[test]
    fn edits_go_to_the_ring_or_stay_pending() {
        let edits = Edits::default();
        assert!(edits.push(Edit::Set(1, 0.25)) && edits.push(Edit::Perform(2, 0.5)));
        assert!(
            edits.push(Edit::Gesture(2, true)),
            "gestures are dropped while inactive"
        );
        assert!(edits.push(Edit::Set(1, 0.75)));
        assert_eq!(edits.pending(), [(1, 0.75), (2, 0.5)]);
        let (producer, mut consumer) = rtrb::RingBuffer::new(2);
        assert_eq!(edits.connect(producer), (0, vec![(1, 0.75), (2, 0.5)]));
        assert!(edits.pending().is_empty());
        assert!(edits.push(Edit::Gesture(3, true)) && edits.push(Edit::Perform(3, 0.1)));
        assert!(!edits.push(Edit::Set(5, 0.3)), "ring full");
        let early = consumer.pop().unwrap();
        assert_eq!(early.edit, Edit::Gesture(3, true));
        edits.clear_pending();
        let stale = consumer.pop().unwrap();
        assert_ne!(stale.generation, edits.generation());
        assert!(edits.push(Edit::Set(5, 0.75)));
        let fresh = consumer.pop().unwrap();
        assert_eq!(fresh.generation, edits.generation());
        edits.disconnect([(3, 0.1)]);
        assert!(edits.push(Edit::Set(4, 0.9)));
        assert_eq!(edits.pending(), [(3, 0.1), (4, 0.9)]);
        edits.clear_pending();
        assert!(edits.pending().is_empty());
    }

    #[test]
    fn restart_flags_map_to_notifications() {
        assert_eq!(
            restart_notifications(kLatencyChanged),
            [HostNotification::LatencyChanged]
        );
        assert_eq!(
            restart_notifications(kParamValuesChanged),
            [
                HostNotification::ParamsChanged,
                HostNotification::StateDirty
            ]
        );
        assert_eq!(
            restart_notifications(kParamTitlesChanged | kIoChanged),
            [
                HostNotification::RestartRequested,
                HostNotification::ParamsChanged
            ]
        );
        assert!(restart_notifications(kMidiCCAssignmentChanged).is_empty());
    }

    #[test]
    fn component_handler_forwards_edits() {
        let host_loop = PollLoop::new(false).unwrap();
        let edits = Arc::new(Edits::default());
        let midi = Arc::new(AtomicBool::new(false));
        let handler =
            ComponentHandler::new(host_loop.notify().clone(), edits.clone(), midi.clone());
        let ptr: ComPtr<IComponentHandler> = handler.to_com_ptr().unwrap();
        unsafe {
            ptr.beginEdit(7);
            ptr.performEdit(7, 1.5);
            ptr.endEdit(7);
            ptr.restartComponent(kMidiCCAssignmentChanged | kLatencyChanged);
        }
        assert_eq!(edits.pending(), [(7, 1.0)]);
        assert_eq!(host_loop.notify().param_edits(), (1, Some((7, 1.0))));
        let (pending, _) = host_loop.notify().take();
        for bit in [
            bits::PARAM_EDITED,
            bits::STATE_DIRTY,
            bits::LATENCY_CHANGED,
            bits::MAIN_THREAD_CALLBACK,
        ] {
            assert!(pending & bit != 0, "{bit:#x} in {pending:#x}");
        }
        assert!(midi.load(Ordering::Acquire));
        let h2: ComPtr<IComponentHandler2> = ptr.cast().unwrap();
        unsafe {
            assert_eq!(h2.setDirty(1), kResultOk);
            assert_eq!(h2.requestOpenEditor(c"editor".as_ptr()), kResultFalse);
        }
        assert!(host_loop.notify().take().0 & bits::STATE_DIRTY != 0);
    }

    /// A plugin-side timer and fd handler that counts calls and unregisters itself on request.
    #[cfg(unix)]
    struct Counter {
        calls: AtomicU32,
    }

    #[cfg(unix)]
    impl Class for Counter {
        type Interfaces = (ITimerHandler, IEventHandler);
    }

    #[cfg(unix)]
    impl ITimerHandlerTrait for Counter {
        unsafe fn onTimer(&self) {
            self.calls.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[cfg(unix)]
    impl IEventHandlerTrait for Counter {
        unsafe fn onFDIsSet(&self, _fd: FileDescriptor) {
            self.calls.fetch_add(100, Ordering::Relaxed);
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_loop_registers_with_the_host_loop() {
        use std::io::Write;
        use std::os::fd::AsRawFd;

        let host_loop = PollLoop::new(false).unwrap();
        let run_loop = Rc::new(RunLoop::new(host_loop.clone(), host_loop.notify().clone()));
        let frame = PlugFrame::new(run_loop.clone(), host_loop.notify().clone());
        let rl: ComPtr<IRunLoop> = frame.to_com_ptr::<IPlugFrame>().unwrap().cast().unwrap();
        let counter = ComWrapper::new(Counter {
            calls: AtomicU32::new(0),
        });
        let timer = counter.as_com_ref::<ITimerHandler>().unwrap().as_ptr();
        let events = counter.as_com_ref::<IEventHandler>().unwrap().as_ptr();
        let (mut tx, rx) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd = rx.as_raw_fd();
        unsafe {
            assert_eq!(rl.registerTimer(timer, 10), kResultOk);
            assert_eq!(rl.registerEventHandler(events, fd), kResultOk);
            // Twice for the same fd and handler: one registration.
            assert_eq!(rl.registerEventHandler(events, fd), kResultOk);
            assert_eq!(rl.registerEventHandler(events, -1), kInvalidArgument);
            assert_eq!(rl.registerTimer(std::ptr::null_mut(), 10), kInvalidArgument);
        }
        assert_eq!(run_loop.counts(), (1, 1));
        assert_eq!(host_loop.registrations(), (1, 1));
        let timer_id = TimerId(1);
        assert!(run_loop.owns_timer(timer_id) && run_loop.owns_fd(fd));

        run_loop.dispatch_timer(timer_id);
        assert_eq!(counter.calls.load(Ordering::Relaxed), 1);
        // Not readable: the fd handler is skipped; readable: called.
        run_loop.dispatch_fd(fd, FdEvents::READ);
        assert_eq!(counter.calls.load(Ordering::Relaxed), 1);
        tx.write_all(b"x").unwrap();
        run_loop.dispatch_fd(fd, FdEvents::READ);
        assert_eq!(counter.calls.load(Ordering::Relaxed), 101);

        unsafe {
            assert_eq!(rl.unregisterTimer(timer), kResultOk);
            assert_eq!(rl.unregisterTimer(timer), kInvalidArgument);
            assert_eq!(rl.unregisterEventHandler(events), kResultOk);
        }
        assert_eq!(host_loop.registrations(), (0, 0));
        unsafe {
            rl.registerTimer(timer, 5);
            rl.registerEventHandler(events, fd);
        }
        run_loop.clear();
        assert_eq!(run_loop.counts(), (0, 0));
        assert_eq!(host_loop.registrations(), (0, 0));
    }

    /// A view stub that records `onSize`.
    struct View {
        sized: Cell<Option<(i32, i32)>>,
    }

    impl Class for View {
        type Interfaces = (IPlugView,);
    }

    impl IPlugViewTrait for View {
        unsafe fn isPlatformTypeSupported(&self, _t: FIDString) -> tresult {
            kResultTrue
        }
        unsafe fn attached(&self, _p: *mut c_void, _t: FIDString) -> tresult {
            kResultOk
        }
        unsafe fn removed(&self) -> tresult {
            kResultOk
        }
        unsafe fn onWheel(&self, _d: f32) -> tresult {
            kResultOk
        }
        unsafe fn onKeyDown(&self, _k: u16, _c: i16, _m: i16) -> tresult {
            kResultOk
        }
        unsafe fn onKeyUp(&self, _k: u16, _c: i16, _m: i16) -> tresult {
            kResultOk
        }
        unsafe fn getSize(&self, _s: *mut ViewRect) -> tresult {
            kResultOk
        }
        unsafe fn onSize(&self, s: *mut ViewRect) -> tresult {
            let r = unsafe { *s };
            self.sized.set(Some((r.right - r.left, r.bottom - r.top)));
            kResultOk
        }
        unsafe fn onFocus(&self, _s: TBool) -> tresult {
            kResultOk
        }
        unsafe fn setFrame(&self, _f: *mut IPlugFrame) -> tresult {
            kResultOk
        }
        unsafe fn canResize(&self) -> tresult {
            kResultTrue
        }
        unsafe fn checkSizeConstraint(&self, _r: *mut ViewRect) -> tresult {
            kResultOk
        }
    }

    #[test]
    fn resize_view_answers_with_on_size_and_notifies() {
        let host_loop = PollLoop::new(false).unwrap();
        let run_loop = Rc::new(RunLoop::new(host_loop.clone(), host_loop.notify().clone()));
        let frame = PlugFrame::new(run_loop, host_loop.notify().clone());
        let frame: ComPtr<IPlugFrame> = frame.to_com_ptr().unwrap();
        let view = ComWrapper::new(View {
            sized: Cell::new(None),
        });
        let mut rect = ViewRect {
            left: 10,
            top: 20,
            right: 310,
            bottom: 220,
        };
        let view_ptr = view.as_com_ref::<IPlugView>().unwrap().as_ptr();
        unsafe {
            assert_eq!(frame.resizeView(view_ptr, &mut rect), kResultOk);
            assert_eq!(
                frame.resizeView(view_ptr, std::ptr::null_mut()),
                kInvalidArgument
            );
        }
        assert_eq!(view.sized.get(), Some((300, 200)));
        let (pending, size) = host_loop.notify().take();
        assert!(pending & bits::GUI_RESIZE_REQUESTED != 0);
        assert_eq!(
            size,
            Some(GuiSize {
                width: 300,
                height: 200
            })
        );
    }
}
