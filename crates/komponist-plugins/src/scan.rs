//! Background plugin scans (ARCHITECTURE §8.3).
//!
//! [`ScanJob::start`] runs on its own thread: it walks the plugin folders for bundles (recursively,
//! as the CLAP spec asks for Linux; a bundle is never entered, on macOS `.clap` and `.vst3` are
//! directories with `Contents/MacOS/<name>` and `Contents/Info.plist`), canonicalizes their paths (the cache key), takes unchanged
//! bundles from the [`ScanCache`] and scans the rest with `<launcher> --scan <bundle>` (`komponist --bridge`),
//! [`PluginManagerConfig::parallel_scans`](crate::PluginManagerConfig) processes at a time, each
//! killed after [`SCAN_TIMEOUT`](crate::catalog::SCAN_TIMEOUT) together with the processes its
//! plugin started (a process group, Windows: a job object; #218). Scan processes end with the app. The finished job writes the new cache file and hands a
//! [`ScanOutcome`] to the plugin manager, which polls for it.
//!
//! What gets cached: successful scans, bundles without plugins, crashes and load errors (a bundle
//! that fails the same way again is not rescanned until it changes). Not cached, so that the next
//! scan tries again: timeouts (the machine may just have been busy) and formats the bridge cannot
//! host yet (VST3 until WP-32).

use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::io::Read;
#[cfg(unix)]
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use komponist_ipc::{
    PluginDescriptor, PluginFormat, PluginKind, ProcessExit, ProcessGroup, ProcessWatcher,
};
#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::catalog::{CachedBundle, DirCount, PluginDirs, ScanCache, ScanReport, SkippedBundle};

/// How deep the folder walk descends below a plugin folder.
const MAX_DEPTH: usize = 8;

/// How often a waiting scan looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Windows: how often a waiting scan reads its output pipes (they cannot be waited on).
#[cfg(windows)]
const PIPE_POLL: Duration = Duration::from_millis(5);

/// Most bytes of scan output kept (a JSON array of descriptors is a few KiB).
const MAX_STDOUT: usize = 16 << 20;

/// Bytes of stderr kept for the error message (the tail).
const MAX_STDERR: usize = 8 << 10;

/// Exit code of `komponist-bridge --scan` for a bundle that could not be scanned.
const EXIT_FAILURE: i32 = 1;
/// Exit code for a format without hosting (VST3 until WP-32).
const EXIT_UNSUPPORTED: i32 = 3;

/// What a finished scan hands to the plugin manager.
#[derive(Debug)]
pub(crate) struct ScanOutcome {
    /// The new cache (also written to the cache file).
    pub cache: ScanCache,
    /// Why the cache file could not be written, if it could not.
    pub cache_error: Option<String>,
    /// The report for the UI.
    pub report: ScanReport,
}

/// A running background scan.
#[derive(Debug)]
pub(crate) struct ScanJob {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    result: mpsc::Receiver<ScanOutcome>,
}

/// What the scan job needs.
#[derive(Debug, Clone)]
pub(crate) struct ScanSetup {
    pub bridge_exe: PathBuf,
    pub bridge_args: Vec<OsString>,
    pub dirs: PluginDirs,
    pub cache: ScanCache,
    pub cache_file: PathBuf,
    /// A scan limited to the private bundle directory keeps cached external plugins.
    pub bundled_only: Option<PathBuf>,
    pub parallel: usize,
    pub timeout: Duration,
}

impl ScanJob {
    /// Starts the scan on a background thread.
    pub(crate) fn start(setup: ScanSetup) -> ScanJob {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, result) = mpsc::channel();
        let flag = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("kp-plugin-scan".into())
            .spawn(move || {
                if let Some(outcome) = run(&setup, &flag) {
                    let _ = tx.send(outcome);
                }
            })
            .expect("spawn the plugin scan thread");
        ScanJob {
            cancel,
            thread: Some(thread),
            result,
        }
    }

    /// The outcome once the scan finished (non-blocking).
    pub(crate) fn try_finish(&mut self) -> Option<ScanOutcome> {
        let outcome = self.result.try_recv().ok()?;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        Some(outcome)
    }
}

