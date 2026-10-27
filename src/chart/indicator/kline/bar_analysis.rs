use crate::{
    chart::{
        Caches, Interaction, Message, TEXT_SIZE, ViewState,
        indicator::{
            kline::{BasisSeriesExt, KlineIndicatorImpl},
            plot::{AnySeries, Series},
        },
    },
    style,
};

use data::chart::{
    BasisSeries, PlotData,
    kline::{FootprintSummary, KlineDataPoint, KlineTrades},
};
use data::util::abbr_large_numbers;
use exchange::{Kline, Trade};
use iced::{
    Alignment, Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
    mouse,
    widget::{
        Canvas,
        canvas::{self, Cache, Geometry},
        container, row, rule, space,
    },
};
use std::{collections::BTreeMap, ops::RangeInclusive};

pub struct BarAnalysisIndicator {
    cache: Caches,
    data: BasisSeries<BarStats>,
    settings: BarAnalysisSettings,
}

/// One column of the table. Trade-based stats come from the raw executions (or what was
/// saved from them at compaction), so they are `None` for candles from cached aggregates.
#[derive(Debug, Clone, Copy)]
struct BarStats {
    summary: FootprintSummary,
    trades: Option<usize>,
    /// Highest and lowest running delta reached inside the bar (it opens at 0).
    max_delta: Option<f64>,
    min_delta: Option<f64>,
}

impl BarStats {
    /// Tick bars keep no execution order: counts only, no intrabar delta path.
    fn from_footprint(footprint: &KlineTrades) -> Option<Self> {
        let summary = FootprintSummary::from_trades(footprint)?;
        let trades = footprint
            .trades
            .values()
            .map(|group| group.buy_count + group.sell_count)
            .sum();
        Some(Self {
            summary,
            trades: Some(trades),
            max_delta: None,
            min_delta: None,
        })
    }

    fn from_datapoint(dp: &KlineDataPoint) -> Option<Self> {
        let base = Self::from_footprint(&dp.footprint)?;
        let intrabar = dp.intrabar_stats();
        Some(Self {
            trades: intrabar.map(|s| s.trades),
            max_delta: intrabar.map(|s| s.max_delta),
            min_delta: intrabar.map(|s| s.min_delta),
            ..base
        })
    }

    fn avg_size(&self) -> Option<f64> {
        self.trades
            .filter(|&n| n > 0)
            .map(|n| self.summary.total.to_f64() / n as f64)
    }
}

impl BarAnalysisIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            data: BasisSeries::default(),
            settings: BarAnalysisSettings::default(),
        }
    }

    fn indicator_elem<'a>(
        &'a self,
        main_chart: &'a ViewState,
        visible_range: RangeInclusive<u64>,
    ) -> Element<'a, Message> {
        let canvas = Canvas::new(BarAnalysisCanvas {
            cache: &self.cache.main,
            ctx: main_chart,
            series: self.data.as_plot_series(),
            settings: self.settings,
            visible_range,
        })
        .height(Length::Fill)
        .width(Length::Fill);

        row![
            canvas,
            rule::vertical(1).style(style::split_ruler),
            container(space::vertical()).width(main_chart.y_labels_width())
        ]
        .into()
    }

    fn rebuild(&mut self, source: &PlotData<KlineDataPoint>) {
        self.data = source.map_basis_series(
            |timeseries| {
                timeseries
                    .datapoints
                    .iter()
                    .filter_map(|(timestamp, dp)| {
                        BarStats::from_datapoint(dp).map(|row| (*timestamp, row))
                    })
                    .collect::<BTreeMap<_, _>>()
            },
            |tickseries| {
                tickseries
                    .datapoints
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, dp)| {
                        BarStats::from_footprint(&dp.footprint).map(|row| (idx as u64, row))
                    })
                    .collect::<BTreeMap<_, _>>()
            },
        );
        self.clear_all_caches();
    }
}

impl KlineIndicatorImpl for BarAnalysisIndicator {
    fn clear_all_caches(&mut self) {
        self.cache.clear_all();
    }

