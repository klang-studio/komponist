//! [`CfEventLoop`]: the bridge main loop's [`HostLoop`] on macOS, built on the thread's CFRunLoop
//! (ARCHITECTURE §8.2; spike #42 Q5/Q6).
//!
//! On macOS the bridge's main thread is the Cocoa main thread. JUCE's message queue and timers are
//! CFRunLoop sources, and the plugin frame of #52 will run eframe (`[NSApp run]`) on it, so the
//! loop cannot be a `poll()` of its own. Everything the bridge waits for reaches the run loop
//! through sources in the **common modes**, so it keeps being served inside `CFRunLoopRun`, inside
//! eframe's event loop, and in `NSEventTrackingRunLoopMode` (live resize, menus):
//!
//! - **fds** (the control socket, the notification wake pipe, every fd the plugin registers): a
//!   libdispatch read source (and a write source for `WRITE` interest) each, on a private serial
//!   queue. Its handler suspends the source, records the fd and signals one version-0
//!   `CFRunLoopSource` with `CFRunLoopWakeUp`; the source's perform on the main thread turns the
//!   records into [`Ready`] (a zero-timeout `poll()` of a plugin fd gives the same `READ` / `WRITE`
//!   / `ERROR` bits as the Linux loop). A source is resumed only after the bridge handled its fd
//!   ([`CfEventLoop::rearm`]), so a readiness that waits for a busy bridge does not spin.
//!   The first version used `CFFileDescriptor`; after some dispatches (a `Deactivate` and an
//!   `Activate` in one) its re-enabled read callback never fired again, and the bridge missed
//!   `Shutdown` in about one of three runs of the bridge tests (#51).
//! - **timers** of the plugin: a repeating `CFRunLoopTimer` each (CF skips missed periods itself).
//! - **one internal timer** for the next deadline: the bridge's next `ParamValues` and the end of a
//!   write cooldown.
//! - **Cross-thread notifications** stay the non-blocking wake pipe of `BridgeNotify`, watched like
//!   the other fds. `notify` runs on the RT thread too (CLAP `request_callback`, `request_restart`,
//!   `request_flush`), where rule 4 allows no lock, and `CFRunLoopSourceSignal` /
//!   `CFRunLoopWakeUp` take CF's internal locks; they run on the dispatch queue's thread instead.
//!   Nothing goes through the (serial) main dispatch queue, which spike #42 warned against.
//!
//! **Dispatch** follows the rules of the Linux loop (`event_loop`): callouts only queue what is
//! ready ([`CfEventLoop::take_ready`]) and call the dispatcher the bridge installed
//! ([`CfEventLoop::set_dispatch`]). The bridge handles the queue outside every borrow of this
//! loop's state and checks before each plugin call that the fd or timer is still registered. A
//! callout that fires while the bridge is busy (a plugin running a nested run loop inside a call)
//! finds it borrowed and leaves its event queued; the busy dispatch drains the queue before it
//! returns.
//!
//! **Write interest** keeps the Linux policy: after a `WRITE` report the fd's write source stays
//! suspended for `DEFAULT_WRITE_INTERVAL`. **Differences** from `poll()`: `ERROR` is seen only
//! together with a read report (so only with `READ` interest), and an fd closed without
//! unregistering is never reported (kqueue drops it) instead of being reported once and parked.
//!
//! **Outer loop** ([`CfEventLoop::run`], [`CfEventLoop::stop`]): `CFRunLoopRun` until the bridge
//! stops it. All sources stay installed when it returns, so the bridge can run something else that
//! spins the same run loop (#52: `eframe::run_native` with run-and-return for a plugin frame) and
//! enter `run` again afterwards.
//!
//! Raw CoreFoundation and libdispatch FFI (both part of the system), no extra crate.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::os::fd::{BorrowedFd, RawFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::DEFAULT_WRITE_INTERVAL;
use komponist_hosting::{FdEvents, HostLoop, HostingError, TimerId};
use rustix::event::{PollFd, PollFlags, Timespec};

mod ffi {
    use std::ffi::{c_char, c_void};

    pub type CFTypeRef = *const c_void;
    pub type CFRunLoopRef = *mut c_void;
    pub type CFRunLoopSourceRef = *mut c_void;
    pub type CFRunLoopTimerRef = *mut c_void;
    pub type CFStringRef = *const c_void;
    pub type CFOptionFlags = usize;
    pub type CFAbsoluteTime = f64;

    /// `CFRunLoopTimerContext`.
    #[repr(C)]
    pub struct TimerContext {
        pub version: isize,
        pub info: *mut c_void,
        pub retain: *const c_void,
        pub release: *const c_void,
        pub copy_description: *const c_void,
    }

    /// `CFRunLoopSourceContext` (version 0).
    #[repr(C)]
    pub struct SourceContext {
        pub version: isize,
        pub info: *mut c_void,
        pub retain: *const c_void,
        pub release: *const c_void,
        pub copy_description: *const c_void,
        pub equal: *const c_void,
        pub hash: *const c_void,
        pub schedule: *const c_void,
        pub cancel: *const c_void,
        pub perform: extern "C" fn(*mut c_void),
    }