impl Drop for ScanJob {
    /// Cancels the scan: running scan processes are killed (within about [`CANCEL_POLL`]).
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One bundle found in a folder.
#[derive(Debug, Clone)]
struct Found {
    /// Index of the folder in `PluginDirs` order (CLAP folders, then VST3 folders).
    dir: usize,
    /// Canonical path.
    path: PathBuf,
    mtime_secs: u64,
}

/// Result of scanning one bundle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scanned {
    pub descriptors: Vec<PluginDescriptor>,
    pub error: Option<String>,
    /// Whether the result goes into the cache.
    pub cacheable: bool,
}

impl Scanned {
    fn failed(error: impl Into<String>, cacheable: bool) -> Scanned {
        Scanned {
            descriptors: Vec::new(),
            error: Some(error.into()),
            cacheable,
        }
    }
}

/// The scan thread; `None` when cancelled.
fn run(setup: &ScanSetup, cancel: &AtomicBool) -> Option<ScanOutcome> {
    let folders: Vec<(PluginFormat, &PathBuf)> = setup
        .dirs
        .clap
        .iter()
        .map(|d| (PluginFormat::Clap, d))
        .chain(setup.dirs.vst3.iter().map(|d| (PluginFormat::Vst3, d)))
        .collect();

    let mut found = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = HashSet::new();
    for (index, (format, dir)) in folders.iter().enumerate() {
        for raw in find_bundles(dir, *format) {
            let path = match bundle_key(&raw) {
                Ok(path) => path,
                Err(reason) => {
                    skipped.push(SkippedBundle { path: raw, reason });
                    continue;
                }
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            found.push(Found {
                dir: index,
                mtime_secs: bundle_mtime(&path),
                path,
            });
        }
    }

    // Cached results first; the rest goes to the workers.
    let mut results: Vec<Option<Scanned>> = found
        .iter()
        .map(|f| {
            setup
                .cache
                .lookup(&f.path, f.mtime_secs)
                .map(|cached| Scanned {
                    descriptors: cached.descriptors.clone(),
                    error: cached.error.clone(),
                    cacheable: true,
                })
        })
        .collect();
    let queue: VecDeque<usize> = (0..found.len()).filter(|&i| results[i].is_none()).collect();
    let workers = setup.parallel.max(1).min(queue.len());
    let queue = Mutex::new(queue);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (queue, found) = (&queue, &found);
            scope.spawn(move || {
                loop {
                    if cancel.load(Ordering::Acquire) {
                        return;
                    }
                    let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                    let Some(index) = next else {
                        return;
                    };
                    let bundle = &found[index].path;
                    let scanned = scan_bundle(
                        &setup.bridge_exe,
                        &setup.bridge_args,
                        bundle,
                        setup.timeout,
                        cancel,
                    );
                    if let Some(scanned) = scanned {
                        let _ = tx.send((index, scanned));
                    }
                }
            });
        }
    });
    drop(tx);
    for (index, scanned) in rx {
        results[index] = Some(scanned);
    }
    if cancel.load(Ordering::Acquire) {
        return None;
    }

    let mut cache = ScanCache::default();
    if let Some(root) = &setup.bundled_only {
        cache.bundles.extend(
            setup
                .cache
                .bundles
                .iter()
                .filter(|bundle| !bundle.path.starts_with(root))
                .cloned(),
        );
    }
    let mut per_dir: Vec<DirCount> = folders
        .iter()
        .map(|(format, dir)| DirCount {
            format: *format,
            path: (*dir).clone(),
            plugins: 0,
        })
        .collect();
    for (bundle, result) in found.iter().zip(results) {
        let Some(result) = result else {
            continue;
        };
        per_dir[bundle.dir].plugins += result
            .descriptors
            .iter()
            .filter(|d| d.kind != PluginKind::Other)
            .count();
        if let Some(reason) = &result.error {
            skipped.push(SkippedBundle {
                path: bundle.path.clone(),
                reason: reason.clone(),
            });
        }
        if result.cacheable {
            cache.bundles.push(CachedBundle {
                path: bundle.path.clone(),
                mtime_secs: bundle.mtime_secs,
                descriptors: result.descriptors,
                error: result.error,
            });
        }
    }
    let cache_error = cache
        .save(&setup.cache_file)
        .err()
        .map(|e| format!("{}: {e}", setup.cache_file.display()));
    let report = ScanReport {
        found: per_dir.iter().map(|d| d.plugins).sum(),
        per_dir,
        skipped,
        finished: SystemTime::now(),
    };
    Some(ScanOutcome {
        cache,
        cache_error,
        report,
    })
}

