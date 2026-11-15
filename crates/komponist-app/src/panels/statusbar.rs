//! Status bar (`size.statusbar` 22; `Screens.dc.html` `stt`, `ks-view.js` `statusView`).
//!
//! Left: what is selected or happening (`type.small`, `text.2`; red while recording or on an
//! error); next to it the hint for the current context (`text.3`); right: the audio output
//! ("PipeWire · 48 kHz · 256 Samples · 5.3 ms", `type.value.s`; "· ohne Echtzeit-Priorität" in the
//! warning colour when the audio thread got none). A status message
//! ([`crate::ui_state::StatusMessage`]) replaces the left text for 7 s; a finished export's
//! message carries the link „Im Ordner zeigen“ right after it, in `accent` (S08 "fertig ·
//! Statuszeile", prototype `linkS`). Texts are verbatim from the screens and the prototype; a
//! leading "● " (recording) is painted as a dot in the text colour, the fonts have no ●
//! (`ks-view.js` `statusView` `dotS`).

use std::time::Instant;

use egui::{Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_audio::{AudioState, HostKind, StreamConfig};
use komponist_engine::TransportState;
use komponist_model::ClipContent;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Srgba, color, space, typography};
use komponist_ui::{px, widgets as w};

use crate::app::{App, STATUS_DURATION};
use crate::metrics::transport::ERROR_DOT;
use crate::ui_state::{FocusArea, StatusKind};

/// Appended to the audio output when the audio thread got no real-time priority.
pub const NO_REALTIME: &str = "ohne Echtzeit-Priorität";

/// The hint while a device parameter is dragged.
pub const PARAM_HINT: &str = "Alt-Klick: Standard · Rechtsklick: Wert eingeben";

/// The three texts of the status bar and their colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    /// Left text.
    pub left: String,
    /// Colour of the left text.
    pub left_color: Srgba,
    /// A green dot before the left text (a success message).
    pub ok_dot: bool,
    /// A link after the left text ("Im Ordner zeigen"); [`show`] reports a click.
    pub link: Option<String>,
    /// The hint next to it.
    pub mid: String,
    /// The audio output, right-aligned.
    pub right: String,
    /// Colour of the right text.
    pub right_color: Srgba,
}

/// "48 kHz", "44.1 kHz".
pub fn fmt_rate(rate: u32) -> String {
    let khz = f64::from(rate) / 1000.0;
    if (khz - khz.round()).abs() < 1e-9 {
        format!("{khz:.0} kHz")
    } else {
        format!("{khz:.1} kHz")
    }
}

