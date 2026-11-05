use super::*;
use data::chart::kline::{ClusterMetric, ClusterRule, ClusterSearchStyle, TradeCoverage};
use std::collections::BTreeMap;
mod engine;
use engine::{Bar, Candidate, Settings};
const DISPLAY_LIMIT: usize = 500;

pub(super) fn quantity_unit(ticker: &TickerInfo) -> exchange::SizeUnit {
    use exchange::{SizeUnit, adapter::MarketKind, unit::qty::volume_size_unit};
    if ticker.market_type() == MarketKind::InversePerps {
        SizeUnit::Quote
    } else {
        volume_size_unit()
    }
}

#[derive(Debug)]
pub(super) struct Marker {
    candidate: Candidate,
    center: Point,
    zone: Rectangle,
    time: u64,
    first_time: u64,
    last_time: u64,
    slot: u8,
    metric: ClusterMetric,
    rule: ClusterRule,
    threshold: f64,
    radius: f32,
    style: ClusterSearchStyle,
    coverage: String,
}

fn color(m: &Marker) -> Color {
    let directional = m.style.options.imbalance_percent > 0
        || matches!(
            m.metric,
            ClusterMetric::Delta
                | ClusterMetric::PositiveDelta
                | ClusterMetric::NegativeDelta
                | ClusterMetric::Bid
                | ClusterMetric::Ask
                | ClusterMetric::Imbalance
        );
    let negative = match m.metric {
        ClusterMetric::Bid | ClusterMetric::NegativeDelta
            if m.style.options.imbalance_percent == 0 =>
        {
            true
        }
        ClusterMetric::Ask | ClusterMetric::PositiveDelta
            if m.style.options.imbalance_percent == 0 =>
        {
            false
        }
        _ => m.candidate.delta() < 0.0,
    };
    let rgb = if directional && negative {
        m.style.sell_color
    } else {
        m.style.color
    };
    Color::from_rgb8(rgb[0], rgb[1], rgb[2])
}

fn history_ready(
    coverage: TradeCoverage,
    interval_covered: bool,
    closed: bool,
    integrity_blocked: bool,
) -> bool {
    closed && !integrity_blocked && (coverage == TradeCoverage::Complete || interval_covered)
}

fn size(
    value: f64,
    threshold: f64,
    min: f64,
    max: f64,
    manual: bool,
    style: ClusterSearchStyle,
) -> f32 {
    let small = style.radius.min(style.options.max_radius).max(1) as f64;
    let big = style.radius.max(style.options.max_radius).max(1) as f64;
    let lo = if threshold > 0.0 { threshold } else { min };
    let hi = if manual && style.options.maximum > 0 {
        style.options.maximum as f64 / 1000.0
    } else {
        max
    };
    (if hi <= lo {
        small
    } else {
        small + (big - small) * ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
    }) as f32
}

