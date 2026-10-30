//! CVD by order size (Hyblock "whale vs retail" style): under one price chart, a panel for
//! the perpetual and one for the spot market of the pair, each with a CVD line per taker
//! order size group. The indicator fetches the trades of both markets itself
//! (`FetchRange::RefTrades`, the usual cache / archive / REST path) over the last
//! `HISTORY_MS`, whatever the zoom, and polls the tail every `POLL_MS`. The fine split
//! (`data::chart::kline::SIZE_BUCKET_USD`) is summed into four groups when drawn, with spot
//! edges ten times lower (`size_group_edges`). Bars whose cached trades carry no split hold
//! the lines flat. Groups differ in volume by an order of magnitude, so each line is
//! normalized to the visible span; the axis shows the largest enabled group.
use crate::chart::{
    Basis, Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::{AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl},
        plot::{AnySeries, Plot, PlotTooltip, Series, TooltipFn, YScale},
    },
};
use crate::connector::fetcher::{FetchRange, is_trade_fetch_enabled, market_counterpart};

use data::chart::kline::{
    SIZE_BUCKETS, SIZE_GROUP_NAMES, SIZE_GROUPS, SizeCvdConfig, size_groups, trades_size_split,
};
use data::util::format_with_commas;
use exchange::{Timeframe, Trade, UnixMs, adapter::MarketKind};

use iced::widget::canvas::{self, Stroke};
use iced::widget::{center, column, row, text};
use std::{collections::BTreeMap, ops::RangeInclusive};

/// History fetched for both markets, independent of the zoom.
const HISTORY_MS: u64 = 3 * 24 * 60 * 60 * 1000;
/// Size of one history request (cache hits are fast, REST pages are not).
const CHUNK_MS: u64 = 60 * 60 * 1000;
/// Tail refresh period; the newest seconds are left to the next poll (exchange lag).
const POLL_MS: u64 = 10_000;
const TAIL_LAG_MS: u64 = 2_000;
/// A request with no answer after this long is considered lost and asked again.
const IN_FLIGHT_TIMEOUT_MS: u64 = 120_000;

/// Retail, medium, large, whales: muted, readable on the dark theme without glare.
const COLORS: [iced::Color; SIZE_GROUPS] = [
    iced::Color::from_rgb(0.47, 0.64, 0.72),
    iced::Color::from_rgb(0.76, 0.69, 0.47),
    iced::Color::from_rgb(0.78, 0.55, 0.44),
    iced::Color::from_rgb(0.64, 0.53, 0.76),
];

/// Cumulative fine split at one bar; grouped when drawn.
#[derive(Debug, Clone, Copy, Default)]
pub struct SizePoint {
    cum: [f32; SIZE_BUCKETS],
    /// Fine split of this bar alone; `None` when its trades carry no split.
    delta: Option<[f32; SIZE_BUCKETS]>,
}

fn grouped(fine: &[f32; SIZE_BUCKETS], spot: bool) -> [f32; SIZE_GROUPS] {
    size_groups(&fine.map(f64::from), spot).map(|v| v as f32)
}

/// Fine split of one bar; `None` once trades without a split landed in it.
type BarSplit = Option<[f64; SIZE_BUCKETS]>;

/// One market: per-bar splits over the covered stretch and what is being fetched.
#[derive(Default)]
struct Side {
    bars: BTreeMap<UnixMs, BarSplit>,
    /// Fetched stretch `[from, to)`, ms.
    covered: Option<(u64, u64)>,
    /// Outstanding request `(from, to, asked_at)`.
    in_flight: Option<(u64, u64, u64)>,
    series: BTreeMap<UnixMs, SizePoint>,
    cache: Caches,
}

impl Side {
    /// Next range to fetch: the tail first, then history back to `window_from`.
    fn next_range(&self, window_from: u64, tail_to: u64) -> Option<(u64, u64)> {
        let Some((from, to)) = self.covered else {
            return Some((window_from.max(tail_to.saturating_sub(CHUNK_MS)), tail_to));
        };
        if tail_to >= to + POLL_MS {
            return Some((to, tail_to.min(to + CHUNK_MS)));
        }
        if from > window_from {
            return Some((window_from.max(from.saturating_sub(CHUNK_MS)), from - 1));
        }
        None
    }

