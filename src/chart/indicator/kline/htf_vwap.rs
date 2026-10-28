//! Developing weekly (from Monday 00:00 UTC) and monthly VWAP. Built from the chart market's
//! hourly candles with the typical price (H+L+C)/3, fetched separately, so both lines exist on
//! any timeframe without a month of trades. Within a few dollars of the trade-based VWAP.
use super::period_levels::{month_starts, week_start};
use crate::chart::{
    Basis, Message, ViewState,
    indicator::kline::{
        AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl, OverlayCtx,
    },
};
use crate::connector::fetcher::FetchRange;

use exchange::unit::Price;
use exchange::{Kline, Timeframe, UnixMs};
use iced::widget::canvas::{self, Frame, Stroke};
use iced::{Alignment, Point};
use std::collections::BTreeMap;

/// Refresh period of the forming hourly candle.
const POLL_MS: u64 = 30_000;

const WEEK_COLOR: iced::Color = iced::Color::from_rgba(0.40, 0.72, 0.86, 0.75);
const MONTH_COLOR: iced::Color = iced::Color::from_rgba(0.92, 0.66, 0.38, 0.75);

pub struct HtfVwapIndicator {
    /// Hourly candle open time → (typical price, volume).
    hourly: BTreeMap<u64, (f64, f64)>,
}

impl HtfVwapIndicator {
    pub fn new() -> Self {
        Self {
            hourly: BTreeMap::new(),
        }
    }
}

/// VWAP points (candle open time, value), one run per anchor period.
fn sessions(
    hourly: &BTreeMap<u64, (f64, f64)>,
    anchor: impl Fn(u64) -> u64,
) -> Vec<Vec<(u64, f64)>> {
    let mut runs: Vec<Vec<(u64, f64)>> = Vec::new();
    let mut current = None;
    let (mut pv, mut v) = (0.0, 0.0);
    for (&time, &(price, volume)) in hourly {
        let start = anchor(time);
        if current != Some(start) {
            current = Some(start);
            (pv, v) = (0.0, 0.0);
            runs.push(Vec::new());
        }
        pv += price * volume;
        v += volume;
        if v > 0.0
            && let Some(run) = runs.last_mut()
        {
            run.push((time, pv / v));
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
        unreachable!("week/month VWAP renders on the main chart canvas")
    }

    fn availability(&self, chart: &ViewState) -> IndicatorAvailability {
        match chart.basis {
            Basis::Tick(_) => {
                IndicatorAvailability::Unavailable(AvailabilityCause::Basis(chart.basis))
            }
            Basis::Time(_) => IndicatorAvailability::Available,
        }
    }

    fn fetch_range(&mut self, _ctx: &FetchCtx) -> Option<FetchRange> {
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
        let label_size = 10.0 / ctx.scaling;
        let anchors: [(fn(u64) -> u64, iced::Color, &str); 2] = [
            (week_start, WEEK_COLOR, "W VWAP"),
            (|t| month_starts(t).0, MONTH_COLOR, "M VWAP"),
        ];
        for (anchor, color, name) in anchors {
            let runs = sessions(&self.hourly, anchor);
            let last_run = runs.len().saturating_sub(1);
            for (i, run) in runs.iter().enumerate() {
                let mut path = canvas::path::Builder::new();
                for (j, &(time, value)) in run.iter().enumerate() {
                    let point = Point::new((ctx.x)(time), (ctx.y)(Price::from_f64(value)));
                    if j == 0 {
                        path.move_to(point);
                    } else {
                        path.line_to(point);
                    }
                }
                frame.stroke(
                    &path.build(),
                    Stroke::default()
                        .with_color(color)
                        .with_width(1.4 / ctx.scaling),
                );
                if i == last_run
                    && let Some(&(time, value)) = run.last()
                {
                    frame.fill_text(canvas::Text {
                        content: format!("{name} {value:.0}"),
                        position: Point::new(
                            (ctx.x)(time) + label_size * 0.5,
                            (ctx.y)(Price::from_f64(value)),
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
        assert_eq!(runs[0], vec![(0, 100.0), (HOUR, 107.5)]);
        assert_eq!(runs[1], vec![(10 * HOUR, 200.0)]);
    }
}
