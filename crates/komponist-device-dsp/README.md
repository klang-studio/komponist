# komponist-device-dsp

Shared math for the bundled devices and their graphs. EQ biquad coefficients, Presse's
compressor transfer curve, and Volt's filter, oscillator and modulation formulas live here
alongside Surf's wavetable recipes and previews.

The crate has no dependencies. The device crates use the formulas in processing and
the UI uses them to draw the same response and modulation ranges.
