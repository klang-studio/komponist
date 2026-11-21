use std::cell::{Cell as Count, RefCell};
use std::mem;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::{
    CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports,
};
use clap_sys::ext::audio_ports_config::{
    CLAP_EXT_AUDIO_PORTS_CONFIG, clap_plugin_audio_ports_config,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency, clap_plugin_latency};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use nice_plug::midi::{Channel, Key, VoiceID};

use super::*;

#[derive(Params)]
struct ProbeParams {
    #[id = "gain"]
    gain: FloatParam,
}

impl Default for ProbeParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new("Gain", 0.375, FloatRange::Linear { min: 0.0, max: 1.0 }),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Observation {
    frames: usize,
    tempo: Option<f64>,
    beats: Option<f64>,
    seconds: Option<f64>,
    bar_start: Option<f64>,
    bar_number: Option<i32>,
}

thread_local! {
    static SLICES: RefCell<[Option<Observation>; 8]> = const { RefCell::new([None; 8]) };
}

#[derive(Default)]
struct Probe {
    params: Arc<ProbeParams>,
    activated_once: bool,
}

impl Plugin for Probe {
    const NAME: &'static str = "CLAP control test";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = "0";
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::Basic;
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            aux_input_ports: &[NonZeroU32::MIN],
            ..AudioIOLayout::const_default()
        },
    ];

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn activate(
        &mut self,
        _: &AudioIOLayout,
        _: &BufferConfig,
        context: &mut impl ActivateContext<Self>,
    ) -> bool {
        if !self.activated_once {
            self.activated_once = true;
            context.set_latency_samples(64);
        }
        true
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let transport = context.transport();
        SLICES.with_borrow_mut(|slices| {
            *slices.iter_mut().find(|entry| entry.is_none()).unwrap() = Some(Observation {
                frames: buffer.samples(),
                tempo: transport.tempo,
                beats: transport.pos_beats,
                seconds: transport.pos_seconds,
                bar_start: transport.bar_start_pos_beats,
                bar_number: transport.bar_number,
            });
        });
        context.set_latency_samples(128);
        for timing in [u32::MAX, 0] {
            let _ = context.try_send_event(NoteEvent::NoteOn {
                timing,
                voice_id: VoiceID::Wildcard,
                channel: Channel::Number(0),
                key: Key::Number(60),
                velocity: 1.0,
            });
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for Probe {
    const CLAP_ID: &'static str = "org.klang-studio.komponist.transport-test";
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[];
}

#[derive(Default)]
struct HostStats {
    plugin: Count<*const clap_plugin>,
    latency_notified: Count<u32>,
    reported_latency: Count<u32>,
    restarts: Count<u32>,
}

fn host(stats: &HostStats) -> clap_host {
    clap_host {
        host_data: ptr::from_ref(stats).cast_mut().cast(),
        get_extension: Some(host_extension),
        request_restart: Some(restart),
        ..HOST
    }
}

fn instance(host: &clap_host, stats: &HostStats) -> Instance {
    let wrapper = Arc::into_raw(unsafe { Wrapper::<Probe>::new(host).unwrap() });
    let plugin = unsafe { (*wrapper).clap_plugin.as_ptr() };
    let instance = Instance(plugin);
    stats.plugin.set(plugin);
    assert!(unsafe { ((*plugin).init.unwrap())(plugin) });
    instance
}

unsafe fn stats<'a>(host: *const clap_host) -> &'a HostStats {
    unsafe { &*(*host).host_data.cast() }
}

unsafe extern "C" fn changed(host: *const clap_host) {
    let stats = unsafe { stats(host) };
    stats.latency_notified.set(stats.latency_notified.get() + 1);
    stats.reported_latency.set(latency(stats.plugin.get()));
}

unsafe extern "C" fn restart(host: *const clap_host) {
    let stats = unsafe { stats(host) };
    stats.restarts.set(stats.restarts.get() + 1);
}

unsafe extern "C" fn host_extension(_: *const clap_host, id: *const c_char) -> *const c_void {
    static LATENCY: clap_host_latency = clap_host_latency {
        changed: Some(changed),
    };
    match unsafe { CStr::from_ptr(id) } {
        id if id == CLAP_EXT_LATENCY => ptr::from_ref(&LATENCY).cast(),
        _ => ptr::null(),
    }
}

fn extension_ptr<T>(plugin: *const clap_plugin, id: &CStr) -> *const T {
    let table = unsafe { ((*plugin).get_extension.unwrap())(plugin, id.as_ptr()) };
    assert!(!table.is_null());
    table.cast()
}

fn latency(plugin: *const clap_plugin) -> u32 {
    let table = unsafe { &*extension_ptr::<clap_plugin_latency>(plugin, CLAP_EXT_LATENCY) };
    unsafe { (table.get.unwrap())(plugin) }
}

fn activate(plugin: *const clap_plugin) {
    assert!(unsafe { ((*plugin).activate.unwrap())(plugin, 48_000.0, 1, 512) });
}

fn transport(
    time: u32,
    tempo: Option<(f64, f64)>,
    beats: i64,
    seconds: i64,
) -> clap_event_transport {
    clap_event_transport {
        header: header::<clap_event_transport>(time, CLAP_EVENT_TRANSPORT),
        flags: CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
            | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
            | CLAP_TRANSPORT_IS_PLAYING
            | if tempo.is_some() {
                CLAP_TRANSPORT_HAS_TEMPO
            } else {
                0
            },
        song_pos_beats: beats * CLAP_BEATTIME_FACTOR,
        song_pos_seconds: seconds * CLAP_SECTIME_FACTOR,
        tempo: tempo.map_or(0.0, |pair| pair.0),
        tempo_inc: tempo.map_or(0.0, |pair| pair.1),
        ..unsafe { mem::zeroed() }
    }
}

fn header<T>(time: u32, type_: u16) -> clap_event_header {
    clap_event_header {
        size: mem::size_of::<T>() as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

fn param(time: u32, id: u32) -> clap_event_param_value {
    clap_event_param_value {
        header: header::<clap_event_param_value>(time, CLAP_EVENT_PARAM_VALUE),
        param_id: id,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value: 0.625,
    }
}

unsafe extern "C" fn input_len(events: *const clap_input_events) -> u32 {
    let items = unsafe { *(*events).ctx.cast::<&[*const clap_event_header]>() };
    items.len() as u32
}

unsafe extern "C" fn input_get(
    events: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    let items = unsafe { *(*events).ctx.cast::<&[*const clap_event_header]>() };
    items.get(index as usize).copied().unwrap_or(ptr::null())
}

struct Output {
    limit: usize,
    events: Vec<(u32, u16)>,
}

impl Output {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            events: Vec::with_capacity(32),
        }
    }

    fn callbacks(&mut self) -> clap_output_events {
        clap_output_events {
            ctx: ptr::from_mut(self).cast(),
            try_push: Some(output_push),
        }
    }
}

unsafe extern "C" fn output_push(
    events: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    let output = unsafe { &mut *(*events).ctx.cast::<Output>() };
    if output.events.len() == output.limit {
        return false;
    }
    let event = unsafe { &*event };
    output.events.push((event.time, event.type_));
    true
}

fn port_buffer(channels: &mut [*mut f32; 1]) -> clap_audio_buffer {
    clap_audio_buffer {
        data32: channels.as_mut_ptr(),
        data64: ptr::null_mut(),
        channel_count: 1,
        latency: 0,
        constant_mask: 0,
    }
}

fn run_process(
    plugin: *const clap_plugin,
    snapshot: &clap_event_transport,
    input: &[*const clap_event_header],
    output: &mut Output,
) -> Vec<Observation> {
    let mut source = [0.0; 400];
    let mut target = [0.0; 400];
    let mut in_channels = [source.as_mut_ptr()];
    let mut out_channels = [target.as_mut_ptr()];
    let inputs = port_buffer(&mut in_channels);
    let mut outputs = port_buffer(&mut out_channels);
    let in_events = clap_input_events {
        ctx: ptr::from_ref(&input).cast_mut().cast(),
        size: Some(input_len),
        get: Some(input_get),
    };
    let out_events = output.callbacks();
    let process = clap_process {
        steady_time: 0,
        frames_count: 400,
        transport: snapshot,
        audio_inputs: &inputs,
        audio_outputs: &mut outputs,
        audio_inputs_count: 1,
        audio_outputs_count: 1,
        in_events: &in_events,
        out_events: &out_events,
    };
    SLICES.with_borrow_mut(|slices| *slices = [None; 8]);
    assert!(unsafe { ((*plugin).start_processing.unwrap())(plugin) });
    assert_ne!(
        unsafe { ((*plugin).process.unwrap())(plugin, &process) },
        CLAP_PROCESS_ERROR
    );
    unsafe { ((*plugin).stop_processing.unwrap())(plugin) };
    SLICES.with_borrow(|slices| slices.iter().copied().flatten().collect())
}

#[test]
fn split_timeline_uses_the_latest_snapshot_and_latencies_commit_on_activation() {
    let stats = HostStats::default();
    let host = host(&stats);
    let instance = instance(&host, &stats);
    let plugin = instance.0;
    activate(plugin);
    assert_eq!(stats.reported_latency.get(), 64);
    assert_eq!(stats.latency_notified.get(), 1);
    assert_eq!(stats.restarts.get(), 0);

    let id = unsafe { Wrapper::<Probe>::custom_extension_param_id(plugin, "gain") }.unwrap();
    let origin = transport(0, Some((120.0, 0.01)), 0, 0);
    let tempo_update = transport(240, Some((90.0, 0.02)), 20, 10);
    let no_tempo_update = transport(350, None, 30, 15);
    let first = param(100, id);
    let second = param(320, id);
    let third = param(370, id);
    let input = [
        &first.header as *const _,
        &tempo_update.header,
        &second.header,
        &no_tempo_update.header,
        &third.header,
    ];
    let slices = run_process(plugin, &origin, &input, &mut Output::new(32));
    assert_eq!(
        slices.iter().map(|slice| slice.frames).collect::<Vec<_>>(),
        [100, 140, 80, 30, 20, 30]
    );
    assert_eq!(slices[1].tempo, Some(121.0));
    let expected_beats = 100.0 * (120.0 + 99.0 * 0.01 * 0.5) / (48_000.0 * 60.0);
    assert!((slices[1].beats.unwrap() - expected_beats).abs() < 1e-12);
    assert_eq!(
        (slices[2].tempo, slices[2].beats, slices[2].seconds),
        (Some(90.0), Some(20.0), Some(10.0))
    );
    assert_eq!(slices[3].tempo, Some(91.6));
    assert!((slices[3].seconds.unwrap() - (10.0 + 80.0 / 48_000.0)).abs() < 1e-12);
    assert_eq!(slices[5].tempo, None);
    assert!((slices[5].seconds.unwrap() - (15.0 + 20.0 / 48_000.0)).abs() < 1e-12);

    assert_eq!(stats.restarts.get(), 1);
    assert_eq!(latency(plugin), 64);
    assert_eq!(stats.latency_notified.get(), 1);
    unsafe { ((*plugin).deactivate.unwrap())(plugin) };
    activate(plugin);
    assert_eq!(stats.reported_latency.get(), 128);
    assert_eq!(stats.latency_notified.get(), 2);
    unsafe { ((*plugin).deactivate.unwrap())(plugin) };
}

#[test]
fn stopped_song_position_remains_fixed_across_automation_slices() {
    let stats = HostStats::default();
    let host = host(&stats);
    let instance = instance(&host, &stats);
    let plugin = instance.0;
    activate(plugin);

    let id = unsafe { Wrapper::<Probe>::custom_extension_param_id(plugin, "gain") }.unwrap();
    let split = param(240, id);
    for tempo in [None, Some((120.0, 0.01))] {
        let mut stopped = transport(0, tempo, 20, 10);
        stopped.flags &= !CLAP_TRANSPORT_IS_PLAYING;
        stopped.flags |= CLAP_TRANSPORT_HAS_TIME_SIGNATURE;
        stopped.tsig_num = 4;
        stopped.tsig_denom = 4;
        stopped.bar_start = 19 * CLAP_BEATTIME_FACTOR;
        stopped.bar_number = 7;
        let slices = run_process(plugin, &stopped, &[&split.header], &mut Output::new(32));
        assert_eq!(slices.len(), 2);
        assert_eq!((slices[0].frames, slices[1].frames), (240, 160));
        assert_eq!(
            slices[1].tempo,
            tempo.map(|(base, increment)| base + 240.0 * increment)
        );
        for slice in &slices {
            assert_eq!((slice.beats, slice.seconds), (Some(20.0), Some(10.0)));
            assert_eq!((slice.bar_start, slice.bar_number), (Some(19.0), Some(7)));
        }
    }
    unsafe { ((*plugin).deactivate.unwrap())(plugin) };
}

#[test]
fn note_output_is_monotone_and_clamped_to_each_process_slice() {
    let stats = HostStats::default();
    let host = host(&stats);
    let instance = instance(&host, &stats);
    let plugin = instance.0;
    activate(plugin);
    let mut output = Output::new(32);
    let split = transport(100, Some((120.0, 0.0)), 0, 0);
    run_process(
        plugin,
        &transport(0, Some((120.0, 0.0)), 0, 0),
        &[&split.header],
        &mut output,
    );
    assert_eq!(
        output.events,
        [
            (99, CLAP_EVENT_NOTE_ON),
            (99, CLAP_EVENT_NOTE_ON),
            (399, CLAP_EVENT_NOTE_ON),
            (399, CLAP_EVENT_NOTE_ON),
        ]
    );
    unsafe { ((*plugin).deactivate.unwrap())(plugin) };
}

#[test]
fn adding_a_sidechain_preserves_the_main_output_and_its_input_pair() {
    let instance = Instance::new::<Probe>();
    let plugin = instance.0;
    let ports = unsafe { &*extension_ptr::<clap_plugin_audio_ports>(plugin, CLAP_EXT_AUDIO_PORTS) };
    let config = unsafe {
        &*extension_ptr::<clap_plugin_audio_ports_config>(plugin, CLAP_EXT_AUDIO_PORTS_CONFIG)
    };
    let main_port = |input| {
        let mut info = MaybeUninit::<clap_audio_port_info>::uninit();
        assert!(unsafe { ports.get.unwrap()(plugin, 0, input, info.as_mut_ptr()) });
        unsafe { info.assume_init() }
    };
    let default_output = main_port(false);
    assert_eq!(default_output.id, 1);
    assert!(unsafe { (config.select.unwrap())(plugin, 1) });
    assert_eq!(unsafe { ports.count.unwrap()(plugin, true) }, 2);
    assert_eq!(main_port(false).id, default_output.id);
    assert_eq!(main_port(true).in_place_pair, default_output.id);
}
