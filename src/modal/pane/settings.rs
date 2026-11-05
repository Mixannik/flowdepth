use crate::chart::comparison::ComparisonChart;
use crate::screen::dashboard::pane::{Event, Message};
use crate::screen::dashboard::panel::timeandsales;
use crate::split_column;
use crate::widget::{classic_slider_row, labeled_slider};
use crate::{style, tooltip, widget::scrollable_content};

use data::chart::heatmap::HeatmapStudy;
use data::chart::kline::FootprintStudy;
use data::chart::{
    KlineChartKind,
    heatmap::{self, CoalesceKind},
    kline::ClusterKind,
};
use data::layout::pane::VisualConfig;
use data::panel::ladder;
use data::panel::timeandsales::{StackedBar, StackedBarRatio, TapeFeed};
use data::util::format_with_commas;

use crate::widget::pick_list;
use iced::widget::{checkbox, space};
use iced::{
    Alignment, Element, Length,
    widget::{
        button, column, container, pane_grid, radio, row, slider, text,
        tooltip::Position as TooltipPosition,
    },
};
use std::time::Duration;

fn cfg_view_container<'a, T>(max_width: u32, content: T) -> Element<'a, Message>
where
    T: Into<Element<'a, Message>>,
{
    container(scrollable_content(content))
        .width(Length::Shrink.max(max_width))
        .padding(28)
        .style(style::chart_modal)
        .into()
}

fn iceberg_cfg_controls<'a>(cfg: heatmap::Config, pane: pane_grid::Pane) -> Element<'a, Message> {
    let update = move |detector: data::orderflow::iceberg::IcebergDetectorConfig| {
        Message::VisualConfigChanged(
            pane,
            VisualConfig::Heatmap(heatmap::Config {
                iceberg_detector: detector,
                ..cfg
            }),
            false,
        )
    };
    let detector = cfg.iceberg_detector;
    let mut controls = column![
        text("Order flow").size(crate::style::text_size::SECTION),
        checkbox(detector.enabled)
            .label("Possible iceberg markers (Binance Linear)")
            .on_toggle(
                move |enabled| update(data::orderflow::iceberg::IcebergDetectorConfig {
                    enabled,
                    ..detector
                })
            ),
        text("Uses raw trades and repeated L2 replenishment; disabled by default.")
            .size(crate::style::text_size::SMALL),
    ]
    .spacing(8);

    if detector.enabled {
        controls = controls
            .push(classic_slider_row(
                text("Distance from touch"),
                slider(
                    0..=10,
                    detector.max_distance_from_touch_ticks,
                    move |value| {
                        update(data::orderflow::iceberg::IcebergDetectorConfig {
                            max_distance_from_touch_ticks: value,
                            ..detector
                        })
                    },
                )
                .into(),
                Some(text(format!(
                    "{} ticks",
                    detector.max_distance_from_touch_ticks
                ))),
            ))
            .push(classic_slider_row(
                text("Reorder window"),
                slider(50..=500, detector.reorder_window_ms, move |value| {
                    update(data::orderflow::iceberg::IcebergDetectorConfig {
                        reorder_window_ms: value,
                        ..detector
                    })
                })
                .step(10u32)
                .into(),
                Some(text(format!("{} ms", detector.reorder_window_ms))),
            ))
            .push(classic_slider_row(
                text("Idle timeout"),
                slider(
                    1_000..=15_000,
                    detector.episode_idle_timeout_ms,
                    move |value| {
                        update(data::orderflow::iceberg::IcebergDetectorConfig {
                            episode_idle_timeout_ms: value,
                            ..detector
                        })
                    },
                )
                .step(500u32)
                .into(),
                Some(text(format!("{} ms", detector.episode_idle_timeout_ms))),
            ))
            .push(classic_slider_row(
                text("Max episode"),
                slider(
                    5_000..=60_000,
                    detector.episode_max_duration_ms,
                    move |value| {
                        update(data::orderflow::iceberg::IcebergDetectorConfig {
                            episode_max_duration_ms: value,
                            ..detector
                        })
                    },
                )
                .step(1_000u32)
                .into(),
                Some(text(format!(
                    "{} s",
                    detector.episode_max_duration_ms / 1_000
                ))),
            ))
            .push(classic_slider_row(
                text("Minimum refill cycles"),
                slider(1..=10, detector.minimum_refill_count, move |value| {
                    update(data::orderflow::iceberg::IcebergDetectorConfig {
                        minimum_refill_count: value,
                        ..detector
                    })
                })
                .into(),
                Some(text(detector.minimum_refill_count)),
            ))
            .push(classic_slider_row(
                text("Executed / displayed"),
                slider(
                    1.0..=10.0,
                    detector.minimum_executed_to_displayed,
                    move |value| {
                        update(data::orderflow::iceberg::IcebergDetectorConfig {
                            minimum_executed_to_displayed: value,
                            ..detector
                        })
                    },
                )
                .step(0.1)
                .into(),
                Some(text(format!(
                    "{:.1}×",
                    detector.minimum_executed_to_displayed
                ))),
            ))
            .push(classic_slider_row(
                text("Minimum refill ratio"),
                slider(0.1..=1.5, detector.minimum_refill_ratio, move |value| {
                    update(data::orderflow::iceberg::IcebergDetectorConfig {
                        minimum_refill_ratio: value,
                        ..detector
                    })
                })
                .step(0.05)
                .into(),
                Some(text(format!(
                    "{:.0}%",
                    detector.minimum_refill_ratio * 100.0
                ))),
            ))
            .push(classic_slider_row(
                text("Maximum adverse move"),
                slider(0..=10, detector.maximum_adverse_ticks, move |value| {
                    update(data::orderflow::iceberg::IcebergDetectorConfig {
                        maximum_adverse_ticks: value,
                        ..detector
                    })
                })
                .into(),
                Some(text(format!("{} ticks", detector.maximum_adverse_ticks))),
            ))
            .push(classic_slider_row(
                text("Minimum score"),
                slider(50..=100, detector.minimum_score, move |value| {
                    update(data::orderflow::iceberg::IcebergDetectorConfig {
                        minimum_score: value,
                        ..detector
                    })
                })
                .into(),
                Some(text(format!("{} / 100", detector.minimum_score))),
            ))
            .push(classic_slider_row(
                text("Marker retention"),
                slider(30..=900, detector.retention_seconds, move |value| {
                    update(data::orderflow::iceberg::IcebergDetectorConfig {
                        retention_seconds: value,
                        ..detector
                    })
                })
                .step(30u32)
                .into(),
                Some(text(format!("{} s", detector.retention_seconds))),
            ))
            .push(
                checkbox(detector.show_weak_candidates)
                    .label("Show weak candidates")
                    .on_toggle(move |show_weak_candidates| {
                        update(data::orderflow::iceberg::IcebergDetectorConfig {
                            show_weak_candidates,
                            ..detector
                        })
                    }),
            );
    }

    controls.into()
}