    fn insert(&mut self, (from, to): (u64, u64), trades: &[Trade], timeframe: Timeframe) {
        let mut by_bar: BTreeMap<UnixMs, Vec<Trade>> = BTreeMap::new();
        for trade in trades {
            by_bar
                .entry(trade.time.floor_to(timeframe))
                .or_default()
                .push(*trade);
        }
        for (bar, trades) in by_bar {
            let split = trades_size_split(&trades);
            let slot = self.bars.entry(bar).or_insert(Some([0.0; SIZE_BUCKETS]));
            *slot = match (*slot, split) {
                (Some(total), Some(part)) => Some(std::array::from_fn(|i| total[i] + part[i])),
                _ => None,
            };
        }
        // `to` is inclusive.
        let (from, to) = (from, to + 1);
        self.covered = Some(match self.covered {
            Some((a, b)) => (a.min(from), b.max(to)),
            None => (from, to),
        });
        self.rebuild();
    }

    /// Lines start at the first covered bar; bars without a split hold them flat.
    fn rebuild(&mut self) {
        self.series.clear();
        let Some((from, _)) = self.covered else {
            self.cache.clear_all();
            return;
        };
        let mut cum = [0.0_f64; SIZE_BUCKETS];
        let first = self.bars.range(UnixMs::new(from)..).next().map(|(t, _)| *t);
        for (time, split) in self.bars.range(first.unwrap_or(UnixMs::new(from))..) {
            if let Some(delta) = split {
                for (c, d) in cum.iter_mut().zip(delta) {
                    *c += d;
                }
            }
            self.series.insert(
                *time,
                SizePoint {
                    cum: cum.map(|v| v as f32),
                    delta: split.map(|d| d.map(|v| v as f32)),
                },
            );
        }
        self.cache.clear_all();
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

pub struct SizeCvdIndicator {
    /// `[chart's own market, the other market of the pair]`.
    sides: [Side; 2],
    timeframe: Option<Timeframe>,
    chart_is_spot: bool,
    config: SizeCvdConfig,
}

impl SizeCvdIndicator {
    pub fn new() -> Self {
        Self {
            sides: Default::default(),
            timeframe: None,
            chart_is_spot: false,
            config: SizeCvdConfig::default(),
        }
    }

    /// The largest enabled group sets the axis.
    fn axis_group(&self) -> Option<usize> {
        (0..SIZE_GROUPS).rev().find(|&i| self.config.buckets[i])
    }

    /// Legend and plot of one market.
    fn market_view<'a>(
        &'a self,
        side: &'a Side,
        spot: bool,
        axis: usize,
        chart: &'a ViewState,
        data_labels_always_visible: bool,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        let size = crate::style::text_size::TINY;
        let title = if spot { "Spot by size" } else { "Perp by size" };
        if side.series.is_empty() {
            let status = if side.in_flight.is_some() {
                "loading trades…"
            } else {
                "no trades yet"
            };
            return column![text(title).size(size), center(text(status).size(size))].into();
        }
        let enabled = self.config.buckets;
        let plot = SizeCvdPlot {
            tooltip: Box::new(move |p: &SizePoint, _next: Option<&SizePoint>| {
                let cum = grouped(&p.cum, spot);
                let delta = p.delta.map(|d| grouped(&d, spot));
                let lines: Vec<String> = (0..SIZE_GROUPS)
                    .rev()
                    .filter(|&i| enabled[i])
                    .map(|i| {
                        let bar = delta.map_or_else(
                            || "no split".to_string(),
                            |d| format_with_commas(f64::from(d[i])),
                        );
                        format!(
                            "{}: {} (bar {bar})",
                            SIZE_GROUP_NAMES[i],
                            format_with_commas(f64::from(cum[i]))
                        )
                    })
                    .collect();
                PlotTooltip::new(lines.join("\n"))
            }),
            enabled,
            axis,
            spot,
        };
        let plot = indicator_row(
            chart,
            &side.cache,
            data_labels_always_visible,
            plot,
            AnySeries::forward_unix_ms(&side.series),
            visible_range,
        );
        let last = side
            .series
            .last_key_value()
            .map(|(_, p)| grouped(&p.cum, spot));
        let mut legend = row![text(title).size(size)].spacing(10);
        for i in (0..SIZE_GROUPS).rev().filter(|&i| enabled[i]) {
            let value = last.map_or_else(
                || "—".to_string(),
                |cum| {
                    let v = cum[i];
                    let sign = if v >= 0.0 { "+" } else { "" };
                    format!("{sign}{}", format_with_commas(f64::from(v)))
                },
            );
            legend = legend.push(
                text(format!("━ {} {value}", SIZE_GROUP_NAMES[i]))
                    .size(size)
                    .color(COLORS[i]),
            );
        }
        legend =
            legend.push(text(format!("axis: {}, normalized", SIZE_GROUP_NAMES[axis])).size(size));
        column![legend, plot].into()
    }
}

impl KlineIndicatorImpl for SizeCvdIndicator {
    fn clear_all_caches(&mut self) {
        for side in &mut self.sides {
            side.cache.clear_all();
        }
    }

