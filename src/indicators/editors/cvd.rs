use super::config_message;
use crate::screen::dashboard::pane::Message;
use data::chart::kline::{
    Config, CvdConfig, IndicatorConfigs, SIZE_BUCKET_LABELS, SizeCvdConfig, SpotCvdConfig,
};
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, text},
};
pub(super) fn view(pane: pane_grid::Pane, cfg: Config) -> Element<'static, Message> {
    column![checkbox(cfg.cvd.show_change_bars).label("Show change bars").on_toggle(move |show_change_bars|
        config_message(pane,Config{cvd:CvdConfig{show_change_bars},..cfg})),
        text("Continuous cumulative delta across the loaded history. Change bars use an independent scale.").size(12),
        text("Directional buy and sell volume is required. Missing directional data creates gaps.").size(12),
    ].spacing(12).into()
}

pub(super) fn spot_view(pane: pane_grid::Pane, cfg: Config) -> Element<'static, Message> {
    let spot_cvd = cfg.indicator_configs.spot_cvd;
    column![
        checkbox(spot_cvd.delta_bars)
            .label("Delta bars")
            .on_toggle(move |delta_bars| config_message(
                pane,
                Config {
                    indicator_configs: IndicatorConfigs {
                        spot_cvd: SpotCvdConfig { delta_bars },
                        ..cfg.indicator_configs
                    },
                    ..cfg
                },
            )),
        text("Per-bar delta of futures (left, blue) and spot (right, orange) behind the lines: up = net buying, down = net selling. Each market on its own scale.")
            .size(12),
    ]
    .spacing(12)
    .into()
}

pub(super) fn size_view(pane: pane_grid::Pane, cfg: Config) -> Element<'static, Message> {
    let size_cvd = cfg.indicator_configs.size_cvd;
    let mut items = column![].spacing(12);
    for (i, label) in SIZE_BUCKET_LABELS.into_iter().enumerate() {
        items = items.push(
            checkbox(size_cvd.buckets[i])
                .label(label)
                .on_toggle(move |on| {
                    let mut buckets = size_cvd.buckets;
                    buckets[i] = on;
                    config_message(
                        pane,
                        Config {
                            indicator_configs: IndicatorConfigs {
                                size_cvd: SizeCvdConfig { buckets },
                                ..cfg.indicator_configs
                            },
                            ..cfg
                        },
                    )
                }),
        );
    }
    items
        .push(
            text("Size of one taker order: trades with the same timestamp and side are summed. The axis shows the largest enabled bucket; the other lines are rescaled to the same height.")
                .size(12),
        )
        .push(
            text("Needs raw trades: bars rebuilt from cached history older than 2 days carry no split and hold the lines flat.")
                .size(12),
        )
        .into()
}
