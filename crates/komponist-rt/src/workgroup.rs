//! Audio workgroups (`os_workgroup`, macOS 11+).
//!
//! On Apple silicon the scheduler and the CPU frequency controller treat a thread as part of the
//! audio work only while it is in an audio workgroup: without one, a realtime thread at light load
//! runs on efficiency cores or down-clocked performance cores and misses deadlines (spike #43).
//! Two kinds, after the cases of `<AudioToolbox/AudioWorkInterval.h>`:
//!
//! - [`Workgroup`]: someone else's workgroup, in practice the device's IO workgroup
//!   (`kAudioDevicePropertyIOThreadOSWorkgroup`, looked up by komponist-audio). Threads that work
//!   at the device's cadence (the engine's workers) [`Workgroup::join`] it and leave before they
//!   end. The HAL's IO thread (the audio callback) is a member already and must not join (a second
//!   join returns `EALREADY`).
//! - [`WorkInterval`]: a workgroup this process creates (`AudioWorkIntervalCreate`) for a thread
//!   at its own cadence (a bridge RT thread). Its creator joins it and marks every work cycle with
//!   [`WorkInterval::start`] / [`WorkInterval::finish`], passing the cycle's deadline, so the
//!   frequency controller knows the real deadline. Only the creating side may start an interval
//!   (libdispatch traps the process otherwise), which is why a [`WorkInterval`] cannot be cloned;
//!   [`WorkInterval::workgroup`] hands out a join-only [`Workgroup`] of it.
//!
//! Both types exist on every OS so callers need no `cfg`; outside macOS they cannot be created
//! ([`WorkInterval::new`] returns `None`, and no [`Workgroup`] exists).
//!
//! RT: join, leave, start and finish are real-time safe (Apple's headers; spike #43 counted no
//! allocation). Creating, cloning (`os_retain`) and dropping (`os_release`) are non-RT.

#[cfg(target_os = "macos")]
pub use apple::{JoinToken, WorkInterval, Workgroup};
#[cfg(not(target_os = "macos"))]
pub use other::{JoinToken, WorkInterval, Workgroup};

#[cfg(target_os = "macos")]
mod apple {
    use core::ffi::{CStr, c_char, c_int, c_void};
    use core::marker::PhantomData;
    use core::ptr::NonNull;

    use crate::mach_time;