    pub type TimerCallout = extern "C" fn(CFRunLoopTimerRef, *mut c_void);

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub static kCFRunLoopCommonModes: CFStringRef;
        pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFAbsoluteTimeGetCurrent() -> CFAbsoluteTime;
        pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        pub fn CFRunLoopRun();
        pub fn CFRunLoopStop(rl: CFRunLoopRef);
        pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
        pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
        pub fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
        pub fn CFRunLoopSourceCreate(
            allocator: CFTypeRef,
            order: isize,
            context: *mut SourceContext,
        ) -> CFRunLoopSourceRef;
        pub fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);
        pub fn CFRunLoopSourceInvalidate(source: CFRunLoopSourceRef);
        pub fn CFRunLoopTimerCreate(
            allocator: CFTypeRef,
            fire_date: CFAbsoluteTime,
            interval: f64,
            flags: CFOptionFlags,
            order: isize,
            callout: TimerCallout,
            context: *mut TimerContext,
        ) -> CFRunLoopTimerRef;
        pub fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, date: CFAbsoluteTime);
        pub fn CFRunLoopTimerInvalidate(timer: CFRunLoopTimerRef);
    }

    #[cfg(test)]
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub static kCFRunLoopDefaultMode: CFStringRef;
        pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after: u8) -> i32;
        pub fn CFRunLoopAddCommonMode(rl: CFRunLoopRef, mode: CFStringRef);
        pub fn CFStringCreateWithCString(
            allocator: CFTypeRef,
            text: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
    }

    /// Any libdispatch object (`dispatch_queue_t`, `dispatch_source_t`).
    pub type DispatchObject = *mut c_void;

    /// `struct dispatch_source_type_s` (opaque).
    #[repr(C)]
    pub struct DispatchSourceType {
        _private: [u8; 0],
    }

    pub type DispatchFunction = extern "C" fn(*mut c_void);

    // libdispatch is part of libSystem.
    unsafe extern "C" {
        pub static _dispatch_source_type_read: DispatchSourceType;
        pub static _dispatch_source_type_write: DispatchSourceType;
        pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchObject;
        pub fn dispatch_source_create(
            kind: *const DispatchSourceType,
            handle: usize,
            mask: usize,
            queue: DispatchObject,
        ) -> DispatchObject;
        pub fn dispatch_set_context(object: DispatchObject, context: *mut c_void);
        pub fn dispatch_source_set_event_handler_f(
            source: DispatchObject,
            handler: DispatchFunction,
        );
        pub fn dispatch_source_set_cancel_handler_f(
            source: DispatchObject,
            handler: DispatchFunction,
        );
        pub fn dispatch_source_cancel(source: DispatchObject);
        pub fn dispatch_resume(object: DispatchObject);
        pub fn dispatch_suspend(object: DispatchObject);
        pub fn dispatch_release(object: DispatchObject);
        pub fn dispatch_sync_f(queue: DispatchObject, context: *mut c_void, work: DispatchFunction);
    }
}

use ffi::*;

/// Interval of the internal deadline timer between two explicit fire dates ("never").
const FAR_FUTURE: f64 = 1.0e9;

/// Who an fd belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// Registered by the plugin through [`HostLoop`].
    Plugin,
    /// The bridge's control socket.
    Control,
    /// The bridge's notification wake fd.
    Wake,
}

/// What became ready since the last [`CfEventLoop::take_ready`].
#[derive(Debug, Default, PartialEq)]
pub struct Ready {
    /// Ready plugin fds with what they are ready for.
    pub fds: Vec<(RawFd, FdEvents)>,
    /// Due plugin timers.
    pub timers: Vec<TimerId>,
    /// The control socket is readable.
    pub control: bool,
    /// The wake fd is readable.
    pub wake: bool,
    /// The internal deadline timer fired.
    pub deadline: bool,
}

impl Ready {
    /// Nothing is ready.
    pub fn is_empty(&self) -> bool {
        *self == Ready::default()
    }
}

// ------------------------------------------------------------------------------------------------
// fds: dispatch sources that signal the run loop
// ------------------------------------------------------------------------------------------------

/// What the dispatch queue and the main thread share: the fds whose sources fired, and the run
/// loop source that tells the main thread.
struct Signal {
    /// `(fd, write)` of every source that fired since the main thread last looked.
    fired: Mutex<Vec<(RawFd, bool)>>,
    /// The version-0 source (retained).
    source: CFRunLoopSourceRef,
    /// The run loop to wake (retained).
    run_loop: CFRunLoopRef,
}

// SAFETY: the CF objects are only retained, released, signalled and woken, which CF allows from
// any thread; `fired` is behind a mutex.
unsafe impl Send for Signal {}
// SAFETY: as above.
unsafe impl Sync for Signal {}

impl Drop for Signal {
    fn drop(&mut self) {
        // SAFETY: retained in `CfEventLoop::with_write_interval`, released once.
        unsafe {
            CFRelease(self.source);
            CFRelease(self.run_loop);
        }
    }
}

/// The context of one dispatch source: freed by its cancel handler.
struct WatchContext {
    signal: Arc<Signal>,
    fd: RawFd,
    write: bool,
    /// The dispatch source (set before it is first resumed).
    source: Cell<DispatchObject>,
    /// The handler suspended the source; the main thread resumes it ([`Watch::resume`]).
    suspended: AtomicBool,
}

