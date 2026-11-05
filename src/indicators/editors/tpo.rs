use super::config_message;
use crate::screen::dashboard::pane::Message;
use crate::style;
use crate::widget::{labeled_slider, pick_list};
use data::chart::{
    gex::GexLevelColor,
    kline::{
        Config as KlineConfig,
        tpo::{TpoAnchor, TpoConfig, TpoHistory},
    },
};
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, text},
};

pub(super) fn view<'a>(pane: pane_grid::Pane, cfg: KlineConfig) -> Element<'a, Message> {
    let tpo = cfg.tpo;
    let update = move |tpo| config_message(pane, KlineConfig { tpo, ..cfg });
    let width = labeled_slider(
        "Profile width",
        10u16..=80,
        u16::from(tpo.width_percent.clamp(10, 80)),
        move |width| {
            update(TpoConfig {
                width_percent: width as u8,
                ..tpo
            })
        },
        |value| format!("{value}%"),
        Some(1),
    );
    let value_area = labeled_slider(
        "Value area",
        5u16..=100,
        u16::from(tpo.value_area_percent.clamp(5, 100)),
        move |percent| {
            update(TpoConfig {
                value_area_percent: percent as u8,
                ..tpo
            })
        },
        |value| format!("{value}%"),
        Some(1),
    );
    column![
        text("Profile period (UTC)"),
        pick_list(
            TpoAnchor::PROFILE_PERIODS,
            Some(tpo.profile_anchor()),
            move |anchor| { update(TpoConfig { anchor, ..tpo }) }
        ),
        text("TPO interval · 30m"),
        width,
        value_area,
        text("Inside Value Area"),
        pick_list(
            GexLevelColor::ALL,
            Some(tpo.value_color),
            move |value_color| { update(TpoConfig { value_color, ..tpo }) }
        ),
        text("Outside Value Area"),
        pick_list(
            GexLevelColor::ALL,
            Some(tpo.outside_color),
            move |outside_color| {
                update(TpoConfig {
                    outside_color,
                    ..tpo
                })
            }
        ),
        text("TPOC color"),
        pick_list(GexLevelColor::ALL, Some(tpo.poc_color), move |poc_color| {
            update(TpoConfig { poc_color, ..tpo })
        }),
        checkbox(tpo.show_poc)
            .label("TPOC line and label")
            .on_toggle(move |show_poc| update(TpoConfig { show_poc, ..tpo })),
        checkbox(tpo.show_vah)
            .label("TVAH line and label")
            .on_toggle(move |show_vah| update(TpoConfig { show_vah, ..tpo })),
        checkbox(tpo.show_val)
            .label("TVAL line and label")
            .on_toggle(move |show_val| update(TpoConfig { show_val, ..tpo })),
        checkbox(tpo.extend_untested)
            .label("Extend untested levels")
            .on_toggle(move |extend_untested| update(TpoConfig {
                extend_untested,
                ..tpo
            })),
        text("TPO history"),
        pick_list(TpoHistory::ALL, Some(tpo.history), move |history| {
            update(TpoConfig { history, ..tpo })
        }),
        text("Uses 30m candle ranges on all time charts. Partial sessions are marked.")
            .size(style::text_size::SMALL),
    ]
    .spacing(6)
    .into()
}
