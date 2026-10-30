//! CVD by order size (Hyblock "whale vs retail" style): one CVD line per taker order size
//! group. Bars keep a fine split (`data::chart::kline::SIZE_BUCKET_USD`, raw executions or
//! cached aggregates); it is summed into four groups here, with edges ten times lower on
//! spot (`size_group_edges`). Bars without a split hold the lines flat. Groups differ in
//! volume by an order of magnitude, so each line is normalized to the visible span; the
//! axis shows the largest enabled group, the legend and tooltip show every value exactly.
use crate::chart::{
    Basis, Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::{AvailabilityCause, IndicatorAvailability, KlineIndicatorImpl},
        plot::{AnySeries, Plot, PlotTooltip, Series, TooltipFn, YScale},
    },
};

use data::chart::{
    PlotData,
    kline::{
        KlineDataPoint, SIZE_BUCKETS, SIZE_GROUP_NAMES, SIZE_GROUPS, SizeCvdConfig, size_groups,
    },
};
use data::util::format_with_commas;
use exchange::{Kline, Trade, UnixMs, adapter::MarketKind};

use iced::widget::canvas::{self, Stroke};
use iced::widget::{center, column, row, text};
use std::{collections::BTreeMap, ops::RangeInclusive};

/// Retail, medium, large, whales: muted, readable on the dark theme without glare.
const COLORS: [iced::Color; SIZE_GROUPS] = [
    iced::Color::from_rgb(0.47, 0.64, 0.72),
    iced::Color::from_rgb(0.76, 0.69, 0.47),
    iced::Color::from_rgb(0.78, 0.55, 0.44),
    iced::Color::from_rgb(0.64, 0.53, 0.76),
];

/// Cumulative fine split at one bar; grouped when drawn, where the market is known.
#[derive(Debug, Clone, Copy, Default)]
pub struct SizePoint {
    cum: [f32; SIZE_BUCKETS],
    /// Fine split of this bar alone; `None` when the bar has no split.
    delta: Option<[f32; SIZE_BUCKETS]>,
}

fn grouped(fine: &[f32; SIZE_BUCKETS], spot: bool) -> [f32; SIZE_GROUPS] {
    size_groups(&fine.map(f64::from), spot).map(|v| v as f32)
}

pub struct SizeCvdIndicator {
    cache: Caches,
    /// Fine per-bar split, `None` for bars without one.
    deltas: BTreeMap<UnixMs, Option<[f64; SIZE_BUCKETS]>>,
    series: BTreeMap<UnixMs, SizePoint>,
    config: SizeCvdConfig,
}

impl SizeCvdIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            deltas: BTreeMap::new(),
            series: BTreeMap::new(),
            config: SizeCvdConfig::default(),
        }
    }

    fn bar_split(dp: &KlineDataPoint) -> Option<[f64; SIZE_BUCKETS]> {
        dp.size_delta()
    }

    fn sync_all(&mut self, source: &PlotData<KlineDataPoint>) {
        self.deltas.clear();
        if let PlotData::TimeBased(series) = source {
            for (time, dp) in &series.datapoints {
                self.deltas.insert(*time, Self::bar_split(dp));
            }
        }
        self.rebuild_series();
    }

    /// Bars hit by `trades` (backfill can land in old ones) and every bar from
    /// `from_index` on.
    fn sync_trades(
        &mut self,
        trades: &[Trade],
        from_index: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        let PlotData::TimeBased(series) = source else {
            return;
        };
        let mut affected: Vec<UnixMs> = trades
            .iter()
            .map(|t| t.time.floor_to(series.interval))
            .collect();
        affected.sort_unstable();
        affected.dedup();
        for time in affected {
            if let Some(dp) = series.datapoints.get(&time) {
                self.deltas.insert(time, Self::bar_split(dp));
            }
        }
        for (time, dp) in series.datapoints.iter().skip(from_index) {
            self.deltas.insert(*time, Self::bar_split(dp));
        }
        self.rebuild_series();
    }

    /// Lines start at the first bar with a split; bars without one hold them flat.
    fn rebuild_series(&mut self) {
        self.series.clear();
        let mut cum = [0.0_f64; SIZE_BUCKETS];
        let mut started = false;
        for (time, split) in &self.deltas {
            started |= split.is_some();
            if !started {
                continue;
            }
            let delta = *split;
            if let Some(delta) = delta {
                for (c, d) in cum.iter_mut().zip(delta) {
                    *c += d;
                }
            }
            self.series.insert(
                *time,
                SizePoint {
                    cum: cum.map(|v| v as f32),
                    delta: delta.map(|d| d.map(|v| v as f32)),
                },
            );
        }
        self.clear_all_caches();
    }

    /// The largest enabled group sets the axis.
    fn axis_group(&self) -> Option<usize> {
        (0..SIZE_GROUPS).rev().find(|&i| self.config.buckets[i])
    }
}