pub fn heatmap_cfg_view<'a>(
    cfg: heatmap::Config,
    pane: pane_grid::Pane,
    study_config: &'a study::Configurator<HeatmapStudy>,
    studies: &'a [HeatmapStudy],
    basis: data::chart::Basis,
) -> Element<'a, Message> {
    let trade_size_slider = {
        let filter = cfg.trade_size_filter;
        labeled_slider(
            "Trade",
            0.0..=50000.0,
            filter,
            move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_size_filter: value,
                        ..cfg
                    }),
                    false,
                )
            },
            |value| format!(">${}", format_with_commas(*value as f64)),
            Some(500.0),
        )
    };

    let order_size_slider = {
        let filter = cfg.order_size_filter;
        labeled_slider(
            "Order",
            0.0..=500_000.0,
            filter,
            move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        order_size_filter: value,
                        ..cfg
                    }),
                    false,
                )
            },
            |value| format!(">${}", format_with_commas(*value as f64)),
            Some(5000.0),
        )
    };

    let circle_scaling_slider = cfg.trade_size_scale.map(|radius_scale| {
        classic_slider_row(
            text("Circle radius scaling"),
            slider(10..=200, radius_scale, move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_size_scale: Some(value),
                        ..cfg
                    }),
                    false,
                )
            })
            .step(10)
            .into(),
            Some(text(format!("{}%", radius_scale)).size(crate::style::text_size::EMPHASIS)),
        )
    });

    let coalescer_cfg: Option<Element<_>> = if let Some(coalescing) = cfg.coalescing {
        let threshold_pct = coalescing.threshold();

        let coalescer_kinds = {
            let average = radio(
                "Average",
                CoalesceKind::Average(threshold_pct),
                Some(coalescing),
                move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Heatmap(heatmap::Config {
                            coalescing: Some(value),
                            ..cfg
                        }),
                        false,
                    )
                },
            )
            .spacing(4);

            let first = radio(
                "First",
                CoalesceKind::First(threshold_pct),
                Some(coalescing),
                move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Heatmap(heatmap::Config {
                            coalescing: Some(value),
                            ..cfg
                        }),
                        false,
                    )
                },
            )
            .spacing(4);

            let max = radio(
                "Max",
                CoalesceKind::Max(threshold_pct),
                Some(coalescing),
                move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Heatmap(heatmap::Config {
                            coalescing: Some(value),
                            ..cfg
                        }),
                        false,
                    )
                },
            )
            .spacing(4);

            row![
                text("Merge method: "),
                row![average, first, max].spacing(12)
            ]
            .spacing(12)
        };

        let threshold_slider = classic_slider_row(
            text("Size similarity"),
            slider(0.05..=0.8, threshold_pct, move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        coalescing: Some(coalescing.with_threshold(value)),
                        ..cfg
                    }),
                    false,
                )
            })
            .step(0.05_f32)
            .into(),
            Some(
                text(format!("{:.0}%", threshold_pct * 100.0))
                    .size(crate::style::text_size::EMPHASIS),
            ),
        );

        Some(
            container(column![coalescer_kinds, threshold_slider].spacing(8))
                .style(style::modal_container)
                .padding(8)
                .into(),
        )
    } else {
        None
    };

    let size_filters_column = column![
        text("Size filters").size(crate::style::text_size::SECTION),
        column![trade_size_slider, order_size_slider].spacing(8),
    ]
    .spacing(8);

    let noise_filters_column = {
        let merge_checkbox = checkbox(cfg.coalescing.is_some())
            .label("Merge orders if sizes are similar")
            .on_toggle(move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        coalescing: if value {
                            Some(CoalesceKind::Average(0.15))
                        } else {
                            None
                        },
                        ..cfg
                    }),
                    false,
                )
            });

        let mut col = column![
            text("Noise filters").size(crate::style::text_size::SECTION),
            merge_checkbox
        ]
        .spacing(8);
        if let Some(c) = coalescer_cfg {
            col = col.push(c);
        }
        col
    };

    let trade_viz_column = {
        let dyn_checkbox = checkbox(cfg.trade_size_scale.is_some())
            .label("Dynamic circle radius")
            .on_toggle(move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_size_scale: if value { Some(100) } else { None },
                        ..cfg
                    }),
                    false,
                )
            });

        let bubbles_3d_checkbox = checkbox(cfg.trade_bubbles_3d)
            .label("3D trade bubbles")
            .on_toggle(move |trade_bubbles_3d| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_bubbles_3d,
                        ..cfg
                    }),
                    false,
                )
            });

        let mut col = column![
            text("Trade visualization").size(crate::style::text_size::SECTION),
            dyn_checkbox,
            bubbles_3d_checkbox
        ]
        .spacing(8);
        if let Some(slider) = circle_scaling_slider {
            col = col.push(slider);
        }
        col
    };

    let orderflow_column = iceberg_cfg_controls(cfg, pane);

    let study_cfg = study_config.view(studies, basis).map(move |msg| {
        Message::PaneEvent(
            pane,
            Event::StudyConfigurator(study::StudyMessage::Heatmap(msg)),
        )
    });

    let content = split_column![
        size_filters_column,
        noise_filters_column,
        trade_viz_column,
        orderflow_column,
        column![text("Studies").size(crate::style::text_size::SECTION), study_cfg].spacing(8),
        row![
            space::horizontal(),
            sync_all_button(pane, VisualConfig::Heatmap(cfg))
        ]
        ; spacing = 12, align_x = Alignment::Start
    ];

    cfg_view_container(360, content)
}

pub fn heatmap_shader_cfg_view<'a>(
    cfg: heatmap::Config,
    pane: pane_grid::Pane,
    study_config: &'a study::Configurator<HeatmapStudy>,
    studies: &'a [HeatmapStudy],
    basis: data::chart::Basis,
) -> Element<'a, Message> {
    let trade_size_slider = {
        let filter = cfg.trade_size_filter;
        labeled_slider(
            "Trade",
            0.0..=50000.0,
            filter,
            move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_size_filter: value,
                        ..cfg
                    }),
                    false,
                )
            },
            |value| format!(">${}", format_with_commas(*value as f64)),
            Some(500.0),
        )
    };

    let order_size_slider = {
        let filter = cfg.order_size_filter;
        labeled_slider(
            "Order",
            0.0..=500_000.0,
            filter,
            move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        order_size_filter: value,
                        ..cfg
                    }),
                    false,
                )
            },
            |value| format!(">${}", format_with_commas(*value as f64)),
            Some(5000.0),
        )
    };

    let circle_scaling_slider = cfg.trade_size_scale.map(|radius_scale| {
        classic_slider_row(
            text("Circle radius scaling"),
            slider(10..=200, radius_scale, move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_size_scale: Some(value),
                        ..cfg
                    }),
                    false,
                )
            })
            .step(10)
            .into(),
            Some(text(format!("{}%", radius_scale)).size(crate::style::text_size::EMPHASIS)),
        )
    });

    let size_filters_column = column![
        text("Size filters").size(crate::style::text_size::SECTION),
        column![trade_size_slider, order_size_slider].spacing(8),
    ]
    .spacing(8);

    let trade_viz_column = {
        let dyn_checkbox = checkbox(cfg.trade_size_scale.is_some())
            .label("Dynamic circle radius")
            .on_toggle(move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_size_scale: if value { Some(100) } else { None },
                        ..cfg
                    }),
                    false,
                )
            });

        let bubbles_3d_checkbox = checkbox(cfg.trade_bubbles_3d)
            .label("3D trade bubbles")
            .on_toggle(move |trade_bubbles_3d| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Heatmap(heatmap::Config {
                        trade_bubbles_3d,
                        ..cfg
                    }),
                    false,
                )
            });

        let mut col = column![
            text("Trade visualization").size(crate::style::text_size::SECTION),
            dyn_checkbox,
            bubbles_3d_checkbox
        ]
        .spacing(8);
        if let Some(slider) = circle_scaling_slider {
            col = col.push(slider);
        }
        col
    };

    let orderflow_column = iceberg_cfg_controls(cfg, pane);

    let study_cfg = study_config.view(studies, basis).map(move |msg| {
        Message::PaneEvent(
            pane,
            Event::StudyConfigurator(study::StudyMessage::Heatmap(msg)),
        )
    });

    let content = split_column![
        size_filters_column,
        trade_viz_column,
        orderflow_column,
        column![text("Studies").size(crate::style::text_size::SECTION), study_cfg].spacing(8),
        row![
            space::horizontal(),
            sync_all_button(pane, VisualConfig::Heatmap(cfg))
        ]
        ; spacing = 12, align_x = Alignment::Start
    ];

    cfg_view_container(360, content)
}

