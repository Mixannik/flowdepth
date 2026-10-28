//! Levels of fixed-range volume profiles (POC, VAH, VAL) continued from the range's end to
//! the right edge, as JDK marks a multi-week range on 4h. A profile drawn on one chart also
//! shows its levels on every other chart of the same ticker (the dashboard collects them
//! with `KlineChart::range_levels` and hands them out with `set_shared_range_levels`).
//!
//! Ranges longer than a few days are profiled from hourly candles: a month of trades is far
//! too much to download, and an hour is fine-grained enough for POC and value area.

use crate::chart::indicator::kline::OverlayCtx;

use exchange::Kline;
use exchange::unit::Price;
use iced::widget::canvas::{self, Frame, LineDash, Path, Stroke};
use iced::{Alignment, Point};
use std::collections::BTreeMap;

pub const HOUR_MS: u64 = 3_600_000;
/// Ranges longer than this are built from hourly candles instead of trades.
pub const CANDLE_PROFILE_AFTER_MS: u64 = 3 * 24 * HOUR_MS;
/// Rows of a candle-based profile over its whole range (rows get coarser, never finer).
pub const MAX_CANDLE_PROFILE_ROWS: usize = 300;
/// Refresh period of the forming hourly candle when a range reaches the present.
const POLL_MS: u64 = 60_000;

const POC_COLOR: iced::Color = iced::Color::from_rgb(0.93, 0.74, 0.32);
const VA_COLOR: iced::Color = iced::Color::from_rgb(0.45, 0.70, 0.92);
const OPACITY: f32 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeLevels {
    /// End of the profiled range: the lines start here.
    pub to: u64,
    pub poc: f64,
    pub vah: f64,
    pub val: f64,
}

impl RangeLevels {
    pub fn prices(&self) -> [f64; 3] {
        [self.poc, self.vah, self.val]
    }
}

pub fn uses_candles(from: u64, to: u64) -> bool {
    to.saturating_sub(from) > CANDLE_PROFILE_AFTER_MS
}

/// Hourly candles of `[from, to)` still to fetch, as a request range. A range that reaches
/// the present keeps polling its forming candle.
pub fn missing_hourly(
    hourly: &BTreeMap<u64, Kline>,
    from: u64,
    to: u64,
    now: u64,
) -> Option<(u64, u64)> {
    let end = to.min(now);
    if end <= from {
        return None;
    }
    let mut bars = hourly.range(from..end).map(|(&time, _)| time);
    let (Some(first), last) = (bars.next(), bars.next_back()) else {
        return Some((from, end));
    };
    let last = last.unwrap_or(first);
    if first >= from + HOUR_MS {
        return Some((from, end));
    }
    if to > now {
        let polled = (now - now % POLL_MS).max(last + 1);
        return Some((last, polled));
    }
    (last + HOUR_MS < end).then_some((last, end))
}

/// Thin dashed lines from each range's end to `right_x`, labelled at their left end.
pub fn draw(levels: &[RangeLevels], frame: &mut Frame, ctx: &OverlayCtx) {
    let OverlayCtx {
        x,
        y,
        left_x,
        right_x,
        scaling,
        ..
    } = *ctx;
    let px = |p: f32| p / scaling;
    let label_size = px(10.0);
    for level in levels {
        let start = x(level.to).max(left_x);
        if start >= right_x {
            continue;
        }
        for (name, price, color, dash) in [
            ("POC", level.poc, POC_COLOR, &[8.0, 4.0][..]),
            ("VAH", level.vah, VA_COLOR, &[4.0, 4.0][..]),
            ("VAL", level.val, VA_COLOR, &[4.0, 4.0][..]),
        ] {
            let color = color.scale_alpha(OPACITY);
            let line_y = y(Price::from_f64(price));
            let stroke = Stroke {
                line_dash: LineDash {
                    segments: dash,
                    offset: 0,
                },
                ..Stroke::default().with_color(color).with_width(px(1.0))
            };
            frame.stroke(
                &Path::line(Point::new(start, line_y), Point::new(right_x, line_y)),
                stroke,
            );
            frame.fill_text(canvas::Text {
                content: format!("{name} {price:.0}"),
                position: Point::new(start + label_size * 0.4, line_y - label_size * 0.15),
                size: iced::Pixels(label_size),
                color: color.scale_alpha(1.5),
                align_x: Alignment::Start.into(),
                align_y: Alignment::End.into(),
                font: crate::style::AZERET_MONO,
                ..canvas::Text::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::unit::qty::Qty;
    use exchange::{UnixMs, Volume};

    fn bar(hour: u64) -> Kline {
        Kline {
            time: UnixMs::new(hour * HOUR_MS),
            open: Price::from_f64(100.0),
            high: Price::from_f64(101.0),
            low: Price::from_f64(99.0),
            close: Price::from_f64(100.0),
            volume: Volume::TotalOnly(Qty::from_f64(1.0)),
        }
    }

    fn hourly(hours: std::ops::Range<u64>) -> BTreeMap<u64, Kline> {
        hours.map(|h| (h * HOUR_MS, bar(h))).collect()
    }

    #[test]
    fn missing_hourly_requests_whole_range_then_nothing() {
        let (from, to, now) = (10 * HOUR_MS, 20 * HOUR_MS, 100 * HOUR_MS);
        assert_eq!(
            missing_hourly(&BTreeMap::new(), from, to, now),
            Some((from, to))
        );
        // Data starting later than the range (e.g. the VWAP's month) does not count.
        assert_eq!(
            missing_hourly(&hourly(15..30), from, to, now),
            Some((from, to))
        );
        assert_eq!(missing_hourly(&hourly(10..20), from, to, now), None);
        assert_eq!(
            missing_hourly(&hourly(10..15), from, to, now),
            Some((14 * HOUR_MS, to))
        );
    }

    #[test]
    fn missing_hourly_polls_forming_candle() {
        let now = 20 * HOUR_MS + 30 * 60_000 + 5_000;
        let got = missing_hourly(&hourly(10..21), 10 * HOUR_MS, 24 * HOUR_MS, now);
        assert_eq!(got, Some((20 * HOUR_MS, now - 5_000)));
    }
}
