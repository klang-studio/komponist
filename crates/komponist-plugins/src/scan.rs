//! Background plugin scans (ARCHITECTURE §8.3).
//!
//! [`ScanJob::start`] runs on its own thread: it walks the plugin folders for bundles (recursively,
//! as the CLAP spec asks for Linux), canonicalizes their paths (the cache key), takes unchanged
//! bundles from the [`ScanCache`] and scans the rest with `komponist-bridge --scan <bundle>`,
//! [`PluginManagerConfig::parallel_scans`](crate::PluginManagerConfig) processes at a time, each
//! killed after [`SCAN_TIMEOUT`](crate::catalog::SCAN_TIMEOUT). The finished job writes the new cache file and hands a
//! [`ScanOutcome`] to the plugin manager, which polls for it.
//!
//! What gets cached: successful scans, bundles without plugins, crashes and load errors (a bundle
//! that fails the same way again is not rescanned until it changes). Not cached, so that the next
//! scan tries again: timeouts (the machine may just have been busy) and formats the bridge cannot
//! host yet (VST3 until WP-32).

use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use komponist_ipc::{PluginDescriptor, PluginFormat, PluginKind, ProcessExit, ProcessWatcher};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::catalog::{CachedBundle, DirCount, PluginDirs, ScanCache, ScanReport, SkippedBundle};

/// How deep the folder walk descends below a plugin folder.
const MAX_DEPTH: usize = 8;

/// How often a waiting scan looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(50);

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
    pub dirs: PluginDirs,
    pub cache: ScanCache,
    pub cache_file: PathBuf,
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
                    let scanned = scan_bundle(&setup.bridge_exe, bundle, setup.timeout, cancel);
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
    if canonical.to_str().is_none() {
        return Err(format!(
            "path {} is not valid UTF-8",
            canonical.to_string_lossy()
        ));
    }
    Ok(canonical)
}

/// Modification time of a bundle in seconds; for a bundle directory (VST3) the newest of the
/// directory and the files of its `Contents/x86_64-linux/`. 0 if unknown.
pub(crate) fn bundle_mtime(path: &Path) -> u64 {
    let secs = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    };
    let mut newest = secs(path);
    if path.is_dir()
        && let Ok(entries) = std::fs::read_dir(path.join("Contents/x86_64-linux"))
    {
        for entry in entries.flatten() {
            newest = newest.max(secs(&entry.path()));
        }
    }
    newest
}

/// Runs `komponist-bridge --scan bundle` with a timeout; `None` when cancelled.
pub(crate) fn scan_bundle(
    bridge_exe: &Path,
    bundle: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Option<Scanned> {
    let mut child = match Command::new(bridge_exe)
        .arg("--scan")
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
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
            let _ = child.kill();
            let _ = child.wait();
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
        if cancel.load(Ordering::Acquire) {
            let _ = watcher.kill();
            let _ = watcher.wait_exit(Some(Duration::from_secs(1)));
            return None;
        }
        let now = Instant::now();
        if now >= deadline {
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
        (None, Some(signal)) => Scanned::failed(format!("crashed (signal {signal})"), true),
        (None, None) => Scanned::failed("scan ended without a status", false),
    }
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

    /// Reads whatever the pipes hold now, without blocking.
    fn read_ready(&mut self) {
        read_pipe(&mut self.stdout, &mut self.out, MAX_STDOUT, false);
        read_pipe(&mut self.stderr, &mut self.err, MAX_STDERR, true);
    }
}

/// Reads from `pipe` while it has data; closes it at EOF. Keeps at most `max` bytes (the tail if
/// `tail`, else the head).
fn read_pipe<R: Read + AsFd>(pipe: &mut Option<R>, buf: &mut Vec<u8>, max: usize, tail: bool) {
    let mut chunk = [0u8; 8192];
    while let Some(reader) = pipe.as_mut() {
        let mut fds = [PollFd::from_borrowed_fd(reader.as_fd(), PollFlags::IN)];
        let zero = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        match poll(&mut fds, Some(&zero)) {
            Ok(n) if n > 0 => {}
            _ => return,
        }
        match reader.read(&mut chunk) {
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
        // A symlink loop must not hang the walk.
        std::os::unix::fs::symlink(&root, root.join("vendor/loop")).unwrap();

        let clap = find_bundles(&root, PluginFormat::Clap);
        let names: Vec<_> = clap
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["Z.clap", "a.CLAP", "vendor/sub/Deep.clap"]);
        let vst3 = find_bundles(&root, PluginFormat::Vst3);
        assert_eq!(vst3, [root.join("B.vst3")]);
        assert!(bundle_mtime(&vst3[0]) > 0);
        assert!(find_bundles(&root.join("missing"), PluginFormat::Clap).is_empty());

        assert_eq!(
            bundle_key(&root.join("vendor/loop/Z.clap")),
            Ok(std::fs::canonicalize(root.join("Z.clap")).unwrap())
        );
        assert!(bundle_key(&root.join("missing.clap")).is_err());
        use std::os::unix::ffi::OsStrExt;
        let bad = root.join(std::ffi::OsStr::from_bytes(b"\xff.clap"));
        assert_eq!(bundle_key(&bad), Err("path is not valid UTF-8".into()));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
