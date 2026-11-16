//! The machine's CPU layout, as far as the worker count needs it.

/// Non-RT. The number of performance cores on Apple silicon (`hw.perflevel0.physicalcpu`: 8 on an
/// M1 Pro, 4 on an M1), which have no SMT and are where realtime audio threads belong. `None`
/// elsewhere and on Intel Macs, which have no performance levels.
pub fn performance_cores() -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        let mut value: libc::c_int = 0;
        let mut size = core::mem::size_of::<libc::c_int>();
        // SAFETY: the name is a C string, `value` and `size` describe a `c_int` the call writes
        // (it fails with `ENOENT` for an unknown name and leaves them alone), no new value is set.
        let rc = unsafe {
            libc::sysctlbyname(
                c"hw.perflevel0.physicalcpu".as_ptr(),
                (&raw mut value).cast(),
                &mut size,
                core::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && size == core::mem::size_of::<libc::c_int>())
            .then(|| usize::try_from(value).ok().filter(|&n| n > 0))
            .flatten()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn performance_cores_fit_the_machine_when_reported() {
        if let Some(p) = super::performance_cores() {
            let cpus = std::thread::available_parallelism().unwrap().get();
            assert!((1..=cpus).contains(&p), "{p} of {cpus} CPUs");
        }
    }
}
