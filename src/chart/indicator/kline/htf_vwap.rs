//! Runtime of the VWAP indicator. The session VWAP (from trades) is drawn by `KlineChart`;
//! this part adds the developing weekly (from Monday 00:00 UTC) and monthly VWAP, built from
//! the chart market's hourly candles with the typical price (H+L+C)/3 and fetched separately,
//! so both lines exist on any timeframe without a month of trades. Within a few dollars of
//! the trade-based VWAP.
use super::period_levels::{month_starts, week_start};
use crate::chart::{
    Basis, Message, ViewState,
    indicator::kline::{
        AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl, OverlayCtx,
    },
};
use crate::connector::fetcher::FetchRange;

use data::chart::kline::VwapConfig;
use exchange::unit::Price;
use exchange::{Kline, Timeframe, UnixMs};
use iced::widget::canvas::{self, Frame, LineDash, Stroke};
use iced::{Alignment, Point};
use std::collections::BTreeMap;

/// Refresh period of the forming hourly candle.
const POLL_MS: u64 = 30_000;

const WEEK_COLOR: iced::Color = iced::Color::from_rgba(0.40, 0.72, 0.86, 0.75);
const MONTH_COLOR: iced::Color = iced::Color::from_rgba(0.92, 0.66, 0.38, 0.75);

pub struct HtfVwapIndicator {
    /// Hourly candle open time → (typical price, volume).
    hourly: BTreeMap<u64, (f64, f64)>,
    config: VwapConfig,
}

