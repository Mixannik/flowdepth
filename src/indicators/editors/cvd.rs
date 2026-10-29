use super::config_message;
use crate::screen::dashboard::pane::Message;
use data::chart::kline::{Config, CvdConfig, IndicatorConfigs, SpotCvdConfig};
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
        text("Per-bar delta of the chart's own market behind the lines, on its own scale.")
            .size(12),
    ]
    .spacing(12)
    .into()
}
