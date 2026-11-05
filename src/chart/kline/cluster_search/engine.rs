//! Algorithmic adaptation of the supplied Cluster Search Plus reference.
//! Input is Flowdepth Footprint rows; no market-data mutation or fetching.
use super::*;
use data::chart::kline::{ClusterSearchOptions, KlineDataPoint};

pub(super) struct Bar<'a> {
    pub time: u64,
    pub dp: &'a KlineDataPoint,
    pub ready: bool,
    pub live: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Candidate {
    pub anchor: usize,
    pub price: Price,
    pub first: usize,
    pub last: usize,
    pub low: Price,
    pub high: Price,
    pub bid: f64,
    pub ask: f64,
    pub trades: Option<f64>,
    pub value: f64,
    pub provisional: bool,
    pub approximate: bool,
}
impl Candidate {
    pub fn delta(self) -> f64 {
        self.ask - self.bid
    }
    fn overlaps(self, other: Self) -> bool {
        self.first <= other.last
            && self.last >= other.first
            && self.low <= other.high
            && self.high >= other.low
    }
    fn stronger(self, other: Self) -> bool {
        self.value > other.value
            || ((self.value - other.value).abs() < 1e-7
                && (self.anchor > other.anchor
                    || (self.anchor == other.anchor && self.price > other.price)))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Settings {
    pub metric: ClusterMetric,
    pub rule: ClusterRule,
    pub minimum: f64,
    pub top: usize,
    pub k: f64,
    pub options: ClusterSearchOptions,
    pub row_step: i64,
    pub period: bool,
    pub lookback: usize,
}

fn value(metric: ClusterMetric, bid: f64, ask: f64, trades: f64) -> f64 {
    match metric {
        ClusterMetric::Volume => bid + ask,
        ClusterMetric::Bid => bid,
        ClusterMetric::Ask => ask,
        ClusterMetric::Delta => (ask - bid).abs(),
        ClusterMetric::PositiveDelta => (ask - bid).max(0.0),
        ClusterMetric::NegativeDelta => (bid - ask).max(0.0),
        ClusterMetric::Trades => trades,
        ClusterMetric::Imbalance => {
            if bid.min(ask) > 0.0 {
                bid.max(ask) / bid.min(ask)
            } else {
                bid.max(ask)
            }
        }
    }
}

fn price_offsets(count: u16, direction: u8) -> Vec<i64> {
    let n = i64::from(count.clamp(1, 200));
    match direction {
        1 => (0..n).collect(),
        2 => (0..n).map(|i| -i).collect(),
        _ => (-n / 2..n - n / 2).collect(),
    }
}

fn create(bars: &[Bar<'_>], seed: usize, price: Price, s: Settings) -> Candidate {
    let offsets = price_offsets(s.options.merge_prices, s.options.merge_direction);
    let prices: Vec<_> = offsets
        .into_iter()
        .map(|i| {
            Price::from_units(
                price
                    .units
                    .saturating_add(i.saturating_mul(s.row_step.max(1))),
            )
        })
        .collect();
    let mut c = Candidate {
        anchor: seed,
        price,
        first: seed.saturating_sub(usize::from(s.options.merge_bars.clamp(1, 200)) - 1),
        last: seed,
        low: *prices.iter().min().unwrap(),
        high: *prices.iter().max().unwrap(),
        bid: 0.0,
        ask: 0.0,
        trades: Some(0.0),
        value: 0.0,
        provisional: false,
        approximate: false,
    };
    let mut strongest = f64::NEG_INFINITY;
    for (index, bar) in bars.iter().enumerate().take(seed + 1).skip(c.first) {
        c.provisional |= !bar.ready || bar.live;
        c.approximate |= bar.dp.compacted_ids.is_some();
        if bar.dp.compacted_ids.is_some() {
            c.trades = None;
        }
        for &p in &prices {
            if let Some(row) = bar.dp.footprint.trades.get(&p) {
                let bid = row.sell_qty.to_f64();
                let ask = row.buy_qty.to_f64();
                let trades = (row.buy_count + row.sell_count) as f64;
                c.bid += bid;
                c.ask += ask;
                if let Some(count) = &mut c.trades {
                    *count += trades;
                }
                let score = value(s.metric, bid, ask, trades);
                // Strict > preserves original oldest-bar / direction-order tie position.
                if score > strongest {
                    strongest = score;
                    c.anchor = index;
                    c.price = p;
                }
            }
        }
    }
    c.value = value(s.metric, c.bid, c.ask, c.trades.unwrap_or(0.0));
    c
}

fn filters(c: Candidate, seed: &Bar<'_>, s: Settings) -> bool {
    let o = s.options;
    if (s.metric == ClusterMetric::Trades || o.min_average > 0 || o.max_average > 0)
        && c.trades.is_none()
    {
        return false;
    }
    if o.time_filter {
        let second = (seed.time / 1000 % 86400) as u32;
        let (start, end) = (o.start_second.min(86399), o.end_second.min(86399));
        let allowed = if start <= end {
            second >= start && second <= end
        } else {
            second >= start || second <= end
        };
        if !allowed {
            return false;
        }
    }
    if c.value < o.min_value as f64 / 1000.0 || c.delta().abs() < o.min_delta as f64 / 1000.0 {
        return false;
    }
    if o.imbalance_percent > 0
        && (c.bid + c.ask <= 0.0
            || c.bid.max(c.ask) / (c.bid + c.ask) * 100.0 < f64::from(o.imbalance_percent))
    {
        return false;
    }
    let average = c
        .trades
        .filter(|n| *n > 0.0)
        .map_or(0.0, |n| (c.bid + c.ask) / n);
    if average > 0.0
        && ((o.min_average > 0 && average < o.min_average as f64 / 1000.0)
            || (o.max_average > 0 && average > o.max_average as f64 / 1000.0))
    {
        return false;
    }
    let k = seed.dp.kline;
    let bullish = k.close >= k.open;
    if (o.bar_direction == 1 && !bullish) || (o.bar_direction == 2 && bullish) {
        return false;
    }
    // Original applies location/high/low filters to the seed bar, not the anchor bar.
    let p = c.price.units;
    if k.high > k.low {
        let relative = (p - k.low.units) as f64 / (k.high.units - k.low.units) as f64;
        let allowed = match o.price_location {
            1 => relative >= 2.0 / 3.0,
            2 => relative > 1.0 / 3.0 && relative < 2.0 / 3.0,
            3 => relative <= 1.0 / 3.0,
            4 => p >= k.open.units,
            5 => p <= k.open.units,
            6 => p >= k.close.units,
            7 => p <= k.close.units,
            _ => true,
        };
        if !allowed {
            return false;
        }
    }
    !(o.range_high > 0 && k.high.units - p > i64::from(o.range_high) * s.row_step)
        && !(o.range_low > 0 && p - k.low.units > i64::from(o.range_low) * s.row_step)
}

fn add(output: &mut Vec<Candidate>, c: Candidate, merged: bool) {
    if merged && let Some(existing) = output.iter_mut().find(|e| e.overlaps(c)) {
        if c.stronger(*existing) {
            *existing = c;
        }
        // Deliberately only the first overlap, not union/find or transitive suppression.
        return;
    }
    output.push(c);
}

fn collect(
    bars: &[Bar<'_>],
    indices: impl Iterator<Item = usize>,
    s: Settings,
    threshold: f64,
    visible: impl Fn(&Candidate) -> bool,
) -> Vec<Candidate> {
    let mut output = Vec::new();
    for seed in indices {
        // Hash iteration has no Tiger equivalent: canonical ascending price order.
        let mut prices: Vec<_> = bars[seed].dp.footprint.trades.keys().copied().collect();
        prices.sort_unstable();
        for price in prices {
            let c = create(bars, seed, price, s);
            if (c.value > 0.0 || s.metric == ClusterMetric::Delta)
                && filters(c, &bars[seed], s)
                && c.value >= threshold
                && visible(&c)
            {
                add(
                    &mut output,
                    c,
                    s.options.merge_bars > 1 || s.options.merge_prices > 1,
                );
            }
        }
    }
    output
}

fn strongest_per_bar(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut best = Vec::<Candidate>::new();
    let mut slots = BTreeMap::<usize, usize>::new();
    for &c in candidates {
        if let Some(&index) = slots.get(&c.anchor) {
            if c.value > best[index].value {
                best[index] = c;
            }
        } else {
            slots.insert(c.anchor, best.len());
            best.push(c);
        }
    }
    best
}

fn percentile(sorted: &[f64], percent: f64) -> f64 {
    let position = percent.clamp(0.0, 100.0) / 100.0 * (sorted.len() - 1) as f64;
    let lo = position.floor() as usize;
    let hi = position.ceil() as usize;
    sorted[lo] * (1.0 - position.fract()) + sorted[hi] * position.fract()
}

fn threshold(candidates: &[Candidate], s: Settings) -> f64 {
    let mut values: Vec<_> = candidates
        .iter()
        .map(|c| c.value)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .collect();
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return f64::INFINITY;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let k = f64::from(s.options.multiplier_tenths) / 10.0;
    let result = match s.rule {
        ClusterRule::Minimum => s.minimum,
        ClusterRule::Top => values[values.len() - s.top.clamp(1, values.len())],
        ClusterRule::Percentile => {
            percentile(&values, f64::from(s.options.percentile_tenths) / 10.0)
        }
        ClusterRule::Average => mean * k,
        ClusterRule::Median => percentile(&values, 50.0) * k,
        ClusterRule::Sigma => {
            mean + s.k
                * (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64)
                    .sqrt()
        }
    };
    result.max(s.options.auto_floor as f64 / 1000.0)
}

pub(super) struct ResultSet {
    pub signals: Vec<Candidate>,
    pub threshold: f64,
    pub min: f64,
    pub max: f64,
}
pub(super) fn search(
    bars: &[Bar<'_>],
    visible: &[usize],
    s: Settings,
    by_y: impl Fn(&Candidate) -> bool,
) -> ResultSet {
    let from = bars.len().saturating_sub(s.lookback.max(1));
    let manual = s.rule == ClusterRule::Minimum;
    let (mut signals, population, limit) = if s.period && !manual {
        let population = strongest_per_bar(&collect(bars, from..bars.len(), s, 0.0, |_| true));
        let limit = threshold(&population, s);
        let signals = collect(
            bars,
            visible
                .iter()
                .copied()
                .filter(|i| !s.options.display_only_lookback || *i >= from),
            s,
            limit,
            by_y,
        );
        (signals, population, limit)
    } else {
        let signals = collect(bars, visible.iter().copied(), s, 0.0, by_y);
        let population = strongest_per_bar(&signals);
        let limit = if manual {
            s.minimum
        } else {
            threshold(&population, s)
        };
        (signals, population, limit)
    };
    signals.retain(|c| {
        c.value >= limit
            && (!manual || s.options.maximum == 0 || c.value <= s.options.maximum as f64 / 1000.0)
    });
    if s.options.single_per_bar {
        signals = strongest_per_bar(&signals);
    }
    if s.rule == ClusterRule::Top && (!s.period || s.options.display_only_lookback) {
        signals.sort_by(|a, b| {
            b.value
                .total_cmp(&a.value)
                .then_with(|| b.anchor.cmp(&a.anchor))
        });
        signals.truncate(s.top.max(1));
    }
    let scale = if s.period && !manual {
        &population
    } else {
        &signals
    };
    let min = scale
        .iter()
        .map(|c| c.value)
        .reduce(f64::min)
        .unwrap_or(0.0);
    let max = scale
        .iter()
        .map(|c| c.value)
        .reduce(f64::max)
        .unwrap_or(0.0);
    ResultSet {
        signals,
        threshold: limit,
        min,
        max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::chart::kline::{BubbleVolumeSummary, GroupedTrades, KlineTrades};
    use exchange::unit::qty::Qty;

    fn dp(rows: &[(i64, f64, f64)]) -> KlineDataPoint {
        let mut footprint = KlineTrades::new();
        for &(price, bid, ask) in rows {
            footprint.trades.insert(
                Price::from_units(price),
                GroupedTrades {
                    sell_qty: Qty::from_f64(bid),
                    buy_qty: Qty::from_f64(ask),
                    buy_count: usize::from(ask > 0.0),
                    sell_count: usize::from(bid > 0.0),
                    first_time: UnixMs::ZERO,
                    last_time: UnixMs::ZERO,
                },
            );
        }
        KlineDataPoint {
            kline: exchange::Kline {
                time: UnixMs::ZERO,
                open: Price::from_units(100),
                high: Price::from_units(106),
                low: Price::from_units(100),
                close: Price::from_units(104),
                volume: exchange::Volume::empty_buy_sell(),
            },
            footprint,
            bubble_summary: BubbleVolumeSummary::default(),
            trade_coverage: TradeCoverage::Partial,
            trade_sequence: Vec::new(),
            trade_ids: Default::default(),
            compacted_ids: None,
            intrabar: None,
            cached_size_delta: None,
        }
    }
    fn bars(points: &[KlineDataPoint]) -> Vec<Bar<'_>> {
        points
            .iter()
            .enumerate()
            .map(|(i, dp)| Bar {
                time: i as u64 * 60000,
                dp,
                ready: true,
                live: false,
            })
            .collect()
    }
    fn settings(rule: ClusterRule) -> Settings {
        Settings {
            metric: ClusterMetric::Volume,
            rule,
            minimum: 0.0,
            top: 2,
            k: 1.0,
            options: ClusterSearchOptions {
                multiplier_tenths: 10,
                ..Default::default()
            },
            row_step: 1,
            period: false,
            lookback: 2,
        }
    }
    fn run(points: &[KlineDataPoint], s: Settings) -> ResultSet {
        search(
            &bars(points),
            &(0..points.len()).collect::<Vec<_>>(),
            s,
            |_| true,
        )
    }

    #[test]
    fn thresholds_use_filtered_bar_maxima_and_linear_percentile() {
        let points = [
            dp(&[(100, 0.0, 1.0), (101, 0.0, 9.0)]),
            dp(&[(100, 0.0, 3.0)]),
        ];
        assert_eq!(run(&points, settings(ClusterRule::Average)).threshold, 6.0);
        let mut s = settings(ClusterRule::Percentile);
        s.options.percentile_tenths = 250;
        assert_eq!(run(&points, s).threshold, 4.5);
        s.rule = ClusterRule::Median;
        s.options.multiplier_tenths = 20;
        assert_eq!(run(&points, s).threshold, 12.0);
        s.rule = ClusterRule::Sigma;
        let result = run(&points, s);
        assert_eq!(result.threshold, 9.0);
        assert_eq!(result.signals.len(), 1);
        s.options.auto_floor = 10000;
        assert_eq!(run(&points, s).threshold, 10.0);
    }
    #[test]
    fn zero_delta_and_flat_sigma_follow_reference() {
        let points = [dp(&[(100, 4.0, 4.0)]), dp(&[(100, 4.0, 4.0)])];
        let mut s = settings(ClusterRule::Sigma);
        assert_eq!(run(&points, s).signals.len(), 2);
        s.metric = ClusterMetric::Delta;
        let result = run(&points, s);
        assert_eq!(result.threshold, 0.0);
        assert_eq!(result.signals.len(), 2);
        s.metric = ClusterMetric::PositiveDelta;
        assert!(run(&points, s).signals.is_empty());
        assert!(run(&[], s).threshold.is_infinite());
    }
    #[test]
    fn top_n_is_final_signal_limit_not_one_signal_per_bar() {
        let points = [
            dp(&[(100, 0.0, 100.0), (101, 0.0, 99.0)]),
            dp(&[(100, 0.0, 90.0)]),
            dp(&[(100, 0.0, 80.0)]),
        ];
        let mut s = settings(ClusterRule::Top);
        let result = run(&points, s);
        assert_eq!(result.threshold, 90.0);
        assert_eq!(
            result.signals.iter().map(|c| c.value).collect::<Vec<_>>(),
            vec![100.0, 99.0]
        );
        s.options.single_per_bar = true;
        assert_eq!(
            run(&points, s)
                .signals
                .iter()
                .map(|c| c.value)
                .collect::<Vec<_>>(),
            vec![100.0, 90.0]
        );
    }
    #[test]
    fn filters_precede_threshold_and_manual_maximum_is_not_auto_cap() {
        let points = [
            dp(&[(100, 49.0, 50.0), (101, 0.0, 8.0)]),
            dp(&[(100, 0.0, 4.0)]),
        ];
        let mut s = settings(ClusterRule::Average);
        s.options.min_delta = 2000;
        s.options.maximum = 5000;
        let result = run(&points, s);
        assert_eq!(result.threshold, 6.0);
        assert_eq!(result.signals[0].value, 8.0);
        s.rule = ClusterRule::Minimum;
        s.minimum = 4.0;
        let result = run(&points, s);
        assert_eq!(result.signals.len(), 1);
        assert_eq!(result.signals[0].value, 4.0);
    }
    #[test]
    fn merged_sum_sign_and_anchor_are_distinct() {
        let points = [
            dp(&[(100, 8.0, 0.0), (101, 0.0, 2.0)]),
            dp(&[(100, 0.0, 7.0), (101, 0.0, 4.0)]),
        ];
        let mut s = settings(ClusterRule::Minimum);
        s.metric = ClusterMetric::Delta;
        s.options.merge_bars = 2;
        s.options.merge_prices = 2;
        s.options.merge_direction = 1;
        let c = create(&bars(&points), 1, Price::from_units(100), s);
        assert_eq!((c.bid, c.ask, c.delta(), c.value), (8.0, 13.0, 5.0, 5.0));
        assert_eq!(
            (
                c.anchor,
                c.price.units,
                c.first,
                c.last,
                c.low.units,
                c.high.units
            ),
            (0, 100, 0, 1, 100, 101)
        );
        assert_eq!(c.trades, Some(4.0));
    }
    #[test]
    fn merge_price_direction_and_ties_use_iteration_order() {
        assert_eq!(price_offsets(4, 0), vec![-2, -1, 0, 1]);
        assert_eq!(price_offsets(3, 1), vec![0, 1, 2]);
        assert_eq!(price_offsets(3, 2), vec![0, -1, -2]);
        let points = [
            dp(&[(100, 0.0, 2.0), (101, 0.0, 2.0)]),
            dp(&[(100, 0.0, 2.0)]),
        ];
        let mut s = settings(ClusterRule::Minimum);
        s.options.merge_bars = 2;
        s.options.merge_prices = 2;
        s.options.merge_direction = 2;
        let c = create(&bars(&points), 1, Price::from_units(101), s);
        assert_eq!((c.anchor, c.price.units), (0, 101));
    }
    #[test]
    fn overlapping_zones_replace_only_first_intersection() {
        let points = [dp(&[(100, 0.0, 1.0)])];
        let c = create(
            &bars(&points),
            0,
            Price::from_units(100),
            settings(ClusterRule::Minimum),
        );
        let a = Candidate {
            low: Price::from_units(100),
            high: Price::from_units(101),
            value: 2.0,
            ..c
        };
        let b = Candidate {
            low: Price::from_units(103),
            high: Price::from_units(104),
            value: 3.0,
            ..c
        };
        let bridge = Candidate {
            low: Price::from_units(101),
            high: Price::from_units(103),
            value: 4.0,
            ..c
        };
        let mut output = vec![a, b];
        add(&mut output, bridge, true);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].value, 4.0);
        assert!(output[0].overlaps(output[1]));
        let weaker = Candidate {
            value: 1.0,
            ..bridge
        };
        add(&mut output, weaker, true);
        assert_eq!(output[0].value, 4.0);
    }
    #[test]
    fn period_population_is_not_visible_population_and_no_limit_outside_lookback() {
        let points = [
            dp(&[(100, 0.0, 100.0)]),
            dp(&[(100, 0.0, 90.0)]),
            dp(&[(100, 0.0, 80.0)]),
            dp(&[(100, 0.0, 70.0)]),
        ];
        let mut s = settings(ClusterRule::Top);
        s.period = true;
        s.top = 1;
        s.options.display_only_lookback = false;
        let result = run(&points, s);
        assert_eq!(result.threshold, 80.0);
        assert_eq!(result.signals.len(), 3);
        s.options.display_only_lookback = true;
        assert_eq!(run(&points, s).signals.len(), 1);
        s.period = false;
        let result = search(&bars(&points), &[0, 1], s, |c| c.value < 100.0);
        assert_eq!(result.threshold, 90.0);
    }
    #[test]
    fn unknown_compact_counts_are_not_real_trades_or_average_size() {
        let mut points = [dp(&[(100, 0.0, 10.0)])];
        points[0].compacted_ids = Some((1, 10));
        let mut s = settings(ClusterRule::Minimum);
        assert_eq!(run(&points, s).signals[0].trades, None);
        s.metric = ClusterMetric::Trades;
        assert!(run(&points, s).signals.is_empty());
        s.metric = ClusterMetric::Volume;
        s.options.min_average = 1;
        assert!(run(&points, s).signals.is_empty());
    }
    #[test]
    fn seed_filters_time_wrap_doji_location_imbalance_and_average() {
        let points = [dp(&[(102, 2.0, 6.0)])];
        let mut bars = bars(&points);
        let mut s = settings(ClusterRule::Minimum);
        let c = create(&bars, 0, Price::from_units(102), s);
        s.options.time_filter = true;
        s.options.start_second = 23 * 3600;
        s.options.end_second = 3600;
        assert!(filters(c, &bars[0], s));
        bars[0].time = 12 * 3600 * 1000;
        assert!(!filters(c, &bars[0], s));
        s.options.time_filter = false;
        s.options.price_location = 2;
        assert!(!filters(c, &bars[0], s));
        s.options.price_location = 3;
        assert!(filters(c, &bars[0], s));
        s.options.imbalance_percent = 76;
        assert!(!filters(c, &bars[0], s));
        s.options.imbalance_percent = 75;
        s.options.max_average = 3999;
        assert!(!filters(c, &bars[0], s));
        s.options.max_average = 4000;
        assert!(filters(c, &bars[0], s));
        s.options.range_high = 3;
        assert!(!filters(c, &bars[0], s));
        s.options.range_high = 0;
        s.options.bar_direction = 2;
        assert!(!filters(c, &bars[0], s));
    }
}