impl HtfVwapIndicator {
    pub fn new() -> Self {
        Self {
            hourly: BTreeMap::new(),
            config: VwapConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct VwapPoint {
    time: u64,
    value: f64,
    /// Volume-weighted standard deviation of price around the VWAP.
    sigma: f64,
}

/// VWAP points, one run per anchor period.
fn sessions(
    hourly: &BTreeMap<u64, (f64, f64)>,
    anchor: impl Fn(u64) -> u64,
) -> Vec<Vec<VwapPoint>> {
    let mut runs: Vec<Vec<VwapPoint>> = Vec::new();
    let mut current = None;
    let (mut pv, mut p2v, mut v) = (0.0, 0.0, 0.0);
    for (&time, &(price, volume)) in hourly {
        let start = anchor(time);
        if current != Some(start) {
            current = Some(start);
            (pv, p2v, v) = (0.0, 0.0, 0.0);
            runs.push(Vec::new());
        }
        pv += price * volume;
        p2v += price * price * volume;
        v += volume;
        if v > 0.0
            && let Some(run) = runs.last_mut()
        {
            let value = pv / v;
            run.push(VwapPoint {
                time,
                value,
                sigma: (p2v / v - value * value).max(0.0).sqrt(),
            });
        }
    }
    runs.retain(|run| !run.is_empty());
    runs
}

impl KlineIndicatorImpl for HtfVwapIndicator {
    fn clear_all_caches(&mut self) {}

    fn clear_crosshair_caches(&mut self) {}

    fn element<'a>(
        &'a self,
        _chart: &'a ViewState,
        _data_labels_always_visible: bool,
        _visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        unreachable!("VWAP renders on the main chart canvas")
    }

    fn availability(&self, chart: &ViewState) -> IndicatorAvailability {
        match chart.basis {
            Basis::Tick(_) => {
                IndicatorAvailability::Unavailable(AvailabilityCause::Basis(chart.basis))
            }
            Basis::Time(_) => IndicatorAvailability::Available,
        }
    }

    fn on_config_changed(&mut self, config: &data::chart::kline::Config) {
        self.config = config.vwap;
    }

    fn fetch_range(&mut self, _ctx: &FetchCtx) -> Option<FetchRange> {
        if !self.config.weekly && !self.config.monthly {
            return None;
        }
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let from = self
            .hourly
            .last_key_value()
            .map_or_else(|| month_starts(now).1, |(&last, _)| last);
        // `to` moves in POLL_MS steps; identical requests are throttled by the handler.
        Some(FetchRange::RefKlines {
            from: UnixMs::new(from),
            to: UnixMs::new(now - now % POLL_MS).max(UnixMs::new(from + 1)),
            timeframe: Timeframe::H1,
            spot: false,
        })
    }

    fn on_ref_klines(&mut self, spot: bool, timeframe: Timeframe, klines: &[Kline]) {
        if spot || timeframe != Timeframe::H1 {
            return;
        }
        self.hourly.extend(klines.iter().map(|k| {
            let typical = (k.high.to_f64() + k.low.to_f64() + k.close.to_f64()) / 3.0;
            (k.time.as_u64(), (typical, k.volume.total().to_f64()))
        }));
    }

    fn draw_overlay(&self, frame: &mut Frame, ctx: &OverlayCtx) {
        let cfg = self.config;
        let label_size = 10.0 / ctx.scaling;
        let width = cfg.line_width.clamp(0.5, 5.0) / ctx.scaling;
        let multiplier = f64::from(cfg.band_multiplier.max(0.0));
        let anchors: [(bool, fn(u64) -> u64, iced::Color, &str); 2] = [
            (cfg.weekly, week_start, WEEK_COLOR, "W VWAP"),
            (cfg.monthly, |t| month_starts(t).0, MONTH_COLOR, "M VWAP"),
        ];
        for (enabled, anchor, color, name) in anchors {
            if !enabled {
                continue;
            }
            let runs = sessions(&self.hourly, anchor);
            let last_run = runs.len().saturating_sub(1);
            for (i, run) in runs.iter().enumerate() {
                let stroke_line = |frame: &mut Frame, offset: f64, stroke: Stroke| {
                    let mut path = canvas::path::Builder::new();
                    for (j, p) in run.iter().enumerate() {
                        let price = Price::from_f64(p.value + offset * p.sigma * multiplier);
                        let point = Point::new((ctx.x)(p.time), (ctx.y)(price));
                        if j == 0 {
                            path.move_to(point);
                        } else {
                            path.line_to(point);
                        }
                    }
                    frame.stroke(&path.build(), stroke);
                };
                stroke_line(
                    frame,
                    0.0,
                    Stroke::default().with_color(color).with_width(width),
                );
                if cfg.show_bands {
                    let band = Stroke {
                        line_dash: LineDash {
                            segments: &[4.0, 4.0],
                            offset: 0,
                        },
                        ..Stroke::default()
                            .with_color(color.scale_alpha(0.5))
                            .with_width(1.0 / ctx.scaling)
                    };
                    stroke_line(frame, 1.0, band);
                    stroke_line(frame, -1.0, band);
                }
                if cfg.show_labels
                    && i == last_run
                    && let Some(p) = run.last()
                {
                    frame.fill_text(canvas::Text {
                        content: format!("{name} {:.0}", p.value),
                        position: Point::new(
                            (ctx.x)(p.time) + label_size * 0.5,
                            (ctx.y)(Price::from_f64(p.value)),
                        ),
                        size: iced::Pixels(label_size),
                        color,
                        align_x: Alignment::Start.into(),
                        align_y: Alignment::Center.into(),
                        font: crate::style::AZERET_MONO,
                        ..canvas::Text::default()
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;

    #[test]
    fn vwap_resets_at_anchor_and_weights_by_volume() {
        let mut hourly = BTreeMap::new();
        hourly.insert(0, (100.0, 1.0));
        hourly.insert(HOUR, (110.0, 3.0));
        hourly.insert(10 * HOUR, (200.0, 2.0));
        let runs = sessions(&hourly, |t| t / (10 * HOUR) * (10 * HOUR));
        assert_eq!(runs.len(), 2);
        let values: Vec<_> = runs[0].iter().map(|p| (p.time, p.value)).collect();
        assert_eq!(values, vec![(0, 100.0), (HOUR, 107.5)]);
        // sqrt((1·100² + 3·110²)/4 − 107.5²) = sqrt(18.75)
        assert!((runs[0][1].sigma - 18.75_f64.sqrt()).abs() < 1e-9);
        assert_eq!(runs[1][0].value, 200.0);
        assert_eq!(runs[1][0].sigma, 0.0);
    }
}
