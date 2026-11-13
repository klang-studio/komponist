//! The host side of a CLAP instance: clack-host handlers that translate plugin callbacks into
//! [`HostNotify`] notifications and [`HostLoop`] registrations.
//!
//! - [`SharedHost`] (any thread): `request_*`, log, thread-check, gui, `params.request_flush`.
//! - [`MainThreadHost`] (main thread): timer-support, posix-fd-support, params rescan, latency,
//!   state, audio-ports, note-ports, note-name. It remembers the timers and fds of its plugin so
//!   that the format layer only dispatches its own, and unregisters leftovers when the instance is
//!   destroyed.

use std::cell::{Cell, RefCell};
use std::os::fd::RawFd;
use std::rc::Rc;
use std::sync::Arc;
use std::thread::ThreadId;
use std::time::Duration;

use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::gui::{GuiSize as ClapGuiSize, HostGui, HostGuiImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_name::{HostNoteName, HostNoteNameImpl};
use clack_extensions::note_ports::{
    HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::posix_fd::{FdFlags, HostPosixFd, HostPosixFdImpl};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, TimerId as ClapTimerId};
use clack_host::prelude::*;
use komponist_ipc::{GuiSize, LogLevel};

use crate::host::{FdEvents, HostLoop, HostNotification, HostNotify, TimerId};

std::thread_local! {
    /// Set while the current thread runs a `HostedProcessor` method (CLAP thread-check
    /// `is_audio_thread`). The bridge RT thread only ever runs processor methods.
    static IN_AUDIO_CALL: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as the audio thread until dropped (restores the previous state, so
/// single-threaded tests that process on the main thread see the main thread again afterwards).
pub(crate) struct AudioThreadScope(bool);

impl AudioThreadScope {
    pub(crate) fn enter() -> AudioThreadScope {
        AudioThreadScope(IN_AUDIO_CALL.replace(true))
    }
}

impl Drop for AudioThreadScope {
    fn drop(&mut self) {
        IN_AUDIO_CALL.set(self.0);
    }
}

/// The handler types of a komponist CLAP host.
pub(crate) struct KpHost;

impl HostHandlers for KpHost {
    type Shared<'a> = SharedHost;
    type MainThread<'a> = MainThreadHost<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostThreadCheck>()
            .register::<HostGui>()
            .register::<HostTimer>()
            .register::<HostPosixFd>()
            .register::<HostParams>()
            .register::<HostLatency>()
            .register::<HostState>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>()
            .register::<HostNoteName>();
    }
}

/// Thread-safe host callbacks.
pub(crate) struct SharedHost {
    notify: Arc<dyn HostNotify>,
    main_thread: ThreadId,
}

impl SharedHost {
    pub(crate) fn new(notify: Arc<dyn HostNotify>, main_thread: ThreadId) -> SharedHost {
        SharedHost {
            notify,
            main_thread,
        }
    }

    fn send(&self, notification: HostNotification) {
        self.notify.notify(notification);
    }
}

impl<'a> SharedHandler<'a> for SharedHost {
    fn request_restart(&self) {
        self.send(HostNotification::RestartRequested);
    }

    fn request_process(&self) {
        self.send(HostNotification::ProcessRequested);
    }

    fn request_callback(&self) {
        self.send(HostNotification::MainThreadCallback);
    }
}

impl HostLogImpl for SharedHost {
    fn log(&self, severity: LogSeverity, message: &str) {
        let level = match severity {
            LogSeverity::Debug => LogLevel::Debug,
            LogSeverity::Info => LogLevel::Info,
            LogSeverity::Warning => LogLevel::Warning,
            LogSeverity::Error
            | LogSeverity::Fatal
            | LogSeverity::HostMisbehaving
            | LogSeverity::PluginMisbehaving => LogLevel::Error,
        };
        self.notify.log(level, message);
    }
}

impl HostThreadCheckImpl for SharedHost {
    fn is_main_thread(&self) -> bool {
        // Checked first so the audio thread never needs `thread::current()`.
        !IN_AUDIO_CALL.get() && std::thread::current().id() == self.main_thread
    }

    fn is_audio_thread(&self) -> bool {
        IN_AUDIO_CALL.get()
    }
}

impl HostGuiImpl for SharedHost {
    fn resize_hints_changed(&self) {
        // The bridge asks `gui_set_size` again on the next user resize; nothing to cache.
    }

    fn request_resize(&self, size: ClapGuiSize) -> Result<(), HostError> {
        self.send(HostNotification::GuiResizeRequested(GuiSize {
            width: size.width,
            height: size.height,
        }));
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        // Embedded GUIs are shown with their frame window; the frame is already visible.
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        // Either way the bridge answers with `gui_close`, which calls `destroy` as CLAP requires.
        self.send(HostNotification::GuiClosed);
    }
}

impl HostParamsImplShared for SharedHost {
    fn request_flush(&self) {
        self.send(HostNotification::ParamFlushRequested);
    }
}

/// Main-thread host callbacks of one instance.
pub(crate) struct MainThreadHost<'a> {
    shared: &'a SharedHost,
    host_loop: Rc<dyn HostLoop>,
    timers: RefCell<Vec<TimerId>>,
    fds: RefCell<Vec<RawFd>>,
}

impl<'a> MainThreadHost<'a> {
    pub(crate) fn new(shared: &'a SharedHost, host_loop: Rc<dyn HostLoop>) -> MainThreadHost<'a> {
        MainThreadHost {
            shared,
            host_loop,
            timers: RefCell::new(Vec::new()),
            fds: RefCell::new(Vec::new()),
        }
    }

    /// Whether this instance registered `timer`.
    pub(crate) fn owns_timer(&self, timer: TimerId) -> bool {
        self.timers.borrow().contains(&timer)
    }

    /// Whether this instance registered `fd`.
    pub(crate) fn owns_fd(&self, fd: RawFd) -> bool {
        self.fds.borrow().contains(&fd)
    }

    fn send(&self, notification: HostNotification) {
        self.shared.notify.notify(notification);
    }

    fn warn(&self, message: &str) {
        self.shared.notify.log(LogLevel::Warning, message);
    }
}

impl Drop for MainThreadHost<'_> {
    /// Runs after the plugin was destroyed: whatever it left registered goes too.
    fn drop(&mut self) {
        for timer in self.timers.take() {
            let _ = self.host_loop.unregister_timer(timer);
        }
        for fd in self.fds.take() {
            let _ = self.host_loop.unregister_fd(fd);
        }
    }
}

impl<'a> MainThreadHandler<'a> for MainThreadHost<'a> {}

impl HostTimerImpl for MainThreadHost<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<ClapTimerId, HostError> {
        let period = Duration::from_millis(u64::from(period_ms.max(1)));
        match self.host_loop.register_timer(period) {
            Ok(id) => {
                self.timers.borrow_mut().push(id);
                Ok(ClapTimerId(id.0))
            }
            Err(e) => {
                self.warn(&format!("timer-support: register_timer failed: {e}"));
                Err(HostError::Message("register_timer failed"))
            }
        }
    }

    fn unregister_timer(&self, timer_id: ClapTimerId) -> Result<(), HostError> {
        let id = TimerId(timer_id.0);
        let mut timers = self.timers.borrow_mut();
        let Some(pos) = timers.iter().position(|t| *t == id) else {
            return Err(HostError::Message("unknown timer"));
        };
        timers.swap_remove(pos);
        drop(timers);
        self.host_loop
            .unregister_timer(id)
            .map_err(|_| HostError::Message("unregister_timer failed"))
    }
}

