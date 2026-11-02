//! Untested profile levels from complete UTC sessions and a continuous later price path.
use super::volume_profile::value_area;
use std::collections::{BTreeMap, BTreeSet};

const DAY_MS: u64 = 86_400_000;
const ROWS: usize = 201;

#[derive(Debug, Clone, PartialEq)]
pub enum BarVolume {
    Levels(Vec<(f64, f64)>),
    /// Candle volume spread over its range; an explicit approximation.
    Total(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub time: u64,
    pub high: f64,
    pub low: f64,
    pub volume: BarVolume,
}

impl Bar {
    fn valid(&self) -> bool {
        self.high.is_finite()
            && self.low.is_finite()
            && self.low > 0.0
            && self.high >= self.low
            && match &self.volume {
                BarVolume::Total(q) => q.is_finite() && *q >= 0.0,
                BarVolume::Levels(rows) => rows.iter().all(|&(p, q)| {
                    // Footprint prices are rounded bin centers and may lie just
                    // outside the exact candle extreme. Session indexing clamps them.
                    p.is_finite() && p >= 0.0 && q.is_finite() && q >= 0.0
                }),
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Daily,
    Weekly,
}
impl SessionKind {
    pub fn milliseconds(self) -> u64 {
        match self {
            Self::Daily => DAY_MS,
            Self::Weekly => 7 * DAY_MS,
        }
    }
    pub fn tag(self) -> &'static str {
        match self {
            Self::Daily => "D",
            Self::Weekly => "W",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelKind {
    Poc,
    Vah,
    Val,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub kind: LevelKind,
    pub session: SessionKind,
    pub price: f64,
    pub from: u64,
    pub approximate: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub levels: Vec<Level>,
    /// Recent closed sessions that cannot be proved complete or untested.
    pub incomplete_sessions: usize,
}

fn profile(members: &[&Bar]) -> Option<([f64; 3], bool)> {
    let high = members
        .iter()
        .map(|b| b.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low = members.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    if !high.is_finite() || !low.is_finite() || high <= low {
        return None;
    }
    let row = (high - low) / (ROWS - 1) as f64;
    if !row.is_finite() || row <= 0.0 {
        return None;
    }
    let index = |p: f64| (((p - low) / row).floor().max(0.0) as usize).min(ROWS - 1);
    let mut weights = vec![0.0; ROWS];
    let mut approximate = false;
    for bar in members {
        match &bar.volume {
            BarVolume::Levels(levels) => {
                for &(p, q) in levels {
                    weights[index(p)] += q;
                }
            }
            BarVolume::Total(q) => {
                approximate = true;
                let first = index(bar.low);
                let last = index(bar.high - row * 1e-6).max(first);
                let share = q / (last - first + 1) as f64;
                for weight in &mut weights[first..=last] {
                    *weight += share;
                }
            }
        }
    }
    let area = value_area(&weights, 0.70)?;
    let center = |i: usize| low + (i as f64 + 0.5) * row;
    Some((
        [center(area.poc), center(area.high), center(area.low)],
        approximate,
    ))
}

/// `align` supplies UTC session boundaries. The newest supplied bar is the
/// calculation's as-of point; the session containing it is still developing.
pub fn calculate(
    bars: &[Bar],
    kind: SessionKind,
    max_sessions: usize,
    interval_ms: u64,
    align: impl Fn(u64, u64) -> u64,
) -> Snapshot {
    let mut out = Snapshot::default();
    let session_ms = kind.milliseconds();
    if bars.is_empty() || interval_ms == 0 || session_ms % interval_ms != 0 {
        return out;
    }
    let sorted: BTreeMap<_, _> = bars.iter().map(|bar| (bar.time, bar)).collect();
    let invalid: BTreeSet<_> = sorted
        .iter()
        .filter_map(|(&time, bar)| (time % interval_ms != 0 || !bar.valid()).then_some(time))
        .collect();
    let Some((&latest, _)) = sorted.last_key_value() else {
        return out;
    };
    let current = align(latest, session_ms);
    let first = current.saturating_sub(session_ms.saturating_mul(max_sessions as u64));
    let mut start = first;
    while start < current {
        let end = start.saturating_add(session_ms);
        if end <= start {
            break;
        }
        let mut members = Vec::new();
        let mut time = start;
        let mut complete = invalid.range(start..=latest).next().is_none();
        // The closed session AND its later price path must have every bar.
        while complete && time <= latest {
            match sorted.get(&time) {
                Some(bar) => {
                    if time < end {
                        members.push(*bar);
                    }
                }
                _ => {
                    complete = false;
                    break;
                }
            }
            let next = time.saturating_add(interval_ms);
            if next <= time {
                complete = false;
                break;
            }
            time = next;
        }
        if !complete {
            out.incomplete_sessions += 1;
        } else if let Some((prices, approximate)) = profile(&members) {
            for (kind_level, price) in [LevelKind::Poc, LevelKind::Vah, LevelKind::Val]
                .into_iter()
                .zip(prices)
            {
                let touched = sorted
                    .range(end..)
                    .any(|(_, bar)| bar.low <= price && price <= bar.high);
                if !touched {
                    out.levels.push(Level {
                        kind: kind_level,
                        session: kind,
                        price,
                        from: end,
                        approximate,
                    });
                }
            }
        }
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    const HOUR: u64 = 3_600_000;
    fn align(t: u64, s: u64) -> u64 {
        t / s * s
    }
    fn source() -> Vec<Bar> {
        (0..30)
            .map(|h| Bar {
                time: h * HOUR,
                high: if h < 24 { 110.0 } else { 130.0 },
                low: if h < 24 { 100.0 } else { 120.0 },
                volume: BarVolume::Levels(vec![(if h < 24 { 105.0 } else { 125.0 }, 50.0)]),
            })
            .collect()
    }
    fn result(bars: &[Bar]) -> Snapshot {
        calculate(bars, SessionKind::Daily, 1, HOUR, align)
    }
    #[test]
    fn complete_baseline_and_candle_approximation_are_distinct() {
        let mut bars = source();
        let exact = result(&bars);
        let poc = exact
            .levels
            .iter()
            .find(|l| l.kind == LevelKind::Poc)
            .unwrap();
        assert!((poc.price - 105.025).abs() < 1e-9);
        assert!(!poc.approximate);
        for bar in &mut bars[..24] {
            bar.volume = BarVolume::Total(100.0);
        }
        assert!(result(&bars).levels.iter().all(|l| l.approximate));
        assert!(!result(&bars).levels.is_empty());
    }
    #[test]
    fn source_prefix_internal_gap_and_later_path_gap_cannot_prove_naked() {
        for missing in [0, 12, 26] {
            let bars = source()
                .into_iter()
                .enumerate()
                .filter_map(|(i, b)| (i != missing).then_some(b))
                .collect::<Vec<_>>();
            let result = result(&bars);
            assert!(result.levels.is_empty());
            assert_eq!(result.incomplete_sessions, 1);
        }
    }
    #[test]
    fn touching_endpoint_and_bad_inputs_do_not_confirm() {
        let mut bars = source();
        let p = result(&bars)
            .levels
            .iter()
            .find(|l| l.kind == LevelKind::Poc)
            .unwrap()
            .price;
        bars[24].low = p;
        bars[24].volume = BarVolume::Total(1.0);
        assert!(
            !result(&bars)
                .levels
                .iter()
                .any(|l| l.kind == LevelKind::Poc)
        );
        let mut bars = source();
        bars[7].volume = BarVolume::Total(f64::NAN);
        assert!(result(&bars).levels.is_empty());
        assert_eq!(result(&bars).incomplete_sessions, 1);
        bars[7].volume = BarVolume::Total(-1.0);
        assert!(result(&bars).levels.is_empty());
        // A coarse footprint grid can round a positive execution into bin zero.
        // OHLC remains positive; indexing clamps the bin to the session range.
        let mut rounded = source();
        rounded[0].volume = BarVolume::Levels(vec![(0.0, 50.0)]);
        assert_eq!(result(&rounded).incomplete_sessions, 0);
        assert!(!result(&rounded).levels.is_empty());
    }

    #[test]
    fn only_the_recent_closed_sessions_are_considered() {
        let bars = (0..=5 * 24)
            .map(|hour| {
                let price = 100.0 + (hour / 24) as f64 * 20.0;
                Bar {
                    time: hour * HOUR,
                    high: price + 10.0,
                    low: price,
                    volume: BarVolume::Levels(vec![(price + 5.0, 50.0)]),
                }
            })
            .collect::<Vec<_>>();
        let result = calculate(&bars, SessionKind::Daily, 2, HOUR, align);
        let sessions = result
            .levels
            .iter()
            .map(|l| l.from)
            .collect::<BTreeSet<_>>();
        assert_eq!(sessions, BTreeSet::from([4 * DAY_MS, 5 * DAY_MS]));
    }
}
