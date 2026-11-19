# komponist-rt

Shares OS realtime thread setup across the audio backend, engine workers and plugin bridges.

## Structure

- [src/policy.rs](src/policy.rs) — Applies the current platform's realtime scheduling policy.
- [src/workgroup.rs](src/workgroup.rs) — Wraps macOS audio workgroups, join tokens and standalone
  work intervals.
- [src/topology.rs](src/topology.rs) — Reports performance-core information on Apple silicon.
- [src/mach_time.rs](src/mach_time.rs) — Handles conversions for the macOS monotonic clock.
- [src/os_sync.rs](src/os_sync.rs) — Implements macOS address waits and wakes with deadline
  handling.

## How it works

Workers and bridges request SCHED_FIFO on Linux, Mach time constraints on macOS or MMCSS on
Windows. macOS workers can join the device's audio workgroup, while bridge threads can drive their
own work intervals. The macOS wait helper accounts for kernel timer leeway. The caller chooses when
to set up a thread and how to report failure.