/// CLAP posix-fd flags as [`FdEvents`].
pub(crate) fn fd_events(flags: FdFlags) -> FdEvents {
    let mut events = FdEvents::NONE;
    if flags.contains(FdFlags::READ) {
        events = events.union(FdEvents::READ);
    }
    if flags.contains(FdFlags::WRITE) {
        events = events.union(FdEvents::WRITE);
    }
    if flags.contains(FdFlags::ERROR) {
        events = events.union(FdEvents::ERROR);
    }
    events
}

/// [`FdEvents`] as CLAP posix-fd flags.
pub(crate) fn fd_flags(events: FdEvents) -> FdFlags {
    let mut flags = FdFlags::empty();
    if events.contains(FdEvents::READ) {
        flags |= FdFlags::READ;
    }
    if events.contains(FdEvents::WRITE) {
        flags |= FdFlags::WRITE;
    }
    if events.contains(FdEvents::ERROR) {
        flags |= FdFlags::ERROR;
    }
    flags
}

impl HostPosixFdImpl for MainThreadHost<'_> {
    fn register_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        if self.owns_fd(fd) {
            return Err(HostError::Message("fd already registered"));
        }
        match self.host_loop.register_fd(fd, fd_events(flags)) {
            Ok(()) => {
                self.fds.borrow_mut().push(fd);
                Ok(())
            }
            Err(e) => {
                self.warn(&format!("posix-fd-support: register_fd({fd}) failed: {e}"));
                Err(HostError::Message("register_fd failed"))
            }
        }
    }

    fn modify_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        if !self.owns_fd(fd) {
            return Err(HostError::Message("unknown fd"));
        }
        self.host_loop
            .modify_fd(fd, fd_events(flags))
            .map_err(|_| HostError::Message("modify_fd failed"))
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostError> {
        let mut fds = self.fds.borrow_mut();
        let Some(pos) = fds.iter().position(|f| *f == fd) else {
            return Err(HostError::Message("unknown fd"));
        };
        fds.swap_remove(pos);
        drop(fds);
        self.host_loop
            .unregister_fd(fd)
            .map_err(|_| HostError::Message("unregister_fd failed"))
    }
}