/// One dispatch source watching an fd for reading or writing.
struct Watch {
    source: DispatchObject,
    queue: DispatchObject,
    context: *const WatchContext,
}

/// Runs on the dispatch queue when the fd is ready: one report per resume.
extern "C" fn watch_fired(context: *mut c_void) {
    // SAFETY: the source's context lives until its cancel handler, which runs after every event
    // handler (both on the same serial queue).
    let context = unsafe { &*context.cast::<WatchContext>() };
    // SAFETY: the live source this handler belongs to.
    unsafe { dispatch_suspend(context.source.get()) };
    context.suspended.store(true, Ordering::Release);
    let signal = &context.signal;
    signal
        .fired
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((context.fd, context.write));
    // SAFETY: retained by `signal`; both calls are allowed from any thread.
    unsafe {
        CFRunLoopSourceSignal(signal.source);
        CFRunLoopWakeUp(signal.run_loop);
    }
}

/// Runs on the dispatch queue after the source was cancelled: frees its context.
extern "C" fn watch_cancelled(context: *mut c_void) {
    // SAFETY: created by `Box::into_raw` in `Watch::new`, freed only here.
    drop(unsafe { Box::from_raw(context.cast::<WatchContext>()) });
}

/// Cancels a watch on its queue, where no event handler runs at the same time (`dispatch_sync_f`).
extern "C" fn cancel_on_queue(context: *mut c_void) {
    // SAFETY: `Watch::drop` passes its source and context and waits for this function.
    let (source, context) = unsafe { *context.cast::<(DispatchObject, *const WatchContext)>() };
    // SAFETY: the live source; a suspended one must be resumed so that it can be cancelled and
    // released.
    unsafe {
        dispatch_source_cancel(source);
        if (*context).suspended.swap(false, Ordering::AcqRel) {
            dispatch_resume(source);
        }
        dispatch_release(source);
    }
}

impl Watch {
    /// A running source for `fd` (reading, or writing if `write`).
    fn new(queue: DispatchObject, signal: &Arc<Signal>, fd: RawFd, write: bool) -> Option<Watch> {
        let context = Box::into_raw(Box::new(WatchContext {
            signal: signal.clone(),
            fd,
            write,
            source: Cell::new(std::ptr::null_mut()),
            suspended: AtomicBool::new(false),
        }));
        // SAFETY: the static source types of libdispatch; the fd stays open while it is watched
        // (the caller's contract). A new source is suspended until the `dispatch_resume` below,
        // so the context is complete before any handler runs.
        unsafe {
            let kind = if write {
                &_dispatch_source_type_write
            } else {
                &_dispatch_source_type_read
            };
            let source = dispatch_source_create(kind, fd as usize, 0, queue);
            if source.is_null() {
                drop(Box::from_raw(context));
                return None;
            }
            (*context).source.set(source);
            dispatch_set_context(source, context.cast());
            dispatch_source_set_event_handler_f(source, watch_fired);
            dispatch_source_set_cancel_handler_f(source, watch_cancelled);
            dispatch_resume(source);
            Some(Watch {
                source,
                queue,
                context,
            })
        }
    }

    /// Resumes the source if its handler suspended it.
    fn resume(&self) {
        // SAFETY: the context lives as long as the watch (freed only after `cancel_on_queue`).
        let context = unsafe { &*self.context };
        if context.suspended.swap(false, Ordering::AcqRel) {
            // SAFETY: suspended exactly once by the handler, resumed once here.
            unsafe { dispatch_resume(self.source) };
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut pair = (self.source, self.context);
        // SAFETY: waits until `cancel_on_queue` ran on the source's queue.
        unsafe { dispatch_sync_f(self.queue, (&raw mut pair).cast(), cancel_on_queue) };
    }
}

struct FdEntry {
    fd: RawFd,
    owner: Owner,
    interest: FdEvents,
    /// No write report before this instant (write cooldown).
    write_after: Instant,
    read: Option<Watch>,
    write: Option<Watch>,
}

struct TimerEntry {
    id: TimerId,
    timer: CFRunLoopTimerRef,
}

#[derive(Default)]
struct State {
    next_timer: u32,
    timers: Vec<TimerEntry>,
    fds: Vec<FdEntry>,
    ready: Ready,
}

/// The dispatcher the callouts call (see the module docs).
type Dispatch = Rc<dyn Fn()>;

/// See the module docs. Main thread only (the thread that created it, whose run loop it uses);
/// shared with the plugin as `Rc<dyn HostLoop>`.
pub struct CfEventLoop {
    run_loop: CFRunLoopRef,
    state: RefCell<State>,
    dispatch: RefCell<Option<Dispatch>>,
    write_interval: Duration,
    /// The internal deadline timer (null until [`CfEventLoop::new`] created it).
    deadline_timer: Cell<CFRunLoopTimerRef>,
    /// The serial queue of the fd sources.
    queue: DispatchObject,
    /// The fd sources' way to the main thread (null source until [`CfEventLoop::new`]).
    signal: RefCell<Option<Arc<Signal>>>,
}

impl std::fmt::Debug for CfEventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CfEventLoop")
            .field("write_interval", &self.write_interval)
            .finish_non_exhaustive()
    }
}

