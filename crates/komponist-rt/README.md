# komponist-rt

Shares OS realtime thread setup across the audio backend, engine workers and plugin bridges.

## Structure

- [src/policy.rs](src/policy.rs) — Applies the current platform's realtime scheduling policy.
- [src/workgroup.rs](src/workgroup.rs) — Wraps macOS audio workgroups, join tokens and standalone
  work intervals.
- [src/topology.rs](src/topology.rs) — Reports performance-core information on Apple silicon.
- [src/mach_time.rs](src/mach_time.rs) — Handles conversions for the macOS monotonic clock.
- [src/os_sync.rs](src/os_sync.rs) — Waits on and wakes a 32-bit word with a deadline on every
  OS: a futex on Linux ([src/os_sync/linux.rs](src/os_sync/linux.rs)), `os_sync_wait_on_address`
  with timer-leeway compensation on macOS ([src/os_sync/mach.rs](src/os_sync/mach.rs)),
  `WaitOnAddress` on Windows ([src/os_sync/windows.rs](src/os_sync/windows.rs)).

## How it works

Workers and bridges request SCHED_FIFO on Linux, Mach time constraints on macOS or MMCSS on
Windows. macOS workers can join the device's audio workgroup, while bridge threads can drive their
own work intervals. The engine's workers and the plugin block handshake wait with `os_sync`; on
macOS it accounts for kernel timer leeway. The caller chooses when
to set up a thread and how to report failure.