    fn clear_crosshair_caches(&mut self) {
        for side in &mut self.sides {
            side.cache.clear_crosshair();
        }
    }

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        data_labels_always_visible: bool,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        if let Some(message) = self.unavailable_message(chart, "CVD by order size") {
            return center(text(message)).into();
        }
        if !is_trade_fetch_enabled() {
            return center(text("CVD by order size needs trade fetching enabled.")).into();
        }
        let Some(axis) = self.axis_group() else {
            return center(text("Enable at least one size group in the settings.")).into();
        };
        if visible_range.end() < visible_range.start() {
            return row![].into();
        }
        let chart_is_spot = chart.ticker_info.market_type() == MarketKind::Spot;
        let has_pair = market_counterpart(chart.ticker_info).is_some();
        // Perp on top, spot below, whichever market the chart shows.
        let (perp, spot) = if chart_is_spot {
            (has_pair.then_some(&self.sides[1]), Some(&self.sides[0]))
        } else {
            (Some(&self.sides[0]), has_pair.then_some(&self.sides[1]))
        };
        let mut panels = column![].spacing(4);
        for (side, is_spot) in [(perp, false), (spot, true)] {
            if let Some(side) = side {
                panels = panels.push(self.market_view(
                    side,
                    is_spot,
                    axis,
                    chart,
                    data_labels_always_visible,
                    visible_range.clone(),
                ));
            }
        }
        panels.into()
    }

    fn availability(&self, chart: &ViewState) -> IndicatorAvailability {
        match chart.basis {
            Basis::Tick(_) => {
                IndicatorAvailability::Unavailable(AvailabilityCause::Basis(chart.basis))
            }
            Basis::Time(_) => IndicatorAvailability::Available,
        }
    }

    fn fetch_range(&mut self, ctx: &FetchCtx) -> Option<FetchRange> {
        if !is_trade_fetch_enabled() {
            return None;
        }
        let chart_is_spot = ctx.main_chart.ticker_info.market_type() == MarketKind::Spot;
        if self.timeframe != Some(ctx.timeframe) || self.chart_is_spot != chart_is_spot {
            self.timeframe = Some(ctx.timeframe);
            self.chart_is_spot = chart_is_spot;
            for side in &mut self.sides {
                side.reset();
            }
        }
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let tail_to = (now - TAIL_LAG_MS) / POLL_MS * POLL_MS;
        let window_from = tail_to.saturating_sub(HISTORY_MS);
        let has_pair = market_counterpart(ctx.main_chart.ticker_info).is_some();

        for side in &mut self.sides {
            if side
                .in_flight
                .is_some_and(|(_, _, asked)| now.saturating_sub(asked) > IN_FLIGHT_TIMEOUT_MS)
            {
                side.in_flight = None;
            }
        }
        // Tails of both markets first, then history, one request per market at a time.
        let candidates = (0..2)
            .filter(|&i| i == 0 || has_pair)
            .filter(|&i| self.sides[i].in_flight.is_none())
            .filter_map(|i| {
                let range = self.sides[i].next_range(window_from, tail_to)?;
                let is_tail = self.sides[i].covered.is_none_or(|(_, to)| range.0 >= to);
                Some((!is_tail, i, range))
            })
            .min()?;
        let (_, i, (from, to)) = candidates;
        self.sides[i].in_flight = Some((from, to, now));
        Some(FetchRange::RefTrades {
            from: UnixMs::new(from),
            to: UnixMs::new(to),
            spot: i == 1,
        })
    }

    fn on_ref_trades(
        &mut self,
        spot: bool,
        range: (UnixMs, UnixMs),
        complete: bool,
        trades: &[Trade],
    ) {
        let Some(timeframe) = self.timeframe else {
            return;
        };
        let side = &mut self.sides[usize::from(spot)];
        let range = (range.0.as_u64(), range.1.as_u64());
        if side
            .in_flight
            .is_some_and(|(from, to, _)| (from, to) == range)
        {
            side.in_flight = None;
        }
        // An incomplete range is asked again whole; the fetched part is cached by then.
        if complete {
            side.insert(range, trades, timeframe);
        }
    }

    fn on_config_changed(&mut self, config: &data::chart::kline::Config) {
        if self.config != config.indicator_configs.size_cvd {
            self.config = config.indicator_configs.size_cvd;
            self.clear_all_caches();
        }
    }
}

