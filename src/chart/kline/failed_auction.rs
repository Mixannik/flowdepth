//! Failed auctions at reference levels (Stoiiic, AMT parts 3–4): price breaks a level
//! (PDH/PDL…, FRVP VAH/VAL, a drawn line) from inside, finds no acceptance out there and closes
//! back inside within the level's `max_outside_ms`. Drawn as the excursion beyond the level,
//! labelled with the aggression traded out there: strong delta in the breakout's direction means
//! trapped traders, weak delta means no initiative.
//!
//! Filters fitted on Binance BTC perp 15m, 45 days to 29.10.2026, at PDH/PDL with up to 4 h
//! outside: unfiltered 172 signals, the excursion extreme held for 4 h after the return 41% of
//! the time; with the filters below 30 signals and 63%.

use crate::chart::indicator::kline::AuctionLevel;
use data::chart::{PlotData, kline::KlineDataPoint};
use exchange::UnixMs;
use exchange::unit::Price;
use iced::{
    Point, Size,
    theme::Palette,
    widget::canvas::{Frame, LineDash, Path, Stroke},
};

/// Price must not have traded beyond the level for this long before the breakout: repeated
/// pokes at a level price is sitting on are balance, not a failed auction.
const QUIET_BEFORE_MS: u64 = 4 * 60 * 60_000;
/// Minimum excursion beyond the level, as a share of price.
const MIN_EXCURSION_SHARE: f64 = 0.001;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ohlc {
    pub time: u64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FailedAuction {
    /// First bar beyond the level.
    pub start: u64,
    /// Bar that closed back inside.
    pub end: u64,
    pub level: f64,
    /// Failed above the level (bearish) or below it (bullish).
    pub above: bool,
    /// Furthest price traded beyond the level.
    pub extreme: f64,
    pub name: &'static str,
}

/// Failed auctions on closed `bars` (oldest first) at `levels`; a level counts at a bar when
/// the bar's time is in its `from..to`.
pub(super) fn detect(bars: &[Ohlc], levels: &[AuctionLevel]) -> Vec<FailedAuction> {
    let mut found: Vec<FailedAuction> = Vec::new();

    for i in 1..bars.len() {
        let bar = bars[i];
        let prev = bars[i - 1];
        for level in levels.iter().filter(|l| (l.from..l.to).contains(&bar.time)) {
            let price = level.price;
            for above in [true, false] {
                let inside = |x: f64| if above { x < price } else { x > price };
                let beyond = |b: &Ohlc| if above { b.high > price } else { b.low < price };
                if !inside(prev.close) || !beyond(&bar) {
                    continue;
                }
                if found
                    .iter()
                    .any(|f| f.start == bar.time && f.above == above)
                {
                    continue; // Coinciding levels (PDH = PWH) fail together: one mark.
                }
                let untouched = bars[..i]
                    .iter()
                    .rev()
                    .take_while(|b| b.time + QUIET_BEFORE_MS >= bar.time)
                    .all(|b| !beyond(b));
                if !untouched {
                    continue;
                }

                let mut extreme = if above { bar.high } else { bar.low };
                let back = bars[i..]
                    .iter()
                    .take_while(|b| b.time - bar.time <= level.max_outside_ms)
                    .find(|b| {
                        extreme = if above {
                            extreme.max(b.high)
                        } else {
                            extreme.min(b.low)
                        };
                        inside(b.close)
                    });
                let Some(back) = back else {
                    continue; // Still out there, or accepted.
                };
                if (extreme - price).abs() < price.abs() * MIN_EXCURSION_SHARE {
                    continue;
                }
                found.push(FailedAuction {
                    start: bar.time,
                    end: back.time,
                    level: price,
                    above,
                    extreme,
                    name: level.name,
                });
            }
        }
    }

    found
}

/// Closed bars of `start..=end` (ms): the forming bar is left out, its close is not final.
pub(super) fn closed_bars(source: &PlotData<KlineDataPoint>, start: u64, end: u64) -> Vec<Ohlc> {
    let PlotData::TimeBased(series) = source else {
        return Vec::new();
    };
    let newest = series.datapoints.keys().next_back().copied();
    series
        .datapoints
        .range(UnixMs::new(start)..=UnixMs::new(end))
        .filter(|(time, _)| Some(**time) != newest)
        .map(|(time, dp)| Ohlc {
            time: time.as_u64(),
            high: dp.kline.high.to_f64(),
            low: dp.kline.low.to_f64(),
            close: dp.kline.close.to_f64(),
        })
        .collect()
}

/// Taker delta traded beyond the level over the auction's bars, as a share of the volume
/// there (−1..=1); `None` without footprint data.
fn delta_beyond(source: &PlotData<KlineDataPoint>, auction: &FailedAuction) -> Option<f64> {
    let PlotData::TimeBased(series) = source else {
        return None;
    };
    let (mut buy, mut sell) = (0.0, 0.0);
    for (_, dp) in series
        .datapoints
        .range(UnixMs::new(auction.start)..=UnixMs::new(auction.end))
    {
        for (price, group) in &dp.footprint.trades {
            let p = price.to_f64();
            if (auction.above && p > auction.level) || (!auction.above && p < auction.level) {
                buy += group.buy_qty.to_f64();
                sell += group.sell_qty.to_f64();
            }
        }
    }
    let total = buy + sell;
    (total > 0.0).then(|| (buy - sell) / total)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    source: &PlotData<KlineDataPoint>,
    frame: &mut Frame,
    earliest: u64,
    latest: u64,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    cell_width: f32,
    scaling: f32,
    // Levels over the window `bars_from..=latest` the caller fetched them for.
    levels: &[AuctionLevel],
    bars_from: u64,
    palette: &Palette,
) {
    let bars = closed_bars(source, bars_from, latest);
    let px = |p: f32| p / scaling;
    let label_size = px(11.0);
    let labels = cell_width * scaling >= super::MIN_TEXT_BAR_PX;
    let price_y = |price: f64| y(Price::from_f64(price));

    for auction in detect(&bars, levels) {
        // Shown while any part of the excursion is in view.
        if auction.end < earliest || auction.start > latest {
            continue;
        }
        // Failing above is bearish, below bullish.
        let color = if auction.above {
            palette.danger.strong.color
        } else {
            palette.success.strong.color
        };
        let left = x(auction.start) - cell_width / 2.0;
        let right = x(auction.end) + cell_width / 2.0;
        let level_y = price_y(auction.level);
        let extreme_y = price_y(auction.extreme);
        let top = level_y.min(extreme_y);
        let height = (level_y - extreme_y).abs().max(px(2.0));

        frame.fill_rectangle(
            Point::new(left, top),
            Size::new(right - left, height),
            color.scale_alpha(0.04),
        );
        frame.stroke(
            &Path::rectangle(Point::new(left, top), Size::new(right - left, height)),
            Stroke {
                line_dash: LineDash {
                    segments: &[px(4.0), px(3.0)],
                    offset: 0,
                },
                ..Stroke::default()
                    .with_color(color.scale_alpha(0.3))
                    .with_width(px(1.0))
            },
        );
        // The level the auction failed at, solid across the excursion.
        frame.stroke(
            &Path::line(Point::new(left, level_y), Point::new(right, level_y)),
            Stroke::default()
                .with_color(color.scale_alpha(0.5))
                .with_width(px(1.5)),
        );

        if labels {
            let delta = delta_beyond(source, &auction)
                .map(|d| format!(" Δ{:+.0}%", d * 100.0))
                .unwrap_or_default();
            let text = format!("Failed {}{delta}", auction.name);
            let label_y = if auction.above {
                extreme_y - px(4.0) - label_size * 1.3
            } else {
                extreme_y + px(4.0)
            };
            super::signals::draw_label(
                frame,
                &text,
                (left + right) / 2.0,
                label_y,
                label_size,
                color,
                palette,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 60 * 60_000;

    fn bar(hour: u64, low: f64, high: f64, close: f64) -> Ohlc {
        Ohlc {
            time: hour * H,
            high,
            low,
            close,
        }
    }

    fn level(price: f64) -> AuctionLevel {
        AuctionLevel {
            price,
            from: 0,
            to: u64::MAX,
            name: "PDH",
            max_outside_ms: 4 * H,
        }
    }

    /// Six quiet hours below 100 000, then a poke above it from `out_at`.
    fn history() -> Vec<Ohlc> {
        (0..6)
            .map(|h| bar(h, 99_500.0, 99_800.0, 99_700.0))
            .collect()
    }

    #[test]
    fn breakout_back_inside_within_the_limit_fails() {
        let mut bars = history();
        bars.push(bar(6, 99_700.0, 100_300.0, 100_200.0));
        bars.push(bar(7, 100_000.0, 100_500.0, 100_100.0));
        bars.push(bar(8, 99_600.0, 100_200.0, 99_800.0));
        let found = detect(&bars, &[level(100_000.0)]);
        assert_eq!(found.len(), 1);
        let f = found[0];
        assert!(f.above);
        assert_eq!((f.start, f.end), (6 * H, 8 * H));
        assert_eq!(f.extreme, 100_500.0);
    }

    #[test]
    fn staying_out_longer_than_the_limit_is_acceptance() {
        let mut bars = history();
        for h in 6..12 {
            bars.push(bar(h, 100_050.0, 100_400.0, 100_200.0));
        }
        bars.push(bar(12, 99_600.0, 100_200.0, 99_800.0));
        assert!(detect(&bars, &[level(100_000.0)]).is_empty());
    }

    #[test]
    fn level_touched_shortly_before_is_balance() {
        let mut bars = history();
        bars[4].high = 100_050.0;
        bars.push(bar(6, 99_700.0, 100_300.0, 99_800.0));
        assert!(detect(&bars, &[level(100_000.0)]).is_empty());
    }

    #[test]
    fn tiny_poke_is_ignored() {
        let mut bars = history();
        // 0.05% beyond, under the 0.1% minimum.
        bars.push(bar(6, 99_700.0, 100_050.0, 99_800.0));
        assert!(detect(&bars, &[level(100_000.0)]).is_empty());
    }

    #[test]
    fn failed_breakdown_mirrors_below() {
        let mut bars: Vec<Ohlc> = (0..6)
            .map(|h| bar(h, 100_200.0, 100_500.0, 100_300.0))
            .collect();
        bars.push(bar(6, 99_600.0, 100_300.0, 100_100.0));
        let found = detect(&bars, &[level(100_000.0)]);
        assert_eq!(found.len(), 1);
        assert!(!found[0].above);
        assert_eq!(found[0].extreme, 99_600.0);
    }

    #[test]
    fn level_counts_only_while_valid() {
        let mut bars = history();
        bars.push(bar(6, 99_700.0, 100_300.0, 99_800.0));
        let mut lvl = level(100_000.0);
        lvl.from = 7 * H;
        assert!(detect(&bars, &[lvl]).is_empty());
    }

    #[test]
    fn coinciding_levels_give_one_mark() {
        let mut bars = history();
        bars.push(bar(6, 99_700.0, 100_300.0, 99_800.0));
        let mut weekly = level(100_000.0);
        weekly.name = "PWH";
        assert_eq!(detect(&bars, &[level(100_000.0), weekly]).len(), 1);
    }
}
