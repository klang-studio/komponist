//! kp-test-crash: the crash fixture for komponist's bridge crash handling (PLAN WP-27).
//!
//! A stereo passthrough with a note input that writes through a null pointer, so the hosting
//! process dies with SIGSEGV or a Windows access violation, in `process` (the audio thread) when
//!
//! - a note-on of key [`CRASH_KEY`] arrives, or
//! - the `crash` parameter rises past [`CRASH_THRESHOLD`]: it was below the threshold at the
//!   previous block (or at activation) and is at or above it now. So a state with `crash` at 1.0
//!   loaded before activation does not crash, but one loaded while active does (nice-plug applies
//!   it like any other parameter change).
//!
//! It can also hang instead (komponist #218, a bridge stuck in a plugin call): with [`HANG_ENV`]
//! set to a file path, `activate` writes the process id to that file and then sleeps [`HANG`].

use nice_plug::midi::Key;
use nice_plug::prelude::*;
use std::sync::Arc;
use std::time::Duration;

/// A note-on of this key crashes the plugin.
pub const CRASH_KEY: u8 = 127;
/// The `crash` parameter crashes the plugin when it rises to this value or above.
pub const CRASH_THRESHOLD: f32 = 0.9;
/// Set to a file path, `activate` writes the process id there and hangs for [`HANG`].
pub const HANG_ENV: &str = "KP_TEST_CRASH_HANG";
/// How long `activate` hangs with [`HANG_ENV`] set: long enough to tell an app-death exit from
/// the socket EOF the bridge sees once the call returns.
pub const HANG: Duration = Duration::from_secs(30);

pub struct KpTestCrash {
    params: Arc<CrashParams>,
    /// The `crash` value at the previous block.
    last_crash: f32,
}

/// Parameter IDs are part of saved states and must never change.
#[derive(Params)]
pub struct CrashParams {
    /// 0 to 1; rising past [`CRASH_THRESHOLD`] crashes the plugin.
    #[id = "crash"]
    pub crash: FloatParam,
}

impl Default for CrashParams {
    fn default() -> Self {
        Self {
            crash: FloatParam::new("Crash", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 }),
        }
    }
}

impl Default for KpTestCrash {
    fn default() -> Self {
        Self {
            params: Arc::new(CrashParams::default()),
            last_crash: 0.0,
        }
    }
}

impl KpTestCrash {
    pub fn params(&self) -> &CrashParams {
        &self.params
    }
}

/// Triggers an access fault by writing through a null pointer.
#[cold]
#[inline(never)]
pub fn crash() -> ! {
    // `black_box` hides the null pointer from the optimizer, so the store is really emitted.
    let null: *mut u32 = std::hint::black_box(std::ptr::null_mut());
    // SAFETY: none, this is the deliberate crash. Address 0 is never mapped on Linux
    // (vm.mmap_min_addr), so the volatile store faults.
    unsafe { null.write_volatile(0xdead) };
    // Only reached where address 0 is writable.
    std::process::abort()
}

impl Plugin for KpTestCrash {
    const NAME: &'static str = "kp-test-crash";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio";
    const EMAIL: &'static str = "info@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::Basic;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        _buffer_config: &BufferConfig,
        _context: &mut impl ActivateContext<Self>,
    ) -> bool {
        if let Some(path) = std::env::var_os(HANG_ENV) {
            // Written whole, then renamed: a reader never sees half a pid.
            let path = std::path::PathBuf::from(path);
            let partial = path.with_extension("partial");
            if std::fs::write(&partial, std::process::id().to_string()).is_ok() {
                let _ = std::fs::rename(&partial, &path);
            }
            std::thread::sleep(HANG);
        }
        self.last_crash = self.params.crash.value();
        true
    }

    fn process(
        &mut self,
        _buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let value = self.params.crash.value();
        if self.last_crash < CRASH_THRESHOLD && value >= CRASH_THRESHOLD {
            crash();
        }
        self.last_crash = value;

        while let Some(event) = context.next_event() {
            if let NoteEvent::NoteOn {
                key: Key::Number(CRASH_KEY),
                ..
            } = event
            {
                crash();
            }
        }
        // The audio passes through: the wrapper has copied the input to the output.
        ProcessStatus::Normal
    }
}

