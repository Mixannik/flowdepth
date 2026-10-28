//! CVD Futures + Spot (Coinglass style): this perpetual's CVD and the spot pair's CVD
//! (Binance spot klines carry the taker-buy volume) in one panel. Both start from the
//! first bar present in both series. Spot volume is several times smaller than perp
//! volume, so each line is normalized to the visible span; the axis shows futures
//! values, the legend and tooltip show both exactly.
use crate::chart::{
    Basis, Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::{AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl},
        plot::{AnySeries, Plot, PlotTooltip, Series, TooltipFn, YScale},
    },
};
use crate::connector::fetcher::{FetchRange, spot_counterpart};

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

/// Cumulative values at one bar.
#[derive(Debug, Clone, Copy, Default)]
pub struct CvdPoint {
    spot: f32,
    perp: f32,
    /// False when the spot bar is not loaded yet (the spot line holds its value).
    has_spot: bool,
}

pub struct SpotCvdIndicator {
    cache: Caches,
    timeframe: Option<Timeframe>,
    /// Per-bar delta of the spot pair.
    spot: BTreeMap<UnixMs, f64>,
    /// Per-bar delta of the chart (perp) bars.
    perp: BTreeMap<UnixMs, f64>,
    series: BTreeMap<UnixMs, CvdPoint>,
}