    /// `os_workgroup_join_token_s` (`<os/workgroup_base.h>`, 64-bit): a signature and 36 opaque
    /// bytes. libdispatch keeps the joined workgroup in thread-local storage, not a pointer to the
    /// token, so the token may move.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RawToken {
        sig: u32,
        opaque: [u8; 36],
    }

    /// `OS_CLOCK_MACH_ABSOLUTE_TIME` (`<os/clock.h>`).
    const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;

    unsafe extern "C" {
        fn os_workgroup_join(wg: *mut c_void, token: *mut RawToken) -> c_int;
        fn os_workgroup_leave(wg: *mut c_void, token: *mut RawToken);
        fn os_workgroup_interval_start(
            wg: *mut c_void,
            start: u64,
            deadline: u64,
            data: *mut c_void,
        ) -> c_int;
        fn os_workgroup_interval_finish(wg: *mut c_void, data: *mut c_void) -> c_int;
        fn os_retain(object: *mut c_void) -> *mut c_void;
        fn os_release(object: *mut c_void);
    }

    #[link(name = "AudioToolbox", kind = "framework")]
    unsafe extern "C" {
        /// Returns a retained `os_workgroup_interval_t`, or null.
        fn AudioWorkIntervalCreate(
            name: *const c_char,
            clock: u32,
            attr: *mut c_void,
        ) -> *mut c_void;
    }

    /// Proof of a join, needed to leave. Bound to the joining thread (not `Send`).
    pub struct JoinToken {
        raw: RawToken,
        _thread: PhantomData<*const ()>,
    }

    impl core::fmt::Debug for JoinToken {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("JoinToken").finish_non_exhaustive()
        }
    }

    /// RT. Joins the calling thread to `wg`.
    fn join(wg: NonNull<c_void>) -> Result<JoinToken, i32> {
        let mut raw = RawToken {
            sig: 0,
            opaque: [0; 36],
        };
        // SAFETY: `wg` is a live workgroup (its owner holds a reference); `raw` is a token of the
        // size the header declares, written by the call.
        let rc = unsafe { os_workgroup_join(wg.as_ptr(), &mut raw) };
        if rc == 0 {
            Ok(JoinToken {
                raw,
                _thread: PhantomData,
            })
        } else {
            Err(rc)
        }
    }

    /// RT. Leaves `wg` with the token of the calling thread's join.
    fn leave(wg: NonNull<c_void>, mut token: JoinToken) {
        // SAFETY: `wg` is live; `token` came from joining it on this thread (`JoinToken` is not
        // `Send`), and is used once (taken by value).
        unsafe { os_workgroup_leave(wg.as_ptr(), &mut token.raw) };
    }

    /// A retained reference to a workgroup someone else drives (the device's IO workgroup):
    /// threads join and leave it. `Clone` retains, `Drop` releases (both non-RT). Equal when it is
    /// the same object.
    #[derive(PartialEq, Eq)]
    pub struct Workgroup(NonNull<c_void>);

    // SAFETY: an `os_workgroup_t` is a reference-counted, thread-safe OS object; join and leave may
    // be called from any thread.
    unsafe impl Send for Workgroup {}
    // SAFETY: as above; `&Workgroup` only allows join and leave.
    unsafe impl Sync for Workgroup {}

    impl Workgroup {
        /// Takes over a +1 reference to an `os_workgroup_t` (e.g. read from
        /// `kAudioDevicePropertyIOThreadOSWorkgroup`, which returns it retained). `None` for null.
        ///
        /// # Safety
        ///
        /// `raw` is null or an `os_workgroup_t` whose reference the caller owns and gives up.
        pub unsafe fn from_retained(raw: *mut c_void) -> Option<Workgroup> {
            NonNull::new(raw).map(Workgroup)
        }

        /// RT. Joins the calling thread. Errors: `EALREADY` (37) when the thread is in a
        /// workgroup already (the audio callback thread is in the device's), `EINVAL` when the
        /// workgroup was cancelled.
        pub fn join(&self) -> Result<JoinToken, i32> {
            join(self.0)
        }

        /// RT. Leaves the workgroup; `token` is the calling thread's join of this workgroup.
        pub fn leave(&self, token: JoinToken) {
            leave(self.0, token);
        }
    }

    impl Clone for Workgroup {
        fn clone(&self) -> Workgroup {
            // SAFETY: `self.0` is live; `os_retain` returns the same object with one more
            // reference, owned by the clone.
            unsafe { os_retain(self.0.as_ptr()) };
            Workgroup(self.0)
        }
    }

    impl Drop for Workgroup {
        fn drop(&mut self) {
            // SAFETY: gives up the one reference this value owns.
            unsafe { os_release(self.0.as_ptr()) };
        }
    }

    impl core::fmt::Debug for Workgroup {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_tuple("Workgroup").field(&self.0).finish()
        }
    }

    /// An audio work interval this process created (`AudioWorkIntervalCreate`, clock
    /// `mach_absolute_time`). The thread that drives it joins it and calls [`WorkInterval::start`]
    /// and [`WorkInterval::finish`] around every cycle. Not `Clone`: only the creating side may
    /// start intervals. `Drop` releases it (non-RT).
    pub struct WorkInterval(NonNull<c_void>);

    // SAFETY: as for `Workgroup`; moving the handle to the thread that drives it is fine. Not
    // `Sync`: one thread drives the intervals.
    unsafe impl Send for WorkInterval {}

    impl WorkInterval {
        /// Non-RT (allocates). A new work interval named `name`, `None` if the system refuses.
        pub fn new(name: &CStr) -> Option<WorkInterval> {
            // SAFETY: `name` is a valid C string for the call; a null attribute means the defaults.
            let raw = unsafe {
                AudioWorkIntervalCreate(
                    name.as_ptr(),
                    OS_CLOCK_MACH_ABSOLUTE_TIME,
                    core::ptr::null_mut(),
                )
            };
            NonNull::new(raw).map(WorkInterval)
        }

        /// Non-RT. A join-only reference to this interval's workgroup (for other threads that
        /// work to its deadlines, or tests that need a workgroup to join).
        pub fn workgroup(&self) -> Workgroup {
            // SAFETY: `self.0` is live; the retained reference belongs to the new `Workgroup`.
            unsafe { os_retain(self.0.as_ptr()) };
            Workgroup(self.0)
        }

        /// RT. Joins the calling thread (see [`Workgroup::join`]).
        pub fn join(&self) -> Result<JoinToken, i32> {
            join(self.0)
        }

        /// RT. Leaves (see [`Workgroup::leave`]).
        pub fn leave(&self, token: JoinToken) {
            leave(self.0, token);
        }

        /// RT. The members began a cycle at `start` that must be done by `deadline` (Mach ticks,
        /// `deadline > start`). The calling thread must be a member; every start needs a
        /// [`WorkInterval::finish`] before the next.
        pub fn start(&self, start: u64, deadline: u64) -> Result<(), i32> {
            // SAFETY: `self.0` is a live interval this process created; the data argument is
            // unused and must be null.
            let rc = unsafe {
                os_workgroup_interval_start(self.0.as_ptr(), start, deadline, core::ptr::null_mut())
            };
            if rc == 0 { Ok(()) } else { Err(rc) }
        }

        /// RT. [`WorkInterval::start`] from now until `deadline` (Mach ticks); a deadline that
        /// has passed becomes one tick from now (spike #43, Q6).
        pub fn start_now(&self, deadline: u64) -> Result<(), i32> {
            let now = mach_time::now();
            self.start(now, deadline.max(now + 1))
        }

        /// RT. The members finished the current cycle.
        pub fn finish(&self) -> Result<(), i32> {
            // SAFETY: as in `start`.
            let rc =
                unsafe { os_workgroup_interval_finish(self.0.as_ptr(), core::ptr::null_mut()) };
            if rc == 0 { Ok(()) } else { Err(rc) }
        }
    }

    impl Drop for WorkInterval {
        fn drop(&mut self) {
            // SAFETY: gives up the reference `AudioWorkIntervalCreate` returned.
            unsafe { os_release(self.0.as_ptr()) };
        }
    }

    impl core::fmt::Debug for WorkInterval {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_tuple("WorkInterval").field(&self.0).finish()
        }
    }
}

