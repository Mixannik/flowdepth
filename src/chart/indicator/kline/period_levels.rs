//! PDH/PDL, PWH/PWL, PMH/PML: highs and lows of the previous UTC day, week (from Monday)
//! and calendar month. Built from the chart market's daily candles, fetched separately, so
//! the levels exist even when the chart has only a few days loaded.
use crate::chart::{
    Basis, Message, ViewState,
    indicator::kline::{AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl},
};
use crate::connector::fetcher::FetchRange;

use chrono::{Datelike, TimeZone, Utc};
use exchange::unit::Price;
use exchange::{Kline, Timeframe, UnixMs};
use iced::widget::canvas::{self, Frame, LineDash, Path, Stroke};
use iced::{Alignment, Point};
use std::collections::BTreeMap;

const DAY_MS: u64 = 24 * 60 * 60_000;
/// Daily candles requested: enough for the previous calendar month.
const HISTORY_DAYS: u64 = 66;
/// Retry period while yesterday's candle is missing.
const POLL_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub period: Period,
    pub is_high: bool,
    pub price: f64,
    /// Start of the current period: the line starts here.
    pub from: u64,
}

impl Level {
    fn name(&self) -> &'static str {
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

fn day_start(ms: u64) -> u64 {
    ms - ms % DAY_MS
}

/// Monday 00:00 UTC of the week containing `ms` (the epoch was a Thursday).
fn week_start(ms: u64) -> u64 {
    const MONDAY_SHIFT: u64 = 3 * DAY_MS;
    let week = 7 * DAY_MS;
    ((ms + MONDAY_SHIFT) / week * week).saturating_sub(MONDAY_SHIFT)
}

/// First day 00:00 UTC of the month containing `ms`, and of the month before it.
fn month_starts(ms: u64) -> (u64, u64) {
    let date = Utc
        .timestamp_millis_opt(ms as i64)
        .single()
        .unwrap_or_default()
        .date_naive();
    let first = date.with_day(1).unwrap_or(date);
    let prev = if first.month() == 1 {
        first
            .with_year(first.year() - 1)
            .and_then(|d| d.with_month(12))
    } else {
        first.with_month(first.month() - 1)
    }
    .unwrap_or(first);
    let to_ms = |d: chrono::NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .map_or(0, |t| t.and_utc().timestamp_millis() as u64)
    };
    (to_ms(first), to_ms(prev))
}

/// Levels of the periods before the one containing `now`; `daily` maps candle open time to
/// (high, low).
pub fn compute(daily: &BTreeMap<u64, (f64, f64)>, now: u64) -> Vec<Level> {
    let today = day_start(now);
    let this_week = week_start(now);
    let (this_month, prev_month) = month_starts(now);
    let periods = [
        (Period::Day, today - DAY_MS, today),
        (Period::Week, this_week - 7 * DAY_MS, this_week),
        (Period::Month, prev_month, this_month),
    ];
    let mut levels = Vec::new();
    for (period, from, to) in periods {
        let mut bars = daily.range(from..to).map(|(_, hl)| *hl).peekable();
        if bars.peek().is_none() {
            continue;
        }
        let (high, low) = bars.fold((f64::MIN, f64::MAX), |(h, l), (bh, bl)| {
            (h.max(bh), l.min(bl))
        });
        for (is_high, price) in [(true, high), (false, low)] {
            levels.push(Level {
                period,
                is_high,
                price,
                from: to,
            });
        }
    }
    levels
}

pub struct PeriodLevelsIndicator {
    /// Daily candle open time → (high, low).
    daily: BTreeMap<u64, (f64, f64)>,
}

impl PeriodLevelsIndicator {
    pub fn new() -> Self {
        Self {
            daily: BTreeMap::new(),
        }
    }
}

impl KlineIndicatorImpl for PeriodLevelsIndicator {
    fn clear_all_caches(&mut self) {}

    fn clear_crosshair_caches(&mut self) {}