pub fn timesales_cfg_view<'a>(
    cfg: timeandsales::Config,
    pane: pane_grid::Pane,
) -> Element<'a, Message> {
    let feed_column = {
        let feed_picklist = pick_list(TapeFeed::ALL, Some(cfg.feed), move |feed| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::TimeAndSales(timeandsales::Config { feed, ..cfg }),
                false,
            )
        });

        let mut col = column![
            text("Feed").size(crate::style::text_size::SECTION),
            feed_picklist
        ]
        .spacing(8);

        match cfg.feed {
            TapeFeed::Trades => {
                let merge = checkbox(cfg.merge_taker_orders)
                    .label("Merge fills of one market order")
                    .on_toggle(move |merge_taker_orders| {
                        Message::VisualConfigChanged(
                            pane,
                            VisualConfig::TimeAndSales(timeandsales::Config {
                                merge_taker_orders,
                                ..cfg
                            }),
                            false,
                        )
                    });
                col = col.push(merge);
            }
            TapeFeed::Liquidations => {
                col = col.push(
                    text("Binance perpetuals only; Binance sends at most one per second")
                        .size(crate::style::text_size::BODY),
                );
            }
        }

        col
    };

    let trade_size_column = {
        let filter = cfg.trade_size_filter;
        let slider = labeled_slider(
            "Trade",
            0.0..=1_000_000.0,
            filter,
            move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::TimeAndSales(timeandsales::Config {
                        trade_size_filter: value,
                        ..cfg
                    }),
                    false,
                )
            },
            |value| format!(">${}", format_with_commas(*value as f64)),
            Some(5_000.0),
        );

        column![
            text("Size filter").size(crate::style::text_size::SECTION),
            slider
        ]
        .spacing(8)
    };

    let retention_minutes = (cfg.trade_retention.as_secs_f32() / 60.0).max(1.0);
    let retention_slider = {
        let slider_ui = slider(1.0..=240.0, retention_minutes, move |new_minutes| {
            let mins = new_minutes.round().max(1.0) as u64;
            Message::VisualConfigChanged(
                pane,
                VisualConfig::TimeAndSales(timeandsales::Config {
                    trade_retention: Duration::from_secs(mins * 60),
                    ..cfg
                }),
                false,
            )
        })
        .step(1.0_f32);

        classic_slider_row(
            text("Keep trades for"),
            slider_ui.into(),
            Some(
                text(format!("≈ {} min", retention_minutes.round() as u64))
                    .size(crate::style::text_size::EMPHASIS),
            ),
        )
    };

    let history_column = column![
        row![
            text("History").size(crate::style::text_size::SECTION),
            tooltip(
                button("i").style(style::button::info),
                Some("Affects the stacked bar, colors and how much you can scroll down"),
                TooltipPosition::Top,
            )
        ]
        .spacing(4)
        .align_y(Alignment::Center),
        retention_slider
    ]
    .spacing(8);

    let stacked_bar: Element<_> = {
        let is_shown = cfg.stacked_bar.is_some();

        let enable_checkbox = checkbox(is_shown).label("Show stacked bar").on_toggle({
            move |value| {
                let current_ratio = cfg.stacked_bar.map(|h| h.ratio()).unwrap_or_default();
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::TimeAndSales(timeandsales::Config {
                        stacked_bar: if value {
                            Some(StackedBar::Compact(current_ratio))
                        } else {
                            None
                        },
                        ..cfg
                    }),
                    false,
                )
            }
        });

        let controls: Option<Element<_>> = cfg.stacked_bar.map(|hist| {
            let ratio = hist.ratio();
            let is_compact = matches!(hist, StackedBar::Compact(_));

            let compact = radio("Compact", true, Some(is_compact), {
                move |_v| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::TimeAndSales(timeandsales::Config {
                            stacked_bar: Some(StackedBar::Compact(ratio)),
                            ..cfg
                        }),
                        false,
                    )
                }
            })
            .spacing(4);

            let full = radio("Full", false, Some(is_compact), {
                move |_v| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::TimeAndSales(timeandsales::Config {
                            stacked_bar: Some(StackedBar::Full(ratio)),
                            ..cfg
                        }),
                        false,
                    )
                }
            })
            .spacing(4);

            let metric_picklist = pick_list(StackedBarRatio::ALL, Some(ratio), move |new_ratio| {
                let new_hist = Some(match cfg.stacked_bar {
                    Some(StackedBar::Full(_)) => StackedBar::Full(new_ratio),
                    _ => StackedBar::Compact(new_ratio),
                });
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::TimeAndSales(timeandsales::Config {
                        stacked_bar: new_hist,
                        ..cfg
                    }),
                    false,
                )
            });

            column![
                iced::widget::rule::horizontal(1),
                text("Mode").size(crate::style::text_size::BODY),
                row![compact, full].spacing(12),
                text("Metric").size(crate::style::text_size::BODY),
                metric_picklist,
            ]
            .spacing(8)
            .into()
        });

        let mut inner = column![enable_checkbox]
            .width(Length::Fill)
            .padding(4)
            .spacing(8);

        if let Some(ctrls) = controls {
            inner = inner.push(ctrls);
        }

        container(inner)
            .style(style::modal_container)
            .padding(8)
            .into()
    };

    let content = split_column![
        feed_column,
        trade_size_column,
        history_column,
        stacked_bar,
        row![space::horizontal(), sync_all_button(pane, VisualConfig::TimeAndSales(cfg))],
        ; spacing = 12, align_x = Alignment::Start
    ];

    cfg_view_container(320, content)
}

pub fn comparison_cfg_view<'a>(
    pane: pane_grid::Pane,
    chart: &'a ComparisonChart,
) -> Element<'a, Message> {
    let series = &chart.series;
    let series_editor = &chart.series_editor;

    let content = column![series_editor.view(series).map(move |msg| {
        Message::PaneEvent(
            pane,
            Event::ComparisonChartInteraction(crate::chart::comparison::Message::Editor(msg)),
        )
    })];

    cfg_view_container(320, content)
}

