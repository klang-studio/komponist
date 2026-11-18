//! Shared audio value controls for the dock toolbar and inspector.
use crate::views::{ViewCtx, mixer::send_gesture};
use crate::{Action, EditAction};
use egui::{Response, Ui};
use komponist_model::edits::AudioClipProp;
use komponist_model::{AudioClip, Clip, ClipTimeMode, PPQ, Project, Tempo, Tick};
use komponist_ui::widgets::number_field::NumberField;

/// A numeric audio property.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Gain,
    FadeIn,
    FadeOut,
    SourceStart,
    Tempo,
    Bars,
}
impl Field {
    /// Accessible label, also used above inspector fields.
    pub fn label(self, t: komponist_i18n::Lang) -> &'static str {
        match self {
            Self::Gain => t.audio_clip_gain(),
            Self::FadeIn => t.audio_clip_fade_in(),
            Self::FadeOut => t.audio_clip_fade_out(),
            Self::SourceStart => t.audio_clip_source_start(),
            Self::Tempo => t.audio_clip_original_tempo(),
            Self::Bars => t.audio_clip_bar_count(),
        }
    }
}
/// Duration of a source file in bars at its original tempo; both directions use the full source.
pub fn source_bars(a: &AudioClip, project: &Project) -> f64 {
    let seconds = a.file.frames as f64 / f64::from(a.file.sample_rate);
    seconds * a.original_tempo.bpm / 60.0 / (project.meter.ticks_per_bar() as f64 / PPQ as f64)
}
/// Original tempo inferred from the source duration and a known bar count.
pub fn tempo_from_bars(a: &AudioClip, project: &Project, bars: f64) -> Tempo {
    let seconds = a.file.frames as f64 / f64::from(a.file.sample_rate);
    Tempo::new(
        (bars * (project.meter.ticks_per_bar() as f64 / PPQ as f64) * 60.0 / seconds)
            .clamp(Tempo::MIN_BPM, Tempo::MAX_BPM),
    )
}
/// Shows a field and dispatches a single grouped undo gesture.
pub fn number(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    clip: &Clip,
    a: &AudioClip,
    field: Field,
    width: f32,
    key: bool,
) -> Response {
    let project = std::sync::Arc::clone(ctx.project);
    let name = field.label(ctx.lang);
    let secs_per_tick = 60.0 / (project.tempo.bpm * PPQ as f64);
    let ms_per_tick = secs_per_tick * 1000.0;
    let (mut value, range, step, default, unit) = match field {
        Field::Gain => (
            f64::from(a.gain_db),
            f64::from(komponist_model::CLIP_GAIN_DB_MIN)
                ..=f64::from(komponist_model::CLIP_GAIN_DB_MAX),
            0.1,
            0.0,
            "dB",
        ),
        Field::FadeIn => (
            a.fade_in.0 as f64 * ms_per_tick,
            0.0..=(clip.length - a.fade_out).0 as f64 * ms_per_tick,
            1.0,
            0.0,
            "ms",
        ),
        Field::FadeOut => (
            a.fade_out.0 as f64 * ms_per_tick,
            0.0..=(clip.length - a.fade_in).0 as f64 * ms_per_tick,
            1.0,
            0.0,
            "ms",
        ),
        Field::SourceStart => (
            a.offset as f64 / f64::from(a.file.sample_rate),
            0.0..=a.file.frames.saturating_sub(1) as f64 / f64::from(a.file.sample_rate),
            0.001,
            0.0,
            "",
        ),
        Field::Tempo => (
            a.original_tempo.bpm,
            Tempo::MIN_BPM..=Tempo::MAX_BPM,
            0.01,
            project.tempo.bpm,
            "BPM",
        ),
        Field::Bars => {
            let per_bpm = a.file.frames as f64
                / f64::from(a.file.sample_rate)
                / 60.0
                / (project.meter.ticks_per_bar() as f64 / PPQ as f64);
            (
                source_bars(a, &project),
                Tempo::MIN_BPM * per_bpm..=Tempo::MAX_BPM * per_bpm,
                0.25,
                1.0,
                "",
            )
        }
    };
    let mut widget = NumberField::new(&mut value, name)
        .range(range)
        .step(step)
        .default_value(default)
        .unit(unit)
        .width(width);
    if key {
        widget = widget.key(name);
    }
    widget = match field {
        Field::Gain => widget
            .format(|v| komponist_model::format::fmt_db(v as f32))
            .parse(|t| {
                komponist_model::format::parse_db(t)
                    .map(|v| f64::from(v.max(komponist_model::CLIP_GAIN_DB_MIN)))
            }),
        Field::SourceStart => widget
            .format(komponist_model::format::fmt_time)
            .parse(parse_time),
        Field::Tempo => widget.decimals(2),
        Field::Bars => {
            let lang = ctx.lang;
            widget
                .format(move |v| lang.audio_clip_bars(format!("{v:.2}")))
                .parse(|text| {
                    let text = text
                        .trim()
                        .trim_start_matches('=')
                        .trim()
                        .trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace());
                    komponist_ui::widgets::number_field::parse_number(text, "")
                })
        }
        _ => widget.decimals(0),
    };
    let r = ui.add_enabled(
        !matches!(field, Field::Tempo | Field::Bars)
            || a.time_mode == ClipTimeMode::RePitch
            || a.warp_enabled(),
        widget,
    );
    if matches!(field, Field::Tempo | Field::Bars) {
        komponist_ui::widgets::accesskit(&r, |node| {
            node.set_description(ctx.lang.audio_clip_source_bars_help())
        });
    }
    let (prop, label) = match field {
        Field::Gain => (AudioClipProp::GainDb(value as f32), "undo-clip-gain"),
        Field::FadeIn => (
            AudioClipProp::FadeIn(Tick((value / ms_per_tick).round() as i64)),
            "undo-clip-fade",
        ),
        Field::FadeOut => (
            AudioClipProp::FadeOut(Tick((value / ms_per_tick).round() as i64)),
            "undo-clip-fade",
        ),
        Field::SourceStart => (
            AudioClipProp::SourceOffset((value * f64::from(a.file.sample_rate)).round() as u64),
            "undo-clip-source-start",
        ),
        Field::Tempo => (
            AudioClipProp::OriginalTempo(Tempo::new(value)),
            "undo-clip-original-tempo",
        ),
        Field::Bars => (
            AudioClipProp::OriginalTempo(tempo_from_bars(a, &project, value)),
            "undo-clip-original-tempo",
        ),
    };
    send_gesture(ctx, &r, label, || {
        Action::Edit(EditAction::SetAudioClipProp {
            clip: clip.id,
            prop,
        })
    });
    r
}
fn parse_time(text: &str) -> Option<f64> {
    if let Some((mins, seconds)) = text.trim().split_once(':') {
        Some(mins.parse::<f64>().ok()? * 60.0 + seconds.parse::<f64>().ok()?)
    } else {
        text.trim().parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> AudioClip {
        AudioClip::new(
            komponist_model::AudioFile::new(
                std::path::Path::new("/missing/loop.wav"),
                None,
                384000,
                48000,
                1,
            ),
            0,
            Tempo::new(120.0),
        )
    }
    #[test]
    fn original_tempo_and_source_bars_convert_both_directions() {
        let a = source();
        let p = Project::new("bars");
        assert_eq!(source_bars(&a, &p), 4.0);
        for bars in [2.0, 3.0, 4.0, 8.0] {
            let mut a = a.clone();
            a.original_tempo = tempo_from_bars(&a, &p, bars);
            assert!((source_bars(&a, &p) - bars).abs() < 0.0001);
        }
    }
    #[test]
    fn source_time_accepts_plain_seconds_and_minutes() {
        assert_eq!(parse_time("01:02.125"), Some(62.125));
        assert_eq!(parse_time("2.5"), Some(2.5));
        assert_eq!(parse_time("bad"), None);
    }
}