impl CfEventLoop {
    /// A loop on the calling thread's run loop with the default write cooldown.
    pub fn new() -> Rc<CfEventLoop> {
        CfEventLoop::with_write_interval(DEFAULT_WRITE_INTERVAL)
    }

    /// A loop with an explicit write cooldown.
    pub fn with_write_interval(write_interval: Duration) -> Rc<CfEventLoop> {
        // SAFETY: the current thread's run loop, retained for the loop's lifetime; a serial queue
        // (NULL attributes), released in `drop`.
        let (run_loop, queue) = unsafe {
            let rl = CFRunLoopGetCurrent();
            CFRetain(rl);
            (
                rl,
                dispatch_queue_create(c"komponist-bridge.fds".as_ptr(), std::ptr::null()),
            )
        };
        let lp = Rc::new(CfEventLoop {
            run_loop,
            state: RefCell::new(State::default()),
            dispatch: RefCell::new(None),
            write_interval,
            deadline_timer: Cell::new(std::ptr::null_mut()),
            queue,
            signal: RefCell::new(None),
        });
        let timer = lp.add_timer(FAR_FUTURE, FAR_FUTURE);
        lp.deadline_timer.set(timer);
        let mut context = SourceContext {
            version: 0,
            info: Rc::as_ptr(&lp).cast_mut().cast(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: signal_performed,
        };
        // SAFETY: CF copies the context; `info` points to the loop, and the source is invalidated
        // in `drop` before the loop goes away. The source and run loop are retained for `Signal`.
        let source = unsafe {
            let source = CFRunLoopSourceCreate(std::ptr::null(), 0, &mut context);
            CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
            CFRetain(run_loop);
            source
        };
        *lp.signal.borrow_mut() = Some(Arc::new(Signal {
            fired: Mutex::new(Vec::new()),
            source,
            run_loop,
        }));
        lp
    }

    /// Installs the function every callout calls after queueing its event (`None` removes it).
    pub fn set_dispatch(&self, dispatch: Option<Dispatch>) {
        *self.dispatch.borrow_mut() = dispatch;
    }

    /// Watches one of the bridge's own fds for reading ([`Owner::Control`] or [`Owner::Wake`]).
    pub fn watch(&self, fd: BorrowedFd<'_>, owner: Owner) -> Result<(), HostingError> {
        use std::os::fd::AsRawFd;
        self.add_fd(fd.as_raw_fd(), owner, FdEvents::READ)
    }

    /// Takes what became ready since the last call.
    pub fn take_ready(&self) -> Ready {
        std::mem::take(&mut self.state.borrow_mut().ready)
    }

    /// Whether `fd` is still registered by the plugin.
    pub fn fd_registered(&self, fd: RawFd) -> bool {
        self.state
            .borrow()
            .fds
            .iter()
            .any(|e| e.fd == fd && e.owner == Owner::Plugin)
    }

    /// Whether `timer` is still registered.
    pub fn timer_registered(&self, timer: TimerId) -> bool {
        self.state.borrow().timers.iter().any(|t| t.id == timer)
    }

    /// Number of plugin timers and plugin fds.
    #[cfg(test)]
    pub fn registrations(&self) -> (usize, usize) {
        let state = self.state.borrow();
        let fds = state.fds.iter().filter(|e| e.owner == Owner::Plugin);
        (state.timers.len(), fds.count())
    }

    /// After the bridge handled everything [`CfEventLoop::take_ready`] returned: resumes every fd
    /// source its handler suspended (a write source only after its cooldown) and sets the
    /// deadline timer to `deadline` or the end of the next write cooldown, whichever is earlier.
    pub fn rearm(&self, deadline: Option<Instant>) {
        let now = Instant::now();
        let mut next = deadline;
        for entry in &self.state.borrow().fds {
            if let Some(read) = &entry.read {
                read.resume();
            }
            if let Some(write) = &entry.write {
                if now >= entry.write_after {
                    write.resume();
                } else {
                    next = Some(next.map_or(entry.write_after, |n| n.min(entry.write_after)));
                }
            }
        }
        let date = next.map_or(FAR_FUTURE, |at| {
            // SAFETY: plain clock read.
            let current = unsafe { CFAbsoluteTimeGetCurrent() };
            current + at.saturating_duration_since(now).as_secs_f64()
        });
        // SAFETY: the timer lives as long as `self`.
        unsafe { CFRunLoopTimerSetNextFireDate(self.deadline_timer.get(), date) };
    }

    /// Runs the run loop (`CFRunLoopRun`) until [`CfEventLoop::stop`]. All sources stay installed
    /// afterwards; calling it again resumes.
    pub fn run(&self) {
        // SAFETY: runs this thread's run loop; callouts reach `self` only while it lives (every
        // source is invalidated in `drop`).
        unsafe { CFRunLoopRun() };
    }

    /// Makes the innermost running [`CfEventLoop::run`] return.
    pub fn stop(&self) {
        // SAFETY: the retained run loop of this thread.
        unsafe { CFRunLoopStop(self.run_loop) };
    }

    /// Runs the run loop in `mode` (the default mode if `None`) for `duration` (tests).
    #[cfg(test)]
    pub fn run_for(&self, mode: Option<CFStringRef>, duration: Duration) {
        let end = Instant::now() + duration;
        // SAFETY: as in `run`.
        let mode = mode.unwrap_or(unsafe { kCFRunLoopDefaultMode });
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            // SAFETY: as in `run`.
            unsafe { CFRunLoopRunInMode(mode, left.as_secs_f64(), 0) };
        }
    }