impl SpotCvdIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            timeframe: None,
            spot: BTreeMap::new(),
            perp: BTreeMap::new(),
            series: BTreeMap::new(),
        }
    }

    fn availability_for(basis: Basis, ticker_info: exchange::TickerInfo) -> IndicatorAvailability {
        match basis {
            Basis::Tick(_) => IndicatorAvailability::Unavailable(AvailabilityCause::Basis(basis)),
            Basis::Time(_) if spot_counterpart(ticker_info).is_none() => {
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

    /// Refreshes perp deltas from bar `from_index` on.
    fn sync_perp(&mut self, source: &PlotData<KlineDataPoint>, from_index: usize) {
        let PlotData::TimeBased(series) = source else {
            self.perp.clear();
            self.rebuild_series();
            return;
        };
        if from_index == 0 {
            self.perp.clear();
        }
        for (time, dp) in series.datapoints.iter().skip(from_index) {
            self.perp.insert(*time, Self::bar_delta(dp));
        }
        self.rebuild_series();
    }

    fn rebuild_series(&mut self) {
        self.series.clear();
        let (Some((&spot_first, _)), Some((&perp_first, _))) =
            (self.spot.first_key_value(), self.perp.first_key_value())
        else {
            self.clear_all_caches();
            return;
        };
        let start = spot_first.max(perp_first);
        let (mut spot_cum, mut perp_cum) = (0.0_f64, 0.0_f64);
        for (time, perp_delta) in self.perp.range(start..) {
            perp_cum += perp_delta;
            let spot_delta = self.spot.get(time);
            spot_cum += spot_delta.copied().unwrap_or(0.0);
            self.series.insert(
                *time,
                CvdPoint {
                    spot: spot_cum as f32,
                    perp: perp_cum as f32,
                    has_spot: spot_delta.is_some(),
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
        let plot = DualCvdPlot {
            tooltip: Box::new(|p: &CvdPoint, _next: Option<&CvdPoint>| {
                let spot = if p.has_spot {
                    format_with_commas(f64::from(p.spot))
                } else {
                    "loading".to_string()
                };
                PlotTooltip::new(format!(
                    "Futures CVD: {}\nSpot CVD: {spot}",
                    format_with_commas(f64::from(p.perp))
                ))
            }),
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
        let legend = row![
            text("CVD").size(size),
            text(format!("━ Futures {}", value(last.map(|p| p.perp))))
                .size(size)
                .color(FUTURES_COLOR),
            text(format!(
                "━ Spot {}",
                value(last.filter(|p| p.has_spot).map(|p| p.spot))
            ))
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
        if self.timeframe != Some(timeframe) {
            self.timeframe = Some(timeframe);
            self.spot.clear();
            self.rebuild_series();
        }

        let (Some((&spot_first, _)), Some((&spot_last, _))) =
            (self.spot.first_key_value(), self.spot.last_key_value())
        else {
            return Some(FetchRange::RefKlines {
                from: ctx.prefetch_earliest,
                to: ctx.kline_latest,
                timeframe,
                spot: true,
            });
        };

        if ctx.visible_earliest < spot_first {
            return Some(FetchRange::RefKlines {
                from: ctx.prefetch_earliest,
                to: spot_first.saturating_sub(1),
                timeframe,
                spot: true,
            });
        }

        // Re-read the forming bar; `to` moves in POLL_MS steps, so the request
        // handler's cooldown on identical ranges limits the polling rate.
        let now = chrono::Utc::now().timestamp_millis() as u64;
        Some(FetchRange::RefKlines {
            from: spot_last,
            to: UnixMs::new(now - now % POLL_MS).max(spot_last.saturating_add(1)),
            timeframe,
            spot: true,
        })
    }

    fn rebuild_from_source(&mut self, source: &PlotData<KlineDataPoint>) {
        self.sync_perp(source, 0);
    }

    fn on_insert_klines(&mut self, _klines: &[Kline], source: &PlotData<KlineDataPoint>) {
        self.sync_perp(source, 0);
    }

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        old_dp_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        self.sync_perp(source, old_dp_len.saturating_sub(1));
    }

    fn on_ticksize_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.sync_perp(source, 0);
    }

    fn on_basis_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.spot.clear();
        self.sync_perp(source, 0);
    }

    fn on_ref_klines(&mut self, klines: &[Kline]) {
        self.spot.extend(klines.iter().map(|k| {
            let delta = k
                .volume
                .buy_sell()
                .map_or(0.0, |(buy, sell)| buy.to_f64() - sell.to_f64());
            (k.time, delta)
        }));
        self.rebuild_series();
    }
}

/// Perp line on the panel's axis, spot line rescaled to the same vertical span.
struct DualCvdPlot {
    tooltip: TooltipFn<CvdPoint>,
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
            let (lo, hi) = extents.unwrap_or((p.perp, p.perp));
            extents = Some((lo.min(p.perp), hi.max(p.perp)));
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
        let mut spot_extents: Option<(f32, f32)> = None;
        s.for_each_in(range.clone(), |_, p| {
            if p.has_spot {
                let (lo, hi) = spot_extents.unwrap_or((p.spot, p.spot));
                spot_extents = Some((lo.min(p.spot), hi.max(p.spot)));
            }
        });
        // Map spot values onto the unpadded perp span of the axis.
        let span = scale.max - scale.min;
        let (target_lo, target_hi) = (
            scale.min + span * PAD / (1.0 + 2.0 * PAD),
            scale.max - span * PAD / (1.0 + 2.0 * PAD),
        );
        let spot_to_axis = |v: f32| match spot_extents {
            Some((lo, hi)) if hi > lo => target_lo + (v - lo) / (hi - lo) * (target_hi - target_lo),
            _ => (target_lo + target_hi) / 2.0,
        };

        let half_bar = ctx.cell_width * 0.45;
        let x_for = |x: u64| ctx.interval_to_x(x) + half_bar;

        let mut perp_path = canvas::path::Builder::new();
        let mut spot_path = canvas::path::Builder::new();
        let (mut perp_started, mut spot_started) = (false, false);
        s.for_each_in(range, |x, p| {
            let px = x_for(x);
            let perp_pt = iced::Point::new(px, scale.to_y(p.perp));
            if perp_started {
                perp_path.line_to(perp_pt);
            } else {
                perp_path.move_to(perp_pt);
                perp_started = true;
            }
            if p.has_spot {
                let spot_pt = iced::Point::new(px, scale.to_y(spot_to_axis(p.spot)));
                if spot_started {
                    spot_path.line_to(spot_pt);
                } else {
                    spot_path.move_to(spot_pt);
                    spot_started = true;
                }
            }
        });
        frame.stroke(
            &perp_path.build(),
            Stroke::default().with_width(1.5).with_color(FUTURES_COLOR),
        );
        frame.stroke(
            &spot_path.build(),
            Stroke::default().with_width(1.5).with_color(SPOT_COLOR),
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
            indicator.perp.insert(UnixMs::new(t), d);
        }
        indicator.spot.insert(UnixMs::new(60_000), 3.0);
        indicator.rebuild_series();

        assert_eq!(indicator.series.len(), 2);
        let first = indicator.series[&UnixMs::new(60_000)];
        assert_eq!((first.spot, first.perp, first.has_spot), (3.0, 1.0, true));
        let last = indicator.series[&UnixMs::new(120_000)];
        assert_eq!((last.spot, last.perp, last.has_spot), (3.0, -1.0, false));
    }
}
