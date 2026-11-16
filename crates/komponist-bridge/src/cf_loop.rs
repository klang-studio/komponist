//! [`CfEventLoop`]: the bridge main loop's [`HostLoop`] on macOS, built on the thread's CFRunLoop
//! (ARCHITECTURE §8.2; spike #42 Q5/Q6).
//!
//! On macOS the bridge's main thread is the Cocoa main thread. JUCE's message queue and timers are
//! CFRunLoop sources, and the plugin frame of #52 will run eframe (`[NSApp run]`) on it, so the
//! loop cannot be a `poll()` of its own. Everything the bridge waits for is a CFRunLoop source in
//! the **common modes**, so it keeps firing inside `CFRunLoopRun`, inside eframe's event loop, and
//! in `NSEventTrackingRunLoopMode` (live resize, menus):
//!
//! - **fds** (the control socket, the notification wake pipe, every fd the plugin registers): a
//!   `CFFileDescriptor` source each. Its callbacks are one-shot: the callout only records the
//!   readiness (a zero-timeout `poll()` of that fd gives the same `READ` / `WRITE` / `ERROR` bits
//!   as the Linux loop) and the fd is armed again only after the bridge handled it ([`rearm`]), so
//!   a readiness that waits for a busy bridge does not spin.
//! - **timers** of the plugin: a repeating `CFRunLoopTimer` each (CF skips missed periods itself).
//! - **one internal timer** for the next deadline: the bridge's next `ParamValues` and the end of a
//!   write cooldown.
//! - **Cross-thread notifications** stay the non-blocking wake pipe of `BridgeNotify` (watched as
//!   above). Spike #42 proposed a `CFRunLoopSource` v0 signalled with `CFRunLoopWakeUp`; those take
//!   CF's internal locks (`CFRunLoopWakeUp` holds the run loop's lock around its `mach_msg`), and
//!   `notify` runs on the RT thread too (CLAP `request_callback`, `request_restart`,
//!   `request_flush`), where rule 4 allows no lock. A pipe `write` takes none in user space, and
//!   the pipe's source is a common-mode CF source like the others, not a block on the (serial)
//!   main dispatch queue, which is what the spike warned against.
//!
//! **Dispatch** follows the rules of the Linux loop (`event_loop`): callouts only queue what is
//! ready ([`CfEventLoop::take_ready`]) and call the dispatcher the bridge installed
//! ([`CfEventLoop::set_dispatch`]). The bridge handles the queue outside every borrow of this
//! loop's state and checks before each plugin call that the fd or timer is still registered. A
//! callout that fires while the bridge is busy (a plugin running a nested run loop inside a call)
//! finds it borrowed and leaves its event queued; the busy dispatch drains the queue before it
//! returns.
//!
//! **Write interest** keeps the Linux policy: after a `WRITE` report the fd gets no write callback
//! for `DEFAULT_WRITE_INTERVAL`. **Differences** from `poll()`: `ERROR` is seen only together with
//! a read callback (so only with `READ` interest), and an fd closed without unregistering is never
//! reported (kqueue drops it) instead of being reported once and parked.
//!
//! **Outer loop** ([`CfEventLoop::run`], [`CfEventLoop::stop`]): `CFRunLoopRun` until the bridge
//! stops it. All sources stay installed when it returns, so the bridge can run something else that
//! spins the same run loop (#52: `eframe::run_native` with run-and-return for a plugin frame) and
//! enter `run` again afterwards.
//!
//! Raw CoreFoundation FFI, as in `komponist_hosting`'s bundle module; no extra crate.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::os::fd::{BorrowedFd, RawFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::DEFAULT_WRITE_INTERVAL;
use komponist_hosting::{FdEvents, HostLoop, HostingError, TimerId};
use rustix::event::{PollFd, PollFlags, Timespec};

mod ffi {
    use std::ffi::c_void;

    pub type CFTypeRef = *const c_void;
    pub type CFRunLoopRef = *mut c_void;
    pub type CFRunLoopSourceRef = *mut c_void;
    pub type CFRunLoopTimerRef = *mut c_void;
    pub type CFFileDescriptorRef = *mut c_void;
    pub type CFStringRef = *const c_void;
    pub type CFOptionFlags = usize;
    pub type CFAbsoluteTime = f64;

    pub const READ_CALLBACK: CFOptionFlags = 1 << 0;
    pub const WRITE_CALLBACK: CFOptionFlags = 1 << 1;

    /// `CFFileDescriptorContext` and `CFRunLoopTimerContext` (the same layout).
    #[repr(C)]
    pub struct Context {
        pub version: isize,
        pub info: *mut c_void,
        pub retain: *const c_void,
        pub release: *const c_void,
        pub copy_description: *const c_void,
    }

