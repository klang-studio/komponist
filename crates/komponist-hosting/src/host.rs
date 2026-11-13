//! What the bridge offers a hosted plugin: its main-thread event loop and a notification sink.
//!
//! - [`HostLoop`] (main thread): timers and file descriptors. The bridge's main loop is one `poll()`
//!   over the control socket, a timerfd and the fds plugins register (ARCHITECTURE §8.2); it needs
//!   no X11 connection of its own. CLAP timer-support / posix-fd-support and the VST3 Linux
//!   `IRunLoop` are all implemented on top of it by the format layers, which map their handler
//!   objects to [`TimerId`]s and fds and dispatch [`crate::HostedPlugin::on_timer`] /
//!   [`crate::HostedPlugin::on_fd`].
//! - [`HostNotify`] (any thread, RT-safe): requests and notifications from the plugin to the host.
//!
//! Both are handed to [`crate::load`] in a [`HostContext`] and kept by the format layer for the
//! lifetime of the instance.

use std::os::fd::RawFd;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use komponist_ipc::{GuiSize, LogLevel};

use crate::plugin::HostingError;

/// Id of a timer registered with [`HostLoop::register_timer`], unique per bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimerId(pub u32);

/// Readiness interest or readiness of a file descriptor (bit set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FdEvents(pub u8);

impl FdEvents {
    /// Nothing.
    pub const NONE: FdEvents = FdEvents(0);
    /// Readable (`POLLIN`).
    pub const READ: FdEvents = FdEvents(1);
    /// Writable (`POLLOUT`).
    pub const WRITE: FdEvents = FdEvents(2);
    /// Error or hang-up (`POLLERR`, `POLLHUP`); always reported, never needs registering.
    pub const ERROR: FdEvents = FdEvents(4);

    /// Whether all bits of `other` are set.
    pub const fn contains(self, other: FdEvents) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets of bits.
    pub const fn union(self, other: FdEvents) -> FdEvents {
        FdEvents(self.0 | other.0)
    }
}

/// The bridge's main-thread event loop as seen by a format layer.
///
/// Methods take `&self` because plugins call them re-entrantly from inside other main-thread calls
/// (e.g. during `gui_open`); implementations use interior mutability. Main thread only.
pub trait HostLoop {
    /// Calls `HostedPlugin::on_timer(id)` every `period` (at least 1 ms; periods are approximate).
    fn register_timer(&self, period: Duration) -> Result<TimerId, HostingError>;

    /// Stops a timer.
    fn unregister_timer(&self, timer: TimerId) -> Result<(), HostingError>;

    /// Watches `fd` for `events`; calls `HostedPlugin::on_fd(fd, ready)` when ready.
    fn register_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError>;

    /// Changes the interest of a watched fd.
    fn modify_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError>;

    /// Stops watching `fd`.
    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostingError>;
}

/// A request or notification from the plugin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HostNotification {
    /// Call `HostedPlugin::on_main_thread` soon (CLAP `request_callback`). Any thread.
    MainThreadCallback,
    /// Deactivate and re-activate (CLAP `request_restart`, VST3 `restartComponent` with
    /// latency/IO changes). Any thread.
    RestartRequested,
    /// The plugin wants processing (CLAP `request_process`). Any thread.
    ProcessRequested,
    /// While not processing, call `flush_params` (CLAP `params.request_flush`). Any thread.
    ParamFlushRequested,
    /// Parameter infos and/or values changed; re-read them (CLAP `params.rescan`, VST3
    /// `kParamValuesChanged`/`kParamTitlesChanged`). Main thread.
    ParamsChanged,
    /// A parameter was edited on the main thread (VST3 `performEdit` from the GUI); the format layer
    /// already forwards it to its processor. Main thread.
    ParamEdited {
        /// Parameter id.
        id: u32,
        /// Plain value.
        value: f64,
    },
    /// The latency changed (CLAP `latency.changed`, VST3 `kLatencyChanged`). Main thread.
    LatencyChanged,
    /// The state changed (CLAP `state.mark_dirty`, VST3 `setDirty` / edits). Main thread.
    StateDirty,
    /// Note names changed (CLAP `note_name.changed`). Main thread.
    NoteNamesChanged,
    /// The GUI asks for a size (CLAP `gui.request_resize`, VST3 `IPlugFrame::resizeView`). Any
    /// thread; only the latest request matters.
    GuiResizeRequested(GuiSize),
    /// The plugin closed or lost its GUI (CLAP `gui.closed`). Main thread.
    GuiClosed,
}

/// Where a format layer sends [`HostNotification`]s and log lines. Implemented by the bridge.
///
/// `Send + Sync` and RT-safe: calls may come from any plugin thread, including the RT thread. An
/// implementation must not allocate, lock or block in [`HostNotify::notify`]; it may coalesce
/// repeated notifications of the same kind (only `ParamEdited` must not be merged across ids).
pub trait HostNotify: Send + Sync {
    /// Records a notification and wakes the bridge's main loop.
    fn notify(&self, notification: HostNotification);

    /// A log line from the plugin (CLAP `log`). Lines from the RT thread may be dropped.
    fn log(&self, level: LogLevel, message: &str);
}

/// The host services a plugin instance gets at load.
#[derive(Clone)]
pub struct HostContext {
    /// Main-thread timers and fds.
    pub host_loop: Rc<dyn HostLoop>,
    /// Any-thread notifications.
    pub notify: Arc<dyn HostNotify>,
    /// Host name reported to plugins (CLAP `clap_host.name`, VST3 `IHostApplication::getName`):
    /// "komponist".
    pub host_name: &'static str,
    /// Host version reported to plugins (the crate version).
    pub host_version: &'static str,
}

impl core::fmt::Debug for HostContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HostContext")
            .field("host_name", &self.host_name)
            .field("host_version", &self.host_version)
            .finish_non_exhaustive()
    }
}
