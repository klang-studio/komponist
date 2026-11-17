//! Cross-process wait and wake on shared 32-bit words, with deadlines (ARCHITECTURE §8.1).
//!
//! Both words of the block handshake (`request_seq`, `done_seq`) live in the shared memory of one
//! bridge, so the waits are *shared* (they find waiters in other processes). A deadline is turned
//! once into an absolute time of the clock `std::time::Instant` measures, so a deadline computed
//! once per block stays valid across spurious wake-ups. Per OS:
//!
//! - Linux: a shared futex, `FUTEX_WAIT_BITSET` with an absolute `CLOCK_MONOTONIC` deadline
//!   (what `Instant` reads on Linux).
//! - macOS (14.4+): `os_sync_wait_on_address_with_deadline` / `os_sync_wake_by_address_all` with
//!   the `SHARED` flags and an absolute `OS_CLOCK_MACH_ABSOLUTE_TIME` deadline, through
//!   `komponist_rt::os_sync` (#45). `Instant` reads `CLOCK_UPTIME_RAW`, which is
//!   `mach_absolute_time` in nanoseconds, so the deadline is the current `mach_absolute_time` plus
//!   the remaining time in mach ticks (rounded up). The kernel ends timed waits late by a timer
//!   leeway that depends on the OS version and on how idle the machine is (12.5 % of the wait on
//!   macOS 27, about 0.7 ms for any wait on the macOS 15 CI runner, #101), so each kernel wait is armed short
//!   of the deadline by 1/4 of the remaining time or by the leeway `komponist_rt::os_sync` has
//!   learned, and the last stretch is slept in short `mach_wait_until` slices with a look at the
//!   word between them.
//! - Windows: there is no cross-process address wait (`WaitOnAddress` finds waiters in its own
//!   process only), so [`wait_while`] and [`wake`] work **within one process** there
//!   (`WaitOnAddress` / `WakeByAddressAll`, the deadline in whole milliseconds on the system clock
//!   tick). The cross-process handshake of `shm` uses the event wait below instead.
//!
//! RT use: [`wait_while`] is the only blocking call the engine's audio thread may make, always
//! with a deadline (on macOS its last stretch sleeps in `mach_wait_until` slices of at most
//! 25 µs); [`wake`] never blocks. Neither allocates or takes a lock.
//!
//! # Windows: events and a high-resolution timer (spike S1, internal notes)
//!
//! Each shared word has an auto-reset event that both processes hold (`WakeEvent`, created by
//! the app, duplicated into the bridge), and each waiting role a high-resolution waitable timer
//! of its own (`DeadlineTimer`, `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`, created once, re-armed
//! with `SetWaitableTimerEx` before each wait). The word stays the source of truth: the waker
//! stores it, then calls `SetEvent`; the waiter re-reads it after every wake, so a stale event
//! (left signalled after a wait that saw the change without blocking) costs one extra loop, never
//! a wrong answer. `wait_while_event` blocks in `WaitForMultipleObjects([event, timer])`. A
//! millisecond timeout (`WaitForSingleObject(ms)`) is never used for a deadline: at the default
//! clock resolution a 1 ms wait took a median of 12 ms in the spike.
//!
//! Two modes, from the spike's numbers (Ryzen 5 5600, Windows 11 23H2):
//!
//! - `Mode::Exact`: the app waiting for `done_seq` on its audio thread. First it spins up to
//!   `SPIN` = 20 µs on the word: an event wake costs 2–4 µs per direction (cold round trip p50
//!   4–8 µs, p99 6–13 µs under MMCSS), and a bridge that answers within the spin saves that (round
//!   trip p50 0.9 µs). Then the timer: it fired p50 +13–20 µs late, but 1–2 % of the waits a whole
//!   0.5 ms quantum late (p99 +504–516 µs, max +562 µs under MMCSS). A timed-out block is already a
//!   dropout of one plugin; ending its wait 0.5 ms late on top would push the whole engine callback
//!   towards an underrun at small buffers (64 frames at 48 kHz are 1.33 ms). So the timer is armed
//!   `TAIL` = 600 µs before the deadline (above the worst lateness measured under MMCSS) and the
//!   rest is spun on the word and the clock: a timed-out wait ends within a few µs of its deadline,
//!   for at most 600 µs of spinning, and only when the bridge is that late (or answers inside that
//!   window, which the spin then sees at once). Without MMCSS the timer's tail reached 5 ms in the
//!   spike; such a wait ends late by the timer's lateness minus `TAIL`.
//! - `Mode::Relaxed`: the bridge waiting for `request_seq`, whose deadline only lets its RT thread
//!   look for shutdown. No spin, the timer armed at the deadline (so up to ≈ 0.5 ms late). The
//!   bridge is idle for most of each period; spinning there would burn a core per bridge for
//!   nothing.
//!
//! Neither mode changes the process' timer resolution (`timeBeginPeriod` is never called): the
//! high-resolution timer does not depend on it. The timer flag needs Windows 10 1803 or later. A
//! rare 0.3–0.4 ms stall of the waiting thread (an interrupt or DPC taking its core, about once per
//! 10 000 round trips in the spike, even under MMCSS) is not a wake latency, and no mode removes
//! it.

