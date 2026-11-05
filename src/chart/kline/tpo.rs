use std::collections::BTreeMap;

use data::chart::kline::tpo::{
    TpoAnchor, TpoConfig, TpoProfileSettings, TpoProfileStatus,
    build_tpo_profile_with_source_timeframe,
};
use exchange::{
    Kline, Timeframe, UnixMs,
    unit::{Price, PriceStep},
};
use iced::{
    Point, Rectangle, Size,
    theme::Palette,
    widget::canvas::{self, Frame, Path, Stroke},
};

const PERIOD: Timeframe = Timeframe::M30;

fn session_range(
    earliest: u64,
    latest: u64,
    now: UnixMs,
    anchor: TpoAnchor,
    extend: bool,
) -> (UnixMs, UnixMs) {
    let latest = UnixMs::new(latest).min(now);
    let end = anchor.offset(anchor.floor(latest), 1);
    let recent = latest
        .floor_to(Timeframe::D1)
        .offset_by_timeframe(Timeframe::D1, -31);
    let start = anchor.floor(UnixMs::new(earliest).max(recent));
    let previous = anchor.offset(anchor.floor(now), -1);
    if extend && end > previous {
        (start.min(previous), end)
    } else {
        (start, end)
    }
}

pub(super) fn missing_range(
    candles: &BTreeMap<UnixMs, Kline>,
    earliest: u64,
    latest: u64,
    now: UnixMs,
    config: &TpoConfig,
) -> Option<(UnixMs, UnixMs)> {
    let (start, end) = session_range(
        earliest,
        latest,
        now,
        config.profile_anchor(),
        config.extend_untested,
    );
    let end = end.min(now.floor_to(PERIOD));
    let step = PERIOD.to_milliseconds();
    let missing = (start.as_u64()..end.as_u64())
        .step_by(step as usize)
        .map(UnixMs::new)
        .filter(|time| !candles.contains_key(time));
    let mut missing = missing;
    let first = missing.next()?;
    let last = missing.last().unwrap_or(first);
    Some((first, last.saturating_add(step).saturating_sub(1)))
}

