//! The compact channel strip (`sec({title:'Kanal', meta:'im Mixer zeigen', channel:1})` and its
//! markup): the compact fader with the scale, the meter, and in a column the level, the pan knob
//! with its value and M S (28 × 20).

use egui::{Rangef, Rect, Ui, pos2, vec2};
use komponist_model::Track;
use komponist_model::edits::{ChannelProp, TrackProp};
use komponist_model::format::{fmt_db, fmt_pan};
use komponist_tokens::{color, size, typography};
use komponist_ui::widgets::fader::{Fader, FaderSize, Layout as FaderLayout, db_text};
use komponist_ui::widgets::knob::Knob;
use komponist_ui::widgets::meter::{Meter, MeterState};
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind, TrackToggle};
use komponist_ui::widgets::value_menu;

use super::{Meta, access_label, frame, label, metrics as m, place};
use crate::actions::{Action, EditAction, UiAction};
use crate::meters::Meters;
use crate::ui_state::DockTab;
use crate::views::ViewCtx;
use crate::views::mixer::send_gesture;

/// The channel section of `track`.
pub(super) fn section(ui: &mut Ui, ctx: &mut ViewCtx<'_>, x: Rangef, y: &mut f32, track: &Track) {
    let t = ctx.lang;
    let (_, show) = frame(
        ui,
        x,
        y,
        t.inspector_section_channel(),
        Meta::Action(t.inspector_channel_show_in_mixer()),
        |ui, col| {
            let area = col.take(m::CHANNEL_HEIGHT);
            strip(ui, ctx, area, track);
        },
    );
    if show.is_some_and(|r| r.clicked()) {
        ctx.send(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    }
}

fn channel_edit(track: &Track, prop: ChannelProp) -> Action {
    Action::Edit(EditAction::SetTrackProp {
        tracks: vec![track.id],
        prop: TrackProp::Channel(prop),
    })
}

fn strip(ui: &mut Ui, ctx: &mut ViewCtx<'_>, area: Rect, track: &Track) {
    let t = ctx.lang;
    let ch = &track.channel;
    // Fader: compact thumb, scale on the left, as high as the meter.
    let thumb = FaderSize::Compact.thumb();
    let length = (area.height() - thumb.y).max(1.0);
    let fader_size = FaderLayout::desired_size(FaderSize::Compact, true, length);
    let fader_rect = Rect::from_min_size(area.min, fader_size);
    let mut db = ch.volume_db;
    let r = place(ui, fader_rect, "fader", |ui| {
        ui.add(
            Fader::new(&mut db, t.inspector_channel_volume())
                .compact()
                .scale(true)
                .track_length(length),
        )
    });
    send_gesture(ctx, &r, "undo-volume", || {
        channel_edit(track, ChannelProp::VolumeDb(db))
    });
    if value_menu::show(&r, || fmt_db(db)) {
        db = 0.0;
        ctx.send(channel_edit(track, ChannelProp::VolumeDb(db)));
    }

    // Meter: LED and bars over the whole height.
    let style = komponist_ui::widgets::meter::Style::MIXER;
    let meter_rect = Rect::from_min_size(
        pos2(fader_rect.right() + m::CHANNEL_GAP, area.top()),
        style.size(area.height()),
    );
    let mut silent = MeterState::new();
    let state = ctx.meters.track_mut(track.id).unwrap_or(&mut silent);
    let r = place(ui, meter_rect, "meter", |ui| {
        ui.add(
            Meter::new(state, Meters::NO_PEAKS, t.inspector_channel_meter()).length(area.height()),
        )
    });
    if r.clicked() {
        let all = ui.input(|i| i.modifiers.alt);
        ctx.send(Action::Ui(UiAction::ResetMeters {
            track: Some(track.id),
            all,
        }));
    }

    // Values column: level at the top, pan in the middle, M S at the bottom.
    let column = Rect::from_x_y_ranges(
        meter_rect.right() + m::CHANNEL_GAP..=area.right(),
        area.y_range(),
    );
    let small = typography::SMALL;
    let value = typography::VALUE_M;
    let key = Rect::from_min_size(column.min, vec2(column.width(), small.line_height));
    label(
        ui,
        key,
        t.inspector_channel_volume(),
        &small,
        None,
        color::TEXT_3,
    );
    let level = Rect::from_min_size(
        pos2(column.left(), key.bottom() + m::VALUE_KEY_GAP),
        vec2(column.width(), value.line_height),
    );
    let level_text = db_text(db);
    label(ui, level, &level_text, &value, None, color::TEXT_1);
    access_label(
        ui,
        key.union(level),
        "level",
        &format!("{} {level_text}", t.inspector_channel_volume()),
    );

    let toggles = Rect::from_min_max(
        pos2(column.left(), column.bottom() - m::TOGGLE[1]),
        column.max,
    );
    let pan_block = small.line_height + m::PAN_KEY_GAP + value.line_height;
    let mid = (level.bottom() + toggles.top()) / 2.0;
    let knob = Rect::from_center_size(
        pos2(column.left() + size::KNOB / 2.0, mid),
        vec2(size::KNOB, size::KNOB),
    );
    let mut pan = ch.pan;
    let r = place(ui, knob, "pan", |ui| {
        ui.add(Knob::pan(&mut pan, t.inspector_channel_pan()))
    });
    send_gesture(ctx, &r, "undo-pan", || {
        channel_edit(track, ChannelProp::Pan(pan))
    });
    if value_menu::show(&r, || fmt_pan(pan)) {
        pan = 0.0;
        ctx.send(channel_edit(track, ChannelProp::Pan(pan)));
    }
    let text_left = knob.right() + m::PAN_GAP;
    let pan_key = Rect::from_min_size(
        pos2(text_left, mid - pan_block / 2.0),
        vec2(column.right() - text_left, small.line_height),
    );
    label(
        ui,
        pan_key,
        t.inspector_channel_pan(),
        &small,
        None,
        color::TEXT_3,
    );
    let pan_value = Rect::from_min_size(
        pos2(text_left, pan_key.bottom() + m::PAN_KEY_GAP),
        vec2(pan_key.width(), value.line_height),
    );
    label(ui, pan_value, &fmt_pan(pan), &value, None, color::TEXT_1);

    let any_solo = ctx.project.any_solo();
    let [tw, th] = m::TOGGLE;
    for (i, kind) in [ToggleKind::Mute, ToggleKind::Solo].into_iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(
                toggles.left() + i as f32 * (tw + m::TOGGLE_GAP),
                toggles.top(),
            ),
            vec2(tw, th),
        );
        let on = match kind {
            ToggleKind::Mute => ch.mute,
            _ => ch.solo,
        };
        let shown = place(ui, r, ("toggle", kind.letter()), |ui| {
            TrackToggle::new(kind, on)
                .implied(kind == ToggleKind::Mute && any_solo && !ch.solo)
                .size(vec2(tw, th))
                .show(ui)
        });
        match shown.event {
            Some(ToggleEvent::Set {
                exclusive: true, ..
            }) => ctx.send(Action::Edit(EditAction::SetExclusive {
                track: track.id,
                flag: komponist_model::edits::ExclusiveFlag::Solo,
            })),
            Some(ToggleEvent::Set { value, .. }) => {
                let prop = match kind {
                    ToggleKind::Mute => ChannelProp::Mute(value),
                    _ => ChannelProp::Solo(value),
                };
                ctx.send(channel_edit(track, prop));
            }
            _ => {}
        }
    }
}