/// Whether `name` ends in `.clap` / `.vst3` (ASCII case-insensitive).
fn is_bundle_name(name: &std::ffi::OsStr, format: PluginFormat) -> bool {
    let ext: &[u8] = match format {
        PluginFormat::Clap => b".clap",
        PluginFormat::Vst3 => b".vst3",
    };
    let name = name.as_encoded_bytes();
    name.len() > ext.len() && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

/// Bundles below `dir` (recursively, not into bundles, not into hidden folders), sorted by path.
/// A missing folder has none.
pub(crate) fn find_bundles(dir: &Path, format: PluginFormat) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    walk(dir, format, 0, &mut visited, &mut out);
    out
}

fn walk(
    dir: &Path,
    format: PluginFormat,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    // Symlinked folders may form loops.
    let Ok(canonical) = std::fs::canonicalize(dir) else {
        return;
    };
    if depth > MAX_DEPTH || !visited.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        let Some(name) = path.file_name() else {
            continue;
        };
        if is_bundle_name(name, format) {
            out.push(path);
        } else if !name.as_encoded_bytes().starts_with(b".") && path.is_dir() {
            walk(&path, format, depth + 1, visited, out);
        }
    }
}

/// The cache key of a bundle: its canonical path, which must be valid UTF-8 (the scan output and
/// the control protocol carry paths as UTF-8 strings).
pub(crate) fn bundle_key(path: &Path) -> Result<PathBuf, String> {
    if path.to_str().is_none() {
        return Err("path is not valid UTF-8".into());
    }
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let canonical = without_verbatim(canonical);
    if canonical.to_str().is_none() {
        return Err(format!(
            "path {} is not valid UTF-8",
            canonical.to_string_lossy()
        ));
    }
    Ok(canonical)
}

/// Windows: `fs::canonicalize` gives verbatim paths (`\\?\C:\…`, `\\?\UNC\server\share\…`). The
/// cache key, the catalog and project files keep the usual form, which every later use (the scan,
/// the bridge's `LoadLibraryExW`) takes as well.
#[cfg(windows)]
pub(crate) fn without_verbatim(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// Modification time of a bundle in seconds. A plain file (a Linux `.clap`) has its own; for a
/// bundle directory (macOS `.clap` / `.vst3`, Linux `.vst3`) it is the newest of the bundle
/// folder, `Contents/`, and the entries of `Contents/` and of the folders in it
/// (`Contents/MacOS/<name>`, `Contents/x86_64-linux/<name>.so`, `Contents/Info.plist`,
/// `Contents/_CodeSignature/CodeResources`). Replacing the binary or the plist therefore
/// triggers a rescan; a directory's own mtime alone would not change. 0 if unknown.
pub(crate) fn bundle_mtime(path: &Path) -> u64 {
    let secs = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    };
    let mut newest = secs(path);
    if !path.is_dir() {
        return newest;
    }
    let contents = path.join("Contents");
    newest = newest.max(secs(&contents));
    let Ok(entries) = std::fs::read_dir(&contents) else {
        return newest;
    };
    for entry in entries.flatten() {
        let entry = entry.path();
        newest = newest.max(secs(&entry));
        if entry.is_dir()
            && let Ok(files) = std::fs::read_dir(&entry)
        {
            for file in files.flatten() {
                newest = newest.max(secs(&file.path()));
            }
        }
    }
    newest
}