    fn clear_crosshair_caches(&mut self) {
        self.cache.clear_crosshair();
    }

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        _data_labels_always_visible: bool,
        visible_range: RangeInclusive<u64>,
    ) -> Element<'a, Message> {
        self.indicator_elem(chart, visible_range)
    }

    fn rebuild_from_source(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }

    fn on_insert_klines(&mut self, klines: &[Kline], source: &PlotData<KlineDataPoint>) {
        match source {
            PlotData::TimeBased(timeseries) => {
                if let Some(data) = self.data.time_mut() {
                    for kline in klines {
                        match timeseries.datapoints.get(&kline.time) {
                            Some(dp) => match BarStats::from_datapoint(dp) {
                                Some(summary) => {
                                    data.insert(kline.time, summary);
                                }
                                None => {
                                    data.remove(&kline.time);
                                }
                            },
                            None => {
                                data.remove(&kline.time);
                            }
                        }
                    }
                }
            }
            PlotData::TickBased(_) => {}
        }
        self.clear_all_caches();
    }

    fn on_insert_trades(
        &mut self,
        trades: &[Trade],
        old_dp_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        match source {
            PlotData::TimeBased(timeseries) => {
                let mut affected = Vec::new();
                for trade in trades {
                    let rounded = trade.time.floor_to(timeseries.interval);
                    if !affected.contains(&rounded) {
                        affected.push(rounded);
                    }
                }
                if let Some(data) = self.data.time_mut() {
                    for ts in affected {
                        match timeseries.datapoints.get(&ts) {
                            Some(dp) => match BarStats::from_datapoint(dp) {
                                Some(summary) => {
                                    data.insert(ts, summary);
                                }
                                None => {
                                    data.remove(&ts);
                                }
                            },
                            None => {
                                data.remove(&ts);
                            }
                        }
                    }
                }
            }
            PlotData::TickBased(tick_aggr) => {
                let new_len = tick_aggr.datapoints.len();
                let start = old_dp_len.saturating_sub(1);
                if let Some(data) = self.data.tick_mut() {
                    // Remove entries for any indices that no longer exist
                    // (safety measure - bars are only appended, never removed)
                    data.retain(|&idx, _| idx < new_len as u64);
                    // Recompute the last old bar (may have been modified) and any new bars
                    for idx in start..new_len {
                        if let Some(dp) = tick_aggr.datapoints.get(idx) {
                            match BarStats::from_footprint(&dp.footprint) {
                                Some(summary) => {
                                    data.insert(idx as u64, summary);
                                }
                                None => {
                                    data.remove(&(idx as u64));
                                }
                            }
                        }
                    }
                }
            }
        }
        self.clear_all_caches();
    }

    fn on_ticksize_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }

    fn on_basis_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BarAnalysisSettings {
    show_buy_sell: bool,
    show_volume: bool,
    show_delta: bool,
    show_delta_pct: bool,
    show_max_min_delta: bool,
    show_trades: bool,
    show_avg_size: bool,
}

impl Default for BarAnalysisSettings {
    fn default() -> Self {
        Self {
            show_buy_sell: true,
            show_volume: true,
            show_delta: true,
            show_delta_pct: true,
            show_max_min_delta: true,
            show_trades: true,
            show_avg_size: true,
        }
    }
}

impl BarAnalysisSettings {
    fn header_labels(&self) -> Vec<&'static str> {
        let mut labels = Vec::with_capacity(9);
        if self.show_buy_sell {
            labels.push("Buy");
            labels.push("Sell");
        }
        if self.show_volume {
            labels.push("Total");
        }
        if self.show_delta {
            labels.push("Δ");
        }
        if self.show_delta_pct {
            labels.push("Δ%");
        }
        if self.show_max_min_delta {
            labels.push("Max Δ");
            labels.push("Min Δ");
        }
        if self.show_trades {
            labels.push("Trades");
        }
        if self.show_avg_size {
            labels.push("Avg");
        }
        labels
    }
}