    // --------------------------------------------------------------------------------------------
    // Sources
    // --------------------------------------------------------------------------------------------

    /// A repeating timer in the common modes, first firing `first` seconds from now.
    fn add_timer(&self, first: f64, interval: f64) -> CFRunLoopTimerRef {
        let mut context = TimerContext {
            version: 0,
            info: (self as *const CfEventLoop).cast_mut().cast(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        // SAFETY: the context is copied by CF; `info` points to `self`, which outlives the timer
        // (invalidated in `drop` or `unregister_timer`).
        unsafe {
            let timer = CFRunLoopTimerCreate(
                std::ptr::null(),
                CFAbsoluteTimeGetCurrent() + first,
                interval,
                0,
                0,
                timer_callout,
                &mut context,
            );
            CFRunLoopAddTimer(self.run_loop, timer, kCFRunLoopCommonModes);
            timer
        }
    }

    /// The dispatch sources `interest` needs.
    fn watches(
        &self,
        fd: RawFd,
        interest: FdEvents,
    ) -> Result<(Option<Watch>, Option<Watch>), HostingError> {
        let signal = self.signal.borrow().clone().expect("created in `new`");
        let make = |write| {
            Watch::new(self.queue, &signal, fd, write)
                .ok_or_else(|| HostingError::HostLoop(format!("no dispatch source for fd {fd}")))
        };
        let read = interest
            .contains(FdEvents::READ)
            .then(|| make(false))
            .transpose()?;
        let write = interest
            .contains(FdEvents::WRITE)
            .then(|| make(true))
            .transpose()?;
        Ok((read, write))
    }

    fn add_fd(&self, fd: RawFd, owner: Owner, interest: FdEvents) -> Result<(), HostingError> {
        if fd < 0 || self.state.borrow().fds.iter().any(|e| e.fd == fd) {
            return Err(HostingError::HostLoop(format!(
                "fd {fd} is invalid or already registered"
            )));
        }
        let (read, write) = self.watches(fd, interest)?;
        self.state.borrow_mut().fds.push(FdEntry {
            fd,
            owner,
            interest,
            write_after: Instant::now(),
            read,
            write,
        });
        Ok(())
    }

    // --------------------------------------------------------------------------------------------
    // Callouts
    // --------------------------------------------------------------------------------------------

    /// The fd sources' run loop source: moves what fired into the ready queue.
    fn fds_fired(&self) {
        let fired = {
            let signal = self.signal.borrow();
            let Some(signal) = signal.as_ref() else {
                return;
            };
            std::mem::take(&mut *signal.fired.lock().unwrap_or_else(PoisonError::into_inner))
        };
        if fired.is_empty() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            let State { fds, ready, .. } = &mut *state;
            for (fd, write) in fired {
                let Some(entry) = fds.iter_mut().find(|e| e.fd == fd) else {
                    continue;
                };
                match entry.owner {
                    Owner::Control => ready.control = true,
                    Owner::Wake => ready.wake = true,
                    Owner::Plugin => {
                        let wanted = if write {
                            FdEvents::WRITE
                        } else {
                            FdEvents::READ
                        };
                        if !entry.interest.contains(wanted) {
                            continue;
                        }
                        let events = readiness(fd, wanted);
                        if events.contains(FdEvents::WRITE) {
                            entry.write_after = Instant::now() + self.write_interval;
                        }
                        if events != FdEvents::NONE {
                            match ready.fds.iter_mut().find(|(f, _)| *f == fd) {
                                Some((_, queued)) => *queued = queued.union(events),
                                None => ready.fds.push((fd, events)),
                            }
                        }
                    }
                }
            }
        }
        self.dispatch();
    }

    fn timer_fired(&self, timer: CFRunLoopTimerRef) {
        {
            let mut state = self.state.borrow_mut();
            if timer == self.deadline_timer.get() {
                state.ready.deadline = true;
            } else if let Some(id) = state.timers.iter().find(|t| t.timer == timer).map(|t| t.id)
                && !state.ready.timers.contains(&id)
            {
                state.ready.timers.push(id);
            }
        }
        self.dispatch();
    }

    fn dispatch(&self) {
        let dispatch = self.dispatch.borrow().clone();
        if let Some(dispatch) = dispatch {
            dispatch();
        }
    }
}

/// What `fd` is ready for right now, within `interest` (`ERROR` always counts), as the Linux
/// loop maps `poll()` results.
fn readiness(fd: RawFd, interest: FdEvents) -> FdEvents {
    let mut flags = PollFlags::empty();
    if interest.contains(FdEvents::READ) {
        flags |= PollFlags::IN;
    }
    if interest.contains(FdEvents::WRITE) {
        flags |= PollFlags::OUT;
    }
    // SAFETY: the plugin keeps `fd` open while it is registered; a closed fd gives POLLNVAL.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut pollfd = [PollFd::from_borrowed_fd(borrowed, flags)];
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if rustix::event::poll(&mut pollfd, Some(&zero)).is_err() {
        return FdEvents::NONE;
    }
    let revents = pollfd[0].revents();
    let mut events = FdEvents::NONE;
    if revents.contains(PollFlags::IN) && interest.contains(FdEvents::READ) {
        events = events.union(FdEvents::READ);
    }
    if revents.contains(PollFlags::OUT) && interest.contains(FdEvents::WRITE) {
        events = events.union(FdEvents::WRITE);
    }
    if revents.intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL) {
        events = events.union(FdEvents::ERROR);
    }
    events
}

