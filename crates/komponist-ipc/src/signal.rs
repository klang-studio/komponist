//! Wait and wake on the shared 32-bit words of the block handshake (ARCHITECTURE §8.1).
//!
//! Both words of the handshake (`request_seq`, `done_seq`) live in the shared memory of one
//! bridge, so the waits must find waiters in other processes:
//!
//! - Linux and macOS: `komponist_rt::os_sync::wait_while` / `wake` with `Scope::Shared` (#243): a
//!   shared futex with an absolute `CLOCK_MONOTONIC` deadline on Linux; on macOS
//!   `os_sync_wait_on_address_with_deadline` with the `SHARED` flags, armed short of the deadline
//!   against the kernel's timer leeway (12.5 % of the wait on macOS 27, about 0.7 ms for any wait on
//!   the macOS 15 CI runner, #101), the last stretch slept in `mach_wait_until` slices of at most
//!   25 µs. RT use: that wait is the only blocking call the engine's audio thread may make, always
//!   with a deadline; the wake never blocks. Neither allocates or takes a lock.
//! - Windows: there is no cross-process address wait (`WaitOnAddress` finds waiters in its own
//!   process only), so the handshake uses the event wait below.
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

pub use komponist_rt::os_sync::WaitOutcome;

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

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Windows: lateness of timed-out event waits, sorted (µs), `n` waits per deadline.
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

    fn percentile(sorted: &[f64], p: f64) -> f64 {
        sorted[((sorted.len() - 1) as f64 * p).round() as usize]
    }

    /// Windows, `Mode::Exact` (module docs): the timer is armed `TAIL` = 600 µs early and the rest
    /// is spun, so a timed-out wait ends within the spin's clock-read granularity of its deadline,
    /// unless the timer fired more than 600 µs late (under MMCSS never in spike S1, max +562 µs;
    /// without MMCSS, as here, 1–2 % of the timer waits up to a few ms). Bounds: median ≤ 50 µs
    /// (measured: about 0.1 µs; the margin is for a loaded build machine), p90 ≤ 600 µs.
    #[test]
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
}