impl KlineIndicatorImpl for SizeCvdIndicator {
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
        if let Some(message) = self.unavailable_message(chart, "CVD by order size") {
            return center(text(message)).into();
        }
        let Some(axis) = self.axis_group() else {
            return center(text("Enable at least one size group in the settings.")).into();
        };
        if self.series.is_empty() {
            return center(text("CVD by order size needs raw trades; none loaded yet.")).into();
        }
        if visible_range.end() < visible_range.start() {
            return row![].into();
        }
        let enabled = self.config.buckets;
        let spot = chart.ticker_info.market_type() == MarketKind::Spot;
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
            &self.cache,
            data_labels_always_visible,
            plot,
            AnySeries::forward_unix_ms(&self.series),
            visible_range,
        );
        let last = self.series.last_key_value().map(|(_, p)| *p);
        let size = crate::style::text_size::TINY;
        let title = if spot {
            "CVD by size · spot"
        } else {
            "CVD by size · perp"
        };
        let mut legend = row![text(title).size(size)].spacing(10);
        for i in (0..SIZE_GROUPS).rev().filter(|&i| enabled[i]) {
            let value = last.map_or_else(
                || "—".to_string(),
                |p| {
                    let v = grouped(&p.cum, spot)[i];
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

    fn availability(&self, chart: &ViewState) -> IndicatorAvailability {
        match chart.basis {
            Basis::Tick(_) => {
                IndicatorAvailability::Unavailable(AvailabilityCause::Basis(chart.basis))
            }
            Basis::Time(_) => IndicatorAvailability::Available,
        }
    }

    fn rebuild_from_source(&mut self, source: &PlotData<KlineDataPoint>) {
        self.sync_all(source);
    }

    fn on_insert_klines(&mut self, _klines: &[Kline], source: &PlotData<KlineDataPoint>) {
        self.sync_all(source);
    }

    fn on_insert_trades(
        &mut self,
        trades: &[Trade],
        old_dp_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        self.sync_trades(trades, old_dp_len.saturating_sub(1), source);
    }

    fn on_ticksize_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.sync_all(source);
    }

    fn on_basis_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.sync_all(source);
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

    #[test]
    fn lines_start_at_first_split_and_hold_through_gaps() {
        let mut indicator = SizeCvdIndicator::new();
        // Fine buckets 3 ($3K+), 5 ($30K+), 8 ($1M+).
        let split = |small: f64, mid: f64, whale: f64| {
            let mut s = [0.0; SIZE_BUCKETS];
            (s[3], s[5], s[8]) = (small, mid, whale);
            Some(s)
        };
        indicator.deltas.insert(UnixMs::new(0), None);
        indicator
            .deltas
            .insert(UnixMs::new(60_000), split(1.0, -2.0, 3.0));
        indicator.deltas.insert(UnixMs::new(120_000), None);
        indicator
            .deltas
            .insert(UnixMs::new(180_000), split(1.0, 1.0, -1.0));
        indicator.rebuild_series();

        assert_eq!(indicator.series.len(), 3);
        let gap = indicator.series[&UnixMs::new(120_000)];
        assert_eq!(grouped(&gap.cum, false), [1.0, -2.0, 0.0, 3.0]);
        assert!(gap.delta.is_none());
        let last = indicator.series[&UnixMs::new(180_000)];
        assert_eq!(grouped(&last.cum, false), [2.0, -1.0, 0.0, 2.0]);
        assert_eq!(indicator.axis_group(), Some(3));

        // On spot $3K is medium and $30K large.
        assert_eq!(grouped(&last.cum, true), [0.0, 2.0, -1.0, 2.0]);
    }
}
