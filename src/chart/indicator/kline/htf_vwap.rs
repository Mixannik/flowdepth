//! Runtime of the VWAP indicator. The session VWAP (from trades) is drawn by `KlineChart`;
//! this part adds the developing weekly (from Monday 00:00 UTC) and monthly VWAP, built from
//! the chart market's hourly candles with the typical price (H+L+C)/3 and fetched separately,
//! so both lines exist without loading a month of trades. Hourly-candle approximation;
//! it is not an exact trade-based VWAP.
use crate::chart::{
    Basis, Message, ViewState,
    indicator::kline::{
        AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl, OverlayCtx,
    },
};
use crate::connector::fetcher::FetchRange;
use data::analysis::htf_vwap::{Anchor, HourlyBar, HtfVwap};

use data::chart::kline::VwapConfig;
use exchange::unit::Price;
use exchange::{Kline, Timeframe, UnixMs};
use iced::widget::canvas::{self, Frame, LineDash, Stroke};
use iced::{Alignment, Point};

/// Refresh period of the forming hourly candle.
const POLL_MS: u64 = 30_000;

const WEEK_COLOR: iced::Color = iced::Color::from_rgba(0.40, 0.72, 0.86, 0.75);
const MONTH_COLOR: iced::Color = iced::Color::from_rgba(0.92, 0.66, 0.38, 0.75);

pub struct HtfVwapIndicator {
    model: HtfVwap,
    config: VwapConfig,
}

impl HtfVwapIndicator {
    pub fn new() -> Self {
        Self {
            model: HtfVwap::new(chrono::Utc::now().timestamp_millis() as u64),
            config: VwapConfig::default(),
        }
    }
}

impl KlineIndicatorImpl for HtfVwapIndicator {
    fn clear_all_caches(&mut self) {}

    fn clear_crosshair_caches(&mut self) {}

