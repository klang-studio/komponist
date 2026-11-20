//! A chromatic sample instrument with an optional slice map.
mod dsp;
#[cfg(test)]
mod tests;

use komponist_device_core::{CatalogSpec, Dsp, InternalProcessor, Spec, SpecCache, no_presets};
use komponist_i18n::Lang;
use komponist_model::params::layout::DeviceLayout;
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};
use serde_json::Value;

use komponist_sampler::SamplerHandle;
pub use komponist_sampler::settings::{MAX_FADE_MS, MAX_SLICES, Settings, Slice, SliceOptions};

/// The device.
pub static SPEC: Spec = Spec {
    catalog: CatalogSpec {
        id: "general-sampler",
        name: "Sampler",
        name_in: Lang::device_name_general_sampler,
        role: komponist_model::DeviceRole::Instrument,
        features: &["instrument", "sampler"],
        vendor: "komponist",
        sidechain: false,
    },
    params,
    layout,
    dsp,
    presets: no_presets,
    event_stage: None,
    preset_aliases: &[],
    migrate_params: Some(migrate_params),
    cache: SpecCache::new(),
};

const SETTINGS_KEY: &str = "sample_edit";

fn read_slice_options(value: &Value) -> SliceOptions {
    let fade = |key: &str| {
        value[key]
            .as_f64()
            .filter(|value| value.is_finite())
            .map(|value| (value as f32).clamp(0.0, MAX_FADE_MS))
    };
    SliceOptions {
        reverse: value["reverse"].as_bool(),
        fade_in_ms: fade("fade_in_ms"),
        fade_out_ms: fade("fade_out_ms"),
        gate: value["gate"].as_bool().unwrap_or(false),
    }
}

fn slice_options_value(options: SliceOptions) -> Value {
    serde_json::json!({
        "reverse": options.reverse,
        "fade_in_ms": options.fade_in_ms,
        "fade_out_ms": options.fade_out_ms,
        "gate": options.gate,
    })
}

pub fn read_settings(state: Option<&[u8]>) -> Settings {
    let Some(value) = state.and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok()) else {
        return Settings::default();
    };
    let value = &value[SETTINGS_KEY];
    let mut settings = Settings {
        first: read_slice_options(&value["first_slice"]),
        slices: value["slices"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|marker| {
                Some(Slice {
                    time: marker.as_f64().or_else(|| marker["time"].as_f64())?,
                    options: read_slice_options(marker),
                })
            })
            .collect(),
    };
    settings.normalize();
    settings
}

pub fn write_settings(state: Option<&[u8]>, settings: &Settings) -> Vec<u8> {
    let mut value = state
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let mut settings = settings.clone();
    settings.normalize();
    let object = value.as_object_mut().expect("object");
    let settings_object = object
        .entry(SETTINGS_KEY)
        .or_insert_with(|| serde_json::json!({}));
    if !settings_object.is_object() {
        *settings_object = serde_json::json!({});
    }
    let entries = settings_object.as_object_mut().expect("object");
    if settings.first == SliceOptions::default() {
        entries.remove("first_slice");
    } else {
        entries.insert("first_slice".into(), slice_options_value(settings.first));
    }
    if settings.slices.is_empty() {
        entries.remove("slices");
    } else {
        let slices = settings
            .slices
            .into_iter()
            .map(|slice| {
                if slice.options == SliceOptions::default() {
                    serde_json::json!(slice.time)
                } else {
                    let mut value = slice_options_value(slice.options);
                    value["time"] = serde_json::json!(slice.time);
                    value
                }
            })
            .collect();
        entries.insert("slices".into(), Value::Array(slices));
    }
    if entries.is_empty() {
        object.remove(SETTINGS_KEY);
    }
    serde_json::to_vec(&value).expect("JSON encodes")
}

#[derive(Clone, Copy)]
enum Param {
    Start,
    End,
    FadeIn,
    FadeOut,
    Mode,
    Direction,
    Root,
    Coarse,
    Fine,
    Sync,
    SourceBpm,
    Gain,
    Pan,
    AmpAttack,
    AmpDecay,
    AmpSustain,
    AmpRelease,
    Filter,
    Cutoff,
    Resonance,
    FilterEnv,
    FilterDecay,
    LfoShape,
    LfoRate,
    LfoTarget,
    LfoDepth,
    VelocityTarget,
    VelocityDepth,
}

const PARAM_COUNT: usize = Param::VelocityDepth as usize + 1;

