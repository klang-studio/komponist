//! A headless CLAP gain effect that publishes a native komponist layout.
mod abi;

use std::ffi::{CStr, c_void};
use std::ptr;
use std::sync::Arc;

use clap_sys::plugin::clap_plugin;
use nice_plug::prelude::*;
use nice_plug::wrapper::clap::Wrapper;
use serde_json::{Value, json};

use abi::{
    ABSENT, BUFFER_TOO_SMALL, ERROR, EXTENSION_ID, LayoutExtension, MAX_LAYOUT_BYTES, OK, PLUGIN_ID,
};

const GAIN_ID: &str = if cfg!(feature = "collision") {
    "Aa"
} else {
    "gain"
};
const MIX_ID: &str = if cfg!(feature = "collision") {
    "BB"
} else {
    "mix"
};

#[derive(Params)]
struct EffectParams {
    #[cfg(not(feature = "reordered"))]
    #[cfg_attr(feature = "collision", id = "Aa")]
    #[cfg_attr(not(feature = "collision"), id = "gain")]
    gain: FloatParam,

    #[cfg_attr(feature = "collision", id = "BB")]
    #[cfg_attr(not(feature = "collision"), id = "mix")]
    mix: FloatParam,

    #[cfg(feature = "reordered")]
    #[cfg_attr(feature = "collision", id = "Aa")]
    #[cfg_attr(not(feature = "collision"), id = "gain")]
    gain: FloatParam,
}

impl Default for EffectParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(
                "Gain",
                0.0,
                FloatRange::Linear {
                    min: -36.0,
                    max: 12.0,
                },
            )
            .with_unit(" dB"),
            mix: FloatParam::new("Mix", 1.0, FloatRange::Linear { min: 0.0, max: 1.0 }),
        }
    }
}

#[derive(Default)]
struct Prototype {
    params: Arc<EffectParams>,
}

impl Plugin for Prototype {
    const NAME: &'static str = "Komponist layout prototype";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio/komponist";
    const EMAIL: &'static str = "info@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let mix = self.params.mix.value();
        let multiplier = 1.0 + mix * (util::db_to_gain(self.params.gain.value()) - 1.0);
        for frame in buffer.iter_samples() {
            for sample in frame {
                *sample *= multiplier;
            }
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for Prototype {
    const CLAP_ID: &'static str = PLUGIN_ID;
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Two stable layout bindings");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::AudioEffect, ClapFeature::Stereo];

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        if id == EXTENSION_ID {
            ptr::from_ref(&LAYOUT).cast()
        } else {
            ptr::null()
        }
    }
}

static LAYOUT: LayoutExtension = LayoutExtension { get_layout };

fn cell(col: u8, param_id: u32, english: &str, german: &str) -> Value {
    json!({
        "widget": "knob",
        "col": col, "row": 0, "cols": 1, "rows": 1, "param_id": param_id,
        "labels": {"en": english, "de": german}
    })
}

unsafe extern "C" fn get_layout(
    plugin: *const clap_plugin,
    dest: *mut u8,
    capacity: u32,
    required_or_written: *mut u32,
) -> u32 {
    if required_or_written.is_null() {
        return ERROR;
    }
    // SAFETY: The host supplies a live writable length pointer and, for copies, a distinct buffer
    // of at least capacity bytes. The instance is the Prototype supplied by the CLAP factory.
    unsafe { required_or_written.write(0) };
    if plugin.is_null() || (dest.is_null() && capacity != 0) {
        return ERROR;
    }
    let Some(gain) = (unsafe { Wrapper::<Prototype>::custom_extension_param_id(plugin, GAIN_ID) })
    else {
        return ABSENT;
    };
    let Some(mix) = (unsafe { Wrapper::<Prototype>::custom_extension_param_id(plugin, MIX_ID) })
    else {
        return ABSENT;
    };
    let layout = json!({
        "default_locale": "en",
        "locales": ["en", "de"],
        "pages": [{
            "key": "main",
            "labels": {"en": "Main", "de": "Hauptseite"},
            "columns": 4,
            "cells": [cell(0, gain, "Gain", "Pegel"), cell(1, mix, "Mix", "Mischung")]
        }]
    });
    let Ok(bytes) = serde_json::to_vec(&layout) else {
        return ERROR;
    };
    let Ok(len) = u32::try_from(bytes.len()) else {
        return ERROR;
    };
    if len == 0 || len > MAX_LAYOUT_BYTES {
        return ERROR;
    }
    unsafe { required_or_written.write(len) };
    if len > capacity {
        return BUFFER_TOO_SMALL;
    }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), dest, bytes.len()) };
    OK
}

nice_export_clap!(Prototype);
