//! Naked (untested) POC, VAH and VAL of previous daily and weekly sessions, drawn as lines
//! from the session's end to the right edge until price trades through them.
//!
//! Session profiles use footprint trades where a bar has them and otherwise spread the
//! candle's volume evenly over its range, so higher timeframes without trade history still
//! get levels.

use data::chart::{
    PlotData,
    kline::{KlineDataPoint, NakedLevelsConfig},
};
use exchange::unit::Price;
use iced::{
    Alignment, Point, Size,
    theme::Palette,
    widget::canvas::{self, Frame, LineDash, Path, Stroke},
};

pub(super) use data::analysis::naked_levels::{Bar, BarVolume, Level, LevelKind, SessionKind};
use data::analysis::naked_levels::{Snapshot, calculate};
use data::chart::kline::TradeCoverage;

const MAX_DAILY_SESSIONS: usize = 3;
const MAX_WEEKLY_SESSIONS: usize = 2;

fn collect_bars(source: &PlotData<KlineDataPoint>) -> Vec<Bar> {
    let PlotData::TimeBased(series) = source else {
        return Vec::new();
    };
    series
        .datapoints
        .values()
        .map(|dp| Bar {
            time: dp.kline.time.as_u64(),
            high: dp.kline.high.to_f64(),
            low: dp.kline.low.to_f64(),
            volume: if dp.trade_coverage != TradeCoverage::Complete
                || dp.footprint.trades.is_empty()
            {
                BarVolume::Total(dp.kline.volume.total().to_f64())
            } else {
                BarVolume::Levels(
                    dp.footprint
                        .trades
                        .iter()
                        .map(|(price, group)| (price.to_f64(), group.total_qty().to_f64()))
                        .collect(),
                )
            },
        })
        .collect()
}

/// Light blue for value-area edges: distinct from the gold POC on dark and light themes.
const VALUE_AREA_COLOR: iced::Color = iced::Color::from_rgb(0.42, 0.72, 1.0);

/// A right-edge label; nearby levels share one so their text never overlaps.
#[derive(Debug, Clone, PartialEq)]
struct Label {
    y: f32,
    names: Vec<String>,
    price: f64,
    is_poc: bool,
}

/// Merges labels closer than `min_gap` (screen-space y, any order) into the first of each run.
fn merge_labels(mut labels: Vec<Label>, min_gap: f32) -> Vec<Label> {
    labels.sort_by(|a, b| a.y.total_cmp(&b.y));
    let mut merged: Vec<Label> = Vec::with_capacity(labels.len());
    for label in labels {
        match merged.last_mut() {
            Some(prev) if label.y - prev.y < min_gap => {
                prev.names.extend(label.names);
                prev.is_poc |= label.is_poc;
            }
            _ => merged.push(label),
        }
    }
    merged
}

fn format_price(price: f64) -> String {
    if price >= 1000.0 {
        format!("{price:.0}")
    } else if price >= 1.0 {
        format!("{price:.2}")
    } else {
        format!("{price:.5}")
    }
}

pub(super) fn snapshot(
    source: &PlotData<KlineDataPoint>,
    align: impl Fn(u64, u64) -> u64 + Copy,
    config: &NakedLevelsConfig,
) -> Snapshot {
    let PlotData::TimeBased(series) = source else {
        return Snapshot::default();
    };
    let bars = collect_bars(source);
    let interval = series.interval.to_milliseconds();
    let mut result = Snapshot::default();
    if config.weekly {
        let weekly = calculate(
            &bars,
            SessionKind::Weekly,
            MAX_WEEKLY_SESSIONS,
            interval,
            align,
        );
        result.incomplete_sessions += weekly.incomplete_sessions;
        result.levels.extend(weekly.levels);
    }
    if config.daily_poc {
        let daily = calculate(
            &bars,
            SessionKind::Daily,
            MAX_DAILY_SESSIONS,
            interval,
            align,
        );
        result.incomplete_sessions += daily.incomplete_sessions;
        result.levels.extend(
            daily
                .levels
                .into_iter()
                .filter(|l| l.kind == LevelKind::Poc),
        );
    }
    result
}

