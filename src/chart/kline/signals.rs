//! Footprint signals following the Coinglass footprint help (coinglass.com/footprint):
//! absorption, trapped traders and exhaustion. Where the help is vague ("lowest part of the
//! bar", "almost no volume") the concrete thresholds are the constants below.

use data::chart::{PlotData, kline::KlineDataPoint};
use exchange::unit::Price;
use iced::{
    Alignment, Point, Size,
    theme::Palette,
    widget::canvas::{self, Frame, Path, Stroke},
};

/// Share of the bar's range treated as its top or bottom "part". Kept below 0.25 so that an
/// evenly traded bar (about EDGE_PART of its volume per edge) stays under the 30% shares.
const EDGE_PART: f64 = 0.20;
/// Absorption: share of the bar's taker selling (buying) that sits in its bottom (top) part.
const ABSORPTION_SHARE: f64 = 0.30;
/// Trapped traders: bars whose high (low) a breakout must exceed.
const TRAPPED_LOOKBACK: usize = 5;
/// Trapped traders: share of the bar's taker buying (selling) in its top (bottom) part.
const TRAPPED_SHARE: f64 = 0.30;
/// Trapped traders: bars allowed to fail to extend before the signal is decided.
const TRAPPED_CONFIRM_BARS: usize = 3;
/// Exhaustion: bars whose high (low) must be exceeded.
const EXHAUSTION_LOOKBACK: usize = 10;
/// Exhaustion: the extreme level's volume relative to the bar's average level volume.
const EXHAUSTION_MAX_EDGE_SHARE: f64 = 0.10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Level {
    pub price: f64,
    pub buy: f64,
    pub sell: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Bar {
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Footprint levels in ascending price order.
    pub levels: Vec<Level>,
}

impl Bar {
    fn range(&self) -> f64 {
        self.high - self.low
    }

    fn top_part_floor(&self) -> f64 {
        self.high - self.range() * EDGE_PART
    }

    fn bottom_part_ceiling(&self) -> f64 {
        self.low + self.range() * EDGE_PART
    }

    fn buy_total(&self) -> f64 {
        self.levels.iter().map(|l| l.buy).sum()
    }

    fn sell_total(&self) -> f64 {
        self.levels.iter().map(|l| l.sell).sum()
    }

    fn buy_in_top(&self) -> f64 {
        let floor = self.top_part_floor();
        self.levels
            .iter()
            .filter(|l| l.price >= floor)
            .map(|l| l.buy)
            .sum()
    }

    fn sell_in_top(&self) -> f64 {
        let floor = self.top_part_floor();
        self.levels
            .iter()
            .filter(|l| l.price >= floor)
            .map(|l| l.sell)
            .sum()
    }

    fn buy_in_bottom(&self) -> f64 {
        let ceiling = self.bottom_part_ceiling();
        self.levels
            .iter()
            .filter(|l| l.price <= ceiling)
            .map(|l| l.buy)
            .sum()
    }

    fn sell_in_bottom(&self) -> f64 {
        let ceiling = self.bottom_part_ceiling();
        self.levels
            .iter()
            .filter(|l| l.price <= ceiling)
            .map(|l| l.sell)
            .sum()
    }

    fn average_level_volume(&self) -> f64 {
        if self.levels.is_empty() {
            return 0.0;
        }
        self.levels.iter().map(|l| l.buy + l.sell).sum::<f64>() / self.levels.len() as f64
    }

    fn is_usable(&self) -> bool {
        self.range() > 0.0 && self.levels.len() >= 3
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum SignalKind {
    /// Heavy selling in the bottom part absorbed: bar closes near its high.
    BullishAbsorption,
    /// Heavy buying in the top part absorbed: bar closes near its low.
    BearishAbsorption,
    /// Breakout buyers stuck above `zone_low..zone_high` after the move failed.
    TrappedLongs { zone_low: f64, zone_high: f64 },
    /// Breakdown sellers stuck below `zone_low..zone_high` after the move failed.
    TrappedShorts { zone_low: f64, zone_high: f64 },
    /// New high on almost no volume at the top, close in the lower half.
    BuyerExhaustion,
    /// New low on almost no volume at the bottom, close in the upper half.
    SellerExhaustion,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Signal {
    /// Index into the `bars` slice passed to [`detect`].
    pub bar: usize,
    pub kind: SignalKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Enabled {
    pub absorption: bool,
    pub trapped: bool,
    pub exhaustion: bool,
}

/// Detects signals on `bars`, ordered oldest first.
pub(super) fn detect(bars: &[Bar], enabled: Enabled) -> Vec<Signal> {
    let mut signals = Vec::new();

    for (i, bar) in bars.iter().enumerate() {
        if !bar.is_usable() {
            continue;
        }

        if enabled.absorption {
            let sells = bar.sell_total();
            if sells > 0.0
                && bar.sell_in_bottom() >= ABSORPTION_SHARE * sells
                && bar.close >= bar.top_part_floor()
            {
                signals.push(Signal {
                    bar: i,
                    kind: SignalKind::BullishAbsorption,
                });
            }
            let buys = bar.buy_total();
            if buys > 0.0
                && bar.buy_in_top() >= ABSORPTION_SHARE * buys
                && bar.close <= bar.bottom_part_ceiling()
            {
                signals.push(Signal {
                    bar: i,
                    kind: SignalKind::BearishAbsorption,
                });
            }
        }

        if enabled.trapped && i >= TRAPPED_LOOKBACK {
            let prior = &bars[i - TRAPPED_LOOKBACK..i];
            let next = &bars[i + 1..bars.len().min(i + 1 + TRAPPED_CONFIRM_BARS)];

            let prior_high = prior.iter().map(|b| b.high).fold(f64::MIN, f64::max);
            let buys = bar.buy_total();
            if bar.high > prior_high
                && buys > 0.0
                && bar.buy_in_top() >= TRAPPED_SHARE * buys
                && bar.buy_in_top() > bar.sell_in_top()
                && next.iter().all(|b| b.high <= bar.high)
                && next.iter().any(|b| b.close <= bar.bottom_part_ceiling())
            {
                signals.push(Signal {
                    bar: i,
                    kind: SignalKind::TrappedLongs {
                        zone_low: bar.top_part_floor(),
                        zone_high: bar.high,
                    },
                });
            }

            let prior_low = prior.iter().map(|b| b.low).fold(f64::MAX, f64::min);
            let sells = bar.sell_total();
            if bar.low < prior_low
                && sells > 0.0
                && bar.sell_in_bottom() >= TRAPPED_SHARE * sells
                && bar.sell_in_bottom() > bar.buy_in_bottom()
                && next.iter().all(|b| b.low >= bar.low)
                && next.iter().any(|b| b.close >= bar.top_part_floor())
            {
                signals.push(Signal {
                    bar: i,
                    kind: SignalKind::TrappedShorts {
                        zone_low: bar.low,
                        zone_high: bar.bottom_part_ceiling(),
                    },
                });
            }
        }

        if enabled.exhaustion && i >= EXHAUSTION_LOOKBACK {
            let prior = &bars[i - EXHAUSTION_LOOKBACK..i];
            let midpoint = bar.low + bar.range() / 2.0;
            let thin = EXHAUSTION_MAX_EDGE_SHARE * bar.average_level_volume();

            let prior_high = prior.iter().map(|b| b.high).fold(f64::MIN, f64::max);
            if let Some(top) = bar.levels.last()
                && bar.high > prior_high
                && top.buy + top.sell <= thin
                && bar.close < midpoint
            {
                signals.push(Signal {
                    bar: i,
                    kind: SignalKind::BuyerExhaustion,
                });
            }

            let prior_low = prior.iter().map(|b| b.low).fold(f64::MAX, f64::min);
            if let Some(bottom) = bar.levels.first()
                && bar.low < prior_low
                && bottom.buy + bottom.sell <= thin
                && bar.close > midpoint
            {
                signals.push(Signal {
                    bar: i,
                    kind: SignalKind::SellerExhaustion,
                });
            }
        }
    }

    signals
}

/// Converts the time-based footprint datapoints in `start..=end` (ms) into bars, oldest first,
/// returning each bar's timestamp alongside.
///
/// Levels come from the bar's trades binned at the fixed analysis step shared with the zones,
/// not from the on-screen footprint rows: auto row size changes those with zoom, and the
/// top/bottom shares (and so the signals) must not change with it.
fn collect_bars(source: &PlotData<KlineDataPoint>, start: u64, end: u64) -> Vec<(u64, Bar)> {
    let PlotData::TimeBased(series) = source else {
        return Vec::new();
    };
    let step = super::zones::series_step(series);

    series
        .datapoints
        .iter()
        .filter(|(time, _)| (start..=end).contains(&time.as_u64()))
        .map(|(time, dp)| {
            let mut levels: Vec<Level> = if dp.trade_sequence.is_empty() {
                // No trade sequence (e.g. history from an aggregated cache): footprint rows.
                dp.footprint
                    .trades
                    .iter()
                    .map(|(price, group)| Level {
                        price: price.to_f64(),
                        buy: group.buy_qty.to_f64(),
                        sell: group.sell_qty.to_f64(),
                    })
                    .collect()
            } else {
                super::zones::bin_rows(dp, step)
                    .into_iter()
                    .map(|row| Level {
                        price: Price::from_units(row.index * step).to_f64(),
                        buy: row.buy,
                        sell: row.sell,
                    })
                    .collect()
            };
            levels.sort_by(|a, b| a.price.total_cmp(&b.price));
            (
                time.as_u64(),
                Bar {
                    high: dp.kline.high.to_f64(),
                    low: dp.kline.low.to_f64(),
                    close: dp.kline.close.to_f64(),
                    levels,
                },
            )
        })
        .collect()
}

/// Draws signals for the visible bars `earliest..=latest`. Bars just outside the view are
/// still read so lookbacks and confirmations near the edges are correct.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    source: &PlotData<KlineDataPoint>,
    frame: &mut Frame,
    earliest: u64,
    latest: u64,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    cell_width: f32,
    cell_height: f32,
    scaling: f32,
    // Chart-space height of what is drawn under each bar's low (the footprint summary), 0 if none.
    below_offset: f32,
    enabled: Enabled,
    palette: &Palette,
) {
    let PlotData::TimeBased(series) = source else {
        return;
    };
    let interval = series.interval.to_milliseconds();
    let lookback = EXHAUSTION_LOOKBACK.max(TRAPPED_LOOKBACK) as u64;
    let bars = collect_bars(
        source,
        earliest.saturating_sub(interval * lookback),
        latest.saturating_add(interval * TRAPPED_CONFIRM_BARS as u64),
    );
    let (times, plain): (Vec<u64>, Vec<Bar>) = bars.into_iter().unzip();

    let px = |p: f32| p / scaling;
    let label_size = px(11.0);
    let stroke_width = px(2.0);
    // Zoomed out, bars are too narrow for text: keep the marks, drop the labels.
    let labels = cell_width * scaling >= super::MIN_TEXT_BAR_PX;

    for signal in detect(&plain, enabled) {
        let time = times[signal.bar];
        if !(earliest..=latest).contains(&time) {
            continue;
        }
        let bar = &plain[signal.bar];
        let center_x = x(time);
        let price_y = |price: f64| y(Price::from_f64(price));

        // Labels under a bar go below its footprint summary (V/Δ) instead of over it.
        let below_bar = |price: f64| price_y(price) + below_offset + px(4.0);

        match signal.kind {
            SignalKind::BullishAbsorption | SignalKind::BearishAbsorption => {
                let bullish = signal.kind == SignalKind::BullishAbsorption;
                let color = if bullish {
                    palette.success.strong.color
                } else {
                    palette.danger.strong.color
                };
                let anchor = if bullish { bar.low } else { bar.high };
                // Ring the bar's edge rows, not half the bar: a big ring hides the numbers.
                let radius = (cell_height * 1.5).clamp(px(6.0), (cell_width * 0.3).max(px(6.0)));
                // Sit the ring inside the bar, touching the extreme, so it never covers the
                // summary text below the low.
                let center_y = if bullish {
                    price_y(anchor) - radius
                } else {
                    price_y(anchor) + radius
                };
                let circle = Path::circle(Point::new(center_x, center_y), radius);
                frame.stroke(
                    &circle,
                    Stroke::default().with_color(color).with_width(stroke_width),
                );
                let label_y = if bullish {
                    below_bar(anchor)
                } else {
                    price_y(anchor) - px(4.0) - label_size
                };
                labels.then(|| {
                    draw_label(
                        frame,
                        "Absorption",
                        center_x,
                        label_y,
                        label_size,
                        color,
                        palette,
                    )
                });
            }
            SignalKind::TrappedLongs {
                zone_low,
                zone_high,
            }
            | SignalKind::TrappedShorts {
                zone_low,
                zone_high,
            } => {
                let longs = matches!(signal.kind, SignalKind::TrappedLongs { .. });
                // Trapped longs are bearish, trapped shorts bullish.
                let color = if longs {
                    palette.danger.strong.color
                } else {
                    palette.success.strong.color
                };
                let left = center_x - cell_width / 2.0;
                let width = cell_width * (1.0 + TRAPPED_CONFIRM_BARS as f32);
                let top = price_y(zone_high);
                let height = (price_y(zone_low) - top).max(px(2.0));
                draw_hatched_zone(
                    frame,
                    left,
                    top,
                    width,
                    height,
                    color,
                    px(6.0),
                    stroke_width,
                );
                let text = if longs {
                    "Trapped longs"
                } else {
                    "Trapped shorts"
                };
                let label_y = if longs {
                    top - px(4.0) - label_size
                } else {
                    below_bar(zone_low).max(top + height + px(4.0))
                };
                labels.then(|| {
                    draw_label(
                        frame,
                        text,
                        left + width / 2.0,
                        label_y,
                        label_size,
                        color,
                        palette,
                    )
                });
            }
            SignalKind::BuyerExhaustion | SignalKind::SellerExhaustion => {
                let buyers = signal.kind == SignalKind::BuyerExhaustion;
                let color = palette.warning.strong.color;
                let tip_y = if buyers {
                    price_y(bar.high) - px(6.0)
                } else {
                    below_bar(bar.low)
                };
                let size = px(7.0);
                // Arrow pointing away from the exhausted extreme.
                let dir = if buyers { -1.0 } else { 1.0 };
                let triangle = Path::new(|b| {
                    b.move_to(Point::new(center_x, tip_y));
                    b.line_to(Point::new(center_x - size, tip_y + dir * size * 1.4));
                    b.line_to(Point::new(center_x + size, tip_y + dir * size * 1.4));
                    b.close();
                });
                frame.fill(&triangle, color);
                let label_y = if buyers {
                    tip_y - size * 1.4 - px(4.0) - label_size
                } else {
                    tip_y + size * 1.4 + px(4.0)
                };
                labels.then(|| {
                    draw_label(
                        frame,
                        "Exhaustion",
                        center_x,
                        label_y,
                        label_size,
                        color,
                        palette,
                    )
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_hatched_zone(
    frame: &mut Frame,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    color: iced::Color,
    spacing: f32,
    stroke_width: f32,
) {
    frame.fill_rectangle(
        Point::new(left, top),
        Size::new(width, height),
        color.scale_alpha(0.12),
    );
    frame.stroke(
        &Path::rectangle(Point::new(left, top), Size::new(width, height)),
        Stroke::default()
            .with_color(color.scale_alpha(0.8))
            .with_width(stroke_width * 0.6),
    );
    // 45° hatching, each line clipped to the zone by hand: `Frame::with_clip` drafts a frame
    // without the chart's translate/scale, so lines drawn in chart space would land elsewhere.
    let hatch = Stroke::default()
        .with_color(color.scale_alpha(0.45))
        .with_width(stroke_width * 0.5);
    if height <= 0.0 || width <= 0.0 || spacing <= 0.0 {
        return;
    }
    let mut offset = -height;
    while offset < width {
        // Line from (left + offset, bottom) rising to (left + offset + height, top),
        // parametrised by t in 0..=1 and clipped to left..=left + width.
        let t_min = (-offset / height).max(0.0);
        let t_max = ((width - offset) / height).min(1.0);
        if t_min < t_max {
            let at = |t: f32| Point::new(left + offset + t * height, top + height - t * height);
            frame.stroke(&Path::line(at(t_min), at(t_max)), hatch);
        }
        offset += spacing;
    }
}

/// Draws `text` centered on `center_x` on a dark backing so it stays readable over cells.
fn draw_label(
    frame: &mut Frame,
    text: &str,
    center_x: f32,
    top_y: f32,
    size: f32,
    color: iced::Color,
    palette: &Palette,
) {
    // Monospace glyphs are ~0.6 em wide.
    let width = text.chars().count() as f32 * size * 0.62 + size * 0.6;
    let height = size * 1.3;
    frame.fill_rectangle(
        Point::new(center_x - width / 2.0, top_y - size * 0.15),
        Size::new(width, height),
        palette.background.base.color.scale_alpha(0.85),
    );
    frame.fill_text(canvas::Text {
        content: text.to_string(),
        position: Point::new(center_x, top_y),
        size: iced::Pixels(size),
        color,
        align_x: Alignment::Center.into(),
        align_y: Alignment::Start.into(),
        font: crate::style::AZERET_MONO,
        ..canvas::Text::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Enabled = Enabled {
        absorption: true,
        trapped: true,
        exhaustion: true,
    };

    /// A bar spanning `low..=high` in steps of 1 with `buy`/`sell` at every level.
    fn flat_bar(low: f64, high: f64, close: f64, buy: f64, sell: f64) -> Bar {
        let mut levels = Vec::new();
        let mut p = low;
        while p <= high {
            levels.push(Level {
                price: p,
                buy,
                sell,
            });
            p += 1.0;
        }
        Bar {
            high,
            low,
            close,
            levels,
        }
    }

    fn kinds(signals: &[Signal]) -> Vec<SignalKind> {
        signals.iter().map(|s| s.kind).collect()
    }

    #[test]
    fn bullish_absorption_needs_bottom_selling_and_close_near_high() {
        // 100..=108, heavy selling on the bottom two levels, close at the high.
        let mut bar = flat_bar(100.0, 108.0, 108.0, 5.0, 2.0);
        bar.levels[0].sell = 30.0;
        bar.levels[1].sell = 30.0;
        let found = detect(&[bar.clone()], ALL);
        assert_eq!(kinds(&found), vec![SignalKind::BullishAbsorption]);

        // Same footprint but closing mid-bar: no signal.
        bar.close = 104.0;
        assert!(detect(&[bar], ALL).is_empty());
    }

    #[test]
    fn bearish_absorption_mirrors_at_the_high() {
        let mut bar = flat_bar(100.0, 108.0, 100.0, 2.0, 5.0);
        bar.levels[7].buy = 30.0;
        bar.levels[8].buy = 30.0;
        assert_eq!(
            kinds(&detect(&[bar], ALL)),
            vec![SignalKind::BearishAbsorption]
        );
    }

    #[test]
    fn balanced_bar_gives_no_signal() {
        let bar = flat_bar(100.0, 108.0, 104.0, 5.0, 5.0);
        assert!(detect(&[bar], ALL).is_empty());
    }

    fn quiet_history(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|_| flat_bar(100.0, 104.0, 102.0, 5.0, 5.0))
            .collect()
    }

    #[test]
    fn trapped_longs_after_failed_breakout() {
        let mut bars = quiet_history(5);
        // Breakout bar 102..=110 with heavy buying at the top, closing mid.
        let mut breakout = flat_bar(102.0, 110.0, 106.0, 2.0, 2.0);
        breakout.levels[7].buy = 20.0;
        breakout.levels[8].buy = 20.0;
        bars.push(breakout);
        // No new high, then a close in the breakout bar's bottom part.
        bars.push(flat_bar(101.0, 108.0, 103.0, 3.0, 3.0));
        let found = detect(
            &bars,
            Enabled {
                absorption: false,
                trapped: true,
                exhaustion: false,
            },
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bar, 5);
        assert!(
            matches!(found[0].kind, SignalKind::TrappedLongs { zone_high, .. } if zone_high == 110.0)
        );
    }

    #[test]
    fn breakout_that_extends_is_not_trapped() {
        let mut bars = quiet_history(5);
        let mut breakout = flat_bar(102.0, 110.0, 109.0, 2.0, 2.0);
        breakout.levels[7].buy = 20.0;
        breakout.levels[8].buy = 20.0;
        bars.push(breakout);
        bars.push(flat_bar(108.0, 115.0, 114.0, 3.0, 3.0));
        let found = detect(
            &bars,
            Enabled {
                absorption: false,
                trapped: true,
                exhaustion: false,
            },
        );
        assert!(found.is_empty());
    }

    #[test]
    fn buyer_exhaustion_on_thin_new_high() {
        let mut bars = quiet_history(10);
        // New high at 110 with almost nothing traded there, close in the lower half.
        let mut bar = flat_bar(100.0, 110.0, 103.0, 5.0, 5.0);
        let top = bar.levels.last_mut().unwrap();
        top.buy = 0.2;
        top.sell = 0.2;
        bars.push(bar);
        let found = detect(
            &bars,
            Enabled {
                absorption: false,
                trapped: false,
                exhaustion: true,
            },
        );
        assert_eq!(kinds(&found), vec![SignalKind::BuyerExhaustion]);
    }

    #[test]
    fn new_high_with_volume_at_the_top_is_not_exhaustion() {
        let mut bars = quiet_history(10);
        bars.push(flat_bar(100.0, 110.0, 103.0, 5.0, 5.0));
        let found = detect(
            &bars,
            Enabled {
                absorption: false,
                trapped: false,
                exhaustion: true,
            },
        );
        assert!(found.is_empty());
    }

    #[test]
    fn seller_exhaustion_mirrors_at_the_low() {
        let mut bars = quiet_history(10);
        let mut bar = flat_bar(90.0, 100.0, 98.0, 5.0, 5.0);
        let bottom = bar.levels.first_mut().unwrap();
        bottom.buy = 0.1;
        bottom.sell = 0.1;
        bars.push(bar);
        let found = detect(
            &bars,
            Enabled {
                absorption: false,
                trapped: false,
                exhaustion: true,
            },
        );
        assert_eq!(kinds(&found), vec![SignalKind::SellerExhaustion]);
    }
}
