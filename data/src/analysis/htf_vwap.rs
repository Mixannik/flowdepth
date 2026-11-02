//! Developing UTC VWAP from hourly typical prices and volumes, independent of UI/I/O.
use super::period_levels::{DAY_MS, month_starts, week_start};
use std::collections::BTreeMap;

pub const HOUR_MS: u64 = 3_600_000;
pub fn hour_start(time: u64) -> u64 {
    time - time % HOUR_MS
}
pub fn retained_since(now: u64) -> u64 {
    week_start(month_starts(now).1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Week,
    Month,
}
impl Anchor {
    pub fn start(self, time: u64) -> u64 {
        match self {
            Self::Week => week_start(time),
            Self::Month => month_starts(time).0,
        }
    }
    fn next(self, start: u64) -> u64 {
        match self {
            Self::Week => start.saturating_add(7 * DAY_MS),
            Self::Month => month_starts(start.saturating_add(32 * DAY_MS)).0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HourlyBar {
    pub time: u64,
    pub price: f64,
    pub volume: f64,
}
#[derive(Debug, Clone, Copy, PartialEq)]
struct StoredBar {
    bar: HourlyBar,
    closed_on_receipt: bool,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VwapPoint {
    pub time: u64,
    pub value: f64,
    pub sigma: f64,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Coverage {
    /// Includes an available forming candle, even when its volume is zero.
    pub observed_hours: usize,
    pub expected_closed_hours: usize,
    pub valid_closed_hours: usize,
    pub missing_hours: Vec<u64>,
    pub invalid_hours: Vec<u64>,
    pub unconfirmed_hours: Vec<u64>,
}
impl Coverage {
    pub fn is_complete(&self) -> bool {
        self.expected_closed_hours == self.valid_closed_hours
            && self.invalid_hours.is_empty()
            && self.missing_hours.is_empty()
            && self.unconfirmed_hours.is_empty()
    }
    pub fn first_gap(&self) -> Option<u64> {
        self.missing_hours
            .iter()
            .chain(&self.invalid_hours)
            .chain(&self.unconfirmed_hours)
            .copied()
            .min()
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub start: u64,
    pub end: u64,
    pub coverage: Coverage,
    pub points: Vec<VwapPoint>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub hour: u64,
    pub weekly: Vec<Run>,
    pub monthly: Vec<Run>,
}
impl Snapshot {
    pub fn runs(&self, anchor: Anchor) -> &[Run] {
        match anchor {
            Anchor::Week => &self.weekly,
            Anchor::Month => &self.monthly,
        }
    }
    pub fn reference(&self, anchor: Anchor) -> Option<f64> {
        let run = self
            .runs(anchor)
            .iter()
            .find(|r| r.start == anchor.start(self.hour))?;
        if !run.coverage.is_complete() {
            return None;
        }
        run.points.last().map(|p| p.value)
    }
    pub fn fetch_from(&self, weekly: bool, monthly: bool) -> u64 {
        [(weekly, Anchor::Week), (monthly, Anchor::Month)]
            .into_iter()
            .filter(|(enabled, _)| *enabled)
            .flat_map(|(_, a)| self.runs(a))
            .filter_map(|r| r.coverage.first_gap())
            .min()
            .unwrap_or(self.hour)
            .min(self.hour)
    }
}

fn calculate(hourly: &BTreeMap<u64, StoredBar>, now: u64) -> Snapshot {
    let hour = hour_start(now);
    let build = |anchor: Anchor| {
        let mut runs = Vec::new();
        let mut start = match anchor {
            Anchor::Week => retained_since(now),
            Anchor::Month => month_starts(now).1,
        };
        // Retention spans at most two calendar months plus their first UTC week.
        // Bound malformed/out-of-calendar timestamps as well as normal operation.
        for _ in 0..12 {
            if start > hour {
                break;
            }
            let next = anchor.next(start);
            if next <= start {
                break;
            }
            let end = next.min(hour.saturating_add(HOUR_MS));
            let closed_end = next.min(hour);
            let mut coverage = Coverage {
                expected_closed_hours: ((closed_end - start) / HOUR_MS) as usize,
                ..Coverage::default()
            };
            let mut points = Vec::new();
            let (mut weight, mut mean, mut m2) = (0.0_f64, 0.0_f64, 0.0_f64);
            let mut continuous = true;
            let mut time = start;
            while time < end {
                let closed = time < hour;
                if hourly.contains_key(&time) {
                    coverage.observed_hours += 1;
                }
                match hourly.get(&time) {
                    None if closed => {
                        coverage.missing_hours.push(time);
                        continuous = false;
                    }
                    None => {} // The current forming candle may not have been published yet.
                    Some(stored) if closed && !stored.closed_on_receipt => {
                        coverage.unconfirmed_hours.push(time);
                        continuous = false;
                    }
                    Some(stored) => {
                        let bar = stored.bar;
                        if !bar.price.is_finite()
                            || bar.price <= 0.0
                            || !bar.volume.is_finite()
                            || bar.volume < 0.0
                        {
                            coverage.invalid_hours.push(time);
                            continuous = false;
                        } else {
                            if closed {
                                coverage.valid_closed_hours += 1;
                            }
                            if continuous {
                                // Weighted Welford: same population variance as E[p²]-E[p]²,
                                // without cancellation when prices are large and variance small.
                                let total = weight + bar.volume;
                                let delta = bar.price - mean;
                                let next_mean = if total > 0.0 {
                                    mean + delta * (bar.volume / total)
                                } else {
                                    mean
                                };
                                let next_m2 = m2 + bar.volume * delta * (bar.price - next_mean);
                                if !total.is_finite()
                                    || !next_mean.is_finite()
                                    || !next_m2.is_finite()
                                {
                                    coverage.invalid_hours.push(time);
                                    continuous = false;
                                } else {
                                    (weight, mean, m2) = (total, next_mean, next_m2);
                                    if weight > 0.0 && (m2 / weight).is_finite() {
                                        points.push(VwapPoint {
                                            time,
                                            value: mean,
                                            sigma: (m2 / weight).max(0.0).sqrt(),
                                        });
                                    } else if weight > 0.0 {
                                        coverage.invalid_hours.push(time);
                                        continuous = false;
                                    }
                                }
                            }
                        }
                    }
                }
                time = time.saturating_add(HOUR_MS);
            }
            runs.push(Run {
                start,
                end,
                coverage,
                points,
            });
            start = next;
        }
        runs
    };
    Snapshot {
        hour,
        weekly: build(Anchor::Week),
        monthly: build(Anchor::Month),
    }
}

pub struct HtfVwap {
    hourly: BTreeMap<u64, StoredBar>,
    current: Snapshot,
}
impl HtfVwap {
    pub fn new(now: u64) -> Self {
        let hourly = BTreeMap::new();
        Self {
            current: calculate(&hourly, now),
            hourly,
        }
    }
    pub fn current(&self) -> &Snapshot {
        &self.current
    }
    pub fn advance_to(&mut self, now: u64) {
        if self.current.hour != hour_start(now) {
            self.hourly
                .retain(|&t, _| t >= retained_since(now) && t <= hour_start(now));
            self.current = calculate(&self.hourly, now);
        }
    }
    pub fn insert(&mut self, bars: impl IntoIterator<Item = HourlyBar>, now: u64) {
        let since = retained_since(now);
        let mut changed = self.current.hour != hour_start(now);
        for bar in bars {
            if bar.time % HOUR_MS != 0 || bar.time < since || bar.time > hour_start(now) {
                continue;
            }
            let stored = StoredBar {
                bar,
                closed_on_receipt: bar.time.saturating_add(HOUR_MS) <= now,
            };
            if self.hourly.get(&bar.time) != Some(&stored) {
                self.hourly.insert(bar.time, stored);
                changed = true;
            }
        }
        self.hourly
            .retain(|&t, _| t >= since && t <= hour_start(now));
        if changed {
            self.current = calculate(&self.hourly, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    fn ms(y: i32, m: u32, d: u32, h: u32) -> u64 {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0)
            .unwrap()
            .timestamp_millis() as u64
    }
    fn history(from: u64, to: u64) -> Vec<HourlyBar> {
        (from..=to)
            .step_by(HOUR_MS as usize)
            .map(|time| HourlyBar {
                time,
                price: 100.0,
                volume: 1.0,
            })
            .collect()
    }
    #[test]
    fn weighted_mean_and_bands_match_reference_and_forming_updates_replace() {
        let now = ms(2026, 11, 2, 1);
        let start = ms(2026, 11, 2, 0);
        let mut model = HtfVwap::new(now);
        model.insert(
            [
                HourlyBar {
                    time: start,
                    price: 100.0,
                    volume: 1.0,
                },
                HourlyBar {
                    time: now,
                    price: 110.0,
                    volume: 3.0,
                },
            ],
            now,
        );
        let run = model.current().weekly.last().unwrap();
        let point = run.points.last().unwrap();
        assert_eq!(point.value, 107.5);
        assert!((point.sigma - 18.75_f64.sqrt()).abs() < 1e-9);
        model.insert(
            [HourlyBar {
                time: now,
                price: 120.0,
                volume: 1.0,
            }],
            now + 1,
        );
        assert_eq!(model.current().reference(Anchor::Week), Some(110.0));
    }
    #[test]
    fn gap_stops_curve_and_reference_then_repairs_without_resetting_anchor() {
        let now = ms(2026, 11, 2, 4);
        let start = ms(2026, 11, 2, 0);
        let gap = start + HOUR_MS;
        let mut model = HtfVwap::new(now);
        model.insert(
            history(start, now).into_iter().filter(|b| b.time != gap),
            now,
        );
        let run = model.current().weekly.last().unwrap();
        assert_eq!(run.points.len(), 1);
        assert_eq!(run.coverage.missing_hours, vec![gap]);
        assert_eq!(model.current().reference(Anchor::Week), None);
        assert!(model.current().fetch_from(true, false) <= gap);
        model.insert(
            [HourlyBar {
                time: gap,
                price: 200.0,
                volume: 1.0,
            }],
            now,
        );
        assert_eq!(model.current().reference(Anchor::Week), Some(120.0));
    }
    #[test]
    fn month_is_independent_from_complete_week_and_no_previous_anchor_fallback() {
        let now = ms(2026, 11, 9, 1);
        let start = ms(2026, 11, 9, 0);
        let mut model = HtfVwap::new(now);
        model.insert(history(start, now), now);
        assert_eq!(model.current().reference(Anchor::Week), Some(100.0));
        assert_eq!(model.current().reference(Anchor::Month), None);
        model.advance_to(ms(2026, 11, 16, 0));
        assert_eq!(model.current().reference(Anchor::Week), None);
    }
    #[test]
    fn forming_hour_requires_closed_refresh_and_zero_volume_is_not_a_gap() {
        let now = ms(2026, 11, 2, 0);
        let mut model = HtfVwap::new(now);
        let bar = HourlyBar {
            time: now,
            price: 100.0,
            volume: 0.0,
        };
        model.insert([bar], now);
        let run = model.current().weekly.last().unwrap();
        assert!(run.coverage.is_complete());
        assert_eq!(run.coverage.observed_hours, 1);
        assert!(run.points.is_empty());
        model.advance_to(now + HOUR_MS);
        assert_eq!(
            model
                .current()
                .weekly
                .last()
                .unwrap()
                .coverage
                .unconfirmed_hours,
            vec![now]
        );
        model.insert([bar], now + HOUR_MS);
        assert!(
            model
                .current()
                .weekly
                .last()
                .unwrap()
                .coverage
                .is_complete()
        );
        assert_eq!(model.current().reference(Anchor::Week), None);
    }
    #[test]
    fn bad_inputs_never_produce_nonfinite_points_and_retention_is_bounded() {
        let now = ms(2026, 11, 2, 1);
        let mut model = HtfVwap::new(now);
        model.insert(
            [
                HourlyBar {
                    time: now - HOUR_MS,
                    price: f64::NAN,
                    volume: 1.0,
                },
                HourlyBar {
                    time: now,
                    price: 100.0,
                    volume: -1.0,
                },
                HourlyBar {
                    time: now + 1,
                    price: 100.0,
                    volume: 1.0,
                },
            ],
            now,
        );
        assert_eq!(model.current().reference(Anchor::Week), None);
        assert!(model.current().weekly.last().unwrap().points.is_empty());
        model.insert(history(retained_since(now), now), now);
        model.advance_to(ms(2027, 2, 1, 0));
        assert!(model.hourly.is_empty());
    }
}
