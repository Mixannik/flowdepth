//! Editors for the reference-level overlays: previous periods and naked session levels.
use super::config_message;
use crate::screen::dashboard::pane::Message;
use crate::widget::labeled_slider;
use data::chart::kline::{Config, IndicatorConfigs, NakedLevelsConfig, PeriodLevelsConfig};
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, text},
};

pub(super) fn period_levels<'a>(pane: pane_grid::Pane, cfg: Config) -> Element<'a, Message> {
    let levels = cfg.indicator_configs.period_levels;
    let set = move |period_levels: PeriodLevelsConfig| {
        config_message(
            pane,
            Config {
                indicator_configs: IndicatorConfigs {
                    period_levels,
                    ..cfg.indicator_configs
                },
                ..cfg
            },
        )
    };
    column![
        checkbox(levels.day)
            .label("Previous day high / low (PDH, PDL)")
            .on_toggle(move |day| set(PeriodLevelsConfig { day, ..levels })),
        checkbox(levels.week)
            .label("Previous week high / low (PWH, PWL)")
            .on_toggle(move |week| set(PeriodLevelsConfig { week, ..levels })),
        checkbox(levels.month)
            .label("Previous month high / low (PMH, PML)")
            .on_toggle(move |month| set(PeriodLevelsConfig { month, ..levels })),
        checkbox(levels.labels)
            .label("Labels")
            .on_toggle(move |labels| set(PeriodLevelsConfig { labels, ..levels })),
        labeled_slider(
            "Opacity",
            0.1..=1.0,
            levels.opacity,
            move |opacity| set(PeriodLevelsConfig { opacity, ..levels }),
            |v| format!("{:.0}%", v * 100.0),
            Some(0.05),
        ),
        text("UTC periods; the week starts on Monday. Built from daily candles.").size(12),
    ]
    .spacing(6)
    .into()
}

pub(super) fn naked_levels<'a>(pane: pane_grid::Pane, cfg: Config) -> Element<'a, Message> {
    let levels = cfg.indicator_configs.naked_levels;
    let set = move |naked_levels: NakedLevelsConfig| {
        config_message(
            pane,
            Config {
                indicator_configs: IndicatorConfigs {
                    naked_levels,
                    ..cfg.indicator_configs
                },
                ..cfg
            },
        )
    };
    column![
        checkbox(levels.weekly)
            .label("Weekly nPOC, nVAH, nVAL (last 2 weeks)")
            .on_toggle(move |weekly| set(NakedLevelsConfig { weekly, ..levels })),
        checkbox(levels.daily_poc)
            .label("Daily nPOC (last 3 days)")
            .on_toggle(move |daily_poc| set(NakedLevelsConfig {
                daily_poc,
                ..levels
            })),
        checkbox(levels.labels)
            .label("Labels")
            .on_toggle(move |labels| set(NakedLevelsConfig { labels, ..levels })),
        text("A level disappears once price trades through it.").size(12),
    ]
    .spacing(6)
    .into()
}