pub(super) fn draw(
    chart: &KlineChart,
    frame: &mut canvas::Frame,
    studies: &[FootprintStudy],
    region: Rectangle,
    palette: &Palette,
) -> Vec<Marker> {
    if !studies
        .iter()
        .any(|s| matches!(s, FootprintStudy::ClusterSearch { .. }))
    {
        return Vec::new();
    }
    let view = chart.state();
    let mut markers = Vec::new();
    let mut status = Vec::new();
    if let (PlotData::TimeBased(series), Basis::Time(interval)) = (&chart.data_source, view.basis) {
        let interval = interval.to_milliseconds();
        let now = UnixMs::now().as_u64();
        let bars: Vec<_> = series
            .datapoints
            .iter()
            .map(|(time, dp)| {
                let end = time.as_u64().saturating_add(interval);
                let live = end > now;
                Bar {
                    time: time.as_u64(),
                    dp,
                    live,
                    ready: history_ready(
                        dp.trade_coverage,
                        chart.is_trade_range_covered(*time, UnixMs::new(end.saturating_sub(1))),
                        !live,
                        chart.ws_integrity_revision.is_some(),
                    ),
                }
            })
            .collect();
        let (left, right) = view.interval_range(&region);
        let visible: Vec<_> = bars
            .iter()
            .enumerate()
            .filter(|(_, b)| b.time >= left && b.time <= right)
            .map(|(i, _)| i)
            .collect();
        for &study in studies {
            let FootprintStudy::ClusterSearch {
                slot,
                metric,
                rule,
                bars: lookback,
                minimum,
                top,
                sigma_tenths,
                style,
            } = study
            else {
                continue;
            };
            let s = Settings {
                metric,
                rule,
                minimum: minimum as f64 / 1000.0,
                top,
                k: sigma_tenths as f64 / 10.0,
                options: style.options,
                row_step: view.tick_size.units.max(1),
                period: !style.visible_range,
                lookback,
            };
            let margin = style.radius.max(style.options.max_radius) as f32 * 2.0 / view.scaling;
            let result = engine::search(&bars, &visible, s, |c| {
                let a = view.price_to_y(c.low);
                let b = view.price_to_y(Price::from_units(c.high.units.saturating_add(s.row_step)));
                a.max(b) >= region.y - margin && a.min(b) <= region.y + region.height + margin
            });
            let scope: Vec<_> = if s.period && rule != ClusterRule::Minimum {
                (bars.len().saturating_sub(lookback.max(1))..bars.len()).collect()
            } else {
                visible.clone()
            };
            let ready = scope.iter().filter(|&&i| bars[i].ready).count();
            let available = scope
                .iter()
                .filter(|&&i| !bars[i].dp.footprint.trades.is_empty())
                .count();
            let live = scope.iter().filter(|&&i| bars[i].live).count();
            let approximate = scope
                .iter()
                .filter(|&&i| bars[i].dp.compacted_ids.is_some())
                .count();
            let partial = scope.len().saturating_sub(ready + live);
            let coverage = format!(
                "Footprint {available}/{} bars · history ready {ready} · partial/unknown {partial} · live {live} · compact {approximate}",
                scope.len()
            );
            status.push(format!(
                "#{slot}: {} signals · {coverage}{}",
                result.signals.len(),
                if available == 0 {
                    " · load Footprint trades"
                } else {
                    ""
                }
            ));
            for c in result.signals {
                let center = Point::new(
                    view.interval_to_x(bars[c.anchor].time),
                    view.price_to_y(c.price),
                );
                let radius = size(
                    c.value,
                    result.threshold,
                    result.min,
                    result.max,
                    rule == ClusterRule::Minimum,
                    style,
                );
                if center.x < region.x - view.cell_width
                    || center.x > region.x + region.width + view.cell_width
                {
                    continue;
                }
                let x1 = view.interval_to_x(bars[c.first].time) - view.cell_width / 2.0;
                let x2 = view.interval_to_x(bars[c.last].time) + view.cell_width / 2.0;
                let y1 = view.price_to_y(c.low);
                let y2 =
                    view.price_to_y(Price::from_units(c.high.units.saturating_add(s.row_step)));
                markers.push(Marker {
                    candidate: c,
                    center,
                    zone: Rectangle {
                        x: x1.min(x2),
                        y: y1.min(y2),
                        width: (x2 - x1).abs(),
                        height: (y2 - y1).abs(),
                    },
                    time: bars[c.anchor].time,
                    first_time: bars[c.first].time,
                    last_time: bars[c.last].time,
                    slot,
                    metric,
                    rule,
                    threshold: result.threshold,
                    radius,
                    style,
                    coverage: coverage.clone(),
                });
            }
        }
    } else {
        status.push("Cluster Search: time bars required".into());
    }
    if markers.len() > DISPLAY_LIMIT {
        status.push(format!("Showing {DISPLAY_LIMIT}/{} signals", markers.len()));
        markers.truncate(DISPLAY_LIMIT);
    }
    let mut labels: Vec<Rectangle> = Vec::new();
    for m in &markers {
        let r = m.radius / view.scaling;
        let p = m.center;
        let ink = color(m);
        if m.style.options.zones {
            frame.stroke(
                &canvas::Path::rectangle(m.zone.position(), m.zone.size()),
                canvas::Stroke::default()
                    .with_color(ink.scale_alpha(0.5))
                    .with_width(1.0 / view.scaling),
            );
        }
        let path = match m.style.options.shape {
            1 => canvas::Path::rectangle(Point::new(p.x - r, p.y - r), Size::new(2.0 * r, 2.0 * r)),
            2 | 3 => canvas::Path::new(|b| {
                b.move_to(Point::new(p.x, p.y - r));
                b.line_to(Point::new(
                    p.x + r,
                    if m.style.options.shape == 2 {
                        p.y
                    } else {
                        p.y + r
                    },
                ));
                if m.style.options.shape == 2 {
                    b.line_to(Point::new(p.x, p.y + r));
                }
                b.line_to(Point::new(
                    p.x - r,
                    if m.style.options.shape == 2 {
                        p.y
                    } else {
                        p.y + r
                    },
                ));
                b.close();
            }),
            4 => canvas::Path::line(Point::new(p.x - r, p.y), Point::new(p.x + r, p.y)),
            _ => canvas::Path::circle(p, r),
        };
        let alpha = f32::from(m.style.options.fill_percent.min(100)) / 100.0;
        if m.style.options.shape != 4 {
            frame.fill(&path, ink.scale_alpha(alpha));
        }
        if m.style.options.border || m.style.options.shape == 4 {
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_color(ink)
                    .with_width(1.5 / view.scaling),
            );
        }
        if m.style.options.text {
            let content = abbr_large_numbers(m.candidate.value);
            let rect = Rectangle {
                x: p.x + r + 3.0 / view.scaling,
                y: p.y - 6.0 / view.scaling,
                width: content.len() as f32 * 7.0 / view.scaling,
                height: 12.0 / view.scaling,
            };
            if !labels.iter().any(|other| other.intersects(&rect)) {
                frame.fill_text(canvas::Text {
                    content,
                    position: rect.position(),
                    color: palette.background.base.text,
                    size: iced::Pixels(11.0 / view.scaling),
                    ..Default::default()
                });
                labels.push(rect);
            }
        }
    }
    for (i, line) in status.into_iter().enumerate() {
        frame.fill_text(canvas::Text {
            content: line,
            position: Point::new(
                region.x + 8.0 / view.scaling,
                region.y + (24.0 + 14.0 * i as f32) / view.scaling,
            ),
            color: palette.background.base.text,
            size: iced::Pixels(11.0 / view.scaling),
            ..Default::default()
        });
    }
    markers
}