pub fn gex_cfg_view<'a>(
    cfg: data::chart::gex::Config,
    pane: pane_grid::Pane,
) -> Element<'a, Message> {
    use data::chart::gex::{GexExpiryFilter, GexSignModel};

    let model = pick_list(GexSignModel::ALL, Some(cfg.sign_model), move |sign_model| {
        Message::VisualConfigChanged(
            pane,
            VisualConfig::Gex(data::chart::gex::Config { sign_model, ..cfg }),
            false,
        )
    });
    let expiry = pick_list(
        GexExpiryFilter::ALL,
        Some(cfg.expiry_filter),
        move |expiry_filter| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Gex(data::chart::gex::Config {
                    expiry_filter,
                    ..cfg
                }),
                false,
            )
        },
    );
    let max_strikes = slider(5..=100, cfg.max_visible_strikes as u32, move |value| {
        Message::VisualConfigChanged(
            pane,
            VisualConfig::Gex(data::chart::gex::Config {
                max_visible_strikes: value as usize,
                ..cfg
            }),
            false,
        )
    });
    let price_range = slider(5.0..=50.0, cfg.price_range_percent, move |value| {
        Message::VisualConfigChanged(
            pane,
            VisualConfig::Gex(data::chart::gex::Config {
                price_range_percent: value,
                ..cfg
            }),
            false,
        )
    });
    let min_oi = slider(0.0..=1_000.0, cfg.min_open_interest, move |value| {
        Message::VisualConfigChanged(
            pane,
            VisualConfig::Gex(data::chart::gex::Config {
                min_open_interest: value,
                ..cfg
            }),
            false,
        )
    });
    let min_gex = slider(0.0..=10_000_000.0, cfg.min_absolute_gex, move |value| {
        Message::VisualConfigChanged(
            pane,
            VisualConfig::Gex(data::chart::gex::Config {
                min_absolute_gex: value,
                ..cfg
            }),
            false,
        )
    });
    let liquidity_depth = slider(5.0..=100.0, cfg.liquidity_depth_bps, move |value| {
        Message::VisualConfigChanged(
            pane,
            VisualConfig::Gex(data::chart::gex::Config {
                liquidity_depth_bps: value,
                ..cfg
            }),
            false,
        )
    });
    let toggle =
        |label: &'static str, current: bool, update: fn(&mut data::chart::gex::Config, bool)| {
            checkbox(current).label(label).on_toggle(move |value| {
                let mut next = cfg;
                update(&mut next, value);
                Message::VisualConfigChanged(pane, VisualConfig::Gex(next), false)
            })
        };
    let content = column![
        text("GEX model").size(crate::style::text_size::SECTION),
        model,
        text("Expiry filter").size(crate::style::text_size::SECTION),
        expiry,
        text(format!("Visible strikes: {}", cfg.max_visible_strikes)),
        max_strikes,
        text(format!("Price range: ±{:.0}%", cfg.price_range_percent)),
        price_range,
        text(format!(
            "Minimum OI: {:.1} {}",
            cfg.min_open_interest, "BTC/ETH/SOL"
        )),
        min_oi,
        text(format!(
            "Minimum absolute GEX: {}",
            crate::widget::chart::gex::format_exposure(cfg.min_absolute_gex)
        )),
        min_gex,
        text("Analytics panels").size(crate::style::text_size::SECTION),
        toggle(
            "Intrinsic pressure",
            cfg.show_intrinsic_stress_panel,
            |c, v| c.show_intrinsic_stress_panel = v
        ),
        toggle("Gamma vs Vega", cfg.show_gamma_vega_panel, |c, v| {
            c.show_gamma_vega_panel = v
        }),
        toggle(
            "Liquidity impact",
            cfg.show_gamma_liquidity_panel,
            |c, v| c.show_gamma_liquidity_panel = v
        ),
        text(format!(
            "Liquidity depth range: ±{:.0} bps",
            cfg.liquidity_depth_bps
        )),
        liquidity_depth,
        toggle(
            "Follow selected/link-group ticker",
            cfg.liquidity_reference_follow_link_group,
            |c, v| c.liquidity_reference_follow_link_group = v
        ),
        toggle("Show call GEX", cfg.show_call_gex, |c, v| c.show_call_gex =
            v),
        toggle("Show put GEX", cfg.show_put_gex, |c, v| c.show_put_gex = v),
        toggle("Show net GEX", cfg.show_net_gex, |c, v| c.show_net_gex = v),
        toggle("Show absolute gamma", cfg.show_absolute_gamma, |c, v| c
            .show_absolute_gamma =
            v),
        toggle("Show source spot", cfg.show_current_price, |c, v| c
            .show_current_price =
            v),
        toggle("Show Call Wall", cfg.show_call_wall, |c, v| c
            .show_call_wall =
            v),
        toggle("Show Put Wall", cfg.show_put_wall, |c, v| c.show_put_wall =
            v),
        toggle("Show Gamma Flip", cfg.show_gamma_flip, |c, v| c
            .show_gamma_flip =
            v),
        toggle("Show summary", cfg.show_summary, |c, v| c.show_summary = v),
        text("Header fields").size(crate::style::text_size::SECTION),
        toggle("Net GEX", cfg.show_header_net_gex, |c, v| c
            .show_header_net_gex =
            v),
        toggle("Absolute GEX", cfg.show_header_absolute_gex, |c, v| c
            .show_header_absolute_gex =
            v),
        toggle("Gamma Flip", cfg.show_header_gamma_flip, |c, v| c
            .show_header_gamma_flip =
            v),
        toggle("Call Wall", cfg.show_header_call_wall, |c, v| c
            .show_header_call_wall =
            v),
        toggle("Put Wall", cfg.show_header_put_wall, |c, v| c
            .show_header_put_wall =
            v),
        toggle("Expiry", cfg.show_header_expiry, |c, v| c
            .show_header_expiry =
            v),
        toggle("Freshness", cfg.show_header_freshness, |c, v| c
            .show_header_freshness =
            v),
        toggle("Snapshot", cfg.show_header_snapshot, |c, v| c
            .show_header_snapshot =
            v),
        toggle("Model", cfg.show_header_model, |c, v| c.show_header_model =
            v),
        toggle("Derive flow (30m)", cfg.show_header_derive_flow, |c, v| c
            .show_header_derive_flow =
            v),
        row![
            space::horizontal(),
            sync_all_button(pane, VisualConfig::Gex(cfg))
        ]
    ]
    .spacing(8);
    cfg_view_container(360, content)
}

