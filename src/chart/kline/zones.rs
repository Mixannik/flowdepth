//! Coinglass footprint zones: stacked imbalances and unfinished auctions, each drawn from its
//! bar to the right until price trades through it.
//!
//! - Stacked imbalance: at least [`STACK_MIN`] adjacent rows of one bar imbalanced on the same
//!   side (the diagonal rule of the Imbalance study, 300% by default, and the dominant side at
//!   least the bar's average one-side row volume, so thin spot rows don't count). Buy stacks act as
//!   support and end when a later bar trades below the zone; sell stacks mirror above.
//! - Unfinished auction: at the bar's extreme tick both aggressive buyers and sellers traded,
//!   so the auction did not finish there. Checked on raw trades, not on the footprint rows:
//!   a coarse row at the high almost always holds both sides. Ends once price trades beyond.

use data::chart::{PlotData, kline::KlineDataPoint};
use exchange::unit::Price;
use iced::{
    Point, Size,
    theme::Palette,
    widget::canvas::{Frame, LineDash, Path, Stroke},
};

/// Adjacent imbalanced rows needed for a stack.
const STACK_MIN: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Row {
    /// Price in rows (price / row step), so adjacent rows differ by exactly 1.
    pub index: i64,
    pub buy: f64,
    pub sell: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Bar {
    pub high: f64,
    pub low: f64,
    /// Footprint rows in ascending price order.
    pub rows: Vec<Row>,
    /// Highest traded price and whether both sides traded there.
    pub top_tick: Option<(f64, bool)>,
    /// Lowest traded price and whether both sides traded there.
    pub bottom_tick: Option<(f64, bool)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImbalanceRule {
    /// Percent the dominant side must exceed the diagonal by (200 = 300% of it).
    pub threshold: usize,
    pub ignore_zeros: bool,
}

impl ImbalanceRule {
    /// Same test as the Imbalance study's markers.
    fn holds(self, qty: f64, diagonal: f64) -> bool {
        if self.ignore_zeros && (qty <= 0.0 || diagonal <= 0.0) {
            return false;
        }
        let required = diagonal * (100 + self.threshold) as f64 / 100.0;
        if required <= 0.0 {
            qty > 0.0
        } else {
            qty > required
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Stack {
    pub bar: usize,
    pub side: Side,
    /// Lowest and highest row index of the stack.
    pub from_row: i64,
    pub to_row: i64,
    /// First later bar that traded through the zone.
    pub end: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Auction {
    pub bar: usize,
    /// True for an unfinished high, false for an unfinished low.
    pub at_high: bool,
    pub price: f64,
    /// First later bar that traded beyond the price.
    pub end: Option<usize>,
}

/// Average volume of one side of a non-empty row (half the average row total): the least an
/// imbalance's dominant side must have. A 4:1 ratio of 0.4 vs 0.1 BTC is noise, common on
/// thin spot rows.
pub(super) fn average_row_qty(row_totals: impl Iterator<Item = f64>) -> f64 {
    let (sum, count) = row_totals
        .filter(|qty| *qty > 0.0)
        .fold((0.0, 0usize), |(sum, count), qty| (sum + qty, count + 1));
    if count == 0 {
        0.0
    } else {
        sum / count as f64 / 2.0
    }
}

/// Stacked imbalances on `bars` (oldest first). `step` is the row size in price.
pub(super) fn stacked_imbalances(bars: &[Bar], step: f64, rule: ImbalanceRule) -> Vec<Stack> {
    let mut stacks = Vec::new();
    for (i, bar) in bars.iter().enumerate() {
        let row_at = |index: i64| {
            bar.rows
                .binary_search_by_key(&index, |r| r.index)
                .ok()
                .map(|k| &bar.rows[k])
        };
        let min_qty = average_row_qty(bar.rows.iter().map(|r| r.buy + r.sell));
        let imbalanced = |row: &Row, side: Side| match side {
            Side::Buy => {
                row.buy >= min_qty
                    && rule.holds(row.buy, row_at(row.index - 1).map_or(0.0, |d| d.sell))
            }
            Side::Sell => {
                row.sell >= min_qty
                    && rule.holds(row.sell, row_at(row.index + 1).map_or(0.0, |d| d.buy))
            }
        };

        for side in [Side::Buy, Side::Sell] {
            let mut run: Option<(i64, i64)> = None;
            let mut flush = |run: Option<(i64, i64)>| {
                if let Some((from_row, to_row)) = run
                    && (to_row - from_row + 1) as usize >= STACK_MIN
                {
                    let low = from_row as f64 * step - step / 2.0;
                    let high = to_row as f64 * step + step / 2.0;
                    let end = bars[i + 1..]
                        .iter()
                        .position(|b| match side {
                            Side::Buy => b.low < low,
                            Side::Sell => b.high > high,
                        })
                        .map(|k| i + 1 + k);
                    stacks.push(Stack {
                        bar: i,
                        side,
                        from_row,
                        to_row,
                        end,
                    });
                }
            };
            for row in &bar.rows {
                if imbalanced(row, side) {
                    run = match run {
                        Some((from, to)) if row.index == to + 1 => Some((from, row.index)),
                        other => {
                            flush(other);
                            Some((row.index, row.index))
                        }
                    };
                } else {
                    flush(run.take());
                }
            }
            flush(run);
        }
    }
    stacks
}

/// Unfinished auctions on `bars` (oldest first). The newest bar is skipped: its extremes are
/// still being built.
pub(super) fn unfinished_auctions(bars: &[Bar]) -> Vec<Auction> {
    let mut auctions = Vec::new();
    let closed = bars.len().saturating_sub(1);
    for (i, bar) in bars[..closed].iter().enumerate() {
        for (tick, at_high) in [(bar.top_tick, true), (bar.bottom_tick, false)] {
            let Some((price, true)) = tick else {
                continue;
            };
            let end = bars[i + 1..]
                .iter()
                .position(|b| {
                    if at_high {
                        b.high > price
                    } else {
                        b.low < price
                    }
                })
                .map(|k| i + 1 + k);
            auctions.push(Auction {
                bar: i,
                at_high,
                price,
                end,
            });
        }
    }
    auctions
}

/// Extreme traded price and whether both sides traded at it.
fn extreme_tick(dp: &KlineDataPoint, highest: bool) -> Option<(f64, bool)> {
    let extreme = if highest {
        dp.trade_sequence.iter().map(|t| t.price).max()
    } else {
        dp.trade_sequence.iter().map(|t| t.price).min()
    }?;
    let (mut buy, mut sell) = (false, false);
    for trade in dp.trade_sequence.iter().filter(|t| t.price == extreme) {
        if trade.is_sell {
            sell = true;
        } else {
            buy = true;
        }
    }
    Some((extreme.to_f64(), buy && sell))
}

/// Rows per typical candle for stack detection. Stacks use this fixed row size, not the
/// on-screen one: auto row size changes with zoom, and zones must not come and go with it.
const ROWS_PER_BAR: i64 = 20;
const NICE_STEPS: [i64; 4] = [10, 20, 25, 50];

/// Row size (price units) giving about [`ROWS_PER_BAR`] rows over the median candle range,
/// rounded up to 1/2/2.5/5 × 10^k.
pub(super) fn analysis_step(mut ranges: Vec<i64>) -> i64 {
    ranges.retain(|r| *r > 0);
    if ranges.is_empty() {
        return 1;
    }
    ranges.sort_unstable();
    let target = (ranges[ranges.len() / 2] / ROWS_PER_BAR).max(1);
    let mut scale = 1;
    loop {
        // NICE_STEPS are in tenths so 2.5 stays integral.
        for nice in NICE_STEPS {
            let step = nice * scale / 10;
            if step >= target && step > 0 {
                return step;
            }
        }
        scale *= 10;
    }
}

/// Rows and extreme ticks of one candle, rebuilt from its trades only when they change.
struct CachedBar {
    trades: usize,
    last_units: i64,
    rows: Vec<Row>,
    top_tick: Option<(f64, bool)>,
    bottom_tick: Option<(f64, bool)>,
}

#[derive(Default)]
struct SeriesCache {
    step: i64,
    bars: rustc_hash::FxHashMap<u64, CachedBar>,
}

thread_local! {
    /// Per chart series (keyed by its address); a few panes at most.
    static CACHE: std::cell::RefCell<rustc_hash::FxHashMap<usize, SeriesCache>> =
        Default::default();
}

/// Fixed analysis step (price units) for the series: from its recent candle ranges, so it does
/// not follow the zoom-dependent footprint row size.
pub(super) fn series_step(series: &data::aggr::time::TimeSeries<KlineDataPoint>) -> i64 {
    analysis_step(
        series
            .datapoints
            .values()
            .rev()
            .take(300)
            .map(|dp| dp.kline.high.units - dp.kline.low.units)
            .collect(),
    )
}

pub(super) fn bin_rows(dp: &KlineDataPoint, step: i64) -> Vec<Row> {
    let mut by_index: rustc_hash::FxHashMap<i64, (f64, f64)> = Default::default();
    for trade in &dp.trade_sequence {
        // Nearest row, ties up, like the footprint's own binning.
        let index = (trade.price.units + step / 2).div_euclid(step);
        let entry = by_index.entry(index).or_default();
        if trade.is_sell {
            entry.1 += trade.qty.to_f64();
        } else {
            entry.0 += trade.qty.to_f64();
        }
    }
    let mut rows: Vec<Row> = by_index
        .into_iter()
        .map(|(index, (buy, sell))| Row { index, buy, sell })
        .collect();
    rows.sort_by_key(|r| r.index);
    rows
}

/// All loaded candles, oldest first, with rows at the returned fixed step (price units).
fn collect_bars(source: &PlotData<KlineDataPoint>, with_ticks: bool) -> (i64, Vec<(u64, Bar)>) {
    let PlotData::TimeBased(series) = source else {
        return (1, Vec::new());
    };
    let step = series_step(series);
    let key = std::ptr::from_ref(series) as usize;

    let bars = CACHE.with_borrow_mut(|cache| {
        if cache.len() > 16 && !cache.contains_key(&key) {
            cache.clear();
        }
        let entry = cache.entry(key).or_default();
        if entry.step != step {
            *entry = SeriesCache {
                step,
                ..Default::default()
            };
        }
        series
            .datapoints
            .iter()
            .map(|(time, dp)| {
                let last_units = dp.trade_sequence.last().map_or(0, |t| t.price.units);
                let cached = entry
                    .bars
                    .entry(time.as_u64())
                    .or_insert_with(|| CachedBar {
                        trades: usize::MAX,
                        last_units: 0,
                        rows: Vec::new(),
                        top_tick: None,
                        bottom_tick: None,
                    });
                if cached.trades != dp.trade_sequence.len() || cached.last_units != last_units {
                    cached.trades = dp.trade_sequence.len();
                    cached.last_units = last_units;
                    cached.rows = bin_rows(dp, step);
                    cached.top_tick = extreme_tick(dp, true);
                    cached.bottom_tick = extreme_tick(dp, false);
                }
                (
                    time.as_u64(),
                    Bar {
                        high: dp.kline.high.to_f64(),
                        low: dp.kline.low.to_f64(),
                        rows: cached.rows.clone(),
                        top_tick: if with_ticks { cached.top_tick } else { None },
                        bottom_tick: if with_ticks { cached.bottom_tick } else { None },
                    },
                )
            })
            .collect()
    });
    (step, bars)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Enabled {
    pub stacked: bool,
    pub unfinished: bool,
}

/// Draws zones that reach into the visible range `earliest..=latest`, up to `right_x`.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    source: &PlotData<KlineDataPoint>,
    frame: &mut Frame,
    earliest: u64,
    latest: u64,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    right_x: f32,
    cell_width: f32,
    rule: ImbalanceRule,
    enabled: Enabled,
    scaling: f32,
    palette: &Palette,
) {
    if !enabled.stacked && !enabled.unfinished {
        return;
    }
    // All loaded history: a zone born far left of the view must not vanish while panning.
    let (step, bars) = collect_bars(source, enabled.unfinished);
    let (times, plain): (Vec<u64>, Vec<Bar>) = bars.into_iter().unzip();
    let px = |p: f32| p / scaling;
    let price_y = |price: f64| y(Price::from_f64(price));
    let half = cell_width / 2.0;
    // A zone ends at the bar that traded through it, otherwise at the right edge.
    let end_x = |end: Option<usize>| end.map_or(right_x, |e| x(times[e]));
    let visible = |bar: usize, end: Option<usize>| {
        times[bar] <= latest && end.is_none_or(|e| times[e] >= earliest)
    };

    if enabled.stacked {
        let step_f = Price::from_units(step).to_f64();
        for stack in stacked_imbalances(&plain, step_f, rule) {
            if !visible(stack.bar, stack.end) {
                continue;
            }
            let color = match stack.side {
                Side::Buy => palette.success.strong.color,
                Side::Sell => palette.danger.strong.color,
            };
            let top = price_y(stack.to_row as f64 * step_f + step_f / 2.0);
            let bottom = price_y(stack.from_row as f64 * step_f - step_f / 2.0);
            let height = (bottom - top).max(px(2.0));
            let center = x(times[stack.bar]);

            // Frame the stacked cells, then a faint band to the right.
            frame.stroke(
                &Path::rectangle(
                    Point::new(center - half, top),
                    Size::new(cell_width, height),
                ),
                Stroke::default().with_color(color).with_width(px(1.5)),
            );
            let band_left = center + half;
            let band_right = end_x(stack.end);
            if band_right > band_left {
                frame.fill_rectangle(
                    Point::new(band_left, top),
                    Size::new(band_right - band_left, height),
                    color.scale_alpha(0.12),
                );
            }
        }
    }

    if enabled.unfinished {
        let stroke = Stroke {
            line_dash: LineDash {
                segments: &[4.0, 3.0],
                offset: 0,
            },
            ..Stroke::default()
                .with_color(palette.background.base.text.scale_alpha(0.55))
                .with_width(px(1.2))
        };
        for auction in unfinished_auctions(&plain) {
            if !visible(auction.bar, auction.end) {
                continue;
            }
            let line_y = price_y(auction.price);
            let left = x(times[auction.bar]) - half;
            let right = end_x(auction.end);
            if right > left {
                frame.stroke(
                    &Path::line(Point::new(left, line_y), Point::new(right, line_y)),
                    stroke,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: ImbalanceRule = ImbalanceRule {
        threshold: 200,
        ignore_zeros: true,
    };

    /// Rows `from..from + n` with the given buy/sell at each.
    fn rows(from: i64, sides: &[(f64, f64)]) -> Vec<Row> {
        sides
            .iter()
            .enumerate()
            .map(|(k, &(buy, sell))| Row {
                index: from + k as i64,
                buy,
                sell,
            })
            .collect()
    }

    fn bar(low: f64, high: f64, rows: Vec<Row>) -> Bar {
        Bar {
            high,
            low,
            rows,
            top_tick: None,
            bottom_tick: None,
        }
    }

    #[test]
    fn three_stacked_buy_imbalances_make_a_support_zone() {
        // Rows 100..=104 (step 1). Buys at 101..=103 are 4x the sell one row below.
        let r = rows(
            100,
            &[
                (1.0, 5.0),
                (20.0, 5.0),
                (20.0, 5.0),
                (20.0, 5.0),
                (5.0, 5.0),
            ],
        );
        let bars = vec![bar(100.0, 104.0, r), bar(102.0, 106.0, rows(102, &[]))];
        let stacks = stacked_imbalances(&bars, 1.0, RULE);
        assert_eq!(
            stacks,
            vec![Stack {
                bar: 0,
                side: Side::Buy,
                from_row: 101,
                to_row: 103,
                end: None,
            }]
        );
    }

    #[test]
    fn thin_rows_are_not_a_stack() {
        // Same 4:1 ratios, but the stacked rows are far below the bar's average row volume.
        let r = rows(
            100,
            &[(0.1, 0.5), (2.0, 0.5), (2.0, 0.5), (2.0, 0.5), (60.0, 60.0)],
        );
        assert!(stacked_imbalances(&[bar(100.0, 104.0, r)], 1.0, RULE).is_empty());
    }

    #[test]
    fn two_imbalances_are_not_a_stack() {
        let r = rows(100, &[(1.0, 5.0), (20.0, 5.0), (20.0, 5.0), (5.0, 5.0)]);
        assert!(stacked_imbalances(&[bar(100.0, 103.0, r)], 1.0, RULE).is_empty());
    }

    #[test]
    fn a_gap_row_breaks_the_stack() {
        let mut r = rows(100, &[(1.0, 5.0), (20.0, 5.0), (20.0, 5.0)]);
        // Row 103 missing, 104 imbalanced against nothing below (ignored as zero).
        r.push(Row {
            index: 104,
            buy: 20.0,
            sell: 5.0,
        });
        assert!(stacked_imbalances(&[bar(100.0, 104.0, r)], 1.0, RULE).is_empty());
    }

    #[test]
    fn buy_stack_ends_when_price_trades_below_it() {
        let r = rows(
            100,
            &[
                (1.0, 5.0),
                (20.0, 5.0),
                (20.0, 5.0),
                (20.0, 5.0),
                (5.0, 5.0),
            ],
        );
        let bars = vec![
            bar(100.0, 104.0, r),
            // Dips into the zone (100.5..103.5) but not below it.
            bar(101.0, 105.0, Vec::new()),
            bar(99.0, 102.0, Vec::new()),
        ];
        let stacks = stacked_imbalances(&bars, 1.0, RULE);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].end, Some(2));
    }

    #[test]
    fn sell_stack_mirrors_against_buys_one_row_up() {
        // Sells at 100..=102 are 4x the buys one row above.
        let r = rows(100, &[(5.0, 20.0), (5.0, 20.0), (5.0, 20.0), (5.0, 1.0)]);
        let stacks = stacked_imbalances(&[bar(100.0, 103.0, r)], 1.0, RULE);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].side, Side::Sell);
        assert_eq!((stacks[0].from_row, stacks[0].to_row), (100, 102));
    }

    fn tick_bar(low: f64, high: f64, top_two_sided: bool, bottom_two_sided: bool) -> Bar {
        Bar {
            top_tick: Some((high, top_two_sided)),
            bottom_tick: Some((low, bottom_two_sided)),
            ..bar(low, high, Vec::new())
        }
    }

    #[test]
    fn two_sided_high_is_unfinished_until_exceeded() {
        let bars = vec![
            tick_bar(100.0, 110.0, true, false),
            // Touching 110 again does not finish it; trading above does.
            tick_bar(104.0, 110.0, false, false),
            tick_bar(105.0, 111.0, false, false),
            tick_bar(105.0, 108.0, false, false),
        ];
        assert_eq!(
            unfinished_auctions(&bars),
            vec![Auction {
                bar: 0,
                at_high: true,
                price: 110.0,
                end: Some(2),
            }]
        );
    }

    #[test]
    fn one_sided_extremes_are_finished() {
        let bars = vec![
            tick_bar(100.0, 110.0, false, false),
            tick_bar(100.0, 110.0, false, false),
        ];
        assert!(unfinished_auctions(&bars).is_empty());
    }

    #[test]
    fn analysis_step_targets_twenty_rows_on_nice_sizes() {
        // Median range 300 → 15 per row → next nice size 20.
        assert_eq!(analysis_step(vec![100, 300, 500]), 20);
        // 1000 → 50; 4000 → 200; 600 → 30 → 50 (2.5 × 10 = 25 < 30).
        assert_eq!(analysis_step(vec![1000]), 50);
        assert_eq!(analysis_step(vec![4000]), 200);
        assert_eq!(analysis_step(vec![600]), 50);
        assert_eq!(analysis_step(vec![450]), 25);
        assert_eq!(analysis_step(Vec::new()), 1);
    }

    #[test]
    fn newest_bar_is_still_building() {
        let bars = vec![
            tick_bar(100.0, 110.0, false, false),
            tick_bar(95.0, 105.0, false, true),
        ];
        assert!(unfinished_auctions(&bars).is_empty());
    }
}