/// The audio output as the status bar shows it: driver, rate, buffer and its latency
/// (`buffer / rate`): "PipeWire · 48 kHz · 256 Samples · 5.3 ms".
pub fn fmt_output(config: &StreamConfig) -> String {
    let host = match config.host {
        HostKind::PipeWire => "PipeWire",
        HostKind::Jack => "JACK",
        HostKind::Alsa => "ALSA",
        HostKind::Null => "Kein Audio-Gerät",
    };
    let ms = f64::from(config.buffer_frames) * 1000.0 / f64::from(config.sample_rate.max(1));
    format!(
        "{host} · {} · {} Samples · {ms:.1} ms",
        fmt_rate(config.sample_rate),
        config.buffer_frames
    )
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What the status bar shows for `app` at `now`.
pub fn model(app: &App, now: Instant) -> StatusModel {
    let mut s = StatusModel {
        left: String::new(),
        left_color: color::TEXT_2,
        ok_dot: false,
        link: None,
        mid: String::new(),
        right: String::new(),
        right_color: color::TEXT_3,
    };
    match app.engine().audio_state() {
        // Without real-time priority dropouts are likely: said at the right, in the warning
        // colour (S07 explains it).
        AudioState::Running { config, .. } if app.engine().stats().realtime_denied() => {
            s.right = format!("{} · {NO_REALTIME}", fmt_output(config));
            s.right_color = color::METER_LOUD;
        }
        AudioState::Running { config, .. } => s.right = fmt_output(config),
        AudioState::NoDevice { .. } => {
            s.left = "Kein Audio-Gerät".to_owned();
            s.left_color = color::STATE_ERROR;
            s.right = "Audio angehalten".to_owned();
            s.right_color = color::STATE_ERROR;
            return s;
        }
        AudioState::Stopped => s.right = "Audio angehalten".to_owned(),
    }
    context(app, &mut s);
    if let Some(msg) = &app.ui().status
        && now.saturating_duration_since(msg.since) < STATUS_DURATION
    {
        s.left = msg.text.clone();
        s.ok_dot = msg.kind == StatusKind::Success;
        s.link = msg.link.as_ref().map(|(label, _)| label.clone());
        s.left_color = match msg.kind {
            StatusKind::Error => color::STATE_ERROR,
            StatusKind::Success | StatusKind::Info => color::TEXT_2,
        };
    }
    s
}

/// Left and middle text from the transport, focus and selection.
fn context(app: &App, s: &mut StatusModel) {
    let p = app.project();
    let ui = app.ui();
    let sel = &ui.selection;
    match app.transport() {
        TransportState::Recording => {
            let armed: Vec<&str> = p
                .tracks
                .iter()
                .filter(|t| t.arm)
                .map(|t| t.name.as_str())
                .collect();
            s.left = format!("● Aufnahme läuft · {}", armed.join(", "));
            // "· Take 1": the take the editor shows (S02).
            if let Some(take) = app
                .recording()
                .and_then(|r| r.first().and_then(|(track, _)| r.take_name(track)))
            {
                s.left.push_str(" · ");
                s.left.push_str(&take);
            }
            s.left_color = color::STATE_RECORD;
            s.mid = "R oder Leertaste beendet · Esc verwirft den Take".to_owned();
            return;
        }
        TransportState::CountIn => {
            s.left = "Vorzähler …".to_owned();
            s.left_color = color::STATE_RECORD;
            return;
        }
        TransportState::Playing | TransportState::Stopped => {}
    }
    // A device parameter held and moved: its name and value (WP-89).
    if let Some(touch) = ui.devices.touched.as_ref().filter(|t| t.dragging)
        && let Some(param) =
            crate::views::devices::model::touched(p, app.engine().host(), &ui.plugin_windows, touch)
    {
        s.left = format!("{} · {}", param.name, param.value);
        s.mid = PARAM_HINT.to_owned();
        return;
    }
    let open = ui.editor.clip.and_then(|c| p.find_clip(c));
    let track = sel.tracks.iter().next().and_then(|t| p.track(*t));
    match (ui.focus, open) {
        (FocusArea::PianoRoll, Some((_, clip))) => {
            if let ClipContent::Midi(midi) = &clip.content {
                let total = midi.active().notes.len();
                s.left = format!(
                    "{} · {} von {total} Noten ausgewählt",
                    clip.name,
                    sel.notes.len()
                );
                s.mid = "Strg-Ziehen auf Note: Velocity · Alt-Ziehen: Kopie".to_owned();
                return;
            }
        }
        (FocusArea::StepSequencer, Some((_, clip))) => {
            if let ClipContent::Step(steps) = &clip.content {
                let len = usize::from(steps.length_steps);
                match sel.step.and_then(|c| c.step) {
                    Some(step) => {
                        s.left = format!("{} · Step {} von {len}", clip.name, step + 1);
                        s.mid = "Klick setzt Step · Ziehen ↕ auf Step: Velocity · Alt-Klick: \
                                 Wahrscheinlichkeit"
                            .to_owned();
                    }
                    None => {
                        let set: usize = steps
                            .rows
                            .iter()
                            .map(|r| r.steps.iter().take(len).filter(|st| st.on).count())
                            .sum();
                        s.left = format!("{} · {set} Steps gesetzt", clip.name);
                        s.mid = "Leertaste spielt das Pattern".to_owned();
                    }
                }
                return;
            }
        }
        (FocusArea::Mixer, _) if ui.layout.dock_maximized => {
            if let Some(t) = track {
                s.left = format!("{} ausgewählt", t.name);
            }
            s.mid = "Tab: zurück zum Arrangement · Klick auf rote Anzeige setzt zurück".to_owned();
            return;
        }
        (FocusArea::Devices, _) => {
            if let Some(t) = track {
                let n = t.channel.devices.len();
                s.left = format!("{} · {}", t.name, plural(n, "Gerät", "Geräte"));
                s.mid = "Gerät ziehen: umsortieren · Alt-Klick auf Schalter: Bypass".to_owned();
                return;
            }
        }
        _ => {}
    }
    if !sel.clips.is_empty() {
        s.left = format!("{} ausgewählt", plural(sel.clips.len(), "Clip", "Clips"));
        s.mid = "Ziehen verschiebt · Alt-Ziehen kopiert · rechte Kante kürzt · Strg D dupliziert \
                 · Entf löscht"
            .to_owned();
    } else if let Some(t) = track {
        s.left = format!("{} ausgewählt", t.name);
        s.mid = "Doppelklick in die Spur legt einen Clip an · R nimmt auf".to_owned();
    } else {
        s.left = if app.project_path().is_none() || app.doc().is_dirty() {
            format!("{} · nicht gespeichert", p.name)
        } else {
            p.name.clone()
        };
        if p.tracks.is_empty() {
            s.mid = "Strg T: MIDI-Spur · Strg S: speichern".to_owned();
        }
    }
}

/// Draws the status bar into `rect`; returns whether the link was clicked.
pub fn show(ui: &mut Ui, rect: Rect, model: &StatusModel) -> bool {
    let painter = ui.painter().clone();
    let ppp = ui.pixels_per_point();
    px::fill(&painter, rect, 0.0, color32(color::SURFACE_1));
    let inner = rect.shrink2(vec2(space::SPACE_3, 0.0));
    let clip = painter.with_clip_rect(inner);

    let right = w::styled_galley(ui, &model.right, &typography::VALUE_S);
    let right_x = inner.right() - right.size().x;
    let right_rect = Rect::from_min_size(w::text_pos(inner, &right, right_x, ppp), right.size());
    let mut x = inner.left();
    let rec_text = model.left.strip_prefix("● ");
    let dot = if model.ok_dot {
        Some(color::STATE_PLAY)
    } else {
        rec_text.map(|_| model.left_color)
    };
    if let Some(dot) = dot {
        let c = pos2(x + ERROR_DOT / 2.0, inner.center().y);
        clip.circle_filled(c, ERROR_DOT / 2.0, color32(dot));
        x += ERROR_DOT + space::SPACE_2;
    }
    let left_limit = right_rect.left() - space::SPACE_4;
    let text_clip = clip.with_clip_rect(Rect::from_x_y_ranges(
        inner.left()..=left_limit,
        inner.y_range(),
    ));
    let mut parts: Vec<(Rect, &str)> = Vec::new();
    let mut link_rect = None;
    let link = model.link.as_deref().unwrap_or("");
    for (i, (text, col)) in [
        (model.left.as_str(), model.left_color),
        (link, color::ACCENT),
        (model.mid.as_str(), color::TEXT_3),
    ]
    .into_iter()
    .enumerate()
    {
        if text.is_empty() {
            continue;
        }
        let shown = if i == 0 {
            rec_text.unwrap_or(text)
        } else {
            text
        };
        let g = w::styled_galley(ui, shown, &typography::SMALL);
        let pos = w::text_pos(inner, &g, x, ppp);
        let r = Rect::from_min_size(pos, g.size());
        x += g.size().x + space::SPACE_4;
        text_clip.galley(pos, g, color32(col));
        if i == 1 {
            link_rect = Some(r.intersect(inner));
        } else {
            parts.push((r.intersect(inner), text));
        }
    }
    clip.galley(right_rect.min, right, color32(model.right_color));
    parts.push((right_rect, model.right.as_str()));
    for (i, (r, text)) in parts.into_iter().enumerate() {
        if text.is_empty() || r.width() <= 0.0 {
            continue;
        }
        let id = ui.scope_id().with(("komponist_status", i));
        let response = ui.interact(r, id, Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    }
    let Some(r) = link_rect.filter(|r| r.width() > 0.0) else {
        return false;
    };
    let id = ui.scope_id().with("komponist_status_link");
    let response = ui
        .interact(r, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| WidgetInfo::labeled(Role::Link, true, link));
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_text_matches_s01() {
        let config = StreamConfig::default();
        assert_eq!(
            fmt_output(&config),
            "PipeWire · 48 kHz · 256 Samples · 5.3 ms"
        );
        assert_eq!(fmt_rate(44_100), "44.1 kHz");
        assert_eq!(fmt_rate(96_000), "96 kHz");
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "Clip", "Clips"), "1 Clip");
        assert_eq!(plural(3, "Clip", "Clips"), "3 Clips");
    }
}