/// Complete session profiles with a continuous post-session price path.
/// Draw already prepared naked levels; no distribution work happens here.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_levels(
    levels: &[Level],
    frame: &mut Frame,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    right_x: f32,
    scaling: f32,
    palette: &Palette,
    config: &NakedLevelsConfig,
) {
    let px = |p: f32| p / scaling;
    let label_size = px(10.0);

    let mut labels = Vec::new();
    for level in levels {
        let left = x(level.from);
        if left >= right_x {
            continue;
        }
        let line_y = y(Price::from_f64(level.price));
        let (color, name) = match level.kind {
            LevelKind::Poc => (palette.warning.strong.color, "nPOC"),
            LevelKind::Vah => (VALUE_AREA_COLOR, "nVAH"),
            LevelKind::Val => (VALUE_AREA_COLOR, "nVAL"),
        };
        let base = Stroke::default().with_color(color.scale_alpha(0.9));
        let stroke = match level.session {
            SessionKind::Weekly => base.with_width(px(1.6)),
            SessionKind::Daily => Stroke {
                line_dash: LineDash {
                    segments: &[6.0, 4.0],
                    offset: 0,
                },
                ..base.with_width(px(1.1))
            },
        };
        frame.stroke(
            &Path::line(Point::new(left, line_y), Point::new(right_x, line_y)),
            stroke,
        );
        labels.push(Label {
            y: line_y,
            names: vec![format!(
                "{name}{} {}",
                if level.approximate { "~" } else { "" },
                level.session.tag()
            )],
            price: level.price,
            is_poc: level.kind == LevelKind::Poc,
        });
    }

    if !config.labels {
        return;
    }
    for label in merge_labels(labels, label_size * 1.4) {
        let color = if label.is_poc {
            palette.warning.strong.color
        } else {
            VALUE_AREA_COLOR
        };
        let text = format!("{} {}", label.names.join(" · "), format_price(label.price));
        let text_w = text.chars().count() as f32 * label_size * 0.62 + label_size * 0.6;
        frame.fill_rectangle(
            Point::new(right_x - text_w, label.y - label_size * 0.65),
            Size::new(text_w, label_size * 1.3),
            palette.background.base.color.scale_alpha(0.9),
        );
        frame.fill_text(canvas::Text {
            content: text,
            position: Point::new(right_x - label_size * 0.3, label.y),
            size: iced::Pixels(label_size),
            color,
            align_x: Alignment::End.into(),
            align_y: Alignment::Center.into(),
            font: crate::style::AZERET_MONO,
            ..canvas::Text::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_footprint_uses_full_candle_fallback_until_complete() {
        use data::aggr::time::TimeSeries;
        use exchange::unit::{PriceStep, Qty};
        use exchange::{Kline, Timeframe, Trade, UnixMs, Volume};
        let step = PriceStep {
            units: Price::from_f64(0.1).units,
        };
        let bar = Kline {
            time: UnixMs::ZERO,
            open: Price::from_f64(100.0),
            high: Price::from_f64(110.0),
            low: Price::from_f64(100.0),
            close: Price::from_f64(105.0),
            volume: Volume::TotalOnly(Qty::from_f64(100.0)),
        };
        let mut series = TimeSeries::<KlineDataPoint>::new(Timeframe::H1, step, &[bar]);
        series.datapoints.get_mut(&UnixMs::ZERO).unwrap().add_trade(
            &Trade {
                id: Some(1),
                time: UnixMs::ZERO,
                is_sell: false,
                price: Price::from_f64(105.0),
                qty: Qty::from_f64(1.0),
            },
            step,
        );
        let mut source = PlotData::TimeBased(series);
        assert_eq!(collect_bars(&source)[0].volume, BarVolume::Total(100.0));
        let PlotData::TimeBased(series) = &mut source else {
            unreachable!()
        };
        series
            .datapoints
            .get_mut(&UnixMs::ZERO)
            .unwrap()
            .trade_coverage = TradeCoverage::Complete;
        assert!(matches!(
            collect_bars(&source)[0].volume,
            BarVolume::Levels(_)
        ));
    }

    fn label(y: f32, name: &str, is_poc: bool) -> Label {
        Label {
            y,
            names: vec![name.to_string()],
            price: f64::from(y),
            is_poc,
        }
    }

    #[test]
    fn close_labels_merge_and_keep_poc_color() {
        let merged = merge_labels(
            vec![
                label(100.0, "nVAH D", false),
                label(40.0, "nVAL W", false),
                label(105.0, "nPOC D", true),
            ],
            10.0,
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].names, vec!["nVAL W"]);
        assert_eq!(merged[1].names, vec!["nVAH D", "nPOC D"]);
        assert!(merged[1].is_poc);
    }

    #[test]
    fn far_labels_stay_separate() {
        let merged = merge_labels(vec![label(0.0, "a", false), label(30.0, "b", false)], 10.0);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn prices_format_by_magnitude() {
        assert_eq!(format_price(81650.36), "81650");
        assert_eq!(format_price(2.5), "2.50");
        assert_eq!(format_price(0.000123), "0.00012");
    }
}