pub fn kline_cfg_view<'a>(
    study_config: &'a study::Configurator<FootprintStudy>,
    cfg: data::chart::kline::Config,
    kind: &'a KlineChartKind,
    pane: pane_grid::Pane,
    basis: data::chart::Basis,
) -> Element<'a, Message> {
    let display_readout_section = {
        let data_labels_checkbox = tooltip(
            checkbox(cfg.data_labels_always_visible)
                .label("Keep latest label visible")
                .on_toggle(move |value| {
                    Message::VisualConfigChanged(
                        pane,
                        VisualConfig::Kline(data::chart::kline::Config {
                            data_labels_always_visible: value,
                            ..cfg
                        }),
                        false,
                    )
                }),
            Some("Show the latest datapoint label even when not hovering"),
            TooltipPosition::Top,
        );
        column![
            text("Data labels").size(crate::style::text_size::SECTION),
            data_labels_checkbox,
        ]
        .spacing(8)
    };

    let content = match kind {
        KlineChartKind::Candles => {
            split_column![
                display_readout_section,
                row![
                    space::horizontal(),
                    sync_all_button(pane, VisualConfig::Kline(cfg))
                ],
                ; spacing = 12, align_x = Alignment::Start
            ]
        }
        KlineChartKind::Footprint {
            clusters,
            scaling,
            studies,
        } => {
            let cluster_picklist =
                pick_list(ClusterKind::ALL, Some(clusters), move |new_cluster_kind| {
                    Message::PaneEvent(pane, Event::ClusterKindSelected(new_cluster_kind))
                });

            let footprint_summary_checkbox = tooltip(
                checkbox(cfg.show_footprint_summary)
                    .label("Show footprint summary")
                    .on_toggle(move |value| {
                        Message::VisualConfigChanged(
                            pane,
                            VisualConfig::Kline(data::chart::kline::Config {
                                show_footprint_summary: value,
                                ..cfg
                            }),
                            false,
                        )
                    }),
                Some("Show per-bar volume and delta below footprint candles"),
                TooltipPosition::Top,
            );

            let auto_row_size_checkbox = tooltip(
                checkbox(cfg.auto_row_size)
                    .label("Auto row size")
                    .on_toggle(move |value| {
                        Message::VisualConfigChanged(
                            pane,
                            VisualConfig::Kline(data::chart::kline::Config {
                                auto_row_size: value,
                                ..cfg
                            }),
                            false,
                        )
                    }),
                Some("Pick the price step automatically so cluster rows stay readable"),
                TooltipPosition::Top,
            );

            let scaling = {
                let picklist = pick_list(
                    data::chart::kline::ClusterScaling::ALL,
                    Some(scaling),
                    move |new_scaling| {
                        Message::PaneEvent(pane, Event::ClusterScalingSelected(new_scaling))
                    },
                );

                if let data::chart::kline::ClusterScaling::Hybrid { weight } = scaling {
                    let hybrid_slider = slider(0.0..=1.0, *weight, move |new_weight| {
                        Message::PaneEvent(
                            pane,
                            Event::ClusterScalingSelected(
                                data::chart::kline::ClusterScaling::Hybrid { weight: new_weight },
                            ),
                        )
                    })
                    .step(0.05_f32);

                    column![
                        picklist,
                        hybrid_slider,
                        text("Blend visible-range and per-candle scaling"),
                    ]
                    .spacing(8)
                } else {
                    column![picklist].spacing(8)
                }
            };

            let available_studies: Vec<_> = data::chart::kline::FootprintStudy::ALL.to_vec();

            let active_studies: Vec<_> = studies
                .iter()
                .copied()
                .filter(|study| {
                    available_studies
                        .iter()
                        .any(|available| available.is_same_type(study))
                })
                .collect();

            let study_cfg = study_config
                .view_available(&active_studies, basis, available_studies)
                .map(move |msg| {
                    Message::PaneEvent(
                        pane,
                        Event::StudyConfigurator(study::StudyMessage::Footprint(msg)),
                    )
                });

            let mut content = split_column![
                display_readout_section,
                column![text("Footprint summary").size(crate::style::text_size::SECTION), footprint_summary_checkbox].spacing(8),
                column![text("Cluster type").size(crate::style::text_size::SECTION), cluster_picklist].spacing(8),
                column![text("Row size").size(crate::style::text_size::SECTION), auto_row_size_checkbox].spacing(8),
                ; spacing = 12, align_x = Alignment::Start
            ];

            content = content.push(
                column![
                    text("Cluster scaling").size(crate::style::text_size::SECTION),
                    scaling
                ]
                .spacing(8),
            );

            content = content
                .push(
                    column![
                        text("Studies").size(crate::style::text_size::SECTION),
                        study_cfg
                    ]
                    .spacing(8),
                )
                .push(row![
                    space::horizontal(),
                    sync_all_button(pane, VisualConfig::Kline(cfg))
                ]);

            content
        }
    };

    cfg_view_container(360, content)
}

pub fn ladder_cfg_view<'a>(cfg: ladder::Config, pane: pane_grid::Pane) -> Element<'a, Message> {
    let dom_options = column![
        text(format!("Ширина DOM · {} px", cfg.width_px.clamp(240, 640))),
        slider(240..=640, cfg.width_px.clamp(240, 640), move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Ladder(ladder::Config {
                    width_px: value,
                    ..cfg
                }),
                false,
            )
        })
        .step(10),
        checkbox(cfg.compact)
            .label("Компактный DOM")
            .on_toggle(move |value| Message::VisualConfigChanged(
                pane,
                VisualConfig::Ladder(ladder::Config {
                    compact: value,
                    ..cfg
                }),
                false
            )),
        checkbox(cfg.show_depth_ruler)
            .label("Depth Ruler — диапазон ликвидности")
            .on_toggle(move |value| Message::VisualConfigChanged(
                pane,
                VisualConfig::Ladder(ladder::Config {
                    show_depth_ruler: value,
                    ..cfg
                }),
                false
            )),
        checkbox(cfg.show_cumulative_depth)
            .label("Cumulative Depth — накопленная глубина")
            .on_toggle(move |value| Message::VisualConfigChanged(
                pane,
                VisualConfig::Ladder(ladder::Config {
                    show_cumulative_depth: value,
                    ..cfg
                }),
                false
            )),
        text("Интенсивность подсветки"),
        slider(0.0..=1.0, cfg.liquidity_intensity, move |value| {
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Ladder(ladder::Config {
                    liquidity_intensity: value,
                    ..cfg
                }),
                false,
            )
        })
        .step(0.05),
        text("Шаг цен: в заголовке панели. Границы Ruler: перетаскивайте метки слева от цены.")
            .size(crate::style::text_size::SMALL),
    ]
    .spacing(8);

    let display_options = {
        let spread = checkbox(cfg.show_spread)
            .label("Show Spread")
            .on_toggle(move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Ladder(ladder::Config {
                        show_spread: value,
                        ..cfg
                    }),
                    false,
                )
            });

        let chase_tracker = checkbox(cfg.show_chase_tracker)
            .label("Show Chase Tracker")
            .on_toggle(move |value| {
                Message::VisualConfigChanged(
                    pane,
                    VisualConfig::Ladder(ladder::Config {
                        show_chase_tracker: value,
                        ..cfg
                    }),
                    false,
                )
            });

        column![
            text("Display Options").size(crate::style::text_size::SECTION),
            column![
                spread,
                row![
                    chase_tracker,
                    tooltip(
                        button("i").style(style::button::info),
                        Some("Highlights consecutive best-price moves and fades when momentum stalls.\nCalculated using raw ungrouped data."),
                        TooltipPosition::Top,
                    )
                ]
                .align_y(Alignment::Center)
                .spacing(4)
            ]
            .spacing(4)
        ]
        .spacing(8)
    };

    let retention_slider = {
        let retention_minutes = (cfg.trade_retention.as_secs_f32() / 60.0).max(1.0);

        let slider_ui = slider(1.0..=60.0, retention_minutes, move |new_minutes| {
            let mins = new_minutes.round().max(1.0) as u64;
            Message::VisualConfigChanged(
                pane,
                VisualConfig::Ladder(ladder::Config {
                    trade_retention: Duration::from_secs(mins * 60),
                    ..cfg
                }),
                false,
            )
        })
        .step(1.0_f32);

        classic_slider_row(
            text("Keep trades for"),
            slider_ui.into(),
            Some(
                text(format!("≈ {} min", retention_minutes.round() as u64))
                    .size(crate::style::text_size::EMPHASIS),
            ),
        )
    };

    let history_column = column![
        text("History").size(crate::style::text_size::SECTION),
        retention_slider
    ]
    .spacing(8);

    let content = split_column![
        dom_options,
        display_options,
        history_column,
        row![
            space::horizontal(),
            sync_all_button(pane, VisualConfig::Ladder(cfg))
        ],
        ; spacing = 12, align_x = Alignment::Start
    ];

    cfg_view_container(320, content)
}

fn sync_all_button<'a>(pane: pane_grid::Pane, config: VisualConfig) -> Element<'a, Message> {
    tooltip(
        button("Sync all").on_press(Message::VisualConfigChanged(pane, config, true)),
        Some("Apply configuration to similar panes"),
        TooltipPosition::Top,
    )
}

pub mod study {
    use crate::{
        split_column,
        style::{self, Icon, icon_text},
    };
    use data::chart::heatmap::{CLEANUP_THRESHOLD, HeatmapStudy, ProfileKind};
    use data::chart::kline::FootprintStudy;
    use iced::{
        Element, padding,
        widget::{button, checkbox, column, container, row, slider, space, text},
    };

