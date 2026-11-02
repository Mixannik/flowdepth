//! PDH/PDL, PWH/PWL, PMH/PML: highs and lows of the previous UTC day, week (from Monday)
//! and calendar month. Built from the chart market's daily candles, fetched separately, so
//! the levels exist even when the chart has only a few days loaded.
use crate::chart::{
    Basis, Message, ViewState,
    indicator::kline::{
        AuctionLevel, AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl,
        OverlayCtx,
    },
};
use crate::connector::fetcher::FetchRange;

use data::analysis::period_levels::{
    DAY_MS, DailyBar, Level, Period, PeriodLevels, Snapshot, day_start,
};
use data::chart::kline::PeriodLevelsConfig;
use exchange::unit::Price;
use exchange::{Kline, Timeframe, UnixMs};
use iced::widget::canvas::{self, Frame, LineDash, Path, Stroke};
use iced::{Alignment, Point};

/// Daily candles requested: enough for the previous calendar month.
const HISTORY_DAYS: u64 = 66;
/// Retry period while any enabled source period is incomplete.
const POLL_MS: u64 = 60_000;

pub struct PeriodLevelsIndicator {
    model: PeriodLevels,
    config: PeriodLevelsConfig,
}

impl PeriodLevelsIndicator {
    pub fn new() -> Self {
        Self {
            model: PeriodLevels::new(chrono::Utc::now().timestamp_millis() as u64),
            config: PeriodLevelsConfig::default(),
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
        self.model.advance_to(now);
        if !self.needs_history() {
            return None;
        }
        // `to` moves in POLL_MS steps; identical requests are throttled by the handler.
        Some(FetchRange::RefKlines {
            from: UnixMs::new(day_start(now).saturating_sub(HISTORY_DAYS * DAY_MS)),
            to: UnixMs::new(now - now % POLL_MS),
            timeframe: Timeframe::D1,
            spot: false,
        })
    }

    fn on_ref_klines(&mut self, spot: bool, timeframe: Timeframe, klines: &[Kline]) {
        if spot || timeframe != Timeframe::D1 {
            return;
        }
        self.model.insert(
            klines.iter().map(|k| DailyBar {
                time: k.time.as_u64(),
                high: k.high.to_f64(),
                low: k.low.to_f64(),
            }),
            chrono::Utc::now().timestamp_millis() as u64,
        );
    }

    fn on_config_changed(&mut self, config: &data::chart::kline::Config) {
        self.config = config.indicator_configs.period_levels;
    }

    fn draw_overlay(&self, frame: &mut Frame, ctx: &OverlayCtx) {
        draw(&self.current_levels(), frame, ctx, &self.config);
    }

    fn reference_prices(&self) -> Vec<f64> {
        self.current_levels()
            .iter()
            .map(|level| level.price)
            .collect()
    }

    fn data_warning(&self) -> Option<String> {
        let missing = self
            .enabled_periods()
            .filter(|p| !p.coverage.is_complete())
            .map(|p| {
                let name = match p.period {
                    Period::Day => "day",
                    Period::Week => "week",
                    Period::Month => "month",
                };
                let date = |ms| {
                    chrono::DateTime::from_timestamp_millis(ms as i64)
                        .map(|t| t.format("%Y-%m-%d").to_string())
                        .unwrap_or_default()
                };
                format!(
                    "Previous {name} ({} to {} UTC): {}/{} valid daily candles; {} invalid.",
                    date(p.source_from),
                    date(p.source_to.saturating_sub(DAY_MS)),
                    p.coverage.valid_days,
                    p.coverage.expected_days,
                    p.coverage.invalid_days.len()
                )
            })
            .collect::<Vec<_>>();
        (!missing.is_empty()).then(|| format!("Period levels incomplete\n{}\nIncomplete levels are hidden and excluded from signals. Daily history is checked every minute.", missing.join("\n")))
    }

    /// Each day's own levels (yesterday's high on that day, and so on), so history shows
    /// failed auctions at the levels of its time.
    fn auction_levels(&self, from: u64, to: u64) -> Vec<AuctionLevel> {
        let mut out = Vec::new();
        let mut day = day_start(from);
        while day <= to {
            out.extend(self.levels_at(day).into_iter().map(|level| AuctionLevel {
                price: level.price,
                from: day,
                to: day + DAY_MS,
                name: level.name(),
                max_outside_ms: match level.period {
                    Period::Day => 4 * 60 * 60_000,
                    Period::Week => DAY_MS,
                    Period::Month => 3 * DAY_MS,
                },
            }));
            day += DAY_MS;
        }
        out
    }
}

impl PeriodLevelsIndicator {
    fn needs_history(&self) -> bool {
        self.enabled_periods().any(|p| !p.coverage.is_complete())
    }