impl ClapPlugin for KpTestCrash {
    const CLAP_ID: &'static str = "de.klang-studio.kp-test-crash";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("komponist test plugin: segfaults on note 127 or when Crash rises past 0.9");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Utility,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for KpTestCrash {
    const VST3_CLASS_ID: [u8; 16] = *b"kpTestCrash00001";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Tools];
}

nice_export_clap!(KpTestCrash);
nice_export_vst3!(KpTestCrash);

#[cfg(test)]
mod tests {
    use super::*;
    use fake_host::{FakeHost, note_on};
    use std::process::{Command, ExitStatus, Stdio};

    /// Names the scenario [`child`] runs.
    const CHILD_ENV: &str = "KP_TEST_CRASH_CHILD";

    fn block() -> Vec<Vec<f32>> {
        vec![vec![0.25; 64], vec![-0.25; 64]]
    }

    /// Runs `scenario` in [`child`], in a new process of this test binary.
    fn run_child(scenario: &str) -> ExitStatus {
        Command::new(std::env::current_exe().unwrap())
            .args(["tests::child", "--exact", "--ignored", "--test-threads=1"])
            .env(CHILD_ENV, scenario)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
    }

    fn prepare_for_crash() {
        #[cfg(target_os = "linux")]
        // SAFETY: affects only this child process. The crash must not generate a core dump.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        // SAFETY: the limit exists for the lifetime of the call and applies only to this child.
        unsafe {
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &limit);
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Diagnostics::Debug::{
                GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
            };
            use windows_sys::Win32::System::ErrorReporting::{
                WER_FAULT_REPORTING_NO_UI, WerSetFlags,
            };
            // SAFETY: plain process-wide settings. An expected crash needs no modal dialog.
            unsafe {
                SetErrorMode(GetErrorMode() | SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
                WerSetFlags(WER_FAULT_REPORTING_NO_UI);
            }
        }
    }

    fn assert_access_fault(status: ExitStatus) {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status}");
        }
        #[cfg(windows)]
        assert_eq!(
            status.code(),
            Some(windows_sys::Win32::Foundation::STATUS_ACCESS_VIOLATION),
            "{status}"
        );
        #[cfg(not(any(unix, windows)))]
        assert!(!status.success(), "{status}");
    }

    #[test]
    #[ignore = "only runs as the child process of the crash tests"]
    fn child() {
        let Ok(scenario) = std::env::var(CHILD_ENV) else {
            return;
        };
        prepare_for_crash();

        let mut host = FakeHost::<KpTestCrash>::activate();
        match scenario.as_str() {
            "note" => {
                host.process(&mut block(), &[note_on(10, CRASH_KEY)]);
            }
            "param" => {
                host.process(&mut block(), &[]);
                host.set_param(&host.plugin.params().crash, 0.95);
                host.process(&mut block(), &[]);
            }
            "harmless" => {
                host.process(&mut block(), &[note_on(10, CRASH_KEY - 1)]);
            }
            _ => panic!("unknown scenario {scenario}"),
        }
    }

    #[test]
    fn note_127_segfaults() {
        assert_access_fault(run_child("note"));
    }

    #[test]
    fn crash_param_rising_past_threshold_segfaults() {
        assert_access_fault(run_child("param"));
    }

    #[test]
    fn child_without_crash_exits_normally() {
        assert!(run_child("harmless").success());
    }

    #[test]
    fn passes_audio_through() {
        let mut host = FakeHost::<KpTestCrash>::activate();
        let mut audio = block();
        host.process(&mut audio, &[note_on(0, 60)]);
        assert_eq!(audio, block());
    }

    #[test]
    fn crash_param_below_threshold_or_already_above_is_harmless() {
        let mut host = FakeHost::<KpTestCrash>::activate();
        host.set_param(&host.plugin.params().crash, 0.89);
        host.process(&mut block(), &[]);

        // Set while inactive, like a state loaded before activation: no rise during processing.
        host.set_param(&host.plugin.params().crash, 1.0);
        let mut host = FakeHost::activate_with(host.plugin);
        host.process(&mut block(), &[]);
        host.set_param(&host.plugin.params().crash, 0.95);
        host.process(&mut block(), &[]);
    }
}