struct SizeCvdPlot {
    tooltip: TooltipFn<SizePoint>,
    enabled: [bool; SIZE_GROUPS],
    /// Group drawn on the panel's own axis; the others are rescaled to its span.
    axis: usize,
    spot: bool,
}

/// Inner share of the panel height used by the lines.
const PAD: f32 = 0.08;

impl<S> Plot<S> for SizeCvdPlot
where
    S: Series<Y = SizePoint>,
{
    fn y_extents(&self, s: &S, range: RangeInclusive<u64>) -> Option<(f32, f32)> {
        let mut extents: Option<(f32, f32)> = None;
        s.for_each_in(range, |_, p| {
            let v = grouped(&p.cum, self.spot)[self.axis];
            let (lo, hi) = extents.unwrap_or((v, v));
            extents = Some((lo.min(v), hi.max(v)));
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
        let span = scale.max - scale.min;
        let (target_lo, target_hi) = (
            scale.min + span * PAD / (1.0 + 2.0 * PAD),
            scale.max - span * PAD / (1.0 + 2.0 * PAD),
        );
        let half_bar = ctx.cell_width * 0.45;

        for group in (0..SIZE_GROUPS).filter(|&i| self.enabled[i]) {
            let mut extents: Option<(f32, f32)> = None;
            s.for_each_in(range.clone(), |_, p| {
                let v = grouped(&p.cum, self.spot)[group];
                let (lo, hi) = extents.unwrap_or((v, v));
                extents = Some((lo.min(v), hi.max(v)));
            });
            let to_axis = |v: f32| {
                if group == self.axis {
                    return v;
                }
                match extents {
                    Some((lo, hi)) if hi > lo => {
                        target_lo + (v - lo) / (hi - lo) * (target_hi - target_lo)
                    }
                    _ => (target_lo + target_hi) / 2.0,
                }
            };
            let mut path = canvas::path::Builder::new();
            let mut started = false;
            s.for_each_in(range.clone(), |x, p| {
                let pt = iced::Point::new(
                    ctx.interval_to_x(x) + half_bar,
                    scale.to_y(to_axis(grouped(&p.cum, self.spot)[group])),
                );
                if started {
                    path.line_to(pt);
                } else {
                    path.move_to(pt);
                    started = true;
                }
            });
            // The largest players on top, a little thicker.
            let width = if group == SIZE_GROUPS - 1 { 1.8 } else { 1.3 };
            frame.stroke(
                &path.build(),
                Stroke::default()
                    .with_width(width)
                    .with_color(COLORS[group].scale_alpha(0.9)),
            );
        }
    }

    fn tooltip_fn(&self) -> Option<&TooltipFn<SizePoint>> {
        Some(&self.tooltip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::unit::{Price, Qty};

    fn trade(id: u64, time: u64, qty: f64, is_sell: bool) -> Trade {
        Trade {
            id: Some(id),
            time: UnixMs::new(time),
            is_sell,
            price: Price::from_f64(100_000.0),
            qty: Qty::from_f64(qty),
        }
    }

    #[test]
    fn fetch_plan_goes_tail_first_then_history() {
        let mut side = Side::default();
        let (window_from, tail_to) = (0, 10 * CHUNK_MS);
        assert_eq!(
            side.next_range(window_from, tail_to),
            Some((9 * CHUNK_MS, 10 * CHUNK_MS))
        );
        side.insert((9 * CHUNK_MS, 10 * CHUNK_MS), &[], Timeframe::M15);
        assert_eq!(
            side.next_range(window_from, tail_to),
            Some((8 * CHUNK_MS, 9 * CHUNK_MS - 1))
        );
        // A newer tail comes before more history.
        assert_eq!(
            side.next_range(window_from, tail_to + POLL_MS + 1),
            Some((10 * CHUNK_MS + 1, tail_to + POLL_MS + 1))
        );
    }

    #[test]
    fn bars_accumulate_and_hold_through_gaps() {
        let mut side = Side::default();
        let m15 = Timeframe::M15.to_milliseconds();
        side.insert(
            (0, 3 * m15 - 1),
            &[trade(1, 10, 1.0, false), trade(2, 2 * m15 + 5, 0.5, true)],
            Timeframe::M15,
        );
        assert_eq!(side.series.len(), 2);
        let last = side.series.values().last().unwrap();
        let sum: f32 = grouped(&last.cum, false).iter().sum();
        assert!((sum - 0.5).abs() < 1e-6);
    }
}
