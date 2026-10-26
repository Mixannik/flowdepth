//! Bar classification by open interest change and delta (JDK method): who drove the move.
//! Price up with OI up and positive delta is new longs, price up with OI down is short
//! covering; mirrored for the downside. Small OI changes are left unmarked.
//!
//! OI comes from the Open Interest indicator, sampled at its source timeframe (5m on a 1m
//! chart), so chart bars are grouped into the periods between two OI samples.

use data::chart::{PlotData, kline::KlineDataPoint};
use iced::{
    Alignment, Point, Size,
    theme::Palette,
    widget::canvas::{self, Frame},
};

/// Previous periods whose average |ΔOI| sets the bar for a "significant" change.
const OI_LOOKBACK: usize = 20;
/// Periods of history needed before anything is classified.
const OI_MIN_HISTORY: usize = 5;
/// A period's |ΔOI| must reach this multiple of the lookback average.
const OI_SIGNIFICANT: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Period {
    pub open: f64,
    pub close: f64,
    /// Taker buys minus taker sells over the period.
    pub delta: f64,
    pub oi_change: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OiBarKind {
    /// Price up, delta up, OI up: aggressive buyers opening (strong).
    NewLongs,
    /// Price up, OI down: shorts closing (weak).
    ShortCovering,
    /// Price down, delta down, OI up: aggressive sellers opening (strong).
    NewShorts,
    /// Price down, OI down: longs closing (weak).
    LongLiquidation,
}

/// Classifies `periods` (oldest first); `None` where the OI change is small or the
/// price/delta/OI combination fits no rule.
pub(super) fn classify(periods: &[Period]) -> Vec<Option<OiBarKind>> {
    periods
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i < OI_MIN_HISTORY {
                return None;
            }
            let prior = &periods[i.saturating_sub(OI_LOOKBACK)..i];
            let average = prior.iter().map(|q| q.oi_change.abs()).sum::<f64>() / prior.len() as f64;
            if average <= 0.0 || p.oi_change.abs() < OI_SIGNIFICANT * average {
                return None;
            }
            let oi_up = p.oi_change > 0.0;
            if p.close > p.open {
                if !oi_up {
                    Some(OiBarKind::ShortCovering)
                } else if p.delta > 0.0 {
                    Some(OiBarKind::NewLongs)
                } else {
                    None
                }
            } else if p.close < p.open {
                if !oi_up {
                    Some(OiBarKind::LongLiquidation)
                } else if p.delta < 0.0 {
                    Some(OiBarKind::NewShorts)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ChartBar {
    pub time: u64,
    pub open: f64,
    pub close: f64,
    pub delta: f64,
}

/// Groups `bars` (ascending time) into the spans between consecutive OI samples `oi`
/// (ascending time): bars with `sample[i] <= time < sample[i + 1]`. Returns the first and last
/// bar time of each non-empty span with its period.
pub(super) fn periods(bars: &[ChartBar], oi: &[(u64, f64)]) -> Vec<(u64, u64, Period)> {
    oi.windows(2)
        .filter_map(|w| {
            let ((from, oi_from), (to, oi_to)) = (w[0], w[1]);
            let start = bars.partition_point(|b| b.time < from);
            let end = bars.partition_point(|b| b.time < to);
            let span = &bars[start..end];
            let (first, last) = (span.first()?, span.last()?);
            Some((
                first.time,
                last.time,
                Period {
                    open: first.open,
                    close: last.close,
                    delta: span.iter().map(|b| b.delta).sum(),
                    oi_change: oi_to - oi_from,
                },
            ))
        })
        .collect()
}

/// Draws a band of colored marks along the bottom of the visible area, under the bars of each
/// classified OI period, so nothing covers the footprint cells.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    source: &PlotData<KlineDataPoint>,
    oi: &std::collections::BTreeMap<exchange::UnixMs, f64>,
    frame: &mut Frame,
    earliest: u64,
    latest: u64,
    x: impl Fn(u64) -> f32,
    band_bottom: f32,
    cell_width: f32,
    scaling: f32,
    palette: &Palette,
) {
    let PlotData::TimeBased(series) = source else {
        return;
    };
    if oi.len() < 2 {
        return;
    }
    let interval = series.interval.to_milliseconds();
    // Enough history before the view for the lookback average, at the coarsest OI source (4h).
    let span = (OI_LOOKBACK as u64 + 2) * interval.max(4 * 3_600_000);
    let from = earliest.saturating_sub(span);
    let to = latest.saturating_add(interval);

    let bars: Vec<ChartBar> = series
        .datapoints
        .range(exchange::UnixMs::new(from)..=exchange::UnixMs::new(to))
        .map(|(time, dp)| ChartBar {
            time: time.as_u64(),
            open: dp.kline.open.to_f64(),
            close: dp.kline.close.to_f64(),
            delta: dp
                .footprint
                .trades
                .values()
                .map(|g| g.buy_qty.to_f64() - g.sell_qty.to_f64())
                .sum(),
        })
        .collect();
    let samples: Vec<(u64, f64)> = oi
        .range(exchange::UnixMs::new(from)..=exchange::UnixMs::new(to))
        .map(|(t, v)| (t.as_u64(), *v))
        .collect();

    let spans = periods(&bars, &samples);
    let plain: Vec<Period> = spans.iter().map(|(_, _, p)| *p).collect();

    let px = |p: f32| p / scaling;
    let height = px(14.0);
    let top = band_bottom - px(2.0) - height;
    let text_size = px(10.0);

    for ((first, last, _), kind) in spans.iter().zip(classify(&plain)) {
        let Some(kind) = kind else { continue };
        if *last < earliest || *first > latest {
            continue;
        }
        let left = x(*first) - cell_width * 0.45;
        let width = x(*last) - x(*first) + cell_width * 0.9;
        let (color, strong, label) = match kind {
            OiBarKind::NewLongs => (palette.success.strong.color, true, "New L"),
            OiBarKind::ShortCovering => (palette.success.strong.color, false, "Cover"),
            OiBarKind::NewShorts => (palette.danger.strong.color, true, "New S"),
            OiBarKind::LongLiquidation => (palette.danger.strong.color, false, "Liq"),
        };
        frame.fill_rectangle(
            Point::new(left, top),
            Size::new(width, height),
            color.scale_alpha(if strong { 0.9 } else { 0.35 }),
        );
        if width * scaling >= super::MIN_TEXT_BAR_PX {
            frame.fill_text(canvas::Text {
                content: label.to_string(),
                position: Point::new(left + width / 2.0, top + height / 2.0),
                size: iced::Pixels(text_size),
                color: if strong {
                    palette.background.base.color
                } else {
                    palette.background.base.text
                },
                align_x: Alignment::Center.into(),
                align_y: Alignment::Center.into(),
                font: crate::style::AZERET_MONO,
                ..canvas::Text::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(open: f64, close: f64, delta: f64, oi_change: f64) -> Period {
        Period {
            open,
            close,
            delta,
            oi_change,
        }
    }

    /// Five quiet periods with |ΔOI| = 10 to set the average.
    fn history() -> Vec<Period> {
        (0..5)
            .map(|i| p(100.0, 100.0, 0.0, if i % 2 == 0 { 10.0 } else { -10.0 }))
            .collect()
    }

    fn last(extra: Period) -> Option<OiBarKind> {
        let mut periods = history();
        periods.push(extra);
        *classify(&periods).last().unwrap()
    }

    #[test]
    fn four_rules() {
        assert_eq!(last(p(100.0, 101.0, 5.0, 20.0)), Some(OiBarKind::NewLongs));
        assert_eq!(
            last(p(100.0, 101.0, -5.0, -20.0)),
            Some(OiBarKind::ShortCovering)
        );
        assert_eq!(
            last(p(101.0, 100.0, -5.0, 20.0)),
            Some(OiBarKind::NewShorts)
        );
        assert_eq!(
            last(p(101.0, 100.0, 5.0, -20.0)),
            Some(OiBarKind::LongLiquidation)
        );
    }

    #[test]
    fn small_oi_change_or_mismatched_delta_is_unmarked() {
        assert_eq!(last(p(100.0, 101.0, 5.0, 5.0)), None);
        assert_eq!(last(p(100.0, 101.0, -5.0, 20.0)), None);
        assert_eq!(last(p(101.0, 100.0, 5.0, 20.0)), None);
        assert_eq!(last(p(100.0, 100.0, 5.0, 20.0)), None);
    }

    #[test]
    fn needs_history() {
        let periods = vec![p(100.0, 101.0, 5.0, 20.0); OI_MIN_HISTORY];
        assert!(classify(&periods).iter().all(Option::is_none));
    }

    #[test]
    fn groups_bars_between_oi_samples() {
        let bar = |time, open, close, delta| ChartBar {
            time,
            open,
            close,
            delta,
        };
        // 1m bars, 5m OI samples at 0, 300k, 600k.
        let bars: Vec<ChartBar> = (0..10)
            .map(|i| bar(i * 60_000, 100.0 + i as f64, 101.0 + i as f64, 1.0))
            .collect();
        let oi = [(0, 1000.0), (300_000, 1030.0), (600_000, 1010.0)];
        let spans = periods(&bars, &oi);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].0, spans[0].1), (0, 240_000));
        assert_eq!(spans[0].2, p(100.0, 105.0, 5.0, 30.0));
        assert_eq!((spans[1].0, spans[1].1), (300_000, 540_000));
        assert_eq!(spans[1].2.oi_change, -20.0);
    }
}