fn utc(time: u64) -> String {
    chrono::DateTime::from_timestamp_millis(time as i64)
        .map(|t| t.format("%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_default()
}
pub(super) fn hover(
    frame: &mut canvas::Frame,
    markers: &[Marker],
    palette: &Palette,
    cursor: Point,
    bounds: Size,
    translation: Vector,
    scaling: f32,
    unit: exchange::SizeUnit,
) -> bool {
    let hits: Vec<_> = markers
        .iter()
        .filter(|m| {
            let x = bounds.width / 2.0 + (translation.x + m.center.x) * scaling;
            let y = bounds.height / 2.0 + (translation.y + m.center.y) * scaling;
            (cursor.x - x).hypot(cursor.y - y) <= m.radius + 2.0
        })
        .take(3)
        .collect();
    let Some(first) = hits.first() else {
        return false;
    };
    let mut lines = Vec::new();
    for m in &hits {
        let c = m.candidate;
        lines.push(format!(
            "Cluster Search #{} · {} · Price {}",
            m.slot,
            utc(m.time),
            c.price.to_f64()
        ));
        lines.push(format!(
            "Volume {:.6} · Bid {:.6} · Ask {:.6} ({unit:?})",
            c.bid + c.ask,
            c.bid,
            c.ask
        ));
        lines.push(format!(
            "Delta {:+.6} · Trades {}",
            c.delta(),
            c.trades
                .map_or("unavailable (compact)".into(), |n| format!("{n:.0}"))
        ));
        lines.push(format!(
            "{:?} {:.6} ≥ {:.6} ({:?})",
            m.metric, c.value, m.threshold, m.rule
        ));
        lines.push(format!(
            "Zone {} — {} · prices {} — {}",
            utc(m.first_time),
            utc(m.last_time),
            c.low.to_f64(),
            c.high.to_f64()
        ));
        lines.push(m.coverage.clone());
        if c.provisional {
            lines.push("Provisional: incomplete/unknown history or developing bar".into());
        }
        if c.approximate {
            lines.push(
                "Approximate price rows: compacted Footprint; trade count unavailable".into(),
            );
        }
    }
    draw_hover_lines(frame, lines, palette, color(first), cursor, bounds);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cluster_search_coverage_uses_chart_history_without_promoting_live_or_gap() {
        assert!(history_ready(TradeCoverage::Partial, true, true, false));
        assert!(history_ready(TradeCoverage::Complete, false, true, false));
        assert!(!history_ready(TradeCoverage::Unknown, false, true, false));
        assert!(!history_ready(TradeCoverage::Complete, true, false, false));
        assert!(!history_ready(TradeCoverage::Complete, true, true, true));
    }
    #[test]
    fn cluster_search_old_settings_default_new_options() {
        let mut json = serde_json::to_value(Config::default()).unwrap();
        for item in json["cluster_search"].as_array_mut().unwrap() {
            item["ClusterSearch"]["style"]
                .as_object_mut()
                .unwrap()
                .remove("options");
        }
        let config: Config = serde_json::from_value(json).unwrap();
        assert_eq!(config.cluster_search, Config::default().cluster_search);
    }
}
