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

const DAY_MS: u64 = 24 * 60 * 60_000;
const WEEK_MS: u64 = 7 * DAY_MS;
/// Share of session volume inside the value area.
const VALUE_AREA_SHARE: f64 = 0.70;
/// Profile rows per session: fine enough for a POC, cheap enough to rebuild every frame.
const ROWS_PER_SESSION: f64 = 200.0;
/// Most recent completed sessions considered, per session length.
const MAX_DAILY_SESSIONS: usize = 3;
const MAX_WEEKLY_SESSIONS: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum BarVolume {
    /// Footprint levels as (price, volume).
    Levels(Vec<(f64, f64)>),
    /// Only the candle's total volume is known.
    Total(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Bar {
    pub time: u64,
    pub high: f64,
    pub low: f64,
    pub volume: BarVolume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionKind {
    Daily,
    Weekly,
}

impl SessionKind {
    fn ms(self) -> u64 {
        match self {
            SessionKind::Daily => DAY_MS,
            SessionKind::Weekly => WEEK_MS,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            SessionKind::Daily => "D",
            SessionKind::Weekly => "W",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LevelKind {
    Poc,
    Vah,
    Val,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Level {
    pub kind: LevelKind,
    pub session: SessionKind,
    pub price: f64,
    /// Session end: the line starts here.
    pub from: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SessionLevels {
    end: u64,
    poc: f64,
    vah: f64,
    val: f64,
}

/// Volume profile of one session's bars; `None` without volume or range.
fn session_levels(bars: &[&Bar], end: u64) -> Option<SessionLevels> {
    let high = bars.iter().map(|b| b.high).fold(f64::MIN, f64::max);
    let low = bars.iter().map(|b| b.low).fold(f64::MAX, f64::min);
    if !(high > low) {
        return None;
    }
    let row = (high - low) / ROWS_PER_SESSION;
    let rows = ROWS_PER_SESSION as usize + 1;
    let index = |price: f64| (((price - low) / row).floor().max(0.0) as usize).min(rows - 1);
    let mut volume = vec![0.0_f64; rows];

    for bar in bars {
        match &bar.volume {
            BarVolume::Levels(levels) => {
                for &(price, qty) in levels {
                    volume[index(price)] += qty;
                }
            }
            BarVolume::Total(qty) => {
                // Half-open [low, high): a candle ending where the next one starts must not
                // put both candles' volume into the shared boundary row.
                let from = index(bar.low);
                let to = index(bar.high - row * 1e-6).max(from);
                let share = qty / (to - from + 1) as f64;
                for slot in &mut volume[from..=to] {
                    *slot += share;
                }
            }
        }
    }

    let total: f64 = volume.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let poc = volume
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i)?;

    // Expand from the POC toward the heavier neighbour until the value area is filled.
    let (mut lo, mut hi, mut inside) = (poc, poc, volume[poc]);
    while inside < total * VALUE_AREA_SHARE && (lo > 0 || hi + 1 < rows) {
        let below = if lo > 0 { volume[lo - 1] } else { -1.0 };
        let above = if hi + 1 < rows { volume[hi + 1] } else { -1.0 };
        if above >= below {
            hi += 1;
            inside += volume[hi];
        } else {
            lo -= 1;
            inside += volume[lo];
        }
    }

    let center = |i: usize| low + (i as f64 + 0.5) * row;
    Some(SessionLevels {
        end,
        poc: center(poc),
        vah: center(hi),
        val: center(lo),
    })
}

/// Naked levels for one session length. `bars` are ordered oldest first; only sessions that
/// have closed before the last bar count, and a level is dropped once any later bar's range
/// contains it.
pub(super) fn naked_levels(
    bars: &[Bar],
    kind: SessionKind,
    max_sessions: usize,
    align: impl Fn(u64, u64) -> u64,
) -> Vec<Level> {
    let Some(last) = bars.last() else {
        return Vec::new();
    };
    let session_ms = kind.ms();
    let current_session = align(last.time, session_ms);

    let mut sessions: Vec<(u64, Vec<&Bar>)> = Vec::new();
    for bar in bars {
        let start = align(bar.time, session_ms);
        if start >= current_session {
            break;
        }
        match sessions.last_mut() {
            Some((s, members)) if *s == start => members.push(bar),
            _ => sessions.push((start, vec![bar])),
        }
    }

    let first_kept = sessions.len().saturating_sub(max_sessions);
    let mut levels = Vec::new();
    for (start, members) in &sessions[first_kept..] {
        let end = start + session_ms;
        let Some(session) = session_levels(members, end) else {
            continue;
        };
        for (level_kind, price) in [
            (LevelKind::Poc, session.poc),
            (LevelKind::Vah, session.vah),
            (LevelKind::Val, session.val),
        ] {
            let touched = bars
                .iter()
                .filter(|b| b.time >= session.end)
                .any(|b| b.low <= price && price <= b.high);
            if !touched {
                levels.push(Level {
                    kind: level_kind,
                    session: kind,
                    price,
                    from: session.end,
                });
            }
        }
    }
    levels
}

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
            volume: if dp.footprint.trades.is_empty() {
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

/// The levels the indicator shows: weekly POC/VAH/VAL and daily POCs, as configured.
pub(super) fn shown_levels(
    source: &PlotData<KlineDataPoint>,
    align: impl Fn(u64, u64) -> u64 + Copy,
    config: &NakedLevelsConfig,
) -> Vec<Level> {
    let bars = collect_bars(source);
    let mut levels = if config.weekly {
        naked_levels(&bars, SessionKind::Weekly, MAX_WEEKLY_SESSIONS, align)
    } else {
        Vec::new()
    };
    if config.daily_poc {
        levels.extend(
            naked_levels(&bars, SessionKind::Daily, MAX_DAILY_SESSIONS, align)
                .into_iter()
                .filter(|level| level.kind == LevelKind::Poc),
        );
    }
    levels
}

/// Draws naked levels from their session end to `right_x`: weekly POC/VAH/VAL as solid lines,
/// daily POCs dashed. Daily value-area edges are left out; on higher timeframes they mostly
/// crowd the price with noise.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    source: &PlotData<KlineDataPoint>,
    frame: &mut Frame,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    right_x: f32,
    scaling: f32,
    align: impl Fn(u64, u64) -> u64 + Copy,
    palette: &Palette,
    config: &NakedLevelsConfig,
) {
    let px = |p: f32| p / scaling;
    let label_size = px(10.0);

    let mut labels = Vec::new();
    for level in shown_levels(source, align, config) {
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
            names: vec![format!("{name} {}", level.session.tag())],
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

    const HOUR: u64 = 60 * 60_000;

    fn day_align(time: u64, ms: u64) -> u64 {
        time / ms * ms
    }

    /// Bar at `hour` hours with all its volume at one price.
    fn bar_at(hour: u64, low: f64, high: f64, volume_at: f64, qty: f64) -> Bar {
        Bar {
            time: hour * HOUR,
            high,
            low,
            volume: BarVolume::Levels(vec![(volume_at, qty), (low, 0.1), (high, 0.1)]),
        }
    }

    fn poc(levels: &[Level]) -> Option<f64> {
        levels
            .iter()
            .find(|l| l.kind == LevelKind::Poc)
            .map(|l| l.price)
    }

    #[test]
    fn untested_poc_of_previous_day_is_naked() {
        // Day 0 trades 100..110 with heavy volume at 105; day 1 stays above 112.
        let mut bars: Vec<Bar> = (0..24)
            .map(|h| bar_at(h, 100.0, 110.0, 105.0, 50.0))
            .collect();
        bars.extend((24..30).map(|h| bar_at(h, 112.0, 118.0, 115.0, 10.0)));
        let levels = naked_levels(&bars, SessionKind::Daily, 5, day_align);
        let price = poc(&levels).expect("day 0 POC stays naked");
        assert!((price - 105.0).abs() < 0.1, "POC at {price}");
        assert!(levels.iter().all(|l| l.from == 24 * HOUR));
    }

    #[test]
    fn traded_through_poc_is_dropped() {
        let mut bars: Vec<Bar> = (0..24)
            .map(|h| bar_at(h, 100.0, 110.0, 105.0, 50.0))
            .collect();
        // Day 1 dips back through 105.
        bars.push(bar_at(24, 104.0, 108.0, 106.0, 10.0));
        let levels = naked_levels(&bars, SessionKind::Daily, 5, day_align);
        assert_eq!(poc(&levels), None);
    }

    #[test]
    fn current_session_is_ignored() {
        let bars: Vec<Bar> = (0..10)
            .map(|h| bar_at(h, 100.0, 110.0, 105.0, 50.0))
            .collect();
        assert!(naked_levels(&bars, SessionKind::Daily, 5, day_align).is_empty());
    }

    #[test]
    fn candle_only_volume_still_gives_levels() {
        // No footprint: volume spread over each candle's range. Most candles sit 100..104.
        let mut bars: Vec<Bar> = (0..20)
            .map(|h| Bar {
                time: h * HOUR,
                high: 104.0,
                low: 100.0,
                volume: BarVolume::Total(100.0),
            })
            .collect();
        bars.extend((20..24).map(|h| Bar {
            time: h * HOUR,
            high: 120.0,
            low: 104.0,
            volume: BarVolume::Total(10.0),
        }));
        bars.push(Bar {
            time: 30 * HOUR,
            high: 130.0,
            low: 125.0,
            volume: BarVolume::Total(5.0),
        });
        let levels = naked_levels(&bars, SessionKind::Daily, 5, day_align);
        let price = poc(&levels).expect("candle-based POC");
        assert!((100.0..=104.0).contains(&price), "POC at {price}");
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

    #[test]
    fn only_recent_sessions_are_kept() {
        // Five days, each trading its own band and never revisited.
        let mut bars = Vec::new();
        for day in 0..5u64 {
            let base = 100.0 + day as f64 * 20.0;
            bars.extend((0..24).map(|h| bar_at(day * 24 + h, base, base + 10.0, base + 5.0, 50.0)));
        }
        bars.push(bar_at(5 * 24, 300.0, 310.0, 305.0, 1.0));
        let levels = naked_levels(&bars, SessionKind::Daily, 2, day_align);
        let sessions: std::collections::BTreeSet<u64> = levels.iter().map(|l| l.from).collect();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.into_iter().next(), Some(4 * 24 * HOUR));
    }
}