    pub type FdCallout = extern "C" fn(CFFileDescriptorRef, CFOptionFlags, *mut c_void);
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
        pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
        pub fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
        pub fn CFRunLoopSourceInvalidate(source: CFRunLoopSourceRef);
        pub fn CFFileDescriptorCreate(
            allocator: CFTypeRef,
            fd: i32,
            close_on_invalidate: u8,
            callout: FdCallout,
            context: *const Context,
        ) -> CFFileDescriptorRef;
        pub fn CFFileDescriptorGetNativeDescriptor(f: CFFileDescriptorRef) -> i32;
        pub fn CFFileDescriptorEnableCallBacks(f: CFFileDescriptorRef, types: CFOptionFlags);
        pub fn CFFileDescriptorDisableCallBacks(f: CFFileDescriptorRef, types: CFOptionFlags);
        pub fn CFFileDescriptorCreateRunLoopSource(
            allocator: CFTypeRef,
            f: CFFileDescriptorRef,
            order: isize,
        ) -> CFRunLoopSourceRef;
        pub fn CFFileDescriptorInvalidate(f: CFFileDescriptorRef);
        pub fn CFRunLoopTimerCreate(
            allocator: CFTypeRef,
            fire_date: CFAbsoluteTime,
            interval: f64,
            flags: CFOptionFlags,
            order: isize,
            callout: TimerCallout,
            context: *mut Context,
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
            text: *const std::ffi::c_char,
            encoding: u32,
        ) -> CFStringRef;
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

/// A `CFFileDescriptor` with its run-loop source.
#[derive(Debug)]
struct FdSource {
    descriptor: CFFileDescriptorRef,
    source: CFRunLoopSourceRef,
}

impl FdSource {
    fn invalidate(self) {
        // SAFETY: both were created by `watch` and are invalidated and released once.
        unsafe {
            CFRunLoopSourceInvalidate(self.source);
            CFFileDescriptorInvalidate(self.descriptor);
            CFRelease(self.source);
            CFRelease(self.descriptor);
        }
    }
}

#[derive(Debug)]
struct FdEntry {
    fd: RawFd,
    owner: Owner,
    interest: FdEvents,
    /// No write callback before this instant (write cooldown).
    write_after: Instant,
    source: FdSource,
}

#[derive(Debug)]
struct TimerEntry {
    id: TimerId,
    timer: CFRunLoopTimerRef,
}

#[derive(Debug, Default)]
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
}