use core::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

/// Outcome of [`wait_while`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitOutcome {
    /// The word no longer holds the awaited value; carries the value read.
    Changed(u32),
    /// The deadline passed while the word still held the value.
    TimedOut,
}

/// What one kernel wait returned, independent of the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// Woken (possibly spuriously), the word changed before the kernel looked, or a signal or a
    /// transient condition ended the wait early: look at the word again.
    Retry,
    /// The deadline passed.
    TimedOut,
    /// An unexpected error (bad alignment or arguments); retrying would spin.
    Failed,
}

/// Blocks while `word == current`, until another thread or process changes it and calls
/// [`wake`], or until `deadline` (`None` = no deadline). Returns at once if the word already
/// differs. Handles spurious wake-ups and `EINTR` internally.
pub fn wait_while(word: &AtomicU32, current: u32, deadline: Option<Instant>) -> WaitOutcome {
    let value = word.load(Ordering::Acquire);
    if value != current {
        return WaitOutcome::Changed(value);
    }
    // Translate the deadline once into an absolute time of the kernel's clock, so retries after
    // spurious wake-ups or EINTR keep the same deadline.
    let timeout = deadline.map(sys::absolute_deadline);
    loop {
        match sys::wait(word, current, timeout) {
            Wait::Retry => {}
            Wait::TimedOut => {
                let value = word.load(Ordering::Acquire);
                return if value != current {
                    WaitOutcome::Changed(value)
                } else {
                    WaitOutcome::TimedOut
                };
            }
            // Not expected for an aligned word and a valid deadline. Do not spin: with a deadline
            // report a timeout, without one fall back to yielding.
            Wait::Failed => {
                let value = word.load(Ordering::Acquire);
                if value != current {
                    return WaitOutcome::Changed(value);
                }
                if deadline.is_some() {
                    return WaitOutcome::TimedOut;
                }
                std::thread::yield_now();
            }
        }
        let value = word.load(Ordering::Acquire);
        if value != current {
            return WaitOutcome::Changed(value);
        }
    }
}

/// Wakes every waiter on `word` (in any process). Call after storing the new value with release
/// ordering. Never blocks.
///
/// Returns the number of woken waiters on Linux; on macOS, where the kernel does not report a
/// count, 1 if it woke at least one waiter and 0 if there was none; on Windows (in-process only,
/// see the module docs) always 0, since `WakeByAddressAll` reports nothing.
pub fn wake(word: &AtomicU32) -> usize {
    sys::wake(word)
}

#[cfg(target_os = "linux")]
mod sys {
    use core::num::NonZeroU32;
    use core::sync::atomic::AtomicU32;
    use std::time::Instant;

    use rustix::io::Errno;
    use rustix::thread::futex;
    use rustix::time::{ClockId, Timespec, clock_gettime};

    use super::Wait;

    /// `FUTEX_BITSET_MATCH_ANY`: wait for any wake-up on the word.
    const MATCH_ANY: NonZeroU32 = NonZeroU32::MAX;

    /// An absolute `CLOCK_MONOTONIC` time.
    pub(super) type Deadline = Timespec;

    /// `deadline` as an absolute `CLOCK_MONOTONIC` time (the clock `Instant` reads on Linux). Two
    /// vDSO clock reads, no syscall, no allocation.
    pub(super) fn absolute_deadline(deadline: Instant) -> Timespec {
        let now = clock_gettime(ClockId::Monotonic);
        let remaining = deadline.saturating_duration_since(Instant::now());
        let mut sec = now.tv_sec.saturating_add(remaining.as_secs() as i64);
        let mut nsec = now.tv_nsec + i64::from(remaining.subsec_nanos());
        if nsec >= 1_000_000_000 {
            nsec -= 1_000_000_000;
            sec = sec.saturating_add(1);
        }
        Timespec {
            tv_sec: sec,
            tv_nsec: nsec,
        }
    }

