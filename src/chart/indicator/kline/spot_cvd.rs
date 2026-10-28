//! CVD Futures + Spot (Coinglass style): this perpetual's CVD and the spot pair's CVD
//! (Binance spot klines carry the taker-buy volume) in one panel. Both start from the
//! first bar present in both series. Spot volume is several times smaller than perp
//! volume, so each line is normalized to the visible span; the axis shows futures
//! values, the legend and tooltip show both exactly.
//!
//! On a spot chart the roles swap: the chart's own bars are the spot line and the
//! perpetual's klines are fetched as the reference (`market_counterpart`).
use crate::chart::{
    Basis, Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::{AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl},
        plot::{AnySeries, Plot, PlotTooltip, Series, TooltipFn, YScale},
    },
};
use crate::connector::fetcher::{FetchRange, market_counterpart};

use data::chart::{PlotData, kline::KlineDataPoint};
use data::util::format_with_commas;
use exchange::{Kline, Timeframe, Trade, UnixMs};

use iced::widget::canvas::{self, Stroke};
use iced::widget::{center, column, row, text};
use std::{collections::BTreeMap, ops::RangeInclusive};

/// Live refresh period of the forming spot bar.
const POLL_MS: u64 = 10_000;

/// Coinglass colours: futures blue, spot orange.
const FUTURES_COLOR: iced::Color = iced::Color::from_rgb(0.31, 0.56, 0.97);
const SPOT_COLOR: iced::Color = iced::Color::from_rgb(0.97, 0.62, 0.25);

/// Cumulative values at one bar: `own` is the chart's market, `other` the reference one.
#[derive(Debug, Clone, Copy, Default)]
pub struct CvdPoint {
    other: f32,
    own: f32,
    /// False when the reference bar is not loaded yet (its line holds its value).
    has_other: bool,
}

pub struct SpotCvdIndicator {
    cache: Caches,
    timeframe: Option<Timeframe>,
    /// The chart shows spot, so the reference series is the perpetual.
    chart_is_spot: bool,
    /// Per-bar delta of the reference market's klines.
    other: BTreeMap<UnixMs, f64>,
    /// Per-bar delta of the chart's own bars.
    own: BTreeMap<UnixMs, f64>,
    series: BTreeMap<UnixMs, CvdPoint>,
}

impl SpotCvdIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            timeframe: None,
            chart_is_spot: false,
            other: BTreeMap::new(),
            own: BTreeMap::new(),
            series: BTreeMap::new(),
        }
    }

    /// (futures, spot) values of a point.
    fn futures_spot(&self, p: &CvdPoint) -> (f32, f32) {
        if self.chart_is_spot {
            (p.other, p.own)
        } else {
            (p.own, p.other)
        }
    }

    fn availability_for(basis: Basis, ticker_info: exchange::TickerInfo) -> IndicatorAvailability {
        match basis {
            Basis::Tick(_) => IndicatorAvailability::Unavailable(AvailabilityCause::Basis(basis)),
            Basis::Time(_) if market_counterpart(ticker_info).is_none() => {
                IndicatorAvailability::Unavailable(AvailabilityCause::Exchange(
                    ticker_info.exchange(),
                ))
            }
            Basis::Time(_) => IndicatorAvailability::Available,
        }
    }

    fn bar_delta(dp: &KlineDataPoint) -> f64 {
        if let Some((buy, sell)) = dp.kline.volume.buy_sell() {
            return buy.to_f64() - sell.to_f64();
        }
        dp.footprint
            .trades
            .values()
            .map(|t| t.buy_qty.to_f64() - t.sell_qty.to_f64())
            .sum()
    }

    /// Refreshes the chart's own deltas from bar `from_index` on.
    fn sync_own(&mut self, source: &PlotData<KlineDataPoint>, from_index: usize) {
        let PlotData::TimeBased(series) = source else {
            self.own.clear();
            self.rebuild_series();
            return;
        };
        if from_index == 0 {
            self.own.clear();
        }
        for (time, dp) in series.datapoints.iter().skip(from_index) {
            self.own.insert(*time, Self::bar_delta(dp));
        }
        self.rebuild_series();
    }

    fn rebuild_series(&mut self) {
        self.series.clear();
        let (Some((&other_first, _)), Some((&own_first, _))) =
            (self.other.first_key_value(), self.own.first_key_value())
        else {
            self.clear_all_caches();
            return;
        };
        let start = other_first.max(own_first);
        let (mut other_cum, mut own_cum) = (0.0_f64, 0.0_f64);
        for (time, own_delta) in self.own.range(start..) {
            own_cum += own_delta;
            let other_delta = self.other.get(time);
            other_cum += other_delta.copied().unwrap_or(0.0);
            self.series.insert(
                *time,
                CvdPoint {
                    other: other_cum as f32,
                    own: own_cum as f32,
                    has_other: other_delta.is_some(),
                },
            );
        }
        self.clear_all_caches();
    }
}

