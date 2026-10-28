use super::config_message;
use crate::screen::dashboard::pane::Message;
use crate::widget::{labeled_slider, pick_list};
use data::chart::kline::{Config as KlineConfig, SessionProfileInterval, VwapConfig};
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, row, text},
};

pub(super) fn view<'a>(pane: pane_grid::Pane, cfg: KlineConfig) -> Element<'a, Message> {
    let vwap = cfg.vwap;
    let set = move |vwap: VwapConfig| config_message(pane, KlineConfig { vwap, ..cfg });

    let session = checkbox(vwap.session)
        .label("Session VWAP (from trades)")
        .on_toggle(move |session| set(VwapConfig { session, ..vwap }));
    let anchor = pick_list(
        SessionProfileInterval::ALL,
        Some(vwap.anchor),
        move |anchor| set(VwapConfig { anchor, ..vwap }),
    );
    let weekly = checkbox(vwap.weekly)
        .label("Weekly VWAP (from Monday UTC)")
        .on_toggle(move |weekly| set(VwapConfig { weekly, ..vwap }));
    let monthly = checkbox(vwap.monthly)
        .label("Monthly VWAP")
        .on_toggle(move |monthly| set(VwapConfig { monthly, ..vwap }));

    let width = labeled_slider(
        "Line width",
        0.5..=5.0,
        vwap.line_width,
        move |line_width| set(VwapConfig { line_width, ..vwap }),
        |v| format!("{v:.1}px"),
        Some(0.1),
    );
    let multiplier = labeled_slider(
        "Band multiplier",
        0.25..=3.0,
        vwap.band_multiplier,
        move |band_multiplier| {
            set(VwapConfig {
                band_multiplier,
                ..vwap
            })
        },
        |v| format!("{v:.2}σ"),
        Some(0.25),
    );
    let bands = checkbox(vwap.show_bands)
        .label("Standard-deviation bands")
        .on_toggle(move |show_bands| set(VwapConfig { show_bands, ..vwap }));
    let labels = checkbox(vwap.show_labels)
        .label("Labels")
        .on_toggle(move |show_labels| {
            set(VwapConfig {
                show_labels,
                ..vwap
            })
        });

    column![
        session,
        row![text("Session anchor").size(12), anchor].spacing(8),
        weekly,
        monthly,
        text("Weekly and monthly lines use hourly candles (typical price × volume), so they work on any timeframe.").size(12),
        width,
        multiplier,
        bands,
        labels,
    ]
    .spacing(6)
    .into()
}
