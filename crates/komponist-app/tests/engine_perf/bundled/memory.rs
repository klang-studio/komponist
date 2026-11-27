//! Resident memory snapshots for the benchmark, outside the recorded callback window.

use std::collections::BTreeMap;

pub(super) fn report(label: &str) {
    eprintln!(
        "{label}: resident memory after timing: {}",
        mib(runner_resident())
    );
}

/// Resident set of this process (working set on Windows), or `None` where it cannot be read.
pub(super) fn runner_resident() -> Option<u64> {
    let runner = std::process::id();
    resident_bytes(&[runner]).get(&runner).copied()
}

pub(super) fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "unavailable".to_owned(),
        |bytes| format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    )
}

#[cfg(target_os = "linux")]
fn resident_bytes(pids: &[u32]) -> BTreeMap<u32, u64> {
    pids.iter()
        .filter_map(|&pid| {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
            let rss_kib = status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))?;
            Some((
                pid,
                rss_kib.split_whitespace().next()?.parse::<u64>().ok()? * 1024,
            ))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn resident_bytes(pids: &[u32]) -> BTreeMap<u32, u64> {
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "pid=", "-o", "rss=", "-p", &list])
        .output();
    parse_resident_rows(output, 1024)
}

#[cfg(windows)]
fn resident_bytes(pids: &[u32]) -> BTreeMap<u32, u64> {
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "Get-Process -Id {list} -ErrorAction SilentlyContinue | ForEach-Object {{\
         [Console]::WriteLine(\"$($_.Id) $($_.WorkingSet64)\") }}"
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output();
    parse_resident_rows(output, 1)
}

#[cfg(any(target_os = "macos", windows))]
fn parse_resident_rows(
    output: std::io::Result<std::process::Output>,
    unit_bytes: u64,
) -> BTreeMap<u32, u64> {
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|row| {
            let mut values = row.split_whitespace();
            let pid = values.next()?.parse::<u32>().ok()?;
            let resident = values.next()?.parse::<u64>().ok()?;
            Some((pid, resident * unit_bytes))
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn resident_bytes(_: &[u32]) -> BTreeMap<u32, u64> {
    BTreeMap::new()
}