/// Runs `komponist-bridge --scan bundle` with a timeout; `None` when cancelled.
pub(crate) fn scan_bundle(
    bridge_exe: &Path,
    bridge_args: &[OsString],
    bundle: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Option<Scanned> {
    let mut command = Command::new(bridge_exe);
    command
        .args(bridge_args)
        .arg("--scan")
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A group of its own, tied to the app (#218): a timeout or a cancel kills the helper processes
    // its plugin started along with it.
    let (mut child, group) = match ProcessGroup::spawn(&mut command) {
        Ok(spawned) => spawned,
        Err(e) => {
            return Some(Scanned::failed(
                format!("cannot start {}: {e}", bridge_exe.display()),
                false,
            ));
        }
    };
    let mut watcher = match ProcessWatcher::new(child.id()) {
        Ok(watcher) => watcher,
        Err(e) => {
            // No group kill: it needs a watcher that knows the leader is not reaped. A failed
            // watcher may have reaped the child already (macOS reaps an exited child before it
            // can fail), so signal the pid only while `try_wait` still sees it running: a pid we
            // have not reaped cannot be reused.
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Some(Scanned::failed(
                format!("cannot watch the scan: {e}"),
                false,
            ));
        }
    };
    let mut pipes = Pipes {
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        out: Vec::new(),
        err: Vec::new(),
    };
    // The watcher reaps the process.
    drop(child);

    let deadline = Instant::now() + timeout;
    let exit = loop {
        // The group goes first, while its leader is not reaped (`ProcessGroup::kill` checks).
        if cancel.load(Ordering::Acquire) {
            let _ = group.kill(&watcher);
            let _ = watcher.kill();
            let _ = watcher.wait_exit(Some(Duration::from_secs(1)));
            return None;
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = group.kill(&watcher);
            let _ = watcher.kill();
            let _ = watcher.wait_exit(Some(Duration::from_secs(1)));
            return Some(Scanned::failed(
                format!("timed out after {} s", timeout.as_secs_f32()),
                false,
            ));
        }
        pipes.wait(&watcher, (deadline - now).min(CANCEL_POLL));
        pipes.read_ready();
        match watcher.try_exit() {
            Ok(Some(exit)) => break exit,
            Ok(None) => {}
            Err(e) => return Some(Scanned::failed(format!("scan process: {e}"), false)),
        }
    };
    // A helper process the plugin started may hold the pipes open: take what is there.
    pipes.read_ready();
    Some(interpret(exit, &pipes.out, &pipes.err, bundle))
}

/// Turns the exit status and output of a scan of `bundle` into a result.
pub(crate) fn interpret(exit: ProcessExit, stdout: &[u8], stderr: &[u8], bundle: &Path) -> Scanned {
    match (exit.code, exit.signal) {
        (Some(0), _) => match serde_json::from_slice::<Vec<PluginDescriptor>>(stdout) {
            Ok(descriptors) if descriptors.is_empty() => {
                Scanned::failed("no plugins in the bundle", true)
            }
            Ok(descriptors) => Scanned {
                descriptors,
                error: None,
                cacheable: true,
            },
            Err(e) => Scanned::failed(format!("unreadable scan output: {e}"), false),
        },
        (Some(EXIT_UNSUPPORTED), _) => Scanned::failed(
            format!("format not supported yet: {}", last_line(stderr, bundle)),
            false,
        ),
        (Some(code), _) => {
            let line = last_line(stderr, bundle);
            let message = if line.is_empty() {
                format!("scan failed (exit code {code})")
            } else {
                line
            };
            Scanned::failed(message, code == EXIT_FAILURE)
        }
        (None, Some(signal)) => Scanned::failed(crash_reason(signal), true),
        (None, None) => Scanned::failed("scan ended without a status", false),
    }
}

/// How a crash reads in the report: the signal, or on Windows the exception (an NTSTATUS error
/// code, which `komponist-ipc` reports in place of the signal).
fn crash_reason(signal: i32) -> String {
    #[cfg(windows)]
    if signal as u32 & 0xC000_0000 == 0xC000_0000 {
        return format!("crashed (exception 0x{:08X})", signal as u32);
    }
    format!("crashed (signal {signal})")
}

/// The bridge's own reason from stderr: the last line it printed, else the last non-empty line;
/// without the bundle path in front (the skipped bundle carries it).
fn last_line(stderr: &[u8], bundle: &Path) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let line = lines
        .iter()
        .rev()
        .find(|l| l.starts_with("komponist-bridge:"))
        .or(lines.last())
        .map_or("", |l| l.trim_start_matches("komponist-bridge:").trim());
    let path = format!("{}:", bundle.display());
    let line = line.strip_prefix(path.as_str()).unwrap_or(line).trim();
    line.chars().take(300).collect()
}

/// The two output pipes of a scan process.
struct Pipes {
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    out: Vec<u8>,
    err: Vec<u8>,
}

impl Pipes {
    /// Waits up to `timeout` until a pipe has data or the process exited.
    #[cfg(unix)]
    fn wait(&self, watcher: &ProcessWatcher, timeout: Duration) {
        let mut fds = Vec::with_capacity(3);
        fds.push(PollFd::from_borrowed_fd(watcher.as_fd(), PollFlags::IN));
        if let Some(out) = &self.stdout {
            fds.push(PollFd::from_borrowed_fd(out.as_fd(), PollFlags::IN));
        }
        if let Some(err) = &self.stderr {
            fds.push(PollFd::from_borrowed_fd(err.as_fd(), PollFlags::IN));
        }
        let ts = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let _ = poll(&mut fds, Some(&ts));
    }

    /// Windows: waits up to `timeout` (at most [`PIPE_POLL`]) until the process exited. A pipe
    /// cannot be waited on for "readable", so the caller reads what arrived after every wait; a
    /// scan prints a few KiB, and a full pipe only holds the plugin up until the next look.
    #[cfg(windows)]
    fn wait(&self, watcher: &ProcessWatcher, timeout: Duration) {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        let wait = timeout.min(PIPE_POLL).as_millis().max(1) as u32;
        // SAFETY: the watcher's live process handle; a plain timed wait.
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                watcher.as_handle().as_raw_handle(),
                wait,
            )
        };
    }

    /// Reads whatever the pipes hold now, without blocking.
    fn read_ready(&mut self) {
        read_pipe(&mut self.stdout, &mut self.out, MAX_STDOUT, false);
        read_pipe(&mut self.stderr, &mut self.err, MAX_STDERR, true);
    }
}