fn verified_level_end(
    candles: &BTreeMap<UnixMs, Kline>,
    start: UnixMs,
    closed_before: UnixMs,
    price: Price,
) -> UnixMs {
    let mut verified = start;
    for (&time, candle) in candles.range(start..closed_before.max(start)) {
        if time != verified || (candle.low <= price && price <= candle.high) {
            break;
        }
        verified = time.offset_by_timeframe(PERIOD, 1);
    }
    verified
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    candles: &BTreeMap<UnixMs, Kline>,
    time_chart: bool,
    frame: &mut Frame,
    earliest: u64,
    latest: u64,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    step: PriceStep,
    region: Rectangle,
    scaling: f32,
    config: &TpoConfig,
    palette: &Palette,
) {
    let draw_notice = |frame: &mut Frame, content: &str| {
        frame.fill_text(canvas::Text {
            content: content.into(),
            position: Point::new(region.x + 8.0 / scaling, region.y + 44.0 / scaling),
            color: palette.background.base.text,
            size: (11.0 / scaling).into(),
            ..Default::default()
        });
    };
    if !time_chart {
        draw_notice(frame, "TPO · switch to a time chart");
        return;
    }
    let now = UnixMs::now();
    let anchor = config.profile_anchor();
    let visible_start = anchor.floor(UnixMs::new(earliest));
    let end = anchor.offset(anchor.floor(UnixMs::new(latest).min(now)), 1);
    let Some(&first_candle) = candles.keys().next() else {
        draw_notice(frame, "TPO · waiting for 30m candles");
        return;
    };
    let mut start = anchor.floor(first_candle);
    if let Some(days) = config.history.days() {
        let latest_day = UnixMs::new(latest).min(now).floor_to(Timeframe::D1);
        let cutoff = latest_day.offset_by_timeframe(Timeframe::D1, 1 - days);
        start = start.max(anchor.floor(cutoff));
    }
    if !config.extend_untested {
        start = start.max(visible_start);
    }
    let checked_until = now.floor_to(PERIOD).min(end);
    let mut has_profile = false;
    while start < end {
        let next = anchor.offset(start, 1);
        if next <= start {
            break;
        }
        let bars: Vec<_> = candles.range(start..next).map(|(_, bar)| *bar).collect();
        let (Some(low), Some(high)) = (
            bars.iter().map(|bar| bar.low).min(),
            bars.iter().map(|bar| bar.high).max(),
        ) else {
            start = next;
            continue;
        };
        // Bound both calculation and drawing work at the screen's useful resolution.
        let Some(price_step) = step.coarsened_to_fit(low, high, 256) else {
            start = next;
            continue;
        };
        let settings = TpoProfileSettings {
            anchor,
            letter_period: PERIOD,
            price_step,
            value_area_percent: config.value_area_percent.clamp(5, 100),
        };
        if let Ok(profile) =
            build_tpo_profile_with_source_timeframe(&bars, start, next, settings, PERIOD, now)
        {
            let left = x(start.as_u64()).max(region.x);
            let right = x(next.as_u64()).min(region.x + region.width);
            let width = ((right - left) * f32::from(config.width_percent.clamp(10, 80)) / 100.0)
                .min(220.0 / scaling);
            if config.extend_untested && profile.status == TpoProfileStatus::Complete {
                draw_levels(
                    frame,
                    &profile,
                    &y,
                    region,
                    scaling,
                    config,
                    palette,
                    x(next.as_u64()).max(region.x),
                    region.x + region.width,
                    |price| x(verified_level_end(candles, next, checked_until, price).as_u64()),
                );
            }
            if width <= 0.0 {
                start = next;
                continue;
            }
            has_profile = true;
            let max_count = profile
                .rows
                .iter()
                .map(|row| row.count())
                .max()
                .unwrap_or(1)
                .max(1) as f32;
            let row_height =
                (y(low) - y(low.checked_add_steps(1, price_step).unwrap_or(low))).abs();
            let block_height = row_height * 0.76;
            let period_width = width / max_count;
            let value_color = super::resolve_gex_color(config.value_color, palette);
            let outside_color = super::resolve_gex_color(config.outside_color, palette);
            for row in &profile.rows {
                if row.periods.is_empty() {
                    continue;
                }
                let top = y(row.price) - block_height / 2.0;
                let in_area = profile
                    .val
                    .zip(profile.vah)
                    .is_some_and(|(val, vah)| row.price >= val && row.price <= vah);
                let color = if in_area {
                    value_color.scale_alpha(0.78)
                } else {
                    outside_color.scale_alpha(0.56)
                };
                if period_width * scaling >= 3.0 {
                    for column in 0..row.count() {
                        frame.fill_rectangle(
                            Point::new(left + column as f32 * period_width, top),
                            Size::new(period_width - 0.65 / scaling, block_height),
                            color,
                        );
                    }
                } else {
                    frame.fill_rectangle(
                        Point::new(left, top),
                        Size::new(period_width * row.count() as f32, block_height),
                        color,
                    );
                }
            }
            draw_levels(
                frame,
                &profile,
                &y,
                region,
                scaling,
                config,
                palette,
                left,
                left + width,
                |_| left + width,
            );
            let status = match profile.status {
                TpoProfileStatus::Complete => "TPO",
                TpoProfileStatus::Developing => "TPO · developing",
                TpoProfileStatus::Incomplete => "TPO · partial",
            };
            frame.fill_text(canvas::Text {
                content: format!("{status} · VA {}%", settings.value_area_percent),
                position: Point::new(left, y(high) - 14.0 / scaling),
                color: palette.background.base.text,
                size: (10.0 / scaling).into(),
                ..Default::default()
            });
        }
        start = next;
    }
    if !has_profile {
        draw_notice(frame, "TPO · waiting for 30m candles");
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_levels(
    frame: &mut Frame,
    profile: &data::chart::kline::tpo::TpoProfile,
    y: &impl Fn(Price) -> f32,
    region: Rectangle,
    scaling: f32,
    config: &TpoConfig,
    palette: &Palette,
    left: f32,
    right: f32,
    level_end: impl Fn(Price) -> f32,
) {
    let levels = [
        ("TPOC", profile.poc.filter(|_| config.show_poc)),
        ("TVAH", profile.vah.filter(|_| config.show_vah)),
        ("TVAL", profile.val.filter(|_| config.show_val)),
    ];
    let mut label_positions = Vec::<f32>::with_capacity(levels.len());
    for (label, price) in levels {
        let Some(price) = price else { continue };
        let right = right.min(level_end(price));
        if right <= left {
            continue;
        }
        let level_y = y(price);
        if level_y < region.y || level_y > region.y + region.height {
            continue;
        }
        let color = if label == "TPOC" {
            super::resolve_gex_color(config.poc_color, palette)
        } else {
            palette.background.base.text.scale_alpha(0.66)
        };
        frame.stroke(
            &Path::line(Point::new(left, level_y), Point::new(right, level_y)),
            Stroke::default()
                .with_color(color)
                .with_width(if label == "TPOC" {
                    1.6 / scaling
                } else {
                    0.8 / scaling
                }),
        );
        if right - left < 42.0 / scaling
            || label_positions
                .iter()
                .any(|previous| (previous - level_y).abs() < 11.0 / scaling)
        {
            continue;
        }
        frame.fill_text(canvas::Text {
            content: label.into(),
            position: Point::new(left + 2.0 / scaling, level_y - 11.0 / scaling),
            color,
            size: (10.0 / scaling).into(),
            ..Default::default()
        });
        label_positions.push(level_y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_period_controls_utc_boundaries_without_changing_candle_interval() {
        let hour = 3_600_000;
        let monday = UnixMs::new(4 * 24 * hour);
        let now = monday.saturating_add(19 * hour + 45 * 60_000);
        for (anchor, start_hour) in [
            (TpoAnchor::FourHours, 16),
            (TpoAnchor::EightHours, 16),
            (TpoAnchor::TwelveHours, 12),
            (TpoAnchor::Daily, 0),
            (TpoAnchor::Weekly, 0),
        ] {
            let config = TpoConfig {
                anchor,
                ..TpoConfig::default()
            };
            assert_eq!(
                missing_range(&BTreeMap::new(), now.as_u64(), now.as_u64(), now, &config),
                Some((
                    monday.saturating_add(start_hour * hour),
                    now.floor_to(PERIOD).saturating_sub(1)
                ))
            );
        }

        let sunday = monday.saturating_add(6 * 24 * hour + 23 * hour);
        assert_eq!(TpoAnchor::Weekly.floor(sunday), monday);
        assert_eq!(
            TpoAnchor::Weekly.offset(monday, 1),
            monday.saturating_add(7 * 24 * hour)
        );
    }

    #[test]
    fn level_extension_stops_at_first_touch_or_first_unverified_slot() {
        let period = PERIOD.to_milliseconds();
        let start = UnixMs::new(period);
        let second = start.saturating_add(period);
        let third = second.saturating_add(period);
        let end = third.saturating_add(period);
        let level = Price::from_units(20);
        let candle = |time, high| Kline {
            time,
            open: Price::from_units(10),
            high: Price::from_units(high),
            low: Price::from_units(8),
            close: Price::from_units(10),
            volume: exchange::Volume::empty_buy_sell(),
        };
        let mut candles = BTreeMap::from([(start, candle(start, 19)), (third, candle(third, 20))]);
        assert_eq!(verified_level_end(&candles, start, end, level), second);
        candles.insert(second, candle(second, 19));
        assert_eq!(verified_level_end(&candles, start, end, level), third);
        candles.insert(third, candle(third, 19));
        assert_eq!(verified_level_end(&candles, start, end, level), end);
        assert_eq!(verified_level_end(&candles, start, second, level), second);
        candles.insert(start, candle(start, 20));
        assert_eq!(verified_level_end(&candles, start, end, level), start);
    }

    #[test]
    fn reference_window_uses_utc_days_and_closed_half_hours() {
        let now = UnixMs::new(3 * 86_400_000 + 45 * 60_000);
        let today = 3 * 86_400_000;
        let candles = BTreeMap::new();
        let range = missing_range(
            &candles,
            today + 5 * 60_000,
            now.as_u64(),
            now,
            &TpoConfig::default(),
        );
        assert_eq!(
            range,
            Some((UnixMs::new(today), UnixMs::new(today + 30 * 60_000 - 1)))
        );
        assert_eq!(
            missing_range(
                &candles,
                today,
                now.as_u64(),
                now,
                &TpoConfig {
                    extend_untested: true,
                    ..TpoConfig::default()
                }
            ),
            Some((
                UnixMs::new(today - 86_400_000),
                UnixMs::new(today + 30 * 60_000 - 1)
            ))
        );
    }
}
