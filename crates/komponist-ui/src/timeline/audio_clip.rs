//! Audio clip geometry shared by painting and arrangement hit testing (audio proposal §3).
use super::mesh::Quads;
use super::{ClipModel, ClipState, WaveMesh};
use crate::widgets::metrics::timeline as m;
use crate::{fonts, px};
use egui::{Painter, Rangef, Rect, Shape, Stroke, pos2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, size, typography};

/// File availability, independent of the peak worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFileState {
    /// Peaks are ready.
    Ready,
    /// The peak worker is running.
    Pending,
    /// The linked file cannot be read.
    Missing,
}
/// Visual data supplied by the arrangement; text is already localized.
#[derive(Clone, Copy, Debug)]
pub struct AudioLook<'a> {
    /// File/peak status.
    pub file: AudioFileState,
    /// Cached envelope.
    pub wave: &'a WaveMesh,
    /// Original BPM when Re-Pitch is enabled.
    pub repitch: Option<f64>,
    /// Localized compact Re-Pitch label.
    pub repitch_label: &'a str,
    /// Localized missing-file or pending message.
    pub message: &'a str,
    /// Clip gain in dB.
    pub gain_db: f32,
    /// Fade-in length in points.
    pub fade_in: f32,
    /// Fade-out length in points.
    pub fade_out: f32,
}
/// Handle geometry for one audio clip.
#[derive(Clone, Copy, Debug)]
pub struct AudioParts {
    /// Body below the clip header.
    pub body: Rect,
    /// Fade-in grip.
    pub fade_in: Rect,
    /// Fade-out grip.
    pub fade_out: Rect,
    /// Gain grip.
    pub gain: Rect,
    /// Gain-line hit band.
    pub gain_band: Rect,
}
/// Gain position in the body; 0 dB is one third from the top.
pub fn gain_y(body: Rect, gain: f32) -> f32 {
    let [lo, hi] = m::GAIN_RANGE_DB;
    body.top() + (hi - gain.clamp(lo, hi)) / (hi - lo) * body.height()
}
/// Gain at a body coordinate, clamped to the line's range.
pub fn gain_from_y(body: Rect, y: f32) -> f32 {
    let [lo, hi] = m::GAIN_RANGE_DB;
    (hi - (y - body.top()) / body.height().max(1.0) * (hi - lo)).clamp(lo, hi)
}
/// Computes handle geometry for painting and hit testing.
pub fn audio_parts(rect: Rect, fade_in: f32, fade_out: f32, gain_db: f32) -> AudioParts {
    let body = Rect::from_min_max(
        pos2(
            rect.left(),
            (rect.top() + size::CLIP_HEADER).min(rect.bottom()),
        ),
        rect.max,
    );
    let h = m::FADE_HANDLE.min(body.width());
    let a = body.left() + fade_in.clamp(0.0, body.width());
    let b = body.right() - fade_out.clamp(0.0, body.width());
    let handle = |x: f32| {
        Rect::from_min_size(
            pos2(
                (x - h / 2.0).clamp(body.left(), (body.right() - h).max(body.left())),
                body.top(),
            ),
            vec2(h, h),
        )
    };
    let y = gain_y(body, gain_db);
    AudioParts {
        body,
        fade_in: handle(a),
        fade_out: handle(b),
        gain: Rect::from_center_size(pos2(body.center().x, y), vec2(m::GAIN_GRIP, m::GAIN_GRIP)),
        gain_band: Rect::from_x_y_ranges(
            body.x_range(),
            Rangef::new(y - m::GAIN_HIT / 2.0, y + m::GAIN_HIT / 2.0),
        ),
    }
}
pub(super) fn paint_wave(
    q: &mut Quads,
    body: Rect,
    visible: Rangef,
    audio: &AudioLook<'_>,
    model: &ClipModel<'_>,
) {
    let p: &TrackPalette = model.palette;
    if audio.file != AudioFileState::Missing {
        let (peak, core) = if model.muted {
            (color32(color::TEXT_3), color32(color::TEXT_3))
        } else {
            (color32(p.wave_peak), color32(p.wave_rms))
        };
        audio
            .wave
            .paint_into(q, body, visible, peak, core, color32(color::METER_CLIP));
    }
}
pub(super) fn paint_badge(
    painter: &Painter,
    head: Rect,
    audio: &AudioLook<'_>,
    ink: egui::Color32,
) -> Rect {
    let Some(tempo) = audio.repitch else {
        return head;
    };
    let label = if head.width() >= m::CLIP_BADGE_MIN_W {
        format!("{tempo:.0}")
    } else {
        audio.repitch_label.to_owned()
    };
    let font = fonts::font_id(&typography::MONO_XS);
    let galley = painter.layout_no_wrap(label, font, ink);
    let glyph = if head.width() >= m::CLIP_BADGE_MIN_W {
        m::CLIP_BADGE_H
    } else {
        0.0
    };
    let w = galley.size().x + m::CLIP_BADGE_PAD_X * 2.0 + glyph;
    let badge = Rect::from_center_size(
        pos2(
            head.right() - w / 2.0 - m::CLIP_BADGE_PAD_X,
            head.center().y,
        ),
        vec2(w, m::CLIP_BADGE_H),
    );
    if badge.left() <= head.left() {
        return head;
    }
    painter.rect_filled(
        badge,
        komponist_tokens::radius::S,
        color32(color::CANVAS_BOX_SELECT),
    );
    if glyph > 0.0 {
        let x = badge.left() + m::CLIP_BADGE_PAD_X;
        let y = badge.center().y;
        let d = m::CLIP_BADGE_H / 4.0;
        for (x, dir) in [(x + d, -1.0), (x + 3.0 * d, 1.0)] {
            painter.line_segment(
                [pos2(x, y - d), pos2(x, y + d)],
                Stroke::new(m::GAIN_LINE_WIDTH, ink),
            );
            let tip = pos2(x, y + dir * d);
            painter.line_segment(
                [tip, pos2(x - d / 2.0, y)],
                Stroke::new(m::GAIN_LINE_WIDTH, ink),
            );
            painter.line_segment(
                [tip, pos2(x + d / 2.0, y)],
                Stroke::new(m::GAIN_LINE_WIDTH, ink),
            );
        }
    }
    painter.galley(
        badge.min + vec2(m::CLIP_BADGE_PAD_X + glyph, 0.0),
        galley,
        ink,
    );
    Rect::from_min_max(head.min, pos2(badge.left(), head.bottom()))
}
pub(super) fn paint_controls(
    painter: &Painter,
    rect: Rect,
    audio: &AudioLook<'_>,
    state: ClipState,
    body_color: egui::Color32,
) {
    let parts = audio_parts(rect, audio.fade_in, audio.fade_out, audio.gain_db);
    let painter = painter.with_clip_rect(parts.body.intersect(painter.clip_rect()));
    let ink = color32(color::TEXT_1);
    let controls = state.selected;
    let b = parts.body;
    if audio.file == AudioFileState::Missing {
        let visible = b.intersect(painter.clip_rect());
        let step = m::HATCH_ON + m::HATCH_OFF;
        let mut x = visible.left() - b.height() - (visible.left() - b.left()).rem_euclid(step);
        while x < visible.right() {
            painter.line_segment(
                [pos2(x, b.bottom()), pos2(x + b.height(), b.top())],
                Stroke::new(m::HATCH_ON, color32(color::CANVAS_HATCH)),
            );
            x += m::HATCH_ON + m::HATCH_OFF;
        }
    }
    if audio.file == AudioFileState::Missing {
        let icon = crate::widgets::metrics::timeline::HEADER_WARNING_ICON;
        super::paint_icon(
            &painter,
            Rect::from_center_size(
                pos2(b.left() + m::CLIP_HEAD_PAD_X + icon / 2.0, b.center().y),
                vec2(icon, icon),
            ),
            komponist_tokens::Icon::TriangleExclamationPoint,
            color32(color::STATE_ERROR),
        );
    }
    if !audio.message.is_empty() {
        let ink = color32(if audio.file == AudioFileState::Missing {
            color::STATE_ERROR
        } else {
            color::TEXT_3
        });
        let galley = painter.layout_no_wrap(
            super::strip_symbol(audio.message, '⚠').to_owned(),
            fonts::font_id(&typography::LABEL),
            ink,
        );
        let x = b.left()
            + m::CLIP_HEAD_PAD_X
            + if audio.file == AudioFileState::Missing {
                m::HEADER_WARNING_ICON + m::CLIP_BADGE_PAD_X
            } else {
                0.0
            };
        let pos = pos2(x, b.center().y - galley.size().y / 2.0);
        if audio.file == AudioFileState::Pending {
            painter.rect_filled(
                Rect::from_min_size(pos, galley.size()).expand(m::CLIP_BADGE_PAD_X),
                0.0,
                body_color,
            );
        }
        painter.galley(pos, galley, ink);
    }
    for (length, handle, left) in [
        (audio.fade_in, parts.fade_in, true),
        (audio.fade_out, parts.fade_out, false),
    ] {
        if controls || length > m::FADE_VISIBLE_MIN {
            let start = pos2(if left { b.left() } else { b.right() }, b.bottom());
            let end = pos2(
                if left {
                    b.left() + length.min(b.width())
                } else {
                    b.right() - length.min(b.width())
                },
                b.top(),
            );
            painter.add(Shape::convex_polygon(
                vec![start, pos2(start.x, b.top()), end],
                color32(color::CANVAS_BOX_SELECT),
                Stroke::NONE,
            ));
            if controls {
                painter.line_segment([start, end], Stroke::new(m::GAIN_LINE_WIDTH, ink));
            }
        }
        if controls {
            px::fill(&painter, handle, 0.0, ink);
        }
    }
    if controls {
        px::hline(&painter, b.x_range(), gain_y(b, audio.gain_db), ink);
        if controls {
            painter.circle_filled(parts.gain.center(), m::GAIN_GRIP / 2.0, ink);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gain_geometry_round_trips_at_every_zoom() {
        for height in [18.0, 36.0, 90.0] {
            let body = Rect::from_min_size(pos2(20.0, 30.0), vec2(160.0, height));
            for db in [-24.0, -3.0, 0.0, 12.0] {
                assert!((gain_from_y(body, gain_y(body, db)) - db).abs() < 0.0001);
            }
            assert_eq!(gain_from_y(body, body.top() - 100.0), 12.0);
            assert_eq!(gain_from_y(body, body.bottom() + 100.0), -24.0);
        }
    }
    #[test]
    fn gain_and_fade_lines_are_hidden_until_selected_even_when_hovered() {
        let wave = WaveMesh::flat(200.0, 0.02, 36.0);
        let audio = AudioLook {
            file: AudioFileState::Ready,
            wave: &wave,
            repitch: None,
            repitch_label: "RP",
            message: "",
            gain_db: -6.0,
            fade_in: 30.0,
            fade_out: 40.0,
        };
        for (selected, hovered) in [(false, false), (false, true), (true, false)] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let painter = ui.ctx().layer_painter(egui::LayerId::background());
                paint_controls(
                    &painter,
                    Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 50.0)),
                    &audio,
                    ClipState {
                        selected,
                        hovered,
                        ..ClipState::REST
                    },
                    color32(color::SURFACE_1),
                );
            });
            let ink = color32(color::TEXT_1);
            output.textures_delta.clear();
            let lines = output
                .shapes
                .iter()
                .filter(|shape| match &shape.shape {
                    Shape::LineSegment { stroke, .. } => stroke.color == ink,
                    Shape::Rect(rect) => {
                        rect.fill == ink && rect.rect.width() > 150.0 && rect.rect.height() < 2.0
                    }
                    _ => false,
                })
                .count();
            assert_eq!(
                lines,
                if selected { 3 } else { 0 },
                "selected={selected}, hovered={hovered}"
            );
        }
    }

    #[test]
    fn fade_handles_stay_inside_narrow_clips() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(4.0, 50.0));
        let p = audio_parts(r, 100.0, 100.0, 0.0);
        assert!(r.contains_rect(p.fade_in));
        assert!(r.contains_rect(p.fade_out));
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use crate::Lang;
    use crate::timeline::clip_access;
    use komponist_model::{Meter, Tick};
    #[test]
    fn missing_and_repitch_are_named_in_both_languages() {
        let palette = TrackPalette::new(150);
        let wave = WaveMesh::flat(140.0, 0.02, 36.0);
        for lang in [Lang::De, Lang::En] {
            let audio = AudioLook {
                file: AudioFileState::Missing,
                wave: &wave,
                repitch: Some(120.0),
                repitch_label: lang.audio_clip_repitch_badge(),
                message: "",
                gain_db: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
            };
            let mut model = ClipModel::new("Verse", &palette);
            model.audio = Some(&audio);
            model.muted = true;
            let a = clip_access(
                lang,
                &model,
                Tick(0),
                Tick(3840),
                Meter::FOUR_FOUR,
                ClipState::SELECTED,
            );
            assert!(a.name.contains(lang.a11y_audio_missing_suffix()));
            assert!(a.name.contains(&lang.a11y_audio_repitch_suffix(120)));
            assert_eq!(a.selected, Some(true));
        }
    }
}