struct BarAnalysisCanvas<'a> {
    cache: &'a Cache,
    ctx: &'a ViewState,
    series: AnySeries<'a, BarStats>,
    settings: BarAnalysisSettings,
    visible_range: RangeInclusive<u64>,
}

impl canvas::Program<Message> for BarAnalysisCanvas<'_> {
    type State = Interaction;

    fn update(
        &self,
        _state: &mut Self::State,
        _event: &Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        None
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let ctx = self.ctx;
        let palette = theme.palette();

        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            if ctx.bounds.width == 0.0 || ctx.scaling <= f32::EPSILON {
                return;
            }

            let pane_height = bounds.height / ctx.scaling;
            let table_top = 0.0;
            let table_height = pane_height.max(24.0);
            let header_labels = self.settings.header_labels();
            let rows_count = (header_labels.len() as f32).max(1.0);
            let row_height = table_height / rows_count;
            let column_width = ctx.cell_width;
            let text_size = (row_height * 0.5).clamp(5.0, TEXT_SIZE);
            // Labels like "-1.2k" / "+12.3%" are about six glyphs wide.
            let text_fits = column_width >= text_size * 0.62 * 6.0;
            let border_color = palette.background.strong.color;

            let max_label_chars = header_labels
                .iter()
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(3);
            let header_width = text_size * max_label_chars as f32 * 0.72 + 12.0;
            let screen_text_size = text_size * ctx.scaling;
            let screen_row_height = row_height * ctx.scaling;
            let screen_table_top = table_top * ctx.scaling;
            let screen_header_width = header_width * ctx.scaling;
            let header_x = bounds.width - screen_header_width;

            // Header row labels and separators
            for (idx, label) in header_labels.iter().enumerate() {
                let row_y = screen_table_top + screen_row_height * idx as f32;
                if idx > 0 {
                    frame.fill_rectangle(
                        Point::new(header_x, row_y),
                        Size::new(screen_header_width, 1.0),
                        border_color,
                    );
                }
                draw_text(
                    frame,
                    label,
                    Point::new(
                        header_x + screen_header_width / 2.0,
                        row_y + screen_row_height / 2.0,
                    ),
                    screen_text_size,
                    palette.background.weakest.text,
                );
            }

            // Header vertical border
            frame.fill_rectangle(
                Point::new(header_x, screen_table_top),
                Size::new(1.0, screen_row_height * rows_count),
                border_color,
            );

            let header_left_chart =
                (header_x - bounds.width / 2.0) / ctx.scaling - ctx.translation.x;
            let view_left = -ctx.translation.x - bounds.width / ctx.scaling;
            let view_right =
                (-ctx.translation.x + bounds.width / ctx.scaling).min(header_left_chart);

            let clip_region = Rectangle::new(
                Point::new(0.0, screen_table_top),
                Size::new(
                    bounds.width - screen_header_width,
                    screen_row_height * rows_count,
                ),
            );
            frame.with_clip(clip_region, |frame| {
                let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);
                frame.translate(center);
                frame.scale(ctx.scaling);
                frame.translate(Vector::new(
                    ctx.translation.x,
                    (-bounds.height / ctx.scaling) / 2.0,
                ));

                self.series
                    .for_each_in(self.visible_range.clone(), |x, stats| {
                        let row = &stats.summary;
                        let column_left = ctx.interval_to_x(x) - column_width / 2.0;

                        if column_left > view_right || column_left + column_width < view_left {
                            return;
                        }

                        frame.fill_rectangle(
                            Point::new(column_left, table_top),
                            Size::new(column_width, table_height),
                            palette.background.weakest.color.scale_alpha(0.22),
                        );

                        let delta_color = if row.delta.to_f64() >= 0.0 {
                            palette.success.base.color
                        } else {
                            palette.danger.base.color
                        };

                        let neutral = palette.background.weakest.text;
                        let missing = || ("—".to_string(), neutral.scale_alpha(0.5), false);

                        // (text, color, tint by sign when the column is too narrow for text)
                        let mut rows: Vec<(String, Color, bool)> = Vec::with_capacity(9);
                        if self.settings.show_buy_sell {
                            rows.push((
                                abbr_large_numbers(row.buy.to_f64()),
                                palette.success.base.color,
                                false,
                            ));
                            rows.push((
                                abbr_large_numbers(row.sell.to_f64()),
                                palette.danger.base.color,
                                false,
                            ));
                        }
                        if self.settings.show_volume {
                            rows.push((abbr_large_numbers(row.total.to_f64()), neutral, false));
                        }
                        if self.settings.show_delta {
                            rows.push((abbr_large_numbers(row.delta.to_f64()), delta_color, true));
                        }
                        if self.settings.show_delta_pct {
                            rows.push((format!("{:+.1}%", row.delta_pct), delta_color, true));
                        }
                        if self.settings.show_max_min_delta {
                            for extreme in [stats.max_delta, stats.min_delta] {
                                rows.push(match extreme {
                                    Some(value) => {
                                        let color = if value > 0.0 {
                                            palette.success.base.color
                                        } else if value < 0.0 {
                                            palette.danger.base.color
                                        } else {
                                            neutral
                                        };
                                        (abbr_large_numbers(value), color, value != 0.0)
                                    }
                                    None => missing(),
                                });
                            }
                        }
                        if self.settings.show_trades {
                            rows.push(match stats.trades {
                                Some(count) => (abbr_large_numbers(count as f64), neutral, false),
                                None => missing(),
                            });
                        }
                        if self.settings.show_avg_size {
                            rows.push(match stats.avg_size() {
                                Some(avg) => (abbr_large_numbers(avg), neutral, false),
                                None => missing(),
                            });
                        }

                        for (idx, (label, color, tint)) in rows.iter().enumerate() {
                            let row_y = table_top + row_height * idx as f32;
                            if idx > 0 {
                                frame.fill_rectangle(
                                    Point::new(column_left, row_y),
                                    Size::new(column_width, 1.0),
                                    border_color,
                                );
                            }
                            if !text_fits {
                                // Too narrow for numbers: tint the delta rows by sign instead.
                                if *tint {
                                    frame.fill_rectangle(
                                        Point::new(column_left, row_y),
                                        Size::new(column_width, row_height),
                                        color.scale_alpha(0.45),
                                    );
                                }
                                continue;
                            }
                            draw_text(
                                frame,
                                label,
                                Point::new(
                                    column_left + column_width / 2.0,
                                    row_y + row_height / 2.0,
                                ),
                                text_size,
                                *color,
                            );
                        }

                        frame.fill_rectangle(
                            Point::new(column_left, table_top),
                            Size::new(1.0, table_height),
                            border_color.scale_alpha(0.4),
                        );
                    });
            });
        });

        vec![geometry]
    }

    fn mouse_interaction(
        &self,
        _state: &Interaction,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::chart::kline::{BubbleVolumeSummary, TradeCoverage};
    use exchange::UnixMs;
    use exchange::unit::{Price, PriceStep, Qty};

    fn candle(trades: &[(u64, bool, f64)]) -> KlineDataPoint {
        let price = Price::from_f64(100.0);
        let mut dp = KlineDataPoint {
            kline: Kline {
                time: UnixMs::new(60_000),
                open: price,
                high: price,
                low: price,
                close: price,
                volume: exchange::Volume::empty_buy_sell(),
            },
            footprint: KlineTrades::new(),
            bubble_summary: BubbleVolumeSummary::default(),
            trade_coverage: TradeCoverage::Unknown,
            trade_sequence: Vec::new(),
            trade_ids: Default::default(),
            compacted_ids: None,
            intrabar: None,
        };
        let step = PriceStep {
            units: Price::from_f64(0.1).units,
        };
        for (i, &(time, is_sell, qty)) in trades.iter().enumerate() {
            dp.add_trade(
                &Trade {
                    id: Some(i as u64 + 1),
                    time: UnixMs::new(time),
                    is_sell,
                    price,
                    qty: Qty::from_f64(qty),
                },
                step,
            );
        }
        dp
    }

    #[test]
    fn running_delta_extremes_follow_trade_time() {
        // Arrival order differs from time order: +3 @61s, -5 @62s, +1 @63s, the last two swapped.
        let dp = candle(&[
            (61_000, false, 3.0),
            (63_000, false, 1.0),
            (62_000, true, 5.0),
        ]);
        let stats = BarStats::from_datapoint(&dp).unwrap();
        assert_eq!(stats.trades, Some(3));
        assert!((stats.max_delta.unwrap() - 3.0).abs() < 1e-9);
        assert!((stats.min_delta.unwrap() + 2.0).abs() < 1e-9);
        assert!((stats.avg_size().unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn compaction_keeps_trade_stats() {
        let trades: Vec<(u64, bool, f64)> = (0..300)
            .map(|i| (61_000 + i, i % 3 == 0, 1.0 + (i % 7) as f64))
            .collect();
        let mut dp = candle(&trades);
        let before = BarStats::from_datapoint(&dp).unwrap();
        dp.compact_trades();
        assert!(dp.compacted_ids.is_some() && dp.trade_sequence.len() < 300);
        let after = BarStats::from_datapoint(&dp).unwrap();
        assert_eq!(after.trades, Some(300));
        assert_eq!(after.max_delta, before.max_delta);
        assert_eq!(after.min_delta, before.min_delta);
    }

    #[test]
    fn backfill_pieces_join_after_compaction() {
        let trades: Vec<(u64, bool, f64)> = (0..600)
            .map(|i| (61_000 + i, i % 3 != 0, 1.0 + (i % 5) as f64))
            .collect();
        let whole = BarStats::from_datapoint(&candle(&trades)).unwrap();

        // Later half lands first and is compacted, then the earlier half arrives.
        let mut dp = candle(&trades[300..]);
        dp.compact_trades();
        assert!(dp.compacted_ids.is_some());
        let step = PriceStep {
            units: Price::from_f64(0.1).units,
        };
        for (i, &(time, is_sell, qty)) in trades[..300].iter().enumerate() {
            dp.add_trade(
                &Trade {
                    id: Some(10_000 + i as u64),
                    time: UnixMs::new(time),
                    is_sell,
                    price: Price::from_f64(100.0),
                    qty: Qty::from_f64(qty),
                },
                step,
            );
        }
        let joined = BarStats::from_datapoint(&dp).unwrap();
        assert_eq!(joined.trades, Some(600));
        let close = |a: Option<f64>, b: Option<f64>| (a.unwrap() - b.unwrap()).abs() < 1e-9;
        assert!(close(joined.max_delta, whole.max_delta));
        assert!(close(joined.min_delta, whole.min_delta));

        // Compacting again keeps the joined result.
        dp.compact_trades();
        let again = BarStats::from_datapoint(&dp).unwrap();
        assert_eq!(again.trades, Some(600));
        assert!(close(again.min_delta, whole.min_delta));
    }

    #[test]
    fn cached_aggregates_hide_trade_stats() {
        let mut dp = candle(&[(61_000, false, 1.0)]);
        dp.compacted_ids = Some((1, 1));
        let stats = BarStats::from_datapoint(&dp).unwrap();
        assert!(stats.trades.is_none() && stats.max_delta.is_none() && stats.avg_size().is_none());
    }
}

fn draw_text(frame: &mut canvas::Frame, text: &str, position: Point, size: f32, color: Color) {
    frame.fill_text(canvas::Text {
        content: text.to_string(),
        position,
        size: iced::Pixels(size),
        color,
        align_x: Alignment::Center.into(),
        align_y: Alignment::Center.into(),
        font: style::AZERET_MONO,
        ..canvas::Text::default()
    });
}