    fn element<'a>(
        &'a self,
        _chart: &'a ViewState,
        _data_labels_always_visible: bool,
        _visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        unreachable!("period levels render on the main chart canvas")
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
        let yesterday = day_start(now) - DAY_MS;
        if self
            .daily
            .last_key_value()
            .is_some_and(|(&last, _)| last >= yesterday)
        {
            return None;
        }
        // `to` moves in POLL_MS steps; identical requests are throttled by the handler.
        Some(FetchRange::RefKlines {
            from: UnixMs::new(day_start(now) - HISTORY_DAYS * DAY_MS),
            to: UnixMs::new(now - now % POLL_MS),
            timeframe: Timeframe::D1,
            spot: false,
        })
    }

    fn on_ref_klines(&mut self, klines: &[Kline]) {
        self.daily.extend(
            klines
                .iter()
                .map(|k| (k.time.as_u64(), (k.high.to_f64(), k.low.to_f64()))),
        );
    }

    fn period_levels(&self) -> Vec<Level> {
        compute(&self.daily, chrono::Utc::now().timestamp_millis() as u64)
    }
}

const WEEK_COLOR: iced::Color = iced::Color::from_rgb(0.72, 0.56, 1.0);
const MONTH_COLOR: iced::Color = iced::Color::from_rgb(1.0, 0.47, 0.72);

/// Draws each level from the start of the current period to `right_x`, labelled at its
/// left end so the labels stay clear of the right-edge naked-level labels.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    levels: &[Level],
    frame: &mut Frame,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    left_x: f32,
    right_x: f32,
    scaling: f32,
    text_color: iced::Color,
) {
    let px = |p: f32| p / scaling;
    let label_size = px(10.0);
    for level in levels {
        let start = x(level.from).max(left_x);
        if start >= right_x {
            continue;
        }
        let line_y = y(Price::from_f64(level.price));
        let (color, width, dashed) = match level.period {
            Period::Day => (text_color.scale_alpha(0.6), 1.0, true),
            Period::Week => (WEEK_COLOR, 1.3, false),
            Period::Month => (MONTH_COLOR, 1.6, false),
        };
        let mut stroke = Stroke::default().with_color(color).with_width(px(width));
        if dashed {
            stroke.line_dash = LineDash {
                segments: &[6.0, 4.0],
                offset: 0,
            };
        }
        frame.stroke(
            &Path::line(Point::new(start, line_y), Point::new(right_x, line_y)),
            stroke,
        );
        frame.fill_text(canvas::Text {
            content: format!("{} {:.0}", level.name(), level.price),
            position: Point::new(start + label_size * 0.4, line_y - label_size * 0.15),
            size: iced::Pixels(label_size),
            color,
            align_x: Alignment::Start.into(),
            align_y: Alignment::End.into(),
            font: crate::style::AZERET_MONO,
            ..canvas::Text::default()
        });
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

    #[test]
    fn previous_day_week_and_month() {
        // Wednesday 2026-10-28; daily bars from 2026-09-01.
        let mut daily = BTreeMap::new();
        let mut t = ms(2026, 9, 1);
        let mut i = 0.0;
        while t <= ms(2026, 10, 28) {
            daily.insert(t, (100.0 + i, 50.0 + i));
            t += DAY_MS;
            i += 1.0;
        }
        let levels = compute(&daily, ms(2026, 10, 28) + 5 * 3_600_000);
        let get = |p: Period, h: bool| {
            levels
                .iter()
                .find(|l| l.period == p && l.is_high == h)
                .map(|l| (l.price, l.from))
        };
        // Day 2026-10-27 is index 56.
        assert_eq!(get(Period::Day, true), Some((156.0, ms(2026, 10, 28))));
        // Week 2026-10-19..25 (Mon..Sun): indices 48..54.
        assert_eq!(get(Period::Week, true), Some((154.0, ms(2026, 10, 26))));
        assert_eq!(get(Period::Week, false), Some((98.0, ms(2026, 10, 26))));
        // September: indices 0..29.
        assert_eq!(get(Period::Month, true), Some((129.0, ms(2026, 10, 1))));
        assert_eq!(get(Period::Month, false), Some((50.0, ms(2026, 10, 1))));
    }

    #[test]
    fn january_looks_back_to_december() {
        assert_eq!(
            month_starts(ms(2027, 1, 15)),
            (ms(2027, 1, 1), ms(2026, 12, 1))
        );
    }
}