    fn element<'a>(
        &'a self,
        _chart: &'a ViewState,
        _data_labels_always_visible: bool,
        _visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        unreachable!("VWAP renders on the main chart canvas")
    }

    fn availability(&self, chart: &ViewState) -> IndicatorAvailability {
        match chart.basis {
            Basis::Tick(_) => {
                IndicatorAvailability::Unavailable(AvailabilityCause::Basis(chart.basis))
            }
            Basis::Time(_) => IndicatorAvailability::Available,
        }
    }

    fn on_config_changed(&mut self, config: &data::chart::kline::Config) {
        self.config = config.vwap;
    }

    fn fetch_range(&mut self, _ctx: &FetchCtx) -> Option<FetchRange> {
        if !self.config.weekly && !self.config.monthly {
            return None;
        }
        let now = chrono::Utc::now().timestamp_millis() as u64;
        self.model.advance_to(now);
        let from = self
            .model
            .current()
            .fetch_from(self.config.weekly, self.config.monthly);
        // `to` moves in POLL_MS steps; identical requests are throttled by the handler.
        Some(FetchRange::RefKlines {
            from: UnixMs::new(from),
            to: UnixMs::new(now - now % POLL_MS).max(UnixMs::new(from.saturating_add(1))),
            timeframe: Timeframe::H1,
            spot: false,
        })
    }

    fn on_ref_klines(&mut self, spot: bool, timeframe: Timeframe, klines: &[Kline]) {
        if spot || timeframe != Timeframe::H1 {
            return;
        }
        self.model.insert(
            klines.iter().map(|k| HourlyBar {
                time: k.time.as_u64(),
                price: (k.high.to_f64() + k.low.to_f64() + k.close.to_f64()) / 3.0,
                volume: k.volume.total().to_f64(),
            }),
            chrono::Utc::now().timestamp_millis() as u64,
        );
    }

    /// Current weekly and monthly VWAP.
    fn reference_prices(&self) -> Vec<f64> {
        [
            (self.config.weekly, Anchor::Week),
            (self.config.monthly, Anchor::Month),
        ]
        .into_iter()
        .filter(|(enabled, _)| *enabled)
        .filter_map(|(_, anchor)| self.model.current().reference(anchor))
        .collect()
    }

    fn data_warning(&self) -> Option<String> {
        let mut details = Vec::new();
        for (enabled, anchor, name) in [
            (self.config.weekly, Anchor::Week, "Weekly"),
            (self.config.monthly, Anchor::Month, "Monthly"),
        ] {
            if !enabled {
                continue;
            }
            let runs = self.model.current().runs(anchor);
            let missing: usize = runs.iter().map(|r| r.coverage.missing_hours.len()).sum();
            let invalid: usize = runs.iter().map(|r| r.coverage.invalid_hours.len()).sum();
            let pending: usize = runs
                .iter()
                .map(|r| r.coverage.unconfirmed_hours.len())
                .sum();
            if let Some(first) = runs.iter().filter_map(|r| r.coverage.first_gap()).min() {
                let date = chrono::DateTime::from_timestamp_millis(first as i64)
                    .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_default();
                details.push(format!("{name} VWAP: {missing} missing, {invalid} invalid, {pending} awaiting closed-hour update. First: {date}."));
            }
        }
        (!details.is_empty()).then(|| format!("VWAP history incomplete\n{}\nLines stop at the first gap; incomplete current VWAP is excluded from level filters. Based on hourly candles.", details.join("\n")))
    }

    fn draw_overlay(&self, frame: &mut Frame, ctx: &OverlayCtx) {
        let cfg = self.config;
        let label_size = 10.0 / ctx.scaling;
        let width = cfg.line_width.clamp(0.5, 5.0) / ctx.scaling;
        let multiplier = f64::from(cfg.band_multiplier.max(0.0));
        let anchors = [
            (cfg.weekly, Anchor::Week, WEEK_COLOR, "W VWAP"),
            (cfg.monthly, Anchor::Month, MONTH_COLOR, "M VWAP"),
        ];
        for (enabled, anchor, color, name) in anchors {
            if !enabled {
                continue;
            }
            let runs = self.model.current().runs(anchor);
            for run in runs.iter().filter(|run| !run.points.is_empty()) {
                let stroke_line = |frame: &mut Frame, offset: f64, stroke: Stroke| {
                    let mut path = canvas::path::Builder::new();
                    for (j, p) in run.points.iter().enumerate() {
                        let price = Price::from_f64(p.value + offset * p.sigma * multiplier);
                        let point = Point::new((ctx.x)(p.time), (ctx.y)(price));
                        if j == 0 {
                            path.move_to(point);
                        } else {
                            path.line_to(point);
                        }
                    }
                    frame.stroke(&path.build(), stroke);
                };
                stroke_line(
                    frame,
                    0.0,
                    Stroke::default().with_color(color).with_width(width),
                );
                if cfg.show_bands {
                    let band = Stroke {
                        line_dash: LineDash {
                            segments: &[4.0, 4.0],
                            offset: 0,
                        },
                        ..Stroke::default()
                            .with_color(color.scale_alpha(0.5))
                            .with_width(1.0 / ctx.scaling)
                    };
                    stroke_line(frame, 1.0, band);
                    stroke_line(frame, -1.0, band);
                }
                if cfg.show_labels
                    && run.start == anchor.start(self.model.current().hour)
                    && run.coverage.is_complete()
                    && let Some(p) = run.points.last()
                {
                    frame.fill_text(canvas::Text {
                        content: format!("{name} {:.0}", p.value),
                        position: Point::new(
                            (ctx.x)(p.time) + label_size * 0.5,
                            (ctx.y)(Price::from_f64(p.value)),
                        ),
                        size: iced::Pixels(label_size),
                        color,
                        align_x: Alignment::Start.into(),
                        align_y: Alignment::Center.into(),
                        font: crate::style::AZERET_MONO,
                        ..canvas::Text::default()
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use data::analysis::htf_vwap::{HOUR_MS, retained_since};

    #[test]
    fn incomplete_vwap_warns_stops_at_gap_and_recovers_references() {
        let now = Utc
            .with_ymd_and_hms(2026, 11, 2, 4, 0, 0)
            .unwrap()
            .timestamp_millis() as u64;
        let gap = now - 2 * HOUR_MS;
        let mut indicator = HtfVwapIndicator {
            model: HtfVwap::new(now),
            config: VwapConfig::default(),
        };
        indicator.config.weekly = true;
        indicator.config.monthly = true;
        indicator.model.insert(
            (retained_since(now)..=now)
                .step_by(HOUR_MS as usize)
                .filter(|&t| t != gap)
                .map(|time| HourlyBar {
                    time,
                    price: 100.0,
                    volume: 1.0,
                }),
            now,
        );
        assert!(indicator.reference_prices().is_empty());
        assert!(
            indicator
                .data_warning()
                .unwrap()
                .contains("2026-11-02 02:00 UTC")
        );
        assert_eq!(indicator.model.current().fetch_from(true, true), gap);
        indicator.model.insert(
            [HourlyBar {
                time: gap,
                price: 100.0,
                volume: 1.0,
            }],
            now,
        );
        assert_eq!(indicator.reference_prices(), vec![100.0, 100.0]);
        assert!(indicator.data_warning().is_none());
        let before = indicator.model.current().clone();
        let _ = indicator.reference_prices();
        assert_eq!(indicator.model.current(), &before);
        indicator.config.weekly = false;
        indicator.config.monthly = false;
        assert!(indicator.reference_prices().is_empty());
        assert!(indicator.data_warning().is_none());
    }
}