impl std::fmt::Debug for CfEventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CfEventLoop")
            .field("state", &self.state)
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
        // SAFETY: the current thread's run loop, retained for the loop's lifetime.
        let run_loop = unsafe {
            let rl = CFRunLoopGetCurrent();
            CFRetain(rl);
            rl
        };
        let lp = Rc::new(CfEventLoop {
            run_loop,
            state: RefCell::new(State::default()),
            dispatch: RefCell::new(None),
            write_interval,
            deadline_timer: Cell::new(std::ptr::null_mut()),
        });
        let timer = lp.add_timer(FAR_FUTURE, FAR_FUTURE);
        lp.deadline_timer.set(timer);
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

    /// After the bridge handled everything [`CfEventLoop::take_ready`] returned: arms every fd
    /// again (the callouts disarm theirs; `WRITE` only after its cooldown) and sets the deadline
    /// timer to `deadline` or the end of the next write cooldown, whichever is earlier.
    pub fn rearm(&self, deadline: Option<Instant>) {
        let now = Instant::now();
        let mut next = deadline;
        for entry in &self.state.borrow().fds {
            if let Some(cooldown_end) = self.arm(entry, now) {
                next = Some(next.map_or(cooldown_end, |n| n.min(cooldown_end)));
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

    fn context(&self) -> Context {
        Context {
            version: 0,
            info: (self as *const CfEventLoop).cast_mut().cast(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        }
    }

    /// A repeating timer in the common modes, first firing `first` seconds from now.
    fn add_timer(&self, first: f64, interval: f64) -> CFRunLoopTimerRef {
        let mut context = self.context();
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

    fn add_fd(&self, fd: RawFd, owner: Owner, interest: FdEvents) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        if fd < 0 || state.fds.iter().any(|e| e.fd == fd) {
            return Err(HostingError::HostLoop(format!(
                "fd {fd} is invalid or already registered"
            )));
        }
        let context = self.context();
        // SAFETY: as in `add_timer`; the fd stays open while it is registered (the caller's
        // contract), and CF does not close it (`close_on_invalidate` false).
        let source = unsafe {
            let descriptor = CFFileDescriptorCreate(std::ptr::null(), fd, 0, fd_callout, &context);
            if descriptor.is_null() {
                return Err(HostingError::HostLoop(format!(
                    "CFFileDescriptorCreate({fd}) failed"
                )));
            }
            let source = CFFileDescriptorCreateRunLoopSource(std::ptr::null(), descriptor, 0);
            CFRunLoopAddSource(self.run_loop, source, kCFRunLoopCommonModes);
            FdSource { descriptor, source }
        };
        let entry = FdEntry {
            fd,
            owner,
            interest,
            write_after: Instant::now(),
            source,
        };
        self.arm(&entry, Instant::now());
        state.fds.push(entry);
        Ok(())
    }

    /// Enables the callbacks `entry` wants now; returns the end of a write cooldown that keeps
    /// its write callback off.
    fn arm(&self, entry: &FdEntry, now: Instant) -> Option<Instant> {
        let mut types = 0;
        let mut cooldown_end = None;
        if entry.interest.contains(FdEvents::READ) {
            types |= READ_CALLBACK;
        }
        if entry.interest.contains(FdEvents::WRITE) {
            if now >= entry.write_after {
                types |= WRITE_CALLBACK;
            } else {
                cooldown_end = Some(entry.write_after);
            }
        }
        // SAFETY: a live descriptor of this loop. Enabling an enabled callback changes nothing.
        unsafe {
            CFFileDescriptorDisableCallBacks(
                entry.source.descriptor,
                (READ_CALLBACK | WRITE_CALLBACK) & !types,
            );
            if types != 0 {
                CFFileDescriptorEnableCallBacks(entry.source.descriptor, types);
            }
        }
        cooldown_end
    }

    // --------------------------------------------------------------------------------------------
    // Callouts
    // --------------------------------------------------------------------------------------------

    fn fd_fired(&self, descriptor: CFFileDescriptorRef) {
        // SAFETY: CF hands the descriptor that fired; it is live during its callout.
        let fd = unsafe { CFFileDescriptorGetNativeDescriptor(descriptor) };
        // Disarmed until the bridge handled it (`rearm`).
        // SAFETY: as above.
        unsafe { CFFileDescriptorDisableCallBacks(descriptor, READ_CALLBACK | WRITE_CALLBACK) };
        {
            let mut state = self.state.borrow_mut();
            let State { fds, ready, .. } = &mut *state;
            let Some(entry) = fds.iter_mut().find(|e| e.fd == fd) else {
                return;
            };
            match entry.owner {
                Owner::Control => ready.control = true,
                Owner::Wake => ready.wake = true,
                Owner::Plugin => {
                    let events = readiness(fd, entry.interest);
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

extern "C" fn fd_callout(
    descriptor: CFFileDescriptorRef,
    _types: CFOptionFlags,
    info: *mut c_void,
) {
    // SAFETY: `info` is the loop that created this source; sources are invalidated before the
    // loop is dropped, so it is alive during every callout.
    let lp = unsafe { &*info.cast::<CfEventLoop>() };
    lp.fd_fired(descriptor);
}

extern "C" fn timer_callout(timer: CFRunLoopTimerRef, info: *mut c_void) {
    // SAFETY: as in `fd_callout`.
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
        let mut state = self.state.borrow_mut();
        let entry = state
            .fds
            .iter_mut()
            .find(|e| e.fd == fd && e.owner == Owner::Plugin)
            .ok_or_else(|| HostingError::HostLoop(format!("unknown fd {fd}")))?;
        entry.interest = events;
        self.arm(entry, Instant::now());
        Ok(())
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        let index = state
            .fds
            .iter()
            .position(|e| e.fd == fd && e.owner == Owner::Plugin)
            .ok_or_else(|| HostingError::HostLoop(format!("unknown fd {fd}")))?;
        state.fds.swap_remove(index).source.invalidate();
        Ok(())
    }
}

impl Drop for CfEventLoop {
    fn drop(&mut self) {
        let state = std::mem::take(self.state.get_mut());
        for entry in state.fds {
            entry.source.invalidate();
        }
        // SAFETY: every timer was created by `add_timer` and is released once; the run loop was
        // retained in `with_write_interval`.
        unsafe {
            for entry in state.timers {
                CFRunLoopTimerInvalidate(entry.timer);
                CFRelease(entry.timer);
            }
            CFRunLoopTimerInvalidate(self.deadline_timer.get());
            CFRelease(self.deadline_timer.get());
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

    /// Events of a callout that fires while the dispatcher is busy (a plugin running a nested run
    /// loop) stay queued, and the fd is not armed again until the busy dispatch rearms.
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
        // One callout entered the dispatcher; the fd stayed disarmed inside the nested loop.
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