    fn enabled(&self, period: Period) -> bool {
        match period {
            Period::Day => self.config.day,
            Period::Week => self.config.week,
            Period::Month => self.config.month,
        }
    }

    fn enabled_periods(
        &self,
    ) -> impl Iterator<Item = &data::analysis::period_levels::PeriodResult> {
        self.model
            .current()
            .periods
            .iter()
            .filter(|p| self.enabled(p.period))
    }

    fn current_levels(&self) -> Vec<Level> {
        self.enabled_periods()
            .flat_map(|p| p.levels.iter().copied())
            .collect()
    }

    fn levels_at(&self, now: u64) -> Vec<Level> {
        let Snapshot { periods, .. } = self.model.at(now);
        periods
            .into_iter()
            .filter(|p| self.enabled(p.period))
            .flat_map(|p| p.levels)
            .collect()
    }
}

/// Muted violet and rose, distinct from the green/red zones.
const WEEK_COLOR: iced::Color = iced::Color::from_rgb(0.66, 0.58, 0.88);
const MONTH_COLOR: iced::Color = iced::Color::from_rgb(0.86, 0.56, 0.70);

/// Draws each level from the start of the current period to `right_x`, labelled at its
/// left end so the labels stay clear of the right-edge naked-level labels.
fn draw(levels: &[Level], frame: &mut Frame, ctx: &OverlayCtx, cfg: &PeriodLevelsConfig) {
    let opacity = cfg.opacity.clamp(0.1, 1.0);
    let OverlayCtx {
        x,
        y,
        left_x,
        right_x,
        scaling,
        text_color,
    } = *ctx;
    let px = |p: f32| p / scaling;
    let label_size = px(10.0);
    for level in levels {
        let start = x(level.from).max(left_x);
        if start >= right_x {
            continue;
        }
        let line_y = y(Price::from_f64(level.price));
        // Background references: thin, dashed and faint so candles and zones stay on top.
        let (color, dash): (_, &'static [f32]) = match level.period {
            Period::Day => (text_color.scale_alpha(opacity * 0.75), &[3.0, 4.0]),
            Period::Week => (WEEK_COLOR.scale_alpha(opacity), &[6.0, 4.0]),
            Period::Month => (MONTH_COLOR.scale_alpha(opacity), &[10.0, 4.0]),
        };
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
        if !cfg.labels {
            continue;
        }
        frame.fill_text(canvas::Text {
            content: format!("{} {:.0}", level.name(), level.price),
            position: Point::new(start + label_size * 0.4, line_y - label_size * 0.15),
            size: iced::Pixels(label_size),
            color: color.scale_alpha(1.6),
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
    use chrono::{TimeZone, Utc};

    #[test]
    fn partial_month_keeps_loading_and_never_enters_reference_or_auction_levels() {
        let now = Utc
            .with_ymd_and_hms(2026, 11, 2, 0, 0, 0)
            .unwrap()
            .timestamp_millis() as u64;
        let missing = Utc
            .with_ymd_and_hms(2026, 10, 12, 0, 0, 0)
            .unwrap()
            .timestamp_millis() as u64;
        let first = Utc
            .with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
            .unwrap()
            .timestamp_millis() as u64;
        let mut indicator = PeriodLevelsIndicator {
            model: PeriodLevels::new(now),
            config: PeriodLevelsConfig::default(),
        };
        indicator.model.insert(
            (first..now)
                .step_by(DAY_MS as usize)
                .filter(|&t| t != missing)
                .map(|time| DailyBar {
                    time,
                    high: 100.0,
                    low: 50.0,
                }),
            now,
        );
        assert!(indicator.needs_history()); // Yesterday exists, the month is not complete.
        assert_eq!(indicator.current_levels().len(), 4);
        assert_eq!(indicator.reference_prices().len(), 4);
        assert!(
            indicator
                .auction_levels(now, now)
                .iter()
                .all(|l| !["PMH", "PML"].contains(&l.name))
        );
        let warning = indicator.data_warning().unwrap();
        assert!(warning.contains("30/31"));
        indicator.config.month = false;
        assert!(!indicator.needs_history());
        assert!(indicator.data_warning().is_none());
        indicator.config.month = true;
        indicator.model.insert(
            [DailyBar {
                time: missing,
                high: 120.0,
                low: 40.0,
            }],
            now,
        );
        assert!(!indicator.needs_history());
        assert!(indicator.data_warning().is_none());
        assert_eq!(indicator.current_levels().len(), 6);
        let snapshot = indicator.model.current().clone();
        assert_eq!(indicator.current_levels(), indicator.levels_at(now));
        let _ = indicator.reference_prices();
        assert_eq!(indicator.model.current(), &snapshot);
    }
}