/// Outside macOS: the same API on uninhabited types.
#[cfg(not(target_os = "macos"))]
mod other {
    use core::ffi::CStr;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Never {}

    /// A device's audio workgroup (macOS only; this type has no values elsewhere).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Workgroup(Never);

    /// Proof of a join (macOS only).
    #[derive(Debug)]
    pub struct JoinToken(Never);

    /// A work interval of this process (macOS only).
    #[derive(Debug)]
    pub struct WorkInterval(Never);

    impl Workgroup {
        /// See the macOS implementation.
        pub fn join(&self) -> Result<JoinToken, i32> {
            match self.0 {}
        }

        /// See the macOS implementation.
        pub fn leave(&self, token: JoinToken) {
            match token.0 {}
        }
    }

    impl WorkInterval {
        /// `None`: there are no work intervals outside macOS.
        pub fn new(_name: &CStr) -> Option<WorkInterval> {
            None
        }

        /// See the macOS implementation.
        pub fn workgroup(&self) -> Workgroup {
            match self.0 {}
        }

        /// See the macOS implementation.
        pub fn join(&self) -> Result<JoinToken, i32> {
            match self.0 {}
        }

        /// See the macOS implementation.
        pub fn leave(&self, token: JoinToken) {
            match token.0 {}
        }

        /// See the macOS implementation.
        pub fn start(&self, _start: u64, _deadline: u64) -> Result<(), i32> {
            match self.0 {}
        }

        /// See the macOS implementation.
        pub fn start_now(&self, _deadline: u64) -> Result<(), i32> {
            match self.0 {}
        }

        /// See the macOS implementation.
        pub fn finish(&self) -> Result<(), i32> {
            match self.0 {}
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::mach_time;
    use std::time::Duration;

    /// `EALREADY` on macOS.
    const EALREADY: i32 = 37;

    #[test]
    fn a_thread_joins_and_leaves_an_interval_and_drives_it() {
        let interval = WorkInterval::new(c"komponist-rt-test").expect("work interval");
        std::thread::spawn(move || {
            let token = interval.join().expect("join");
            // A second join, also of another workgroup, is refused while the thread is a member.
            let again = interval.workgroup();
            assert_eq!(again.join().map(drop), Err(EALREADY));
            let period = mach_time::ticks(Duration::from_millis(2));
            assert_no_alloc::assert_no_alloc(|| {
                for _ in 0..3 {
                    let now = mach_time::now();
                    interval.start(now, now + period).expect("start");
                    interval.finish().expect("finish");
                }
                // A passed deadline becomes one tick from now.
                interval.start_now(0).expect("start_now");
                interval.finish().expect("finish");
            });
            // A finish without a start is an error, not a trap.
            assert!(interval.finish().is_err());
            interval.leave(token);
            // After leaving, the thread may join again.
            let token = again.join().expect("join after leave");
            again.leave(token);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn workgroups_of_one_interval_are_equal_and_outlive_it() {
        let interval = WorkInterval::new(c"komponist-rt-test").expect("work interval");
        let a = interval.workgroup();
        let b = a.clone();
        assert_eq!(a, b);
        drop(interval);
        drop(a);
        // `b` holds its own reference: joining still works on another thread.
        std::thread::spawn(move || {
            let token = b.join().expect("join");
            b.leave(token);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_null_workgroup_is_none() {
        // SAFETY: null is allowed.
        assert!(unsafe { Workgroup::from_retained(core::ptr::null_mut()) }.is_none());
    }
}