impl KlineIndicatorImpl for SpotCvdIndicator {
    fn clear_all_caches(&mut self) {
        self.cache.clear_all();
    }

    fn clear_crosshair_caches(&mut self) {
        self.cache.clear_crosshair();
    }

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        data_labels_always_visible: bool,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        if let Some(message) = self.unavailable_message(chart, "CVD Futures + Spot") {
            return center(text(message)).into();
        }
        if visible_range.end() < visible_range.start() {
            return row![].into();
        }
        let chart_is_spot = self.chart_is_spot;
        let (own_color, other_color) = if chart_is_spot {
            (SPOT_COLOR, FUTURES_COLOR)
        } else {
            (FUTURES_COLOR, SPOT_COLOR)
        };
        let plot = DualCvdPlot {
            tooltip: Box::new(move |p: &CvdPoint, _next: Option<&CvdPoint>| {
                let own = format_with_commas(f64::from(p.own));
                let other = if p.has_other {
                    format_with_commas(f64::from(p.other))
                } else {
                    "loading".to_string()
                };
                let (futures, spot) = if chart_is_spot {
                    (other, own)
                } else {
                    (own, other)
                };
                PlotTooltip::new(format!("Futures CVD: {futures}\nSpot CVD: {spot}"))
            }),
            own_color,
            other_color,
        };
        let plot = indicator_row(
            chart,
            &self.cache,
            data_labels_always_visible,
            plot,
            AnySeries::forward_unix_ms(&self.series),
            visible_range,
        );
        let last = self.series.last_key_value().map(|(_, p)| *p);
        let value = |v: Option<f32>| {
            v.map_or_else(
                || "—".to_string(),
                |v| {
                    let sign = if v >= 0.0 { "+" } else { "" };
                    format!("{sign}{}", format_with_commas(f64::from(v)))
                },
            )
        };
        let size = crate::style::text_size::TINY;
        let futures = last
            .filter(|p| !chart_is_spot || p.has_other)
            .map(|p| self.futures_spot(&p).0);
        let spot = last
            .filter(|p| chart_is_spot || p.has_other)
            .map(|p| self.futures_spot(&p).1);
        let legend = row![
            text("CVD").size(size),
            text(format!("━ Futures {}", value(futures)))
                .size(size)
                .color(FUTURES_COLOR),
            text(format!("━ Spot {}", value(spot)))
                .size(size)
                .color(SPOT_COLOR),
            text("normalized").size(size),
        ]
        .spacing(10);
        column![legend, plot].into()
    }

    fn availability(&self, chart: &ViewState) -> IndicatorAvailability {
        Self::availability_for(chart.basis, chart.ticker_info)
    }

    fn fetch_range(&mut self, ctx: &FetchCtx) -> Option<FetchRange> {
        let availability =
            Self::availability_for(Basis::Time(ctx.timeframe), ctx.main_chart.ticker_info);
        if !matches!(availability, IndicatorAvailability::Available) {
            return None;
        }
        let timeframe = ctx.timeframe;
        let chart_is_spot =
            ctx.main_chart.ticker_info.exchange() == exchange::adapter::Exchange::BinanceSpot;
        if self.timeframe != Some(timeframe) || self.chart_is_spot != chart_is_spot {
            self.timeframe = Some(timeframe);
            self.chart_is_spot = chart_is_spot;
            self.other.clear();
            self.rebuild_series();
        }

        let (Some((&other_first, _)), Some((&other_last, _))) =
            (self.other.first_key_value(), self.other.last_key_value())
        else {
            return Some(FetchRange::RefKlines {
                from: ctx.prefetch_earliest,
                to: ctx.kline_latest,
                timeframe,
                spot: true,
            });
        };

        if ctx.visible_earliest < other_first {
            return Some(FetchRange::RefKlines {
                from: ctx.prefetch_earliest,
                to: other_first.saturating_sub(1),
                timeframe,
                spot: true,
            });
        }

        // Re-read the forming bar; `to` moves in POLL_MS steps, so the request
        // handler's cooldown on identical ranges limits the polling rate.
        let now = chrono::Utc::now().timestamp_millis() as u64;
        Some(FetchRange::RefKlines {
            from: other_last,
            to: UnixMs::new(now - now % POLL_MS).max(other_last.saturating_add(1)),
            timeframe,
            spot: true,
        })
    }

    fn rebuild_from_source(&mut self, source: &PlotData<KlineDataPoint>) {
        self.sync_own(source, 0);
    }

    fn on_insert_klines(&mut self, _klines: &[Kline], source: &PlotData<KlineDataPoint>) {
        self.sync_own(source, 0);
    }

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        old_dp_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        self.sync_own(source, old_dp_len.saturating_sub(1));
    }

    fn on_ticksize_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.sync_own(source, 0);
    }

    fn on_basis_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.other.clear();
        self.sync_own(source, 0);
    }

    fn on_ref_klines(&mut self, spot: bool, timeframe: Timeframe, klines: &[Kline]) {
        if !spot || self.timeframe != Some(timeframe) {
            return;
        }
        self.other.extend(klines.iter().map(|k| {
            let delta = k
                .volume
                .buy_sell()
                .map_or(0.0, |(buy, sell)| buy.to_f64() - sell.to_f64());
            (k.time, delta)
        }));
        self.rebuild_series();
    }
}