fn params(t: Lang) -> Vec<ParamInfo> {
    let position = |id, label, default| {
        ParamInfo::new(id, label, ParamKind::lin(0.0, 1.0), default).unit(Unit::Percent)
    };
    let time = |id, label, max, default| {
        ParamInfo::new(id, label, ParamKind::lin(0.0, max), default).unit(Unit::Ms)
    };
    let choice = |id, label, names: &[&str], default| {
        ParamInfo::new(
            id,
            label,
            ParamKind::Enum(names.iter().copied().map(Into::into).collect()),
            default,
        )
    };
    let targets = [
        t.param_general_sampler_off(),
        t.param_general_sampler_pitch(),
        t.param_general_sampler_cutoff(),
        t.param_general_sampler_start(),
    ];
    vec![
        position("start", t.param_general_sampler_start(), 0.0),
        position("end", t.param_general_sampler_end(), 1.0),
        time(
            "fade_in",
            t.param_general_sampler_fade_in(),
            f64::from(MAX_FADE_MS),
            0.0,
        ),
        time(
            "fade_out",
            t.param_general_sampler_fade_out(),
            f64::from(MAX_FADE_MS),
            0.0,
        ),
        choice(
            "mode",
            t.param_general_sampler_mode(),
            &[
                t.choice_sampler_mode_one_shot(),
                t.choice_sampler_mode_gate(),
                t.param_general_sampler_slices(),
            ],
            1.0,
        ),
        choice(
            "direction",
            t.param_general_sampler_direction(),
            &[
                t.param_general_sampler_forward(),
                t.param_general_sampler_reverse(),
            ],
            0.0,
        ),
        ParamInfo::new(
            "root",
            t.param_general_sampler_root(),
            ParamKind::Int { min: 0, max: 127 },
            60.0,
        )
        .formatted(|key| komponist_model::format::note_name(key as u8)),
        ParamInfo::new(
            "coarse",
            t.param_general_sampler_coarse(),
            ParamKind::Int { min: -48, max: 48 },
            0.0,
        )
        .unit(Unit::Semitones),
        ParamInfo::new(
            "fine",
            t.param_general_sampler_fine(),
            ParamKind::lin(-100.0, 100.0),
            0.0,
        )
        .unit(Unit::Cents),
        ParamInfo::new("sync", t.param_general_sampler_sync(), ParamKind::Bool, 0.0)
            .flags(false, false),
        ParamInfo::new(
            "source_bpm",
            t.param_general_sampler_source_bpm(),
            ParamKind::lin(40.0, 240.0),
            120.0,
        )
        .flags(false, false),
        ParamInfo::new(
            "gain",
            t.param_general_sampler_gain(),
            ParamKind::scaled(-70.0, 12.0, Scale::Db),
            0.0,
        )
        .unit(Unit::Db),
        ParamInfo::new(
            "pan",
            t.param_general_sampler_pan(),
            ParamKind::scaled(-1.0, 1.0, Scale::Pan),
            0.0,
        ),
        time("amp_attack", t.param_general_sampler_attack(), 5000.0, 0.0),
        time("amp_decay", t.param_general_sampler_decay(), 10000.0, 500.0),
        position("amp_sustain", t.param_general_sampler_sustain(), 1.0),
        time(
            "amp_release",
            t.param_general_sampler_release(),
            10000.0,
            200.0,
        ),
        choice(
            "filter",
            t.param_general_sampler_filter(),
            &[
                t.param_general_sampler_off(),
                t.param_general_sampler_low_pass(),
                t.param_general_sampler_high_pass(),
                t.param_general_sampler_band_pass(),
            ],
            0.0,
        ),
        ParamInfo::new(
            "cutoff",
            t.param_general_sampler_cutoff(),
            ParamKind::scaled(20.0, 20000.0, Scale::Log),
            20000.0,
        )
        .unit(Unit::Hz),
        position("resonance", t.param_general_sampler_resonance(), 0.0),
        ParamInfo::new(
            "filter_env",
            t.param_general_sampler_filter_env(),
            ParamKind::lin(-8.0, 8.0),
            0.0,
        )
        .unit(Unit::Octaves),
        time(
            "filter_decay",
            t.param_general_sampler_filter_decay(),
            10000.0,
            200.0,
        ),
        choice(
            "lfo_shape",
            t.param_general_sampler_lfo_shape(),
            &[
                t.param_general_sampler_sine(),
                t.param_general_sampler_triangle(),
                t.param_general_sampler_saw_up(),
                t.param_general_sampler_saw_down(),
                t.param_general_sampler_square(),
                t.param_general_sampler_sample_hold(),
            ],
            0.0,
        ),
        ParamInfo::new(
            "lfo_rate",
            t.param_general_sampler_lfo_rate(),
            ParamKind::scaled(0.05, 30.0, Scale::Log),
            2.0,
        )
        .unit(Unit::Hz)
        .formatted(|rate| format!("{rate:.2} Hz")),
        choice(
            "lfo_target",
            t.param_general_sampler_lfo_target(),
            &targets,
            0.0,
        ),
        ParamInfo::new(
            "lfo_depth",
            t.param_general_sampler_lfo_depth(),
            ParamKind::lin(-1.0, 1.0),
            0.0,
        )
        .unit(Unit::Percent),
        choice(
            "velocity_target",
            t.param_general_sampler_velocity_target(),
            &targets,
            0.0,
        ),
        ParamInfo::new(
            "velocity_depth",
            t.param_general_sampler_velocity_depth(),
            ParamKind::lin(-1.0, 1.0),
            0.0,
        )
        .unit(Unit::Percent),
    ]
}