extern "C" fn signal_performed(info: *mut c_void) {
    // SAFETY: `info` is the loop that created this source; it is invalidated before the loop is
    // dropped, so the loop is alive during every perform.
    let lp = unsafe { &*info.cast::<CfEventLoop>() };
    lp.fds_fired();
}

extern "C" fn timer_callout(timer: CFRunLoopTimerRef, info: *mut c_void) {
    // SAFETY: as in `signal_performed`.
    let lp = unsafe { &*info.cast::<CfEventLoop>() };
    lp.timer_fired(timer);
}

impl HostLoop for CfEventLoop {
    fn register_timer(&self, period: Duration) -> Result<TimerId, HostingError> {
        let period = period.max(Duration::from_millis(1)).as_secs_f64();
        let timer = self.add_timer(period, period);
        let mut state = self.state.borrow_mut();
        state.next_timer = state.next_timer.wrapping_add(1);
        let id = TimerId(state.next_timer);
        state.timers.push(TimerEntry { id, timer });
        Ok(id)
    }

    fn unregister_timer(&self, timer: TimerId) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        let index = state
            .timers
            .iter()
            .position(|t| t.id == timer)
            .ok_or_else(|| HostingError::HostLoop(format!("unknown timer {}", timer.0)))?;
        let entry = state.timers.swap_remove(index);
        // SAFETY: created by `add_timer`, invalidated and released once.
        unsafe {
            CFRunLoopTimerInvalidate(entry.timer);
            CFRelease(entry.timer);
        }
        Ok(())
    }

    fn register_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError> {
        self.add_fd(fd, Owner::Plugin, events)
    }

    fn modify_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError> {
        if !self.fd_registered(fd) {
            return Err(HostingError::HostLoop(format!("unknown fd {fd}")));
        }
        let (read, write) = self.watches(fd, events)?;
        let old = {
            let mut state = self.state.borrow_mut();
            let entry = state
                .fds
                .iter_mut()
                .find(|e| e.fd == fd && e.owner == Owner::Plugin)
                .expect("checked above");
            entry.interest = events;
            (
                std::mem::replace(&mut entry.read, read),
                std::mem::replace(&mut entry.write, write),
            )
        };
        // The old sources are cancelled outside the borrow (`dispatch_sync_f`).
        drop(old);
        Ok(())
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostingError> {
        let entry = {
            let mut state = self.state.borrow_mut();
            let index = state
                .fds
                .iter()
                .position(|e| e.fd == fd && e.owner == Owner::Plugin)
                .ok_or_else(|| HostingError::HostLoop(format!("unknown fd {fd}")))?;
            state.fds.swap_remove(index)
        };
        drop(entry);
        Ok(())
    }
}