    #[derive(Debug, Clone, Copy)]
    pub enum StudyMessage {
        Footprint(Message<FootprintStudy>),
        Heatmap(Message<HeatmapStudy>),
    }

    pub trait Study: Sized + Copy + 'static {
        fn is_same_type(&self, other: &Self) -> bool;
        fn all() -> Vec<Self>;
        fn view_config<'a>(
            &self,
            basis: data::chart::Basis,
            on_change: impl Fn(Self) -> Message<Self> + Copy + 'a,
        ) -> Element<'a, Message<Self>>;
    }

    impl Study for FootprintStudy {
        fn is_same_type(&self, other: &Self) -> bool {
            FootprintStudy::is_same_type(self, other)
        }

        fn all() -> Vec<Self> {
            FootprintStudy::ALL.to_vec()
        }

        fn view_config<'a>(
            &self,
            _basis: data::chart::Basis,
            on_change: impl Fn(Self) -> Message<Self> + Copy + 'a,
        ) -> Element<'a, Message<Self>> {
            match *self {
                FootprintStudy::ClusterSearch {
                    slot,
                    metric,
                    rule,
                    bars,
                    minimum,
                    top,
                    sigma_tenths,
                    style,
                } => {
                    use data::chart::kline::{ClusterMetric as Metric, ClusterRule as Rule};
                    let change = move |metric, rule, bars, minimum, top, sigma_tenths, style| {
                        on_change(FootprintStudy::ClusterSearch {
                            slot,
                            metric,
                            rule,
                            bars,
                            minimum,
                            top,
                            sigma_tenths,
                            style,
                        })
                    };
                    let restyle =
                        move |style| change(metric, rule, bars, minimum, top, sigma_tenths, style);
                    macro_rules! choice {
                        ($field:ident, $items:expr) => {
                            row($items.map(|(v, label)| {
                                button(text(label))
                                    .style(if style.options.$field == v {
                                        button::primary
                                    } else {
                                        button::secondary
                                    })
                                    .on_press({
                                        let mut next = style;
                                        next.options.$field = v;
                                        restyle(next)
                                    })
                                    .into()
                            }))
                            .spacing(3)
                        };
                    }
                    macro_rules! quantity {
                        ($field:ident, $label:expr) => {
                            column![
                                text($label),
                                iced::widget::text_input(
                                    "0 = off",
                                    format!("{:.3}", style.options.$field as f64 / 1000.0)
                                )
                                .on_input(move |v| {
                                    let mut next = style;
                                    if let Ok(v) = v.parse::<f64>()
                                        && v.is_finite()
                                        && v >= 0.0
                                    {
                                        next.options.$field = (v * 1000.0).round() as u64;
                                    }
                                    restyle(next)
                                })
                            ]
                            .spacing(3)
                        };
                    }
                    macro_rules! integer {
                        ($field:ident, $label:expr, $max:expr) => {
                            column![
                                text($label),
                                iced::widget::text_input("0", style.options.$field.to_string())
                                    .on_input(move |v| {
                                        let mut next = style;
                                        if let Ok(v) = v.parse::<u32>() {
                                            next.options.$field = v.min($max) as _;
                                        }
                                        restyle(next)
                                    })
                            ]
                            .spacing(3)
                        };
                    }
                    macro_rules! toggle {
                        ($field:ident, $label:expr) => {
                            checkbox(style.options.$field)
                                .label($label)
                                .on_toggle(move |v| {
                                    let mut next = style;
                                    next.options.$field = v;
                                    restyle(next)
                                })
                        };
                    }
                    let metrics = column![
                        row(
                            [Metric::Volume, Metric::Bid, Metric::Ask, Metric::Delta].map(|m| {
                                button(text(format!("{m:?}")))
                                    .style(if m == metric {
                                        button::primary
                                    } else {
                                        button::secondary
                                    })
                                    .on_press(change(
                                        m,
                                        rule,
                                        bars,
                                        minimum,
                                        top,
                                        sigma_tenths,
                                        style,
                                    ))
                                    .into()
                            })
                        )
                        .spacing(3),
                        row([
                            Metric::PositiveDelta,
                            Metric::NegativeDelta,
                            Metric::Trades,
                            Metric::Imbalance
                        ]
                        .map(|m| button(text(format!("{m:?}")))
                            .style(if m == metric {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .on_press(change(m, rule, bars, minimum, top, sigma_tenths, style))
                            .into()))
                        .spacing(3)
                    ]
                    .spacing(3);
                    let rule_button = |r, label| {
                        button(label)
                            .style(if r == rule {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .on_press(change(metric, r, bars, minimum, top, sigma_tenths, style))
                    };
                    let rules = column![
                        row![
                            rule_button(Rule::Minimum, "Manual"),
                            rule_button(Rule::Top, "Top N"),
                            rule_button(Rule::Percentile, "Percentile")
                        ]
                        .spacing(3),
                        row![
                            rule_button(Rule::Average, "Average × K"),
                            rule_button(Rule::Median, "Median × K"),
                            rule_button(Rule::Sigma, "Std × K")
                        ]
                        .spacing(3)
                    ]
                    .spacing(3);
                    let mut controls = column![
                        text(
                            "Порог после фильтров и объединения; авто — по максимуму каждого бара"
                        ),
                        metrics,
                        rules
                    ]
                    .spacing(7);
                    controls = match rule {
                        Rule::Minimum => controls
                            .push(text("Minimum (текущие единицы объёма / выбранной метрики)"))
                            .push(
                                iced::widget::text_input(
                                    "Minimum",
                                    format!("{:.3}", minimum as f64 / 1000.0),
                                )
                                .on_input(move |v| {
                                    let minimum = v
                                        .parse::<f64>()
                                        .ok()
                                        .filter(|v| v.is_finite() && *v >= 0.0)
                                        .map(|v| (v * 1000.0).round() as u64)
                                        .unwrap_or(minimum);
                                    change(metric, rule, bars, minimum, top, sigma_tenths, style)
                                }),
                            )
                            .push(quantity!(maximum, "Maximum (0 = без верхней границы)")),
                        Rule::Top => controls.push(text("Top N")).push(
                            iced::widget::text_input("Top N", top.to_string()).on_input(move |v| {
                                change(
                                    metric,
                                    rule,
                                    bars,
                                    minimum,
                                    v.parse::<usize>().unwrap_or(top).clamp(1, 5000),
                                    sigma_tenths,
                                    style,
                                )
                            }),
                        ),
                        Rule::Percentile => controls
                            .push(text(format!(
                                "Percentile {:.1}%",
                                style.options.percentile_tenths as f32 / 10.0
                            )))
                            .push(
                                slider(
                                    0.0..=100.0,
                                    style.options.percentile_tenths as f32 / 10.0,
                                    move |v| {
                                        let mut next = style;
                                        next.options.percentile_tenths = (v * 10.0).round() as u16;
                                        restyle(next)
                                    },
                                )
                                .step(0.1),
                            ),
                        Rule::Average | Rule::Median => controls
                            .push(text(format!(
                                "K = {:.1}",
                                style.options.multiplier_tenths as f32 / 10.0
                            )))
                            .push(
                                slider(
                                    0.0..=20.0,
                                    style.options.multiplier_tenths as f32 / 10.0,
                                    move |v| {
                                        let mut next = style;
                                        next.options.multiplier_tenths = (v * 10.0).round() as u16;
                                        restyle(next)
                                    },
                                )
                                .step(0.1),
                            ),
                        Rule::Sigma => controls
                            .push(text(format!(
                                "Mean + {:.1} × Std",
                                sigma_tenths as f32 / 10.0
                            )))
                            .push(
                                slider(0.0..=20.0, sigma_tenths as f32 / 10.0, move |v| {
                                    change(
                                        metric,
                                        rule,
                                        bars,
                                        minimum,
                                        top,
                                        (v * 10.0).round() as u32,
                                        style,
                                    )
                                })
                                .step(0.1),
                            ),
                    };
                    if rule != Rule::Minimum {
                        controls = controls.push(
                            checkbox(style.visible_range)
                                .label("AutoVisible: видимые бары и цены")
                                .on_toggle(move |v| {
                                    let mut next = style;
                                    next.visible_range = v;
                                    restyle(next)
                                }),
                        );
                        if !style.visible_range {
                            controls = controls
                                .push(text("AutoPeriod: последние N загруженных баров"))
                                .push(
                                    iced::widget::text_input("Lookback", bars.to_string())
                                        .on_input(move |v| {
                                            change(
                                                metric,
                                                rule,
                                                v.parse::<usize>().unwrap_or(bars).clamp(1, 5000),
                                                minimum,
                                                top,
                                                sigma_tenths,
                                                style,
                                            )
                                        }),
                                )
                                .push(toggle!(
                                    display_only_lookback,
                                    "Показывать только последние N баров"
                                ));
                        }
                        controls = controls.push(quantity!(
                            auto_floor,
                            "Минимальный авто-порог (MinAutoThreshold)"
                        ));
                    }
                    controls=controls.push(text("Объединение")).push(integer!(merge_bars,"MergeBars (1–200)",200)).push(integer!(merge_prices,"MergePrices (1–200 строк Footprint)",200)).push(choice!(merge_direction,[(0,"Both"),(1,"Up"),(2,"Down")])).push(toggle!(single_per_bar,"Один сильнейший сигнал на бар"))
                        .push(text("Фильтры до расчёта порога (0 = выключен)"))
                        .push(quantity!(min_value,"MinValue выбранной метрики"))
                        .push(quantity!(min_delta,"Минимум |Delta|"))
                        .push(integer!(imbalance_percent,"Доля большей стороны Bid/Ask (%)",100))
                        .push(quantity!(min_average,"Минимальный средний размер сделки"))
                        .push(quantity!(max_average,"Максимальный средний размер сделки"))
                        .push(text("Trades / средний размер недоступны для compact-строк: они исключаются при этих фильтрах."))
                        .push(integer!(range_high,"Не дальше от High (строк)",10000))
                        .push(integer!(range_low,"Не дальше от Low (строк)",10000))
                        .push(choice!(bar_direction,[(0,"Любой бар"),(1,"Бычий"),(2,"Медвежий")]))
                        .push(choice!(price_location,[(0,"Любая цена"),(1,"Верх"),(2,"Середина"),(3,"Низ")]))
                        .push(choice!(price_location,[(4,"≥ Open"),(5,"≤ Open"),(6,"≥ Close"),(7,"≤ Close")]))
                        .push(toggle!(time_filter,"Фильтр времени открытия бара (UTC)"));
                    if style.options.time_filter {
                        controls = controls
                            .push(integer!(
                                start_second,
                                "Начало: секунды от 00:00 UTC",
                                86399
                            ))
                            .push(integer!(
                                end_second,
                                "Конец: секунды от 00:00 UTC; допускается через полночь",
                                86399
                            ));
                    }
                    let colors = [
                        [65, 185, 255],
                        [75, 220, 140],
                        [255, 190, 60],
                        [190, 130, 255],
                        [245, 85, 100],
                    ];
                    let swatches = |sell: bool| {
                        row(colors.map(|rgb| {
                            button(text("●").color(iced::Color::from_rgb8(rgb[0], rgb[1], rgb[2])))
                                .on_press({
                                    let mut next = style;
                                    if sell {
                                        next.sell_color = rgb;
                                    } else {
                                        next.color = rgb;
                                    }
                                    restyle(next)
                                })
                                .into()
                        }))
                        .spacing(4)
                    };
                    controls
                        .push(text("Отображение"))
                        .push(choice!(
                            shape,
                            [
                                (0, "Круг"),
                                (1, "Квадрат"),
                                (2, "Ромб"),
                                (3, "Треугольник"),
                                (4, "Линия")
                            ]
                        ))
                        .push(text("Цвет / положительное направление"))
                        .push(swatches(false))
                        .push(text("Отрицательное направление"))
                        .push(swatches(true))
                        .push(text(format!("Радиус от {} px", style.radius)))
                        .push(
                            slider(1.0..=30.0, style.radius as f32, move |v| {
                                let mut next = style;
                                next.radius = v as u8;
                                restyle(next)
                            })
                            .step(1.0),
                        )
                        .push(integer!(max_radius, "Радиус до (px)", 60))
                        .push(integer!(fill_percent, "Непрозрачность заливки (%)", 100))
                        .push(toggle!(border, "Контур"))
                        .push(toggle!(text, "Значение у маркера"))
                        .push(toggle!(zones, "Границы объединённых областей"))
                        .padding(8)
                        .spacing(7)
                        .into()
                }
                FootprintStudy::NPoC { lookback } => {
                    let slider_ui = slider(10.0..=400.0, lookback as f32, move |new_value| {
                        on_change(FootprintStudy::NPoC {
                            lookback: new_value as usize,
                        })
                    })
                    .step(10.0_f32);

                    column![text(format!("Lookback: {lookback} datapoints")), slider_ui]
                        .padding(8)
                        .spacing(4)
                        .into()
                }
                FootprintStudy::Imbalance {
                    threshold,
                    color_scale,
                    ignore_zeros,
                } => {
                    let qty_threshold = {
                        let info_text = text(format!("Ask:Bid threshold: {threshold}%"));

                        let threshold_slider =
                            slider(100.0..=800.0, threshold as f32, move |new_value| {
                                on_change(FootprintStudy::Imbalance {
                                    threshold: new_value as usize,
                                    color_scale,
                                    ignore_zeros,
                                })
                            })
                            .step(25.0_f32);

                        column![info_text, threshold_slider,].padding(8).spacing(4)
                    };

                    let color_scaling = {
                        let color_scale_enabled = color_scale.is_some();
                        let color_scale_value = color_scale.unwrap_or(100);

                        let color_scale_checkbox = checkbox(color_scale_enabled)
                            .label("Dynamic color scaling")
                            .on_toggle(move |is_enabled| {
                                on_change(FootprintStudy::Imbalance {
                                    threshold,
                                    color_scale: if is_enabled {
                                        Some(color_scale_value)
                                    } else {
                                        None
                                    },
                                    ignore_zeros,
                                })
                            });

                        if color_scale_enabled {
                            let scaling_slider = column![
                                text(format!("Opaque color at: {color_scale_value}x")),
                                slider(50.0..=2000.0, color_scale_value as f32, move |new_value| {
                                    on_change(FootprintStudy::Imbalance {
                                        threshold,
                                        color_scale: Some(new_value as usize),
                                        ignore_zeros,
                                    })
                                })
                                .step(50.0_f32)
                            ]
                            .spacing(2);

                            column![color_scale_checkbox, scaling_slider]
                                .padding(8)
                                .spacing(8)
                        } else {
                            column![color_scale_checkbox].padding(8)
                        }
                    };

                    let ignore_zeros_checkbox = {
                        let cbox = checkbox(ignore_zeros).label("Ignore zeros").on_toggle(
                            move |is_checked| {
                                on_change(FootprintStudy::Imbalance {
                                    threshold,
                                    color_scale,
                                    ignore_zeros: is_checked,
                                })
                            },
                        );

                        column![cbox].padding(8).spacing(4)
                    };

                    split_column![qty_threshold, color_scaling, ignore_zeros_checkbox]
                        .padding(4)
                        .into()
                }
                FootprintStudy::Signals {
                    absorption,
                    trapped,
                    exhaustion,
                    stacked,
                    unfinished,
                    near_levels,
                    failed_auctions,
                } => {
                    let flags = [
                        absorption,
                        trapped,
                        exhaustion,
                        stacked,
                        unfinished,
                        failed_auctions,
                        near_levels,
                    ];
                    let labels = [
                        "Absorption",
                        "Trapped traders",
                        "Exhaustion",
                        "Stacked imbalances",
                        "Unfinished auctions",
                        "Failed auctions (PDH/PDL, PWH/PWL, PMH/PML, FRVP VAH/VAL, lines)",
                        "Only near levels (FRVP, period, naked, W/M VWAP; 0.3%)",
                    ];
                    let toggles = labels.into_iter().enumerate().map(|(i, label)| {
                        checkbox(flags[i])
                            .label(label)
                            .on_toggle(move |value| {
                                let mut f = flags;
                                f[i] = value;
                                on_change(FootprintStudy::Signals {
                                    absorption: f[0],
                                    trapped: f[1],
                                    exhaustion: f[2],
                                    stacked: f[3],
                                    unfinished: f[4],
                                    failed_auctions: f[5],
                                    near_levels: f[6],
                                })
                            })
                            .into()
                    });

                    iced::widget::Column::with_children(toggles)
                        .padding(8)
                        .spacing(8)
                        .into()
                }
            }
        }
    }

    impl Study for HeatmapStudy {
        fn is_same_type(&self, other: &Self) -> bool {
            std::mem::discriminant(self) == std::mem::discriminant(other)
        }

        fn all() -> Vec<Self> {
            HeatmapStudy::ALL.to_vec()
        }

        fn view_config<'a>(
            &self,
            basis: data::chart::Basis,
            on_change: impl Fn(Self) -> Message<Self> + Copy + 'a,
        ) -> Element<'a, Message<Self>> {
            let interval_ms = match basis {
                data::chart::Basis::Time(interval) => interval.to_milliseconds(),
                data::chart::Basis::Tick(_) => {
                    return iced::widget::center(text(
                        "Heatmap studies are not supported for tick-based charts",
                    ))
                    .into();
                }
            };

            match self {
                HeatmapStudy::VolumeProfile(kind) => match kind {
                    ProfileKind::FixedWindow(datapoint_count) => {
                        let duration_secs = (*datapoint_count as u64 * interval_ms) / 1000;
                        let min_range = CLEANUP_THRESHOLD / 20;

                        let duration_text = if duration_secs < 60 {
                            format!("{} seconds", duration_secs)
                        } else {
                            let minutes = duration_secs / 60;
                            let seconds = duration_secs % 60;
                            if seconds == 0 {
                                format!("{} minutes", minutes)
                            } else {
                                format!("{}m {}s", minutes, seconds)
                            }
                        };

                        let slider = slider(
                            min_range as f32..=CLEANUP_THRESHOLD as f32,
                            *datapoint_count as f32,
                            move |new_datapoint_count| {
                                on_change(HeatmapStudy::VolumeProfile(ProfileKind::FixedWindow(
                                    new_datapoint_count as usize,
                                )))
                            },
                        )
                        .step(40.0_f32);

                        let switch_kind = button(text("Switch to visible range")).on_press(
                            on_change(HeatmapStudy::VolumeProfile(ProfileKind::VisibleRange)),
                        );

                        column![
                            row![space::horizontal(), switch_kind,],
                            text(format!(
                                "Window: {} datapoints ({})",
                                datapoint_count, duration_text
                            )),
                            slider,
                        ]
                        .padding(8)
                        .spacing(4)
                        .into()
                    }
                    ProfileKind::VisibleRange => {
                        let switch_kind = button(text("Switch to fixed window")).on_press(
                            on_change(HeatmapStudy::VolumeProfile(ProfileKind::FixedWindow(
                                CLEANUP_THRESHOLD / 5_usize,
                            ))),
                        );

                        column![row![space::horizontal(), switch_kind,],]
                            .padding(8)
                            .spacing(4)
                            .into()
                    }
                },
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub enum Message<S: Study> {
        CardToggled(S),
        StudyToggled(S, bool),
        StudyValueChanged(S),
    }

    pub enum Action<S: Study> {
        ToggleStudy(S, bool),
        ConfigureStudy(S),
    }

    pub struct Configurator<S: Study> {
        expanded_card: Option<S>,
    }

    impl<S: Study> Default for Configurator<S> {
        fn default() -> Self {
            Self {
                expanded_card: None,
            }
        }
    }

    impl<S: Study + ToString> Configurator<S> {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn update(&mut self, message: Message<S>) -> Option<Action<S>> {
            match message {
                Message::CardToggled(study) => {
                    let should_collapse = self
                        .expanded_card
                        .as_ref()
                        .is_some_and(|expanded| expanded.is_same_type(&study));

                    if should_collapse {
                        self.expanded_card = None;
                    } else {
                        self.expanded_card = Some(study);
                    }
                }
                Message::StudyToggled(study, is_checked) => {
                    return Some(Action::ToggleStudy(study, is_checked));
                }
                Message::StudyValueChanged(study) => {
                    return Some(Action::ConfigureStudy(study));
                }
            }

            None
        }

        pub fn view<'a>(
            &self,
            active_studies: &[S],
            basis: data::chart::Basis,
        ) -> Element<'a, Message<S>> {
            self.view_available(active_studies, basis, S::all())
        }

        pub fn view_available<'a>(
            &self,
            active_studies: &[S],
            basis: data::chart::Basis,
            available_studies: Vec<S>,
        ) -> Element<'a, Message<S>> {
            let mut content = column![].spacing(4);

            for available_study in available_studies {
                content =
                    content.push(self.create_study_row(available_study, active_studies, basis));
            }

            content.into()
        }

        fn create_study_row<'a>(
            &self,
            study: S,
            active_studies: &[S],
            basis: data::chart::Basis,
        ) -> Element<'a, Message<S>> {
            let (is_selected, study_config) = {
                let mut is_selected = false;
                let mut study_config = None;

                for s in active_studies {
                    if s.is_same_type(&study) {
                        is_selected = true;
                        study_config = Some(*s);
                        break;
                    }
                }
                (is_selected, study_config)
            };

            let checkbox = checkbox(is_selected)
                .label(study_config.map_or(study.to_string(), |s| s.to_string()))
                .on_toggle(move |checked| Message::StudyToggled(study, checked));

            let mut checkbox_row = row![checkbox, space::horizontal()]
                .height(36)
                .align_y(iced::Alignment::Center)
                .padding(padding::left(8).right(4))
                .spacing(4);

            let is_expanded = self
                .expanded_card
                .as_ref()
                .is_some_and(|expanded| expanded.is_same_type(&study));

            if is_selected {
                checkbox_row = checkbox_row.push(
                    button(icon_text(Icon::Cog, 12))
                        .on_press(Message::CardToggled(study))
                        .style(move |theme, status| {
                            style::button::transparent(theme, status, is_expanded)
                        }),
                );
            }

            let mut column = column![checkbox_row];

            if is_expanded && let Some(config) = study_config {
                column = column.push(config.view_config(basis, Message::StudyValueChanged));
            }

            container(column).style(style::modal_container).into()
        }
    }
}
