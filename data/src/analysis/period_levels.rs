//! Previous UTC period extremes, calculated only from complete daily history.
use chrono::{Datelike, TimeZone, Utc};
use std::collections::BTreeMap;

pub const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DailyBar {
    pub time: u64,
    pub high: f64,
    pub low: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub period: Period,
    pub is_high: bool,
    pub price: f64,
    /// Current period start: the previous period's level becomes valid here.
    pub from: u64,
}

impl Level {
    pub fn name(&self) -> &'static str {
        match (self.period, self.is_high) {
            (Period::Day, true) => "PDH",
            (Period::Day, false) => "PDL",
            (Period::Week, true) => "PWH",
            (Period::Week, false) => "PWL",
            (Period::Month, true) => "PMH",
            (Period::Month, false) => "PML",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub expected_days: usize,
    pub valid_days: usize,
    pub missing_days: Vec<u64>,
    pub invalid_days: Vec<u64>,
}

impl Coverage {
    pub fn is_complete(&self) -> bool {
        self.expected_days > 0 && self.valid_days == self.expected_days
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeriodResult {
    pub period: Period,
    /// Half-open source interval, excluding the forming current period.
    pub source_from: u64,
    pub source_to: u64,
    pub coverage: Coverage,
    pub levels: Vec<Level>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub day: u64,
    pub periods: [PeriodResult; 3],
}

pub fn day_start(ms: u64) -> u64 {
    ms - ms % DAY_MS
}

pub fn week_start(ms: u64) -> u64 {
    const SHIFT: u64 = 3 * DAY_MS; // epoch was Thursday; weeks begin Monday
    (ms.saturating_add(SHIFT) / (7 * DAY_MS) * (7 * DAY_MS)).saturating_sub(SHIFT)
}

pub fn month_starts(ms: u64) -> (u64, u64) {
    let Some(date) = i64::try_from(ms)
        .ok()
        .and_then(|t| Utc.timestamp_millis_opt(t).single())
    else {
        return (0, 0);
    };
    let first = date.date_naive().with_day(1).expect("valid day one");
    let Some(previous) = first.checked_sub_months(chrono::Months::new(1)) else {
        return (0, 0);
    };
    let millis = |day: chrono::NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .and_then(|t| u64::try_from(t.and_utc().timestamp_millis()).ok())
            .unwrap_or(0)
    };
    (millis(first), millis(previous))
}

pub fn compute(daily: &BTreeMap<u64, (f64, f64)>, now: u64) -> Snapshot {
    let today = day_start(now);
    let week = week_start(now);
    let (month, previous_month) = month_starts(now);
    let periods = [
        (Period::Day, today.saturating_sub(DAY_MS), today),
        (Period::Week, week.saturating_sub(7 * DAY_MS), week),
        (Period::Month, previous_month, month),
    ]
    .map(|(period, from, to)| {
        let mut coverage = Coverage {
            expected_days: match period {
                Period::Day => 1,
                Period::Week => 7,
                Period::Month => ((to - from) / DAY_MS) as usize,
            },
            valid_days: 0,
            missing_days: Vec::new(),
            invalid_days: Vec::new(),
        };
        let (mut high, mut low) = (f64::NEG_INFINITY, f64::INFINITY);
        let mut day = from;
        while day < to {
            match daily.get(&day) {
                Some(&(h, l)) if h.is_finite() && l.is_finite() && l > 0.0 && h >= l => {
                    coverage.valid_days += 1;
                    high = high.max(h);
                    low = low.min(l);
                }
                Some(_) => coverage.invalid_days.push(day),
                None => coverage.missing_days.push(day),
            }
            day = day.saturating_add(DAY_MS);
        }
        let levels = if coverage.is_complete() {
            [(true, high), (false, low)]
                .map(|(is_high, price)| Level {
                    period,
                    is_high,
                    price,
                    from: to,
                })
                .to_vec()
        } else {
            Vec::new()
        };
        PeriodResult {
            period,
            source_from: from,
            source_to: to,
            coverage,
            levels,
        }
    });
    Snapshot {
        day: today,
        periods,
    }
}

/// Calculation owner: current results change only when inputs or UTC day change.
/// Historical snapshots are explicit queries; drawing only reads `current()`.
pub struct PeriodLevels {
    daily: BTreeMap<u64, (f64, f64)>,
    current: Snapshot,
}

impl PeriodLevels {
    pub fn new(now: u64) -> Self {
        let daily = BTreeMap::new();
        Self {
            current: compute(&daily, now),
            daily,
        }
    }
    pub fn current(&self) -> &Snapshot {
        &self.current
    }
    pub fn at(&self, now: u64) -> Snapshot {
        compute(&self.daily, now)
    }
    pub fn advance_to(&mut self, now: u64) {
        if self.current.day != day_start(now) {
            self.current = compute(&self.daily, now);
        }
    }
    pub fn insert(&mut self, bars: impl IntoIterator<Item = DailyBar>, now: u64) {
        let mut changed = self.current.day != day_start(now);
        for bar in bars {
            // Never floor an incorrectly timestamped bar into a valid UTC day.
            if bar.time % DAY_MS != 0 || bar.time >= day_start(now) {
                continue;
            }
            let hl = (bar.high, bar.low);
            if self.daily.get(&bar.time) != Some(&hl) {
                self.daily.insert(bar.time, hl);
                changed = true;
            }
        }
        if changed {
            self.current = compute(&self.daily, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(y: i32, m: u32, d: u32) -> u64 {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0)
            .unwrap()
            .timestamp_millis() as u64
    }
    fn history(from: u64, to: u64) -> Vec<DailyBar> {
        (from..to)
            .step_by(DAY_MS as usize)
            .enumerate()
            .map(|(i, time)| DailyBar {
                time,
                high: 100.0 + i as f64,
                low: 50.0 + i as f64,
            })
            .collect()
    }
    #[test]
    fn full_history_preserves_previous_levels_and_excludes_forming_day() {
        let now = ms(2026, 10, 28);
        let mut model = PeriodLevels::new(now);
        let mut bars = history(ms(2026, 9, 1), now);
        bars.push(DailyBar {
            time: now,
            high: 99999.0,
            low: 1.0,
        });
        model.insert(bars, now);
        let get = |period, high| {
            model
                .current()
                .periods
                .iter()
                .flat_map(|p| &p.levels)
                .find(|l| l.period == period && l.is_high == high)
                .map(|l| (l.price, l.from))
        };
        assert_eq!(get(Period::Day, true), Some((156.0, now)));
        assert_eq!(get(Period::Week, true), Some((154.0, ms(2026, 10, 26))));
        assert_eq!(get(Period::Week, false), Some((98.0, ms(2026, 10, 26))));
        assert_eq!(get(Period::Month, true), Some((129.0, ms(2026, 10, 1))));
        assert_eq!(get(Period::Month, false), Some((50.0, ms(2026, 10, 1))));
        model.advance_to(now + DAY_MS);
        // The previously forming bar must be refetched after closing, not
        // promoted to a complete yesterday from its earlier partial snapshot.
        assert!(model.current().periods[0].levels.is_empty());
    }
    #[test]
    fn hole_in_month_does_not_invalidate_complete_day_or_week_and_can_be_repaired() {
        let now = ms(2026, 10, 28);
        let missing = ms(2026, 9, 12);
        let mut model = PeriodLevels::new(now);
        let bars = history(ms(2026, 9, 1), now);
        model.insert(bars.iter().copied().filter(|b| b.time != missing), now);
        let p = &model.current().periods;
        assert!(p[0].coverage.is_complete() && p[1].coverage.is_complete());
        assert_eq!(p[2].coverage.valid_days, 29);
        assert_eq!(p[2].coverage.missing_days, vec![missing]);
        assert!(p[2].levels.is_empty());
        model.insert(bars.into_iter().filter(|b| b.time == missing), now);
        assert!(model.current().periods[2].coverage.is_complete());
    }
    #[test]
    fn calendar_boundaries_leap_february_and_rollover_are_explicit() {
        assert_eq!(
            month_starts(ms(2027, 1, 15)),
            (ms(2027, 1, 1), ms(2026, 12, 1))
        );
        assert_eq!(week_start(ms(2026, 11, 2)), ms(2026, 11, 2));
        assert_eq!(week_start(ms(2026, 11, 1)), ms(2026, 10, 26));
        let mut model = PeriodLevels::new(ms(2024, 3, 1));
        model.insert(history(ms(2024, 2, 1), ms(2024, 3, 1)), ms(2024, 3, 1));
        assert_eq!(model.current().periods[2].coverage.expected_days, 29);
        assert!(model.current().periods[2].coverage.is_complete());
        model.advance_to(ms(2024, 3, 2));
        assert!(model.current().periods[0].levels.is_empty());
        assert_eq!(model.current().day, ms(2024, 3, 2));
    }
    #[test]
    fn invalid_and_unaligned_bars_never_become_confirmed_levels() {
        let now = ms(2026, 11, 2);
        let yesterday = now - DAY_MS;
        let mut model = PeriodLevels::new(now);
        for (high, low) in [
            (f64::NAN, 1.0),
            (10.0, f64::INFINITY),
            (1.0, 2.0),
            (0.0, 0.0),
        ] {
            model.insert(
                [DailyBar {
                    time: yesterday,
                    high,
                    low,
                }],
                now,
            );
            assert!(model.current().periods[0].levels.is_empty());
            assert_eq!(
                model.current().periods[0].coverage.invalid_days,
                vec![yesterday]
            );
        }
        model.insert(
            [DailyBar {
                time: yesterday + 1,
                high: 10.0,
                low: 5.0,
            }],
            now,
        );
        assert!(model.current().periods[0].levels.is_empty());
        assert!(
            compute(&BTreeMap::new(), 0)
                .periods
                .iter()
                .all(|p| p.levels.is_empty())
        );
    }
}