impl Drop for CfEventLoop {
    fn drop(&mut self) {
        let state = std::mem::take(self.state.get_mut());
        // Cancels every fd source (each waits for its queue).
        drop(state.fds);
        let signal = self.signal.get_mut().take();
        // SAFETY: every timer was created by `add_timer` and is released once; the run loop was
        // retained in `with_write_interval`; the run loop source stays allocated while `Signal`
        // (shared with cancel handlers still queued) retains it, but performs no more.
        unsafe {
            for entry in state.timers {
                CFRunLoopTimerInvalidate(entry.timer);
                CFRelease(entry.timer);
            }
            CFRunLoopTimerInvalidate(self.deadline_timer.get());
            CFRelease(self.deadline_timer.get());
            if let Some(signal) = &signal {
                CFRunLoopSourceInvalidate(signal.source);
            }
            dispatch_release(self.queue);
            CFRelease(self.run_loop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::net::UnixStream;

    /// A loop whose dispatcher collects everything that became ready and rearms.
    fn collecting() -> (Rc<CfEventLoop>, Rc<RefCell<Vec<Ready>>>) {
        let lp = CfEventLoop::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let weak = Rc::downgrade(&lp);
        let sink = seen.clone();
        lp.set_dispatch(Some(Rc::new(move || {
            let Some(lp) = weak.upgrade() else {
                return;
            };
            let ready = lp.take_ready();
            sink.borrow_mut().push(ready);
            lp.rearm(None);
        })));
        (lp, seen)
    }

    fn timers(seen: &RefCell<Vec<Ready>>) -> Vec<TimerId> {
        seen.borrow()
            .iter()
            .flat_map(|r| r.timers.clone())
            .collect()
    }

    fn fds(seen: &RefCell<Vec<Ready>>) -> Vec<(RawFd, FdEvents)> {
        seen.borrow().iter().flat_map(|r| r.fds.clone()).collect()
    }

    #[test]
    fn timers_fire_periodically_and_stop() {
        let (lp, seen) = collecting();
        let id = lp.register_timer(Duration::from_millis(10)).unwrap();
        lp.run_for(None, Duration::from_millis(105));
        let fired = timers(&seen);
        assert!(
            (7..=11).contains(&fired.len()),
            "{} timer calls",
            fired.len()
        );
        assert!(fired.iter().all(|t| *t == id));
        assert!(lp.timer_registered(id));
        lp.unregister_timer(id).unwrap();
        assert!(!lp.timer_registered(id));
        assert!(lp.unregister_timer(id).is_err());
        seen.borrow_mut().clear();
        lp.run_for(None, Duration::from_millis(30));
        assert!(timers(&seen).is_empty());
        assert_eq!(lp.registrations(), (0, 0));
    }

    #[test]
    fn own_fds_and_plugin_fds_are_told_apart() {
        let (lp, seen) = collecting();
        let (control, mut app) = UnixStream::pair().unwrap();
        let (plugin, mut other) = UnixStream::pair().unwrap();
        lp.watch(control.as_fd(), Owner::Control).unwrap();
        lp.register_fd(plugin.as_raw_fd(), FdEvents::READ).unwrap();
        assert!(lp.register_fd(plugin.as_raw_fd(), FdEvents::READ).is_err());
        assert!(lp.register_fd(control.as_raw_fd(), FdEvents::READ).is_err());
        assert!(lp.fd_registered(plugin.as_raw_fd()) && !lp.fd_registered(control.as_raw_fd()));

        lp.run_for(None, Duration::from_millis(20));
        assert!(seen.borrow().is_empty());
        app.write_all(b"x").unwrap();
        lp.run_for(None, Duration::from_millis(20));
        assert!(seen.borrow().iter().any(|r| r.control));
        assert!(fds(&seen).is_empty());

        seen.borrow_mut().clear();
        other.write_all(b"y").unwrap();
        lp.run_for(None, Duration::from_millis(20));
        // Level-triggered like `poll()`: the unread byte is reported once per dispatch.
        let reported = fds(&seen);
        assert!(!reported.is_empty());
        assert!(
            reported
                .iter()
                .all(|r| *r == (plugin.as_raw_fd(), FdEvents::READ))
        );

        lp.unregister_fd(plugin.as_raw_fd()).unwrap();
        assert!(!lp.fd_registered(plugin.as_raw_fd()));
        assert!(lp.modify_fd(plugin.as_raw_fd(), FdEvents::READ).is_err());
        assert!(
            lp.unregister_fd(control.as_raw_fd()).is_err(),
            "not the plugin's"
        );
        // A hung-up peer shows as READ | ERROR.
        seen.borrow_mut().clear();
        let (hup, peer) = UnixStream::pair().unwrap();
        lp.register_fd(hup.as_raw_fd(), FdEvents::READ).unwrap();
        drop(peer);
        lp.run_for(None, Duration::from_millis(20));
        let reported = fds(&seen);
        assert!(
            reported.contains(&(hup.as_raw_fd(), FdEvents::READ.union(FdEvents::ERROR))),
            "{reported:?}"
        );
        lp.unregister_fd(hup.as_raw_fd()).unwrap();
    }

    /// The case that lost `Shutdown` with `CFFileDescriptor` (#51): data that arrives while the
    /// dispatcher is still busy with the previous message, and data that arrives long after the
    /// rearm, are both reported, many times over.
    #[test]
    fn every_message_is_reported() {
        let lp = CfEventLoop::new();
        let (control, mut app) = UnixStream::pair().unwrap();
        control.set_nonblocking(true).unwrap();
        lp.watch(control.as_fd(), Owner::Control).unwrap();
        let received = Rc::new(Cell::new(0u32));
        let weak = Rc::downgrade(&lp);
        let r = received.clone();
        let reader = control.try_clone().unwrap();
        lp.set_dispatch(Some(Rc::new(move || {
            let lp = weak.upgrade().unwrap();
            while lp.take_ready().control {
                let mut buf = [0u8; 64];
                while let Ok(n) = std::io::Read::read(&mut &reader, &mut buf) {
                    if n == 0 {
                        break;
                    }
                    r.set(r.get() + n as u32);
                }
                // A slow handler: the next byte arrives before the rearm.
                std::thread::sleep(Duration::from_millis(1));
            }
            lp.rearm(None);
        })));
        let writer = std::thread::spawn(move || {
            for i in 0..200u32 {
                app.write_all(b"m").unwrap();
                std::thread::sleep(Duration::from_micros(if i % 2 == 0 { 300 } else { 3000 }));
            }
            app
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while received.get() < 200 && Instant::now() < deadline {
            lp.run_for(None, Duration::from_millis(10));
        }
        let _app = writer.join().unwrap();
        assert_eq!(received.get(), 200);
        lp.set_dispatch(None);
    }

    /// Spike WP-15 §1.7: an always-writable fd with WRITE interest must not make the loop spin.
    #[test]
    fn write_interest_is_rate_limited() {
        let (lp, seen) = collecting();
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let fd = ours.as_raw_fd();
        lp.register_fd(fd, FdEvents::READ.union(FdEvents::WRITE))
            .unwrap();
        lp.run_for(None, Duration::from_millis(500));
        let reported = fds(&seen);
        // 500 ms / 50 ms = 10 cooldowns; allow for timing slack.
        assert!(
            (5..=12).contains(&reported.len()),
            "{} WRITE reports",
            reported.len()
        );
        assert!(reported.iter().all(|r| *r == (fd, FdEvents::WRITE)));
        assert!(
            seen.borrow().len() < 40,
            "{} dispatches",
            seen.borrow().len()
        );
        // Without WRITE interest: nothing at all.
        lp.modify_fd(fd, FdEvents::READ).unwrap();
        seen.borrow_mut().clear();
        lp.run_for(None, Duration::from_millis(100));
        assert!(fds(&seen).is_empty());
        lp.unregister_fd(fd).unwrap();
    }

    /// Events that arrive while the dispatcher is busy (a plugin running a nested run loop) stay
    /// queued, and the fd is not reported again until the busy dispatch rearms.
    #[test]
    fn a_busy_dispatcher_keeps_events_queued() {
        let lp = CfEventLoop::new();
        let (plugin, mut other) = UnixStream::pair().unwrap();
        let fd = plugin.as_raw_fd();
        lp.register_fd(fd, FdEvents::READ).unwrap();
        let busy = Rc::new(Cell::new(false));
        let calls = Rc::new(Cell::new(0u32));
        let handled = Rc::new(RefCell::new(Vec::new()));
        let weak = Rc::downgrade(&lp);
        let (b, c, h) = (busy.clone(), calls.clone(), handled.clone());
        lp.set_dispatch(Some(Rc::new(move || {
            c.set(c.get() + 1);
            if b.get() {
                return;
            }
            let lp = weak.upgrade().unwrap();
            b.set(true);
            // The plugin's call runs a nested loop: callouts fire, the dispatcher is busy.
            lp.run_for(None, Duration::from_millis(30));
            loop {
                let ready = lp.take_ready();
                if ready.is_empty() {
                    break;
                }
                h.borrow_mut().push(ready);
            }
            b.set(false);
        })));
        other.write_all(b"x").unwrap();
        lp.run_for(None, Duration::from_millis(5));
        // One callout entered the dispatcher; the fd stayed suspended inside the nested loop.
        assert_eq!(calls.get(), 1);
        let handled = handled.borrow();
        assert_eq!(handled.len(), 1);
        assert_eq!(handled[0].fds, [(fd, FdEvents::READ)]);
        lp.unregister_fd(fd).unwrap();
    }

    /// The deadline timer fires at the deadline `rearm` set, and not again until the next one.
    #[test]
    fn rearm_sets_the_deadline() {
        let (lp, seen) = collecting();
        lp.run_for(None, Duration::from_millis(30));
        assert!(seen.borrow().is_empty());
        let t0 = Instant::now();
        lp.rearm(Some(t0 + Duration::from_millis(20)));
        // The collecting dispatcher rearms with no deadline after each dispatch.
        lp.run_for(None, Duration::from_millis(60));
        let seen = seen.borrow();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].deadline);
    }

    /// Sources are in the common modes: they fire in any mode added to them (AppKit adds
    /// `NSEventTrackingRunLoopMode` for live resize and menus).
    #[test]
    fn sources_fire_in_every_common_mode() {
        let (lp, seen) = collecting();
        // SAFETY: a constant C string; the mode lives for the test.
        let mode = unsafe {
            CFStringCreateWithCString(
                std::ptr::null(),
                c"KpTrackingLikeMode".as_ptr(),
                0x0800_0100,
            )
        };
        // SAFETY: this thread's run loop.
        unsafe { CFRunLoopAddCommonMode(CFRunLoopGetCurrent(), mode) };
        let id = lp.register_timer(Duration::from_millis(5)).unwrap();
        let (control, mut app) = UnixStream::pair().unwrap();
        lp.watch(control.as_fd(), Owner::Control).unwrap();
        app.write_all(b"x").unwrap();
        lp.run_for(Some(mode), Duration::from_millis(40));
        assert!(timers(&seen).contains(&id));
        assert!(seen.borrow().iter().any(|r| r.control));
        // SAFETY: created above.
        unsafe { CFRelease(mode) };
    }

    /// `run` returns after `stop` from inside a callout, and can be entered again.
    #[test]
    fn run_stops_and_resumes() {
        let lp = CfEventLoop::new();
        let count = Rc::new(Cell::new(0));
        let weak = Rc::downgrade(&lp);
        let c = count.clone();
        lp.set_dispatch(Some(Rc::new(move || {
            let lp = weak.upgrade().unwrap();
            lp.take_ready();
            c.set(c.get() + 1);
            lp.stop();
        })));
        lp.register_timer(Duration::from_millis(5)).unwrap();
        let t0 = Instant::now();
        lp.run();
        lp.run();
        assert_eq!(count.get(), 2);
        assert!(t0.elapsed() < Duration::from_secs(1));
        lp.set_dispatch(None);
    }
}