    /// One `FUTEX_WAIT_BITSET` on a shared futex (no `FUTEX_PRIVATE_FLAG`).
    pub(super) fn wait(word: &AtomicU32, current: u32, deadline: Option<Deadline>) -> Wait {
        match futex::wait_bitset(
            word,
            futex::Flags::empty(),
            current,
            deadline.as_ref(),
            MATCH_ANY,
        ) {
            Ok(()) | Err(Errno::AGAIN) | Err(Errno::INTR) => Wait::Retry,
            Err(Errno::TIMEDOUT) => Wait::TimedOut,
            // EFAULT, EINVAL, ENOSYS.
            Err(_) => Wait::Failed,
        }
    }

    pub(super) fn wake(word: &AtomicU32) -> usize {
        // A shared futex (no FUTEX_PRIVATE_FLAG): the waiter may sit in another process.
        futex::wake(word, futex::Flags::empty(), i32::MAX as u32).unwrap_or(0)
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use core::sync::atomic::AtomicU32;
    use std::time::Instant;

    use komponist_rt::mach_time;
    use komponist_rt::os_sync::{self, Scope};

    use super::Wait;

    /// An absolute `mach_absolute_time` value (ticks).
    pub(super) type Deadline = u64;

    /// `deadline` as an absolute `mach_absolute_time` (the clock behind `Instant`), never earlier
    /// than `deadline`.
    pub(super) fn absolute_deadline(deadline: Instant) -> u64 {
        mach_time::deadline(deadline)
    }

    /// One `os_sync_wait_on_address[_with_deadline]` with the `SHARED` flag (the word may sit in
    /// shared memory and be woken from another process), armed short of the deadline to make up
    /// for the kernel's timer leeway, or near the deadline one poll slice ([`os_sync`]).
    pub(super) fn wait(word: &AtomicU32, current: u32, deadline: Option<Deadline>) -> Wait {
        match os_sync::wait(word, current, deadline, Scope::Shared) {
            os_sync::Wait::Retry => Wait::Retry,
            os_sync::Wait::TimedOut => Wait::TimedOut,
            os_sync::Wait::Failed => Wait::Failed,
        }
    }

    pub(super) fn wake(word: &AtomicU32) -> usize {
        usize::from(os_sync::wake_all(word, Scope::Shared))
    }
}

/// Windows, in-process only: `WaitOnAddress` / `WakeByAddressAll`.
#[cfg(windows)]
mod sys {
    use core::ffi::c_void;
    use core::sync::atomic::AtomicU32;
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{ERROR_TIMEOUT, GetLastError};
    use windows_sys::Win32::System::Threading::{INFINITE, WaitOnAddress, WakeByAddressAll};

    use super::Wait;

    /// `Instant` itself: `WaitOnAddress` takes a relative timeout, computed per wait.
    pub(super) type Deadline = Instant;

    pub(super) fn absolute_deadline(deadline: Instant) -> Instant {
        deadline
    }

    /// One `WaitOnAddress` with the remaining time rounded up to whole milliseconds. Its timeout
    /// may end early by up to a clock tick, so `TimedOut` is reported only once `Instant` has
    /// reached the deadline; before that the caller just waits again.
    pub(super) fn wait(word: &AtomicU32, current: u32, deadline: Option<Deadline>) -> Wait {
        let timeout = match deadline {
            None => INFINITE,
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Wait::TimedOut;
                }
                // Below INFINITE (u32::MAX), which would mean "no timeout".
                remaining
                    .as_nanos()
                    .div_ceil(1_000_000)
                    .min(u128::from(INFINITE - 1)) as u32
            }
        };
        // SAFETY: both pointers are valid for 4 bytes for the duration of the call; `word` is an
        // aligned atomic.
        let woken = unsafe {
            WaitOnAddress(
                word.as_ptr().cast::<c_void>(),
                (&raw const current).cast::<c_void>(),
                4,
                timeout,
            )
        };
        if woken != 0 {
            return Wait::Retry;
        }
        // SAFETY: plain FFI call without arguments.
        match unsafe { GetLastError() } {
            ERROR_TIMEOUT if deadline.is_some_and(|d| Instant::now() >= d) => Wait::TimedOut,
            ERROR_TIMEOUT => Wait::Retry,
            _ => Wait::Failed,
        }
    }

    pub(super) fn wake(word: &AtomicU32) -> usize {
        // SAFETY: `word` is a valid address; waking an address nobody waits on is a no-op.
        unsafe { WakeByAddressAll(word.as_ptr().cast::<c_void>()) };
        0
    }
}