/// The chart's own line on the panel's axis, the reference line rescaled to the same
/// vertical span.
struct DualCvdPlot {
    tooltip: TooltipFn<CvdPoint>,
    own_color: iced::Color,
    other_color: iced::Color,
}

/// Inner share of the panel height used by the lines.
const PAD: f32 = 0.08;

impl<S> Plot<S> for DualCvdPlot
where
    S: Series<Y = CvdPoint>,
{
    fn y_extents(&self, s: &S, range: RangeInclusive<u64>) -> Option<(f32, f32)> {
        let mut extents: Option<(f32, f32)> = None;
        s.for_each_in(range, |_, p| {
            let (lo, hi) = extents.unwrap_or((p.own, p.own));
            extents = Some((lo.min(p.own), hi.max(p.own)));
        });
        extents
    }

    fn adjust_extents(&self, min: f32, max: f32) -> (f32, f32) {
        let pad = (max - min) * PAD;
        (min - pad, max + pad)
    }

    fn draw(
        &self,
        frame: &mut canvas::Frame,
        ctx: &ViewState,
        _theme: &iced::Theme,
        s: &S,
        range: RangeInclusive<u64>,
        scale: &YScale,
    ) {
        let mut other_extents: Option<(f32, f32)> = None;
        s.for_each_in(range.clone(), |_, p| {
            if p.has_other {
                let (lo, hi) = other_extents.unwrap_or((p.other, p.other));
                other_extents = Some((lo.min(p.other), hi.max(p.other)));
            }
        });
        // Map reference values onto the unpadded span of the axis.
        let span = scale.max - scale.min;
        let (target_lo, target_hi) = (
            scale.min + span * PAD / (1.0 + 2.0 * PAD),
            scale.max - span * PAD / (1.0 + 2.0 * PAD),
        );
        let other_to_axis = |v: f32| match other_extents {
            Some((lo, hi)) if hi > lo => target_lo + (v - lo) / (hi - lo) * (target_hi - target_lo),
            _ => (target_lo + target_hi) / 2.0,
        };

        let half_bar = ctx.cell_width * 0.45;
        let x_for = |x: u64| ctx.interval_to_x(x) + half_bar;

        let mut own_path = canvas::path::Builder::new();
        let mut other_path = canvas::path::Builder::new();
        let (mut own_started, mut other_started) = (false, false);
        s.for_each_in(range, |x, p| {
            let px = x_for(x);
            let own_pt = iced::Point::new(px, scale.to_y(p.own));
            if own_started {
                own_path.line_to(own_pt);
            } else {
                own_path.move_to(own_pt);
                own_started = true;
            }
            if p.has_other {
                let other_pt = iced::Point::new(px, scale.to_y(other_to_axis(p.other)));
                if other_started {
                    other_path.line_to(other_pt);
                } else {
                    other_path.move_to(other_pt);
                    other_started = true;
                }
            }
        });
        frame.stroke(
            &own_path.build(),
            Stroke::default().with_width(1.5).with_color(self.own_color),
        );
        frame.stroke(
            &other_path.build(),
            Stroke::default()
                .with_width(1.5)
                .with_color(self.other_color),
        );
    }

    fn tooltip_fn(&self) -> Option<&TooltipFn<CvdPoint>> {
        Some(&self.tooltip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulates_from_first_common_bar() {
        let mut indicator = SpotCvdIndicator::new();
        for (t, d) in [(0, 5.0), (60_000, 1.0), (120_000, -2.0)] {
            indicator.own.insert(UnixMs::new(t), d);
        }
        indicator.other.insert(UnixMs::new(60_000), 3.0);
        indicator.rebuild_series();

        assert_eq!(indicator.series.len(), 2);
        let first = indicator.series[&UnixMs::new(60_000)];
        assert_eq!((first.other, first.own, first.has_other), (3.0, 1.0, true));
        let last = indicator.series[&UnixMs::new(120_000)];
        assert_eq!((last.other, last.own, last.has_other), (3.0, -1.0, false));

        // On a spot chart the own line is spot.
        indicator.chart_is_spot = true;
        assert_eq!(indicator.futures_spot(&last), (3.0, -1.0));
    }
}
