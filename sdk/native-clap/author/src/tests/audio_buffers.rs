use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::process::{CLAP_PROCESS_CONTINUE_IF_NOT_QUIET, CLAP_PROCESS_ERROR, clap_process};

use super::*;

type P = SidechainProbe;

#[derive(Default, Params)]
struct NoParams {}

#[derive(Default)]
struct SidechainProbe(Arc<NoParams>);

impl Plugin for SidechainProbe {
    const NAME: &'static str = "Sidechain probe";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = "0";
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(1),
        main_output_channels: NonZeroU32::new(1),
        aux_input_ports: &[NonZeroU32::MIN],
        ..AudioIOLayout::const_default()
    }];

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.0.clone()
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        aux: &mut AuxiliaryBuffers,
        _: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        buffer.as_slice()[0].copy_from_slice(aux.inputs[0].as_slice_immutable()[0]);
        ProcessStatus::Normal
    }
}

impl ClapPlugin for SidechainProbe {
    const CLAP_ID: &'static str = "org.klang-studio.komponist.sdk-buffer-test";
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[];
}

fn buffer(channel: &mut *mut f32) -> clap_audio_buffer {
    clap_audio_buffer {
        data32: channel,
        data64: ptr::null_mut(),
        channel_count: 1,
        latency: 0,
        constant_mask: 0,
    }
}

#[test]
fn the_clap_wrapper_uses_only_the_reported_ports_and_activated_frame_limit() {
    let instance = Instance::new::<P>();
    let plugin = instance.0;
    let callbacks = unsafe { &*plugin };
    assert!(unsafe { callbacks.activate.unwrap()(plugin, 48_000.0, 1, 4) });
    assert!(unsafe { callbacks.start_processing.unwrap()(plugin) });

    let mut input = [2.0_f32; 2];
    let mut unused = [5.0_f32; 2];
    let mut output = [9.0_f32; 2];
    let mut input_ptr = input.as_mut_ptr();
    let mut unused_ptr = unused.as_mut_ptr();
    let mut output_ptr = output.as_mut_ptr();
    // Deliberately provide space for a second port while reporting only the main input.
    // This makes accidental reads past the reported count observable without invalid pointers.
    let inputs = [buffer(&mut input_ptr), buffer(&mut unused_ptr)];
    let mut outputs = [buffer(&mut output_ptr)];
    let mut process = clap_process {
        steady_time: 0,
        frames_count: 2,
        transport: ptr::null(),
        audio_inputs: inputs.as_ptr(),
        audio_outputs: outputs.as_mut_ptr(),
        audio_inputs_count: 1,
        audio_outputs_count: 1,
        in_events: ptr::null(),
        out_events: ptr::null(),
    };
    assert_eq!(
        unsafe { callbacks.process.unwrap()(plugin, &process) },
        CLAP_PROCESS_CONTINUE_IF_NOT_QUIET
    );
    assert_eq!(output, [0.0; 2]);

    process.frames_count = 5;
    assert_eq!(
        unsafe { callbacks.process.unwrap()(plugin, &process) },
        CLAP_PROCESS_ERROR
    );
    unsafe {
        callbacks.stop_processing.unwrap()(plugin);
        callbacks.deactivate.unwrap()(plugin);
    }
}