#[cfg(windows)]
pub(crate) use win::{DeadlineTimer, Mode, WakeEvent, wait_while_event};

/// Windows: the cross-process wait of the block handshake (module docs, "Windows").
#[cfg(windows)]
mod win {
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::io;
    use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
        CreateWaitableTimerExW, INFINITE, SetEvent, SetWaitableTimerEx, TIMER_ALL_ACCESS,
        WaitForMultipleObjects, WaitForSingleObject,
    };

    use super::WaitOutcome;

    /// How long [`Mode::Exact`] spins on the word before it blocks (spike S1: 20 µs; 50 µs gained
    /// nothing more).
    pub(crate) const SPIN: Duration = Duration::from_micros(20);

    /// How long before the deadline [`Mode::Exact`] arms its timer; the rest is spun. Above the
    /// timer's worst lateness under MMCSS in spike S1 (+562 µs).
    pub(crate) const TAIL: Duration = Duration::from_micros(600);

    /// `WaitForMultipleObjects` result: the timer (index 1) fired.
    const TIMER_FIRED: u32 = WAIT_OBJECT_0 + 1;

    /// How precisely a wait ends at its deadline (module docs).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Mode {
        /// Spin [`SPIN`] first; arm the timer [`TAIL`] early and spin to the deadline.
        Exact,
        /// No spinning; the timer at the deadline (up to ≈ 0.5 ms late).
        Relaxed,
    }

    /// A handle from a create call, or the OS error.
    fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: a fresh handle that nothing else owns.
            Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
        }
    }

    /// An auto-reset event that wakes the waiter of one shared word, in this or another process.
    #[derive(Debug)]
    pub(crate) struct WakeEvent(OwnedHandle);

    impl WakeEvent {
        /// A new, non-signalled, non-inheritable auto-reset event.
        pub(crate) fn new() -> io::Result<WakeEvent> {
            // SAFETY: no security attributes, no name.
            let event = unsafe { CreateEventW(core::ptr::null(), 0, 0, core::ptr::null()) };
            owned(event).map(WakeEvent)
        }

        /// An event handle received from the other side (it must be an event: anything else makes
        /// every wait fail, which callers see as a timeout).
        pub(crate) fn from_handle(handle: OwnedHandle) -> WakeEvent {
            WakeEvent(handle)
        }

        /// Wakes the waiter. Call after storing the word with release ordering. Never blocks.
        pub(crate) fn wake(&self) {
            // SAFETY: a valid event handle. It cannot fail for an event with EVENT_MODIFY_STATE,
            // which creation and DUPLICATE_SAME_ACCESS give.
            unsafe { SetEvent(self.0.as_raw_handle()) };
        }
    }

    impl AsHandle for WakeEvent {
        fn as_handle(&self) -> BorrowedHandle<'_> {
            self.0.as_handle()
        }
    }

    /// The deadline timer of one waiting role: a high-resolution synchronization timer, used by
    /// one thread at a time (the role objects of `shm` guarantee it; two concurrent waiters would
    /// only get wrong deadlines, nothing unsafe).
    #[derive(Debug)]
    pub(crate) struct DeadlineTimer(OwnedHandle);

    impl DeadlineTimer {
        /// A new high-resolution waitable timer (Windows 10 1803+).
        pub(crate) fn new() -> io::Result<DeadlineTimer> {
            // SAFETY: no security attributes, no name.
            let timer = unsafe {
                CreateWaitableTimerExW(
                    core::ptr::null(),
                    core::ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            owned(timer).map(DeadlineTimer)
        }

        /// Arms the timer to fire after `after` (rounded up to 100 ns, at least 100 ns); re-arming
        /// also resets a signal left from an earlier expiry. `false` if the call failed.
        fn arm(&self, after: Duration) -> bool {
            let ticks = after.as_nanos().div_ceil(100).clamp(1, i64::MAX as u128) as i64;
            // Negative: relative to now.
            let due = -ticks;
            // SAFETY: a valid timer handle; no completion routine, no wake context, no tolerable
            // delay (coalescing would only make it later).
            unsafe {
                SetWaitableTimerEx(
                    self.0.as_raw_handle(),
                    &due,
                    0,
                    None,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                ) != 0
            }
        }

        fn cancel(&self) {
            // SAFETY: a valid timer handle.
            unsafe { CancelWaitableTimer(self.0.as_raw_handle()) };
        }
    }

    /// Blocks while `word == current`, until the other side stores a new value and signals
    /// `event`, or until `deadline` (`None` = none). `timer` belongs to the calling role. Never
    /// allocates; the only blocking call is `WaitForMultipleObjects` (`WaitForSingleObject` without
    /// a deadline) with an `INFINITE` timeout.
    pub(crate) fn wait_while_event(
        word: &AtomicU32,
        current: u32,
        deadline: Option<Instant>,
        event: &WakeEvent,
        timer: &DeadlineTimer,
        mode: Mode,
    ) -> WaitOutcome {
        let changed = || {
            let value = word.load(Ordering::Acquire);
            (value != current).then_some(value)
        };
        if let Some(value) = changed() {
            return WaitOutcome::Changed(value);
        }
        let tail = match mode {
            Mode::Exact => {
                let start = Instant::now();
                let until = deadline.map_or(start + SPIN, |d| d.min(start + SPIN));
                if let Some(value) = spin_until(changed, until) {
                    return WaitOutcome::Changed(value);
                }
                TAIL
            }
            Mode::Relaxed => Duration::ZERO,
        };
        loop {
            let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if let (Some(deadline), Some(remaining)) = (deadline, remaining)
                && remaining <= tail
            {
                // Also `remaining == 0`: then there is nothing to spin, only the last look.
                return match spin_until(changed, deadline).or_else(changed) {
                    Some(value) => WaitOutcome::Changed(value),
                    None => WaitOutcome::TimedOut,
                };
            }
            let handles = [event.0.as_raw_handle(), timer.0.as_raw_handle()];
            let woke = match remaining {
                Some(remaining) if timer.arm(remaining - tail) => {
                    // SAFETY: two valid handles that outlive the call.
                    let woke = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                    if woke == WAIT_OBJECT_0 {
                        timer.cancel();
                    }
                    woke
                }
                // The timer could not be armed (a broken handle): give up below rather than block
                // past the deadline.
                Some(_) => u32::MAX,
                // SAFETY: a valid handle that outlives the call.
                None => unsafe { WaitForSingleObject(handles[0], INFINITE) },
            };
            if let Some(value) = changed() {
                return WaitOutcome::Changed(value);
            }
            match woke {
                // The event (possibly stale) or the timer: look at the word and the clock again.
                WAIT_OBJECT_0 | TIMER_FIRED => {}
                // WAIT_FAILED (a handle that is no event): do not spin.
                _ if deadline.is_some() => return WaitOutcome::TimedOut,
                _ => std::thread::yield_now(),
            }
        }
    }

    /// Spins until `changed` reports a value or `until` passes.
    fn spin_until(changed: impl Fn() -> Option<u32>, until: Instant) -> Option<u32> {
        loop {
            if let Some(value) = changed() {
                return Some(value);
            }
            if Instant::now() >= until {
                return None;
            }
            core::hint::spin_loop();
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("komponist-ipc: no wait/wake backend (signal.rs) for this OS");

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn returns_at_once_if_the_word_differs() {
        let word = AtomicU32::new(5);
        assert_eq!(wait_while(&word, 4, None), WaitOutcome::Changed(5));
        assert_eq!(
            wait_while(&word, 4, Some(Instant::now() - Duration::from_millis(1))),
            WaitOutcome::Changed(5)
        );
    }

    #[test]
    fn past_deadline_times_out_at_once() {
        let word = AtomicU32::new(1);
        let t0 = Instant::now();
        assert_eq!(wait_while(&word, 1, Some(t0)), WaitOutcome::TimedOut);
        assert!(t0.elapsed() < Duration::from_millis(5));
    }

    #[test]
    fn times_out_at_the_deadline() {
        let word = AtomicU32::new(1);
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_millis(20);
        assert_eq!(wait_while(&word, 1, Some(deadline)), WaitOutcome::TimedOut);
        let now = Instant::now();
        assert!(now >= deadline, "woke {:?} early", deadline - now);
        assert!(now - deadline < Duration::from_millis(50), "woke too late");
    }

    #[test]
    fn wake_from_another_thread() {
        let word = Arc::new(AtomicU32::new(0));
        let other = Arc::clone(&word);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            other.store(7, Ordering::Release);
            wake(&other)
        });
        let outcome = wait_while(&word, 0, Some(Instant::now() + Duration::from_secs(5)));
        assert_eq!(outcome, WaitOutcome::Changed(7));
        assert!(waker.join().unwrap() <= 1);
    }

    #[test]
    fn wait_without_deadline_returns_on_change() {
        let word = Arc::new(AtomicU32::new(3));
        let other = Arc::clone(&word);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            other.store(4, Ordering::Release);
            wake(&other);
        });
        assert_eq!(wait_while(&word, 3, None), WaitOutcome::Changed(4));
        waker.join().unwrap();
    }

    #[test]
    fn wake_without_waiters_wakes_none() {
        let word = AtomicU32::new(0);
        assert_eq!(wake(&word), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn monotonic_deadline_matches_instant() {
        use rustix::time::{ClockId, clock_gettime};
        let now = clock_gettime(ClockId::Monotonic);
        let ts = sys::absolute_deadline(Instant::now() + Duration::from_millis(1500));
        let diff = (ts.tv_sec - now.tv_sec) as f64 + (ts.tv_nsec - now.tv_nsec) as f64 * 1e-9;
        assert!((1.499..1.6).contains(&diff), "{diff}");
        assert!((0..1_000_000_000).contains(&ts.tv_nsec));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn mach_deadline_matches_instant() {
        use komponist_rt::mach_time;
        let second = mach_time::ticks(Duration::from_secs(1)) as f64;
        let now = mach_time::now();
        let deadline = sys::absolute_deadline(Instant::now() + Duration::from_millis(1500));
        let diff = (deadline - now) as f64 / second;
        assert!((1.499..1.6).contains(&diff), "{diff}");
    }

    /// The kernel's timer leeway is compensated (spike #43, Q4; #101): on a time-constraint thread
    /// a 1 ms wait that times out ends at most 50 µs later than the machine's plainest timed wait
    /// (`mach_wait_until`: a few µs on Apple silicon; a bare shared-scope kernel wait: 125–260 µs
    /// more on macOS 27, 425 µs more on the macOS 15 CI runner). Medians of 40 waits; prints the
    /// numbers for the CI log (`--nocapture`); skipped if the kernel refuses the policy or the
    /// machine's timers are too coarse for the check (the GitHub macOS runner's VM once: 192 µs,
    /// #94).
    #[test]
    #[cfg(target_os = "macos")]
    fn a_timed_out_wait_ends_close_to_its_deadline() {
        use komponist_rt::mach_time;
        use komponist_rt::os_sync::{self, Scope};
        let overshoot = std::thread::spawn(|| {
            let period = Duration::from_millis(2);
            if let Err(e) = komponist_rt::set_current_thread_realtime(period, 80) {
                eprintln!("skipped: {e}");
                return None;
            }
            let word = AtomicU32::new(0);
            mach_time::against_baseline(|| {
                let bare = os_sync::kernel_overshoot(Scope::Shared);
                let compensated = mach_time::median_overshoot(|deadline| {
                    assert_eq!(wait_while(&word, 0, Some(deadline)), WaitOutcome::TimedOut);
                });
                eprintln!(
                    "wait_while: bare kernel wait {bare:?}, compensated {compensated:?}, learned \
                     leeway {:?}",
                    os_sync::learned_leeway(Scope::Shared)
                );
                compensated
            })
        })
        .join()
        .unwrap();
        if let Some((baseline, median)) = overshoot {
            assert!(
                median < baseline + Duration::from_micros(50),
                "median overshoot {median:?}, the machine's timers {baseline:?}"
            );
        }
    }

    /// Windows: lateness of timed-out event waits, sorted (µs), `n` waits per deadline.
    #[cfg(windows)]
    fn event_wait_lateness(mode: Mode, n: usize) -> Vec<f64> {
        let (event, timer) = (WakeEvent::new().unwrap(), DeadlineTimer::new().unwrap());
        let word = AtomicU32::new(0);
        let mut late = Vec::new();
        for ms in [1, 2, 3] {
            for _ in 0..n {
                let deadline = Instant::now() + Duration::from_millis(ms);
                let outcome = wait_while_event(&word, 0, Some(deadline), &event, &timer, mode);
                let now = Instant::now();
                assert_eq!(outcome, WaitOutcome::TimedOut);
                assert!(now >= deadline, "{mode:?} woke {:?} early", deadline - now);
                late.push((now - deadline).as_secs_f64() * 1e6);
            }
        }
        late.sort_by(f64::total_cmp);
        late
    }

    #[cfg(windows)]
    fn percentile(sorted: &[f64], p: f64) -> f64 {
        sorted[((sorted.len() - 1) as f64 * p).round() as usize]
    }

    /// Windows, `Mode::Exact` (module docs): the timer is armed `TAIL` = 600 µs early and the rest
    /// is spun, so a timed-out wait ends within the spin's clock-read granularity of its deadline,
    /// unless the timer fired more than 600 µs late (under MMCSS never in spike S1, max +562 µs;
    /// without MMCSS, as here, 1–2 % of the timer waits up to a few ms). Bounds: median ≤ 50 µs
    /// (measured: about 0.1 µs; the margin is for a loaded build machine), p90 ≤ 600 µs.
    #[test]
    #[cfg(windows)]
    fn exact_event_waits_end_at_their_deadline() {
        let late = event_wait_lateness(Mode::Exact, 100);
        let (p50, p90, max) = (
            percentile(&late, 0.5),
            percentile(&late, 0.9),
            late[late.len() - 1],
        );
        eprintln!("Exact, timed out at 1/2/3 ms, µs late: p50 {p50:.1} p90 {p90:.1} max {max:.1}");
        assert!(p50 <= 50.0, "p50 {p50} µs");
        assert!(p90 <= 600.0, "p90 {p90} µs");
    }

    /// Windows, `Mode::Relaxed`: never early, with lateness comparable to this machine's native
    /// high-resolution timer. A shared runner can add a whole scheduling quantum even to the
    /// median; compare against the interleaved native timer's p90 plus 50 µs of wrapper overhead.
    #[test]
    #[cfg(windows)]
    fn relaxed_event_waits_end_after_their_deadline() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE,
            SetWaitableTimerEx, TIMER_ALL_ACCESS, WaitForSingleObject,
        };

        // Build the reference independently, so a regression in DeadlineTimer's creation or
        // arming cannot make both the implementation and its baseline equally slow.
        // SAFETY: unnamed, non-inheritable timer with no security attributes.
        let handle = unsafe {
            CreateWaitableTimerExW(
                core::ptr::null(),
                core::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        assert!(!handle.is_null(), "{}", std::io::Error::last_os_error());
        // SAFETY: a fresh valid timer handle, transferred to its sole owner.
        let reference = unsafe { OwnedHandle::from_raw_handle(handle) };
        let (event, timer) = (WakeEvent::new().unwrap(), DeadlineTimer::new().unwrap());
        let word = AtomicU32::new(0);
        let (mut native, mut late) = (Vec::new(), Vec::new());
        for ms in [1_u32, 2, 3] {
            for round in 0..50 {
                // Alternate order to avoid giving either wait a consistent timer-phase bias.
                for baseline in [round % 2 == 0, round % 2 != 0] {
                    let deadline = Instant::now() + Duration::from_millis(u64::from(ms));
                    if baseline {
                        let due = -i64::from(ms) * 10_000; // relative time, in 100 ns units
                        // SAFETY: the reference timer is valid for both calls, with no callback
                        // or coalescing delay. This thread is its only user.
                        unsafe {
                            assert_ne!(
                                SetWaitableTimerEx(
                                    reference.as_raw_handle(),
                                    &due,
                                    0,
                                    None,
                                    core::ptr::null(),
                                    core::ptr::null(),
                                    0,
                                ),
                                0
                            );
                            assert_eq!(
                                WaitForSingleObject(reference.as_raw_handle(), INFINITE),
                                WAIT_OBJECT_0
                            );
                        }
                    } else {
                        assert_eq!(
                            wait_while_event(
                                &word,
                                0,
                                Some(deadline),
                                &event,
                                &timer,
                                Mode::Relaxed,
                            ),
                            WaitOutcome::TimedOut
                        );
                    }
                    let now = Instant::now();
                    assert!(now >= deadline, "baseline={baseline} woke early");
                    let samples = if baseline { &mut native } else { &mut late };
                    samples.push((now - deadline).as_secs_f64() * 1e6);
                }
            }
        }
        native.sort_by(f64::total_cmp);
        late.sort_by(f64::total_cmp);
        let (p50, max) = (percentile(&late, 0.5), late[late.len() - 1]);
        let native_p90 = percentile(&native, 0.9);
        eprintln!(
            "Relaxed, timed out at 1/2/3 ms, µs late: p50 {p50:.1} max {max:.1}; \
             native timer p50 {:.1} p90 {native_p90:.1}",
            percentile(&native, 0.5)
        );
        assert!(
            p50 <= native_p90 + 50.0,
            "p50 {p50} µs, native timer p90 {native_p90} µs"
        );
    }

    /// Windows: an event wake from another thread, with and without a deadline, in both modes;
    /// a stale event (signalled while nobody waited) is one more loop, not a wrong answer.
    #[test]
    #[cfg(windows)]
    fn event_wakes_from_another_thread() {
        let event = Arc::new(WakeEvent::new().unwrap());
        let timer = DeadlineTimer::new().unwrap();
        let word = Arc::new(AtomicU32::new(0));
        // Stale signal from a wake nobody waited for.
        event.wake();
        for (round, (mode, deadline)) in [
            (Mode::Exact, Some(Duration::from_secs(5))),
            (Mode::Relaxed, Some(Duration::from_secs(5))),
            (Mode::Exact, None),
            (Mode::Relaxed, None),
        ]
        .into_iter()
        .enumerate()
        {
            let round = round as u32;
            let (w, e) = (Arc::clone(&word), Arc::clone(&event));
            let waker = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(5));
                w.store(round + 1, Ordering::Release);
                e.wake();
            });
            let deadline = deadline.map(|d| Instant::now() + d);
            let outcome = wait_while_event(&word, round, deadline, &event, &timer, mode);
            assert_eq!(outcome, WaitOutcome::Changed(round + 1), "{mode:?}");
            waker.join().unwrap();
        }
    }

    /// Windows: the event wait and wake allocate nothing (timed out, changed, woken).
    #[test]
    #[cfg(windows)]
    fn event_waits_and_wakes_allocate_nothing() {
        let event = Arc::new(WakeEvent::new().unwrap());
        let timer = DeadlineTimer::new().unwrap();
        let word = Arc::new(AtomicU32::new(0));
        let (w, e) = (Arc::clone(&word), Arc::clone(&event));
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            assert_no_alloc::assert_no_alloc(|| {
                w.store(1, Ordering::Release);
                e.wake();
            });
        });
        let outcome = assert_no_alloc::assert_no_alloc(|| {
            let differs = wait_while_event(
                &AtomicU32::new(3),
                7,
                Some(Instant::now()),
                &event,
                &timer,
                Mode::Exact,
            );
            let soon = || Some(Instant::now() + Duration::from_millis(1));
            let exact =
                wait_while_event(&AtomicU32::new(2), 2, soon(), &event, &timer, Mode::Exact);
            let relaxed =
                wait_while_event(&AtomicU32::new(2), 2, soon(), &event, &timer, Mode::Relaxed);
            let later = Some(Instant::now() + Duration::from_secs(5));
            let woken = wait_while_event(&word, 0, later, &event, &timer, Mode::Exact);
            (differs, exact, relaxed, woken)
        });
        assert_eq!(
            outcome,
            (
                WaitOutcome::Changed(3),
                WaitOutcome::TimedOut,
                WaitOutcome::TimedOut,
                WaitOutcome::Changed(1)
            )
        );
        waker.join().unwrap();
    }

    #[test]
    fn waits_and_wakes_allocate_nothing() {
        let word = Arc::new(AtomicU32::new(0));
        let other = Arc::clone(&word);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            other.store(1, Ordering::Release);
            wake(&other);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let outcome = assert_no_alloc::assert_no_alloc(|| {
            let differs = wait_while(&AtomicU32::new(3), 7, Some(Instant::now()));
            let soon = Instant::now() + Duration::from_millis(1);
            let late = wait_while(&AtomicU32::new(2), 2, Some(soon));
            (differs, late, wait_while(&word, 0, Some(deadline)))
        });
        assert_eq!(
            outcome,
            (
                WaitOutcome::Changed(3),
                WaitOutcome::TimedOut,
                WaitOutcome::Changed(1)
            )
        );
        waker.join().unwrap();
    }
}