impl HostParamsImplMainThread for MainThreadHost<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {
        self.send(HostNotification::ParamsChanged);
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {
        // komponist v0.1 has no automation or modulation to clear.
    }
}

impl HostLatencyImpl for MainThreadHost<'_> {
    fn changed(&self) {
        self.send(HostNotification::LatencyChanged);
    }
}

impl HostStateImpl for MainThreadHost<'_> {
    fn mark_dirty(&self) {
        self.send(HostNotification::StateDirty);
    }
}

impl HostAudioPortsImpl for MainThreadHost<'_> {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        // Ports are read again on every activation, so any change is fine after a restart.
        true
    }

    fn rescan(&self, flags: AudioPortRescanFlags) {
        if !(flags - AudioPortRescanFlags::NAMES).is_empty() {
            self.send(HostNotification::RestartRequested);
        }
    }
}

impl HostNotePortsImpl for MainThreadHost<'_> {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }

    fn rescan(&self, _flags: NotePortRescanFlags) {
        // Note ports are read again on every activation.
        self.send(HostNotification::RestartRequested);
    }
}

impl HostNoteNameImpl for MainThreadHost<'_> {
    fn changed(&self) {
        self.send(HostNotification::NoteNamesChanged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fd_flags_round_trip() {
        for bits in 0..8u8 {
            let events = FdEvents(bits);
            assert_eq!(fd_events(fd_flags(events)), events);
        }
    }

    #[test]
    fn audio_thread_scope_nests() {
        assert!(!IN_AUDIO_CALL.get());
        {
            let _outer = AudioThreadScope::enter();
            assert!(IN_AUDIO_CALL.get());
            {
                let _inner = AudioThreadScope::enter();
            }
            assert!(IN_AUDIO_CALL.get());
        }
        assert!(!IN_AUDIO_CALL.get());
    }
}