/// How many bytes `reader` can give without blocking: `None` at EOF (or on an error).
#[cfg(unix)]
fn ready_bytes<R: AsFd>(reader: &R) -> Option<usize> {
    let mut fds = [PollFd::from_borrowed_fd(reader.as_fd(), PollFlags::IN)];
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match poll(&mut fds, Some(&zero)) {
        // Readable: data, or EOF (which the read then reports as 0).
        Ok(n) if n > 0 => Some(usize::MAX),
        _ => Some(0),
    }
}

/// Windows: the bytes queued in the pipe (`PeekNamedPipe`); `None` once the writer is gone and
/// nothing is left.
#[cfg(windows)]
fn ready_bytes<R: std::os::windows::io::AsHandle>(reader: &R) -> Option<usize> {
    use std::os::windows::io::AsRawHandle;
    let mut available = 0u32;
    // SAFETY: a valid pipe handle; only the count is asked for.
    let ok = unsafe {
        windows_sys::Win32::System::Pipes::PeekNamedPipe(
            reader.as_handle().as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available as usize)
}

/// Reads from `pipe` while it has data; closes it at EOF. Keeps at most `max` bytes (the tail if
/// `tail`, else the head).
#[cfg(unix)]
fn read_pipe<R: Read + AsFd>(pipe: &mut Option<R>, buf: &mut Vec<u8>, max: usize, tail: bool) {
    read_pipe_with(pipe, buf, max, tail, ready_bytes);
}

/// Reads from `pipe` while it has data; closes it at EOF. Keeps at most `max` bytes (the tail if
/// `tail`, else the head).
#[cfg(windows)]
fn read_pipe<R: Read + std::os::windows::io::AsHandle>(
    pipe: &mut Option<R>,
    buf: &mut Vec<u8>,
    max: usize,
    tail: bool,
) {
    read_pipe_with(pipe, buf, max, tail, ready_bytes);
}

fn read_pipe_with<R: Read>(
    pipe: &mut Option<R>,
    buf: &mut Vec<u8>,
    max: usize,
    tail: bool,
    ready: fn(&R) -> Option<usize>,
) {
    let mut chunk = [0u8; 8192];
    while let Some(reader) = pipe.as_mut() {
        let len = match ready(reader) {
            None => {
                *pipe = None;
                return;
            }
            Some(0) => return,
            Some(n) => n.min(chunk.len()),
        };
        match reader.read(&mut chunk[..len]) {
            Ok(0) | Err(_) => *pipe = None,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > max {
                    if tail {
                        buf.drain(..buf.len() - max);
                    } else {
                        buf.truncate(max);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLE: &str = "/a/Y.clap";

    fn exit(code: Option<i32>, signal: Option<i32>) -> ProcessExit {
        ProcessExit { code, signal }
    }

    #[test]
    fn exit_statuses_are_interpreted() {
        let json = br#"[{"format":"clap","id":"x.y","name":"Y","vendor":"X","version":"1",
            "kind":"effect","features":["audio-effect","reverb"],"bundle":"/a/Y.clap"}]"#;
        let ok = interpret(exit(Some(0), None), json, b"", Path::new(BUNDLE));
        assert_eq!(ok.error, None);
        assert!(ok.cacheable);
        assert_eq!(ok.descriptors[0].id, "x.y");

        let empty = interpret(exit(Some(0), None), b"[]\n", b"", Path::new(BUNDLE));
        assert_eq!(empty.error.as_deref(), Some("no plugins in the bundle"));
        assert!(empty.cacheable);

        let garbage = interpret(exit(Some(0), None), b"[{", b"", Path::new(BUNDLE));
        assert!(garbage.error.unwrap().starts_with("unreadable scan output"));
        assert!(!garbage.cacheable);

        let failed = interpret(
            exit(Some(1), None),
            b"",
            b"plugin noise\nkomponist-bridge: /a/Y.clap: no entry point\nmore noise\n",
            Path::new(BUNDLE),
        );
        assert_eq!(failed.error.as_deref(), Some("no entry point"));
        assert!(failed.cacheable);

        let unsupported = interpret(
            exit(Some(3), None),
            b"",
            b"komponist-bridge: /a/Y.vst3: unsupported: VST3\n",
            Path::new("/a/Y.vst3"),
        );
        assert_eq!(
            unsupported.error.as_deref(),
            Some("format not supported yet: unsupported: VST3")
        );
        assert!(!unsupported.cacheable);

        let crashed = interpret(exit(None, Some(11)), b"", b"", Path::new(BUNDLE));
        assert_eq!(crashed.error.as_deref(), Some("crashed (signal 11)"));
        assert!(crashed.cacheable);
        // Windows: an NTSTATUS crash code reads as the exception.
        #[cfg(windows)]
        {
            let violation = 0xC000_0005_u32 as i32;
            let crashed = interpret(exit(None, Some(violation)), b"", b"", Path::new(BUNDLE));
            assert_eq!(
                crashed.error.as_deref(),
                Some("crashed (exception 0xC0000005)")
            );
        }

        let usage = interpret(exit(Some(2), None), b"", b"", Path::new(BUNDLE));
        assert_eq!(usage.error.as_deref(), Some("scan failed (exit code 2)"));
        assert!(!usage.cacheable);
    }

    #[test]
    fn bundles_are_found_recursively_and_sorted() {
        let root = std::env::temp_dir().join(format!("kp-scan-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vendor/sub")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::create_dir_all(root.join("B.vst3/Contents/x86_64-linux")).unwrap();
        for file in [
            "Z.clap",
            "a.CLAP",
            "vendor/sub/Deep.clap",
            ".hidden/Hidden.clap",
            "readme.txt",
            "B.vst3/Contents/x86_64-linux/B.so",
        ] {
            std::fs::write(root.join(file), b"x").unwrap();
        }
        // A symlink loop must not hang the walk (Windows: creating symlinks needs a privilege).
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("vendor/loop")).unwrap();

        let clap = find_bundles(&root, PluginFormat::Clap);
        let names: Vec<_> = clap.iter().map(|p| relative(&root, p)).collect();
        assert_eq!(names, ["Z.clap", "a.CLAP", "vendor/sub/Deep.clap"]);
        let vst3 = find_bundles(&root, PluginFormat::Vst3);
        assert_eq!(vst3, [root.join("B.vst3")]);
        assert!(bundle_mtime(&vst3[0]) > 0);
        assert!(find_bundles(&root.join("missing"), PluginFormat::Clap).is_empty());

        #[cfg(unix)]
        assert_eq!(
            bundle_key(&root.join("vendor/loop/Z.clap")),
            Ok(canonical(&root.join("Z.clap")))
        );
        assert_eq!(
            bundle_key(&root.join("Z.clap")),
            Ok(canonical(&root.join("Z.clap")))
        );
        assert!(bundle_key(&root.join("missing.clap")).is_err());
        #[cfg(unix)]
        let bad =
            root.join(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"\xff.clap"));
        // Windows: an unpaired surrogate.
        #[cfg(windows)]
        let bad = root.join(
            <std::ffi::OsString as std::os::windows::ffi::OsStringExt>::from_wide(&[
                0xD800,
                u16::from(b'.'),
                u16::from(b'c'),
            ]),
        );
        assert_eq!(bundle_key(&bad), Err("path is not valid UTF-8".into()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `path` below `root`, with `/` between its components on every OS.
    fn relative(root: &Path, path: &Path) -> String {
        let parts: Vec<&str> = path
            .strip_prefix(root)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_str().unwrap())
            .collect();
        parts.join("/")
    }

    /// The canonical path as `bundle_key` gives it (Windows: without the verbatim prefix).
    fn canonical(path: &Path) -> PathBuf {
        let path = std::fs::canonicalize(path).unwrap();
        #[cfg(windows)]
        let path = without_verbatim(path);
        path
    }

    /// Windows: verbatim paths lose their prefix; others stay as they are.
    #[cfg(windows)]
    #[test]
    fn verbatim_prefixes_are_dropped() {
        for (from, to) in [
            (r"\\?\C:\Program Files\x.clap", r"C:\Program Files\x.clap"),
            (r"\\?\UNC\server\share\x.clap", r"\\server\share\x.clap"),
            (r"C:\x.clap", r"C:\x.clap"),
            (r"\\?\GLOBALROOT\Device\x", r"\\?\GLOBALROOT\Device\x"),
        ] {
            assert_eq!(without_verbatim(from.into()), PathBuf::from(to));
        }
    }

    /// A bundle folder shaped like the ones macOS plugins ship as (`Contents/MacOS/<name>`,
    /// `Contents/Info.plist`).
    fn macos_bundle(root: &Path, name: &str) -> PathBuf {
        let bundle = root.join(name);
        let stem = name.rsplit_once('.').unwrap().0;
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(
            bundle.join("Contents/MacOS").join(stem),
            b"\xcf\xfa\xed\xfe",
        )
        .unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), b"<plist/>").unwrap();
        bundle
    }

    #[test]
    fn macos_bundle_directories_are_found_whole_and_not_entered() {
        let root = std::env::temp_dir().join(format!("kp-scan-macos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Vendor")).unwrap();
        let clap = macos_bundle(&root, "Surge XT.clap");
        let nested = macos_bundle(&root.join("Vendor"), "Deep.clap");
        let vst3 = macos_bundle(&root, "Vital.vst3");
        // A bundle inside a bundle (a helper app, a nested plugin) is not a plugin of its own.
        macos_bundle(&clap.join("Contents/Resources"), "Inner.clap");
        std::fs::write(root.join(".DS_Store"), b"x").unwrap();

        let found = find_bundles(&root, PluginFormat::Clap);
        assert_eq!(found, [clap.clone(), nested]);
        assert_eq!(find_bundles(&root, PluginFormat::Vst3), [vst3]);
        // The cache key is the canonical path of the bundle folder.
        assert_eq!(bundle_key(&clap), Ok(canonical(&clap)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_bundle_directory_changes_when_its_binary_or_plist_does() {
        let root = std::env::temp_dir().join(format!("kp-scan-mtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let bundle = macos_bundle(&root, "Fx.clap");
        let set = |file: &Path, secs: u64| {
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        };
        // Everything old, then touch one file at a time.
        let binary = bundle.join("Contents/MacOS/Fx");
        let plist = bundle.join("Contents/Info.plist");
        set(&binary, 1_000);
        set(&plist, 1_000);
        let before = bundle_mtime(&bundle);
        assert!(before > 0);
        set(&binary, 4_000_000_000);
        assert_eq!(bundle_mtime(&bundle), 4_000_000_000);
        set(&binary, 1_000);
        set(&plist, 4_100_000_000);
        assert_eq!(bundle_mtime(&bundle), 4_100_000_000);
        // A plain file (a Linux `.clap`) has its own mtime.
        let file = root.join("Plain.clap");
        std::fs::write(&file, b"x").unwrap();
        set(&file, 4_200_000_000);
        assert_eq!(bundle_mtime(&file), 4_200_000_000);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Whether `pid` still runs: not a zombie (a reparented one may wait for an init that does
    /// not reap, a container's).
    #[cfg(unix)]
    fn running(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return false;
            };
            // "pid (comm) state …"
            let state = stat
                .rfind(')')
                .and_then(|i| stat[i + 1..].trim_start().chars().next());
            state.is_some_and(|s| s != 'Z' && s != 'X')
        }
        #[cfg(target_os = "macos")]
        {
            let out = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("run ps");
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        }
    }

    /// #218: a scan that times out is killed with the processes it started (its process group),
    /// not only the scan process itself.
    #[test]
    #[cfg(unix)]
    fn a_timed_out_scan_is_killed_with_the_processes_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("helper.pid");
        // The "scan process" starts a helper, names it and hangs, as a plugin might.
        let script = format!("sleep 30 & echo $! > '{}'; wait", marker.display());
        let args: Vec<OsString> = vec!["-c".into(), script.into(), "sh".into()];
        let cancel = AtomicBool::new(false);
        let timeout = Duration::from_millis(500);
        let scanned = scan_bundle(Path::new("sh"), &args, Path::new(BUNDLE), timeout, &cancel)
            .expect("not cancelled");
        let error = scanned.error.unwrap_or_default();
        assert!(error.starts_with("timed out"), "{error}");
        assert!(!scanned.cacheable);
        let helper: u32 = std::fs::read_to_string(&marker)
            .expect("the scan process named its helper")
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while running(helper) {
            assert!(
                Instant::now() < deadline,
                "helper {helper} outlived its scan"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