// Earlier states stored slice mode separately and used mode 2 for looping.
fn migrate_params(params: &mut komponist_device_core::StateParams) {
    if !params.contains_key("slicing") && !params.contains_key("loop_start") {
        return;
    }
    if params
        .get("slicing")
        .and_then(Value::as_f64)
        .is_some_and(|slicing| slicing >= 0.5)
    {
        params.insert("mode".into(), serde_json::json!(2.0));
    } else if params.get("mode").and_then(Value::as_f64) == Some(2.0) {
        params.insert("mode".into(), serde_json::json!(1.0));
    }
    if params.get("direction").and_then(Value::as_f64) == Some(2.0) {
        params.insert("direction".into(), serde_json::json!(0.0));
    }
}

/// Source tempo and sync switch stored in project state.
pub fn sync_settings(state: Option<&[u8]>) -> (f64, bool) {
    let values = SPEC.decode_state(state).unwrap_or_else(|_| SPEC.defaults());
    (
        values[Param::SourceBpm as usize],
        values[Param::Sync as usize] >= 0.5,
    )
}

fn layout(t: Lang) -> DeviceLayout {
    DeviceLayout::knob_grid(params(t).into_iter().map(|info| info.id), 11)
}

fn dsp() -> Box<dyn Dsp> {
    Box::new(dsp::GeneralSamplerDsp::new())
}

/// Creates this device's DSP and its associated non-realtime handle.
pub fn dsp_with_handle() -> (Box<dyn Dsp>, SamplerHandle) {
    let dsp = dsp::GeneralSamplerDsp::new();
    let handle = dsp.handle();
    (Box::new(dsp), handle)
}

/// Creates a processor and its UI handle from a project state, falling back to parameter defaults
/// if the state cannot be decoded. The handle also receives the saved slicing settings.
pub fn processor_from_state(state: Option<&[u8]>) -> (InternalProcessor, SamplerHandle) {
    let values = SPEC.decode_state(state).unwrap_or_else(|_| SPEC.defaults());
    let (dsp, handle) = dsp_with_handle();
    let processor = InternalProcessor::from_dsp(&SPEC, &values, dsp);
    handle.set_general_settings(&read_settings(state));
    (processor, handle)
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn old_slice_and_loop_states_load_as_slices_and_gate() {
        let read = |state: &[u8]| SPEC.decode_state(Some(state)).unwrap();
        let sliced = read(br#"{"params":{"mode":0,"slicing":1,"direction":1}}"#);
        assert_eq!(sliced[Param::Mode as usize], 2.0);
        assert_eq!(sliced[Param::Direction as usize], 1.0);
        let looped = read(br#"{"params":{"mode":2,"slicing":0,"direction":2}}"#);
        assert_eq!(looped[Param::Mode as usize], 1.0);
        assert_eq!(looped[Param::Direction as usize], 0.0);
        let encoded = SPEC.encode_state(&sliced);
        assert_eq!(read(&encoded), sliced);
    }

    #[test]
    fn settings_round_trip_without_losing_parameters_or_sample_links() {
        let state = br#"{"params":{"gain":-12},"samples":{"1":{"abs":"/sample.wav"}}}"#;
        let mut settings = Settings::from_markers([1.0, 0.5, 0.5, f64::NAN, 0.0]);
        assert_eq!(
            settings
                .slices
                .iter()
                .map(|slice| slice.time)
                .collect::<Vec<_>>(),
            [0.5, 1.0]
        );
        settings.first.gate = true;
        settings.slices[0].options = SliceOptions {
            reverse: Some(true),
            fade_in_ms: Some(8.0),
            fade_out_ms: Some(10.0),
            gate: false,
        };
        let edited = write_settings(Some(state), &settings);
        assert_eq!(read_settings(Some(&edited)), settings);
        let gain = SPEC
            .wire(&komponist_model::params::ParamId::new("gain"))
            .unwrap();
        let edited = SPEC.with_value(Some(&edited), gain, -6.0).unwrap();
        assert_eq!(read_settings(Some(&edited)), settings);
        assert_eq!(komponist_sampler::links::read(Some(&edited)).len(), 1);
    }
}
