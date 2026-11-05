use super::Message;
use crate::style;
use data::panel::ladder::{ChaseTracker, Config, GroupedDepth, Side, TradeStore};
use exchange::Trade;
use exchange::unit::qty::{Qty, QuantityUnit, volume_size_unit};
use exchange::unit::{Price, PriceStep};
use exchange::{TickerInfo, UnixMs, depth::Depth};
use iced::widget::{button, column, container, row, text};
use std::sync::Arc;

use iced::widget::canvas::{self, Canvas, Path, Stroke, Text};
use iced::{Alignment, Element, Event, Point, Rectangle, Renderer, Size, Theme, mouse};

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const TEXT_SIZE: f32 = style::text_size::SMALL;
const ROW_HEIGHT: f32 = 16.0;
const DEPTH_SCALES: [(u16, &str); 4] = [(10, "±0.1%"), (25, "±0.25%"), (50, "±0.5%"), (100, "±1%")];
const DEFAULT_DEPTH_BPS: u16 = 50;

const COL_PADDING: f32 = 5.0;
const RULER_X: f32 = 7.0;
const MONO_CHAR_ADVANCE: f32 = 0.62;

const CHASE_CIRCLE_RADIUS: f32 = 4.0;
/// Maximum interval between chase updates to consider them part of the same chase
const CHASE_MIN_INTERVAL: Duration = Duration::from_millis(200);

impl super::Panel for Ladder {
    fn ladder_control(&mut self, control: Control) {
        match control {
            Control::Preset(bps) => {
                let Some(mid) = self.mid_price() else {
                    return;
                };
                self.ruler_prices = Some(mid_band(mid, bps));
                self.reset_scroll();
            }
            Control::Ruler => {
                self.config.show_depth_ruler = !self.config.show_depth_ruler;
                if self.config.show_depth_ruler {
                    self.ruler_prices = self.ruler_band();
                }
            }
            Control::Cumulative(enabled) => self.config.show_cumulative_depth = enabled,
            Control::DepthScale(bps) => {
                if DEPTH_SCALES.iter().any(|(valid, _)| *valid == bps) {
                    self.depth_bps = bps;
                }
            }
            Control::Unit(unit) => {
                if !self.has_btc_usdt_units() {
                    return;
                }
                self.unit_override = Some(unit);
                if let Some(depth) = self.depth.clone() {
                    self.regroup_from_depth(&depth);
                }
            }
        }
        self.invalidate(Some(Instant::now()));
    }

    fn move_depth_ruler(&mut self, upper: bool, price: Price) {
        if let Some(mut band) = self.ruler_band() {
            if upper {
                band[1] = price
                    .max(self.best_price(Side::Ask).unwrap_or(price))
                    .max(band[0].add_steps(1, self.step));
            } else {
                band[0] = price
                    .min(self.best_price(Side::Bid).unwrap_or(price))
                    .min(band[1].add_steps(-1, self.step));
            }
            self.ruler_prices = Some(band);
            self.invalidate(Some(Instant::now()));
        }
    }
    fn scroll(&mut self, delta: f32) {
        self.scroll_px += delta;
        Ladder::invalidate(self, Some(Instant::now()));
    }

    fn reset_scroll(&mut self) {
        self.scroll_px = 0.0;
        Ladder::invalidate(self, Some(Instant::now()));
    }

    fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        Ladder::invalidate(self, now)
    }

    fn is_empty(&self) -> bool {
        if self.pending_tick_size.is_some() {
            return true;
        }
        self.grouped_asks().is_empty() && self.grouped_bids().is_empty() && self.trades.is_empty()
    }

    fn view<'a>(&'a self, _timezone: data::UserTimezone) -> Element<'a, Message> {
        let control = |label, action, active| {
            button(text(label).size(11))
                .padding([3, 7])
                .style(move |theme, status| style::button::toolbar(theme, status, active))
                .on_press(Message::LadderControl(action))
        };
        let mut toolbar = row![
            control(
                "DOM",
                Control::Cumulative(false),
                !self.config.show_cumulative_depth
            ),
            control(
                "Depth",
                Control::Cumulative(true),
                self.config.show_cumulative_depth
            ),
        ]
        .spacing(2)
        .align_y(Alignment::Center);
        if !self.config.show_cumulative_depth {
            toolbar = toolbar.push(control(
                "Ruler",
                Control::Ruler,
                self.config.show_depth_ruler,
            ));
        }
        if self.has_btc_usdt_units() {
            toolbar = toolbar
                .push(control(
                    "BTC",
                    Control::Unit(QuantityUnit::Base),
                    self.unit() == QuantityUnit::Base,
                ))
                .push(control(
                    "USDT",
                    Control::Unit(QuantityUnit::Quote),
                    self.unit() == QuantityUnit::Quote,
                ));
        }
        let mut body = column![toolbar.wrap()].spacing(4);
        if self.config.show_cumulative_depth {
            let mut scale = row![text("Range").size(11).style(style::secondary_text)]
                .spacing(2)
                .align_y(Alignment::Center);
            for (bps, label) in DEPTH_SCALES {
                scale = scale.push(control(
                    label,
                    Control::DepthScale(bps),
                    self.depth_bps == bps,
                ));
            }
            body = body.push(scale.wrap());
        } else if self.config.show_depth_ruler {
            let mut presets = row![text("Range").size(11).style(style::secondary_text)]
                .spacing(2)
                .align_y(Alignment::Center);
            if self.mid_price().is_some() {
                for (label, bps) in [("±0.1%", 10), ("±0.25%", 25), ("±0.5%", 50)] {
                    presets = presets.push(control(label, Control::Preset(bps), false));
                }
            }
            body = body.push(presets.wrap()).push(self.ruler_summary());
        }
        let height = if self.config.show_cumulative_depth {
            iced::Length::Fixed(210.0)
        } else {
            iced::Length::Fill
        };
        container(body.push(Canvas::new(self).width(iced::Length::Fill).height(height)))
            .width(f32::from(self.config.width_px.clamp(240, 640)))
            .padding(4)
            .into()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Control {
    Preset(u16),
    Ruler,
    Cumulative(bool),
    DepthScale(u16),
    Unit(QuantityUnit),
}

pub struct Ladder {
    ticker_info: TickerInfo,
    pub config: Config,
    cache: canvas::Cache,
    last_tick: Instant,
    step: PriceStep,
    scroll_px: f32,
    last_exchange_ts_ms: Option<UnixMs>,
    orderbook: [GroupedDepth; 2],
    trades: TradeStore,
    pending_tick_size: Option<PriceStep>,
    raw_price_spread: Option<Price>,
    ruler_prices: Option<[Price; 2]>,
    depth: Option<Arc<Depth>>,
    source_unit: QuantityUnit,
    unit_override: Option<QuantityUnit>,
    mid: Option<Price>,
    depth_bps: u16,
}

impl Ladder {
    fn row_height(&self) -> f32 {
        if self.config.compact {
            13.0
        } else {
            ROW_HEIGHT
        }
    }

    fn ruler_band(&self) -> Option<[Price; 2]> {
        self.ruler_prices.or_else(|| {
            Some([
                self.best_price(Side::Bid)?.add_steps(-10, self.step),
                self.best_price(Side::Ask)?.add_steps(10, self.step),
            ])
        })
    }

    pub fn set_config(&mut self, config: Config) {
        self.set_show_chase_tracker(config.show_chase_tracker);
        self.config = config;
        if self.config.show_depth_ruler && self.ruler_prices.is_none() {
            self.ruler_prices = self.ruler_band();
        }
        self.invalidate(Some(Instant::now()));
    }

    pub fn new(config: Option<Config>, ticker_info: TickerInfo, step: PriceStep) -> Self {
        Self {
            trades: TradeStore::new(),
            config: config.unwrap_or_default(),
            ticker_info,
            cache: canvas::Cache::default(),
            last_tick: Instant::now(),
            step,
            scroll_px: 0.0,
            last_exchange_ts_ms: None,
            orderbook: [GroupedDepth::new(), GroupedDepth::new()],
            raw_price_spread: None,
            ruler_prices: None,
            depth: None,
            source_unit: QuantityUnit::for_market(ticker_info.market_type(), volume_size_unit()),
            unit_override: None,
            mid: None,
            depth_bps: DEFAULT_DEPTH_BPS,
            pending_tick_size: None,
        }
    }

    pub fn insert_trades(&mut self, buffer: &[Trade]) {
        self.trades.insert_trades(buffer, self.step);
    }

    pub fn insert_depth(&mut self, depth: &Arc<Depth>, update_t: UnixMs) {
        if let Some(next) = self.pending_tick_size.take() {
            self.step = next;
            self.trades.rebuild_grouped(self.step);
        }

        let raw_best_bid = depth.bids.last_key_value().map(|(p, _)| *p);
        let raw_best_ask = depth.asks.first_key_value().map(|(p, _)| *p);
        self.raw_price_spread = match (raw_best_bid, raw_best_ask) {
            (Some(bid), Some(ask)) => Some(ask - bid),
            _ => None,
        };

        if self.config.show_chase_tracker {
            let max_int = CHASE_MIN_INTERVAL;
            self.chase_tracker_mut(Side::Bid)
                .update(raw_best_bid, true, update_t, max_int);
            self.chase_tracker_mut(Side::Ask)
                .update(raw_best_ask, false, update_t, max_int);
        } else {
            self.chase_tracker_mut(Side::Bid).reset();
            self.chase_tracker_mut(Side::Ask).reset();
        }

        if self
            .trades
            .maybe_cleanup(update_t, self.config.trade_retention, self.step)
        {
            self.invalidate(Some(Instant::now()));
        }

        self.source_unit =
            QuantityUnit::for_market(self.ticker_info.market_type(), volume_size_unit());
        self.mid = raw_best_bid
            .zip(raw_best_ask)
            .map(|(bid, ask)| Price::from_f64((bid.to_f64() + ask.to_f64()) * 0.5));
        self.depth = Some(Arc::clone(depth));
        self.regroup_from_depth(depth);
        if self.config.show_depth_ruler && self.ruler_prices.is_none() {
            self.ruler_prices = self.ruler_band();
        }
        self.last_exchange_ts_ms = Some(update_t);
    }

    fn trade_qty_at(&self, price: Price) -> (Qty, Qty) {
        self.trades.trade_qty_at(price)
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    fn grouped_asks(&self) -> &BTreeMap<Price, Qty> {
        &self.orderbook[Side::Ask.idx()].orders
    }

    fn grouped_bids(&self) -> &BTreeMap<Price, Qty> {
        &self.orderbook[Side::Bid.idx()].orders
    }

    fn chase_tracker(&self, side: Side) -> &ChaseTracker {
        &self.orderbook[side.idx()].chase
    }

    fn chase_tracker_mut(&mut self, side: Side) -> &mut ChaseTracker {
        &mut self.orderbook[side.idx()].chase
    }

    fn best_price(&self, side: Side) -> Option<Price> {
        self.orderbook[side.idx()].best_price(side)
    }

    pub fn min_tick_size(&self) -> f32 {
        self.ticker_info.min_ticksize.into()
    }

    pub fn tick_size(&self) -> PriceStep {
        self.pending_tick_size.unwrap_or(self.step)
    }

    pub fn set_tick_size(&mut self, step: PriceStep) {
        self.pending_tick_size = Some(step);
        self.invalidate(Some(Instant::now()));
    }

    pub fn set_show_chase_tracker(&mut self, enabled: bool) {
        if self.config.show_chase_tracker != enabled {
            self.config.show_chase_tracker = enabled;
            if !enabled {
                self.chase_tracker_mut(Side::Bid).reset();
                self.chase_tracker_mut(Side::Ask).reset();
            }

            self.invalidate(Some(Instant::now()));
        }
    }

    fn mid_price(&self) -> Option<Price> {
        self.mid
    }

    fn has_btc_usdt_units(&self) -> bool {
        matches!(
            self.ticker_info.exchange(),
            exchange::adapter::Exchange::BinanceLinear | exchange::adapter::Exchange::BinanceSpot
        ) && self.ticker_info.ticker.to_full_symbol_and_type().0 == "BTCUSDT"
    }

    fn unit(&self) -> QuantityUnit {
        self.unit_override.unwrap_or(self.source_unit)
    }

    fn unit_label(&self) -> &'static str {
        match (self.has_btc_usdt_units(), self.unit()) {
            (true, QuantityUnit::Base) => "BTC",
            (true, QuantityUnit::Quote) => "USDT",
            (false, unit) => unit.cache_tag(),
        }
    }

    fn show_trade_columns(&self, width: f32) -> bool {
        width >= 450.0
            && !self.config.compact
            && !self.config.show_cumulative_depth
            && self.unit() == self.source_unit
    }

    fn show_chase_lane(&self, width: f32) -> bool {
        self.config.show_chase_tracker && !self.config.compact && width >= 240.0
    }

    fn convert_qty(source: QuantityUnit, target: QuantityUnit, price: Price, qty: Qty) -> Qty {
        match (source, target) {
            (QuantityUnit::Base, QuantityUnit::Quote) => {
                Qty::from_f64(qty.to_f64() * price.to_f64())
            }
            (QuantityUnit::Quote, QuantityUnit::Base) if price.units > 0 => {
                Qty::from_f64(qty.to_f64() / price.to_f64())
            }
            _ => qty,
        }
    }

    fn regroup_from_depth(&mut self, depth: &Depth) {
        let (source, target) = (self.source_unit, self.unit());
        for (side, levels) in [(Side::Bid, &depth.bids), (Side::Ask, &depth.asks)] {
            self.orderbook[side.idx()].regroup_with(levels, side, self.step, |p, q| {
                Self::convert_qty(source, target, p, q)
            });
        }
    }

    fn ruler_summary(&self) -> Element<'_, Message> {
        let Some(([lower, upper], depth)) = self.ruler_band().zip(self.depth.as_deref()) else {
            return text("Waiting for both sides").size(11).into();
        };
        let sum = |levels: &BTreeMap<Price, Qty>| {
            (!levels.is_empty()).then(|| {
                levels
                    .range(lower..=upper)
                    .map(|(&p, &q)| Self::convert_qty(self.source_unit, self.unit(), p, q).to_f64())
                    .sum::<f64>()
            })
        };
        let (bid, ask) = (sum(&depth.bids), sum(&depth.asks));
        let format = |q: Option<f64>| {
            q.map(data::util::abbr_large_numbers)
                .unwrap_or_else(|| "—".into())
        };
        let ratio = bid
            .zip(ask)
            .filter(|(_, ask)| *ask > 0.0)
            .map(|(bid, ask)| format!("{:.2}:1", bid / ask))
            .unwrap_or_else(|| "—".into());
        let unit = self.unit_label();
        let coverage = if bid.is_none() || ask.is_none() {
            " · Missing side"
        } else if depth.bids.keys().next().is_some_and(|p| *p > lower)
            || depth.asks.keys().next_back().is_some_and(|p| *p < upper)
        {
            " · Partial"
        } else {
            ""
        };
        column![
            row![
                text(format!("Bid {} {unit}", format(bid))).size(11),
                text(format!("Ask {} {unit}", format(ask))).size(11),
                text(format!("B/A {ratio}{coverage}"))
                    .size(11)
                    .style(style::secondary_text)
            ]
            .spacing(6)
            .wrap(),
            text(format!(
                "Ruler {} – {}",
                self.format_price(lower),
                self.format_price(upper)
            ))
            .size(11)
            .style(style::secondary_text),
        ]
        .spacing(2)
        .into()
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        self.cache.clear();
        if let Some(now) = now {
            self.last_tick = now;
        }
        None
    }

    fn format_price(&self, price: Price) -> String {
        let precision = self.ticker_info.min_ticksize;
        price.to_string(precision)
    }

    fn format_quantity(&self, qty: Qty) -> String {
        data::util::abbr_large_numbers(qty.to_f64())
    }
}

impl canvas::Program<Message> for Ladder {
    type State = Option<bool>;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: iced::Rectangle,
        cursor: iced_core::mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
        ) {
            if state.take().is_some() {
                return Some(canvas::Action::request_redraw().and_capture());
            }
        }
        if self.config.show_cumulative_depth {
            return None;
        }
        let position = cursor.position_in(bounds)?;
        if self.config.show_depth_ruler {
            if let (Some(grid), Some(band)) = (self.build_price_grid(), self.ruler_band()) {
                if position.x <= 16.0
                    && matches!(
                        event,
                        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                    )
                {
                    for (i, price) in band.into_iter().enumerate() {
                        if self
                            .price_to_screen_y(price, &grid, bounds.height)
                            .is_some_and(|y| (position.y - y).abs() <= 7.0)
                        {
                            *state = Some(i == 1);
                            return Some(canvas::Action::request_redraw().and_capture());
                        }
                    }
                }
                if let Some(upper) = *state {
                    if matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. })) {
                        let idx = ((position.y - bounds.height * 0.5 + self.scroll_px)
                            / self.row_height())
                        .round() as i32;
                        let price = grid.index_to_price(idx).unwrap_or(grid.best_bid);
                        return Some(
                            canvas::Action::publish(Message::DepthRulerMoved(upper, price))
                                .and_capture(),
                        );
                    }
                }
            }
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Middle | mouse::Button::Left | mouse::Button::Right,
            )) => Some(canvas::Action::publish(Message::ResetScroll).and_capture()),
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let scroll_amount = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => -(*y) * self.row_height(),
                    mouse::ScrollDelta::Pixels { y, .. } => -*y,
                };

                Some(canvas::Action::publish(Message::Scrolled(scroll_amount)).and_capture())
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: iced_core::mouse::Cursor,
    ) -> mouse::Interaction {
        if self.config.show_cumulative_depth || !self.config.show_depth_ruler {
            return mouse::Interaction::default();
        }
        if state.is_some() {
            return mouse::Interaction::ResizingVertically;
        }
        if let Some(position) = cursor.position_in(bounds).filter(|p| p.x <= 16.0) {
            if let (Some(grid), Some(band)) = (self.build_price_grid(), self.ruler_band()) {
                if band.iter().any(|p| {
                    self.price_to_screen_y(*p, &grid, bounds.height)
                        .is_some_and(|y| (position.y - y).abs() <= 7.0)
                }) {
                    return mouse::Interaction::ResizingVertically;
                }
            }
        }
        mouse::Interaction::default()
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: iced_core::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry<Renderer>> {
        let visual = self.cache.draw(renderer, bounds.size(), |frame| {
            if self.config.show_cumulative_depth {
                self.draw_cumulative(frame, bounds, theme);
                return;
            }
            let Some(grid) = self.build_price_grid() else {
                return;
            };
            let palette = theme.palette();
            let bid = palette.success.base.color;
            let ask = palette.danger.base.color;
            let fg = palette.background.base.text;
            let (rows, maxima) = self.visible_rows(bounds, &grid);
            let cols = self.column_ranges(bounds.width, &grid);

            let mut walls: Vec<_> = rows
                .iter()
                .filter_map(|row| match row.row {
                    DomRow::Ask { qty, .. } | DomRow::Bid { qty, .. } if qty > Qty::ZERO => {
                        Some((qty, row.y))
                    }
                    _ => None,
                })
                .collect();
            walls.sort_by(|(left, _), (right, _)| right.cmp(left));
            walls.truncate(8);

            for row in &rows {
                let (price, qty, color) = match row.row {
                    DomRow::Ask { price, qty } => (price, qty, ask),
                    DomRow::Bid { price, qty } => (price, qty, bid),
                    DomRow::Spread => {
                        self.draw_spread(frame, bounds.width, row.y, theme);
                        continue;
                    }
                };
                let wall = walls.iter().any(|&(_, y)| y == row.y);
                let intensity = self.config.liquidity_intensity.clamp(0.0, 1.0);
                let alpha = if wall {
                    0.28 + 0.32 * intensity
                } else {
                    0.12 + 0.12 * intensity
                };
                self.fill_bar(
                    frame,
                    cols.volume,
                    row.y,
                    qty,
                    maxima.vis_max_order_qty,
                    color,
                    alpha,
                );
                if wall {
                    frame.fill_rectangle(
                        Point::new(cols.volume.0, row.y + 2.0),
                        Size::new(2.0, self.row_height() - 4.0),
                        color,
                    );
                }
                self.draw_cell_text(
                    frame,
                    &self.format_price(price),
                    cols.price_end,
                    row.y,
                    color,
                    Alignment::End,
                );
                if qty > Qty::ZERO && cols.volume.1 - cols.volume.0 > 30.0 {
                    self.draw_cell_text(
                        frame,
                        &self.format_quantity(qty),
                        cols.volume.1 - COL_PADDING,
                        row.y,
                        fg,
                        Alignment::End,
                    );
                }
                for (range, qty, color) in
                    [(cols.sell, row.sell_t, ask), (cols.buy, row.buy_t, bid)]
                {
                    if qty > Qty::ZERO {
                        self.fill_bar(
                            frame,
                            range,
                            row.y,
                            qty,
                            maxima.vis_max_trade_qty,
                            color,
                            0.24,
                        );
                        self.draw_cell_text(
                            frame,
                            &self.format_quantity(qty),
                            range.1 - COL_PADDING,
                            row.y,
                            fg,
                            Alignment::End,
                        );
                    }
                }
            }
            if self.show_chase_lane(bounds.width) {
                for (side, x, color) in [
                    (Side::Bid, cols.price_end + 6.0, bid),
                    (Side::Ask, cols.price_end + 15.0, ask),
                ] {
                    self.draw_chase_trail(
                        frame,
                        &grid,
                        bounds,
                        self.chase_tracker(side),
                        x,
                        None,
                        color,
                        side.is_bid(),
                    );
                }
            }
            self.draw_ruler(frame, &grid, bounds, theme);
        });
        vec![visual]
    }
}

#[derive(Default)]
struct Maxima {
    vis_max_order_qty: f32,
    vis_max_trade_qty: f32,
}

struct VisibleRow {
    row: DomRow,
    y: f32,
    buy_t: Qty,
    sell_t: Qty,
}

struct ColumnRanges {
    price_end: f32,
    volume: (f32, f32),
    sell: (f32, f32),
    buy: (f32, f32),
}

impl Ladder {
    fn column_ranges(&self, width: f32, grid: &PriceGrid) -> ColumnRanges {
        let price_chars = self
            .format_price(grid.best_ask)
            .len()
            .max(self.format_price(grid.best_bid).len());
        let price_end =
            (18.0 + price_chars as f32 * TEXT_SIZE * MONO_CHAR_ADVANCE).min(width * 0.56);
        let show_trades = self.show_trade_columns(width);
        let trade_width = if show_trades { 48.0 } else { 0.0 };
        let volume_start = (price_end
            + if self.show_chase_lane(width) {
                23.0
            } else {
                COL_PADDING
            })
        .min(width);
        let volume_end = (width - 2.0 * trade_width).max(volume_start);
        ColumnRanges {
            price_end,
            volume: (volume_start, volume_end),
            sell: (volume_end, (volume_end + trade_width).min(width)),
            buy: ((volume_end + trade_width).min(width), width),
        }
    }

    fn fill_bar(
        &self,
        frame: &mut canvas::Frame,
        range: (f32, f32),
        y: f32,
        qty: Qty,
        maximum: f32,
        color: iced::Color,
        alpha: f32,
    ) {
        if maximum > 0.0 && qty > Qty::ZERO && range.1 > range.0 {
            frame.fill_rectangle(
                Point::new(range.0, y + 1.0),
                Size::new(
                    (range.1 - range.0) * (qty.to_f32_lossy() / maximum).min(1.0),
                    self.row_height() - 2.0,
                ),
                color.scale_alpha(alpha),
            );
        }
    }

    fn draw_spread(&self, frame: &mut canvas::Frame, width: f32, y: f32, theme: &Theme) {
        let palette = theme.palette();
        frame.fill_rectangle(
            Point::new(0.0, y),
            Size::new(width, self.row_height()),
            palette.primary.base.color.scale_alpha(0.30),
        );
        let Some(mid) = self.mid_price() else {
            self.draw_cell_text(
                frame,
                "Waiting for both sides",
                18.0,
                y,
                palette.background.base.text,
                Alignment::Start,
            );
            return;
        };
        let digits = 1 + (-self.ticker_info.min_ticksize.power).max(0) as usize;
        let mut label = format!("{:.*}", digits, mid.to_f64());
        if self.config.show_spread || self.has_btc_usdt_units() {
            if let Some(spread) = self.raw_price_spread {
                label.push_str(&format!("  ·  Spread {}", self.format_price(spread)));
            }
        }
        self.draw_cell_text(
            frame,
            &label,
            18.0,
            y,
            palette.background.base.text,
            Alignment::Start,
        );
    }

    fn draw_cumulative(&self, frame: &mut canvas::Frame, bounds: Rectangle, theme: &Theme) {
        let palette = theme.palette();
        let fg = palette.background.base.text;
        let Some((mid, book)) = self
            .mid_price()
            .filter(|p| p.units > 0)
            .zip(self.depth.as_deref())
        else {
            self.draw_cell_text(
                frame,
                "Waiting for both sides",
                6.0,
                4.0,
                fg,
                Alignment::Start,
            );
            return;
        };
        let Some((bid_edge, ask_edge)) = book.bids.keys().next().zip(book.asks.keys().next_back())
        else {
            self.draw_cell_text(
                frame,
                "Waiting for both sides",
                6.0,
                4.0,
                fg,
                Alignment::Start,
            );
            return;
        };
        let [lower, upper] = mid_band(mid, self.depth_bps);
        let limit = f64::from(self.depth_bps) / 100.0;
        let label = DEPTH_SCALES
            .iter()
            .find(|(bps, _)| *bps == self.depth_bps)
            .map_or("0.5%", |(_, label)| label.trim_start_matches('±'));
        let convert =
            |price, qty| Self::convert_qty(self.source_unit, self.unit(), price, qty).to_f64();
        let mut bids = book.bids.range(lower..=mid).rev();
        let mut asks = book.asks.range(mid..=upper);
        let total = |levels: &mut dyn Iterator<Item = (&Price, &Qty)>| {
            levels.map(|(&p, &q)| convert(p, q)).sum::<f64>()
        };
        let bid_total = total(&mut bids.clone());
        let ask_total = total(&mut asks.clone());
        let partial = *bid_edge > lower || *ask_edge < upper;
        let unit = self.unit_label();
        let left = 6.0;
        let right = bounds.width - 6.0;
        let bid = palette.success.base.color;
        let ask = palette.danger.base.color;
        self.draw_cell_text(
            frame,
            &format!(
                "Within ±{label} of mid{}",
                if partial { " · Partial" } else { "" }
            ),
            left,
            1.0,
            fg.scale_alpha(0.8),
            Alignment::Start,
        );
        self.draw_cell_text(
            frame,
            &format!("Bid {} {unit}", data::util::abbr_large_numbers(bid_total)),
            left,
            18.0,
            bid,
            Alignment::Start,
        );
        self.draw_cell_text(
            frame,
            &format!("Ask {} {unit}", data::util::abbr_large_numbers(ask_total)),
            right,
            18.0,
            ask,
            Alignment::End,
        );

        let top = 43.0;
        let bottom = bounds.height - 25.0;
        if right - left < 60.0 || bottom <= top {
            return;
        }
        let x_at = |pct: f64| left + (right - left) * (pct / limit).clamp(0.0, 1.0) as f32;
        let grid = fg.scale_alpha(0.15);
        for fraction in [0.0, 0.5, 1.0] {
            let x = left + (right - left) * fraction;
            let y = bottom - (bottom - top) * fraction;
            frame.stroke(
                &Path::line(Point::new(x, top), Point::new(x, bottom)),
                Stroke::default().with_color(grid),
            );
            frame.stroke(
                &Path::line(Point::new(left, y), Point::new(right, y)),
                Stroke::default().with_color(grid),
            );
        }
        for (x, label, align) in [
            (left, "0%".to_owned(), Alignment::Start),
            (
                x_at(limit * 0.5),
                format!("{}%", limit * 0.5),
                Alignment::Center,
            ),
            (right, label.to_owned(), Alignment::End),
        ] {
            self.draw_cell_text(frame, &label, x, bottom + 4.0, fg, align);
        }
        let maximum = bid_total.max(ask_total);
        let draw = |frame: &mut canvas::Frame,
                    levels: &mut dyn Iterator<Item = (&Price, &Qty)>,
                    edge: Price,
                    color| {
            let path = Path::new(|p| {
                p.move_to(Point::new(left, bottom));
                let mut accumulated = 0.0;
                let mut previous_y = bottom;
                for (&price, &qty) in levels {
                    let x = x_at(distance_pct(mid, price));
                    accumulated += convert(price, qty);
                    let y = if maximum > 0.0 {
                        bottom - (bottom - top) * (accumulated / maximum).min(1.0) as f32
                    } else {
                        bottom
                    };
                    p.line_to(Point::new(x, previous_y));
                    p.line_to(Point::new(x, y));
                    previous_y = y;
                }
                // Extend the last known value only through the observed depth.
                p.line_to(Point::new(x_at(distance_pct(mid, edge)), previous_y));
            });
            frame.stroke(&path, Stroke::default().with_color(color).with_width(2.0));
        };
        draw(frame, &mut bids, *bid_edge, bid);
        draw(frame, &mut asks, *ask_edge, ask);
    }

    fn draw_cell_text(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        text: &str,
        x_anchor: f32,
        y: f32,
        color: iced::Color,
        align: Alignment,
    ) {
        frame.fill_text(Text {
            content: text.to_string(),
            position: Point::new(x_anchor, y + self.row_height() / 2.0),
            color,
            size: TEXT_SIZE.into(),
            font: style::AZERET_MONO,
            align_x: align.into(),
            align_y: Alignment::Center.into(),
            ..Default::default()
        });
    }

    fn draw_chase_trail(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        grid: &PriceGrid,
        bounds: Rectangle,
        tracker: &ChaseTracker,
        pos_x: f32,
        best_offer_y: Option<f32>,
        color: iced::Color,
        is_bid: bool,
    ) {
        let radius = CHASE_CIRCLE_RADIUS;
        if let Some((start_p_raw, end_p_raw, alpha)) = tracker.segment() {
            let start_p = start_p_raw.round_to_side_step(is_bid, grid.tick);
            let end_p = end_p_raw.round_to_side_step(is_bid, grid.tick);

            let color = color.scale_alpha(alpha);
            let stroke_w = 2.0;
            let pad_to_circle = radius + stroke_w * 0.5;

            let start_y = self.price_to_screen_y(start_p, grid, bounds.height);
            let end_y = self
                .price_to_screen_y(end_p, grid, bounds.height)
                .or(best_offer_y);

            if let Some(end_y) = end_y {
                if let Some(start_y) = start_y {
                    let dy = end_y - start_y;
                    if dy.abs() > pad_to_circle {
                        let line_end_y = end_y - dy.signum() * pad_to_circle;
                        let line_path =
                            Path::line(Point::new(pos_x, start_y), Point::new(pos_x, line_end_y));
                        frame.stroke(
                            &line_path,
                            Stroke::default().with_color(color).with_width(stroke_w),
                        );
                    }
                }

                let circle = &Path::circle(Point::new(pos_x, end_y), radius);
                frame.fill(circle, color);
            }
        }
    }

    fn draw_ruler(
        &self,
        frame: &mut canvas::Frame,
        grid: &PriceGrid,
        bounds: Rectangle,
        theme: &Theme,
    ) {
        if !self.config.show_depth_ruler {
            return;
        }
        let Some([lower, upper]) = self.ruler_band() else {
            return;
        };
        let palette = theme.palette();
        for (price, color) in [
            (lower, palette.success.base.color),
            (upper, palette.danger.base.color),
        ] {
            if let Some(y) = self.price_to_screen_y(price, grid, bounds.height) {
                if (0.0..bounds.height).contains(&y) {
                    frame.fill_rectangle(
                        Point::new(RULER_X - 4.0, y - 2.0),
                        Size::new(8.0, 4.0),
                        color,
                    );
                }
            }
        }
    }

    fn build_price_grid(&self) -> Option<PriceGrid> {
        let best_bid = match (self.best_price(Side::Bid), self.best_price(Side::Ask)) {
            (Some(bb), _) => bb,
            (None, Some(ba)) => ba.add_steps(-1, self.step),
            (None, None) => {
                let (min_t, max_t) = self.trades.price_range()?;
                let steps = Price::steps_between_inclusive(min_t, max_t, self.step).unwrap_or(1);
                max_t.add_steps(-(steps as i64 / 2), self.step)
            }
        };
        let best_ask = best_bid.add_steps(1, self.step);

        Some(PriceGrid {
            best_bid,
            best_ask,
            tick: self.step,
        })
    }

    fn visible_rows(&self, bounds: Rectangle, grid: &PriceGrid) -> (Vec<VisibleRow>, Maxima) {
        let asks_grouped = self.grouped_asks();
        let bids_grouped = self.grouped_bids();

        let mut visible: Vec<VisibleRow> = Vec::new();
        let mut maxima = Maxima::default();
        let mid_screen_y = bounds.height * 0.5;
        let scroll = self.scroll_px;

        let y0 = mid_screen_y + PriceGrid::top_y(0, self.row_height()) - scroll;
        let idx_top = ((0.0 - y0) / self.row_height()).floor() as i32;

        let rows_needed = (bounds.height / self.row_height()).ceil() as i32 + 1;
        let idx_bottom = idx_top + rows_needed;

        for idx in idx_top..=idx_bottom {
            if idx == 0 {
                let top_y_screen = mid_screen_y + PriceGrid::top_y(0, self.row_height()) - scroll;
                if top_y_screen < bounds.height && top_y_screen + self.row_height() > 0.0 {
                    visible.push(VisibleRow {
                        row: DomRow::Spread,
                        y: top_y_screen,
                        buy_t: Qty::default(),
                        sell_t: Qty::default(),
                    });
                }
                continue;
            }

            let Some(price) = grid.index_to_price(idx) else {
                continue;
            };

            let is_bid = idx > 0;
            let levels = if is_bid { bids_grouped } else { asks_grouped };
            let order_qty = levels.get(&price).copied().unwrap_or_default();

            let top_y_screen = mid_screen_y + PriceGrid::top_y(idx, self.row_height()) - scroll;
            if top_y_screen >= bounds.height || top_y_screen + self.row_height() <= 0.0 {
                continue;
            }

            maxima.vis_max_order_qty = maxima.vis_max_order_qty.max(order_qty.to_f32_lossy());
            let (buy_t, sell_t) = if self.show_trade_columns(bounds.width) {
                self.trade_qty_at(price)
            } else {
                (Qty::ZERO, Qty::ZERO)
            };
            maxima.vis_max_trade_qty = maxima
                .vis_max_trade_qty
                .max(buy_t.to_f32_lossy().max(sell_t.to_f32_lossy()));

            let row = if is_bid {
                DomRow::Bid {
                    price,
                    qty: order_qty,
                }
            } else {
                DomRow::Ask {
                    price,
                    qty: order_qty,
                }
            };

            visible.push(VisibleRow {
                row,
                y: top_y_screen,
                buy_t,
                sell_t,
            });
        }

        visible.sort_by(|a, b| a.y.total_cmp(&b.y));
        (visible, maxima)
    }

    fn price_to_screen_y(&self, price: Price, grid: &PriceGrid, bounds_height: f32) -> Option<f32> {
        let mid_screen_y = bounds_height * 0.5;
        let scroll = self.scroll_px;

        let idx = if price >= grid.best_ask {
            let steps = Price::steps_between_inclusive(grid.best_ask, price, grid.tick)?;
            -(steps as i32)
        } else if price <= grid.best_bid {
            let steps = Price::steps_between_inclusive(price, grid.best_bid, grid.tick)?;
            steps as i32
        } else {
            return Some(mid_screen_y - scroll);
        };

        let y = mid_screen_y + PriceGrid::top_y(idx, self.row_height()) - scroll
            + self.row_height() / 2.0;
        Some(y)
    }
}

fn mid_band(mid: Price, bps: u16) -> [Price; 2] {
    let offset = (i128::from(mid.units) * i128::from(bps) / 10_000) as i64;
    [
        Price::from_units(mid.units.saturating_sub(offset)),
        Price::from_units(mid.units.saturating_add(offset)),
    ]
}

fn distance_pct(mid: Price, price: Price) -> f64 {
    price.units.abs_diff(mid.units) as f64 / mid.units as f64 * 100.0
}

enum DomRow {
    Ask { price: Price, qty: Qty },
    Spread,
    Bid { price: Price, qty: Qty },
}

struct PriceGrid {
    best_bid: Price,
    best_ask: Price,
    tick: PriceStep,
}

impl PriceGrid {
    /// Returns None for index 0 (spread row)
    fn index_to_price(&self, idx: i32) -> Option<Price> {
        if idx == 0 {
            return None;
        }
        if idx > 0 {
            let off = (idx - 1) as i64; // 1 => best_bid, 2 => best_bid - 1 tick
            Some(self.best_bid.add_steps(-off, self.tick))
        } else {
            let off = (-1 - idx) as i64; // -1 => best_ask, -2 => best_ask + 1 tick
            Some(self.best_ask.add_steps(off, self.tick))
        }
    }

    fn top_y(idx: i32, height: f32) -> f32 {
        (idx as f32) * height - height * 0.5
    }
}

#[cfg(test)]
mod dom2_tests {
    use super::super::Panel;
    use super::*;
    use exchange::{Ticker, adapter::Exchange};
    fn ladder() -> Ladder {
        let ticker = TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceSpot),
            1.0,
            0.001,
            None,
        );
        let mut ladder = Ladder::new(None, ticker, ticker.min_ticksize.into());
        let bids = [(Price::from_f64(100.0), Qty::from_f64(2.0))]
            .into_iter()
            .collect();
        let asks = [(Price::from_f64(102.0), Qty::from_f64(3.0))]
            .into_iter()
            .collect();
        ladder.orderbook[0].regroup_from_raw(&bids, Side::Bid, ladder.step);
        ladder.orderbook[1].regroup_from_raw(&asks, Side::Ask, ladder.step);
        ladder.set_config(Config {
            show_depth_ruler: true,
            ..Config::default()
        });
        ladder
    }
    #[test]
    fn dom2_unit_conversion_uses_each_raw_price_before_grouping() {
        let mut panel = ladder();
        panel.source_unit = QuantityUnit::Base;
        panel.unit_override = Some(QuantityUnit::Quote);
        panel.step = PriceStep {
            units: Price::from_f64(100.0).units,
        };
        let depth = Depth {
            bids: [
                (Price::from_f64(60_010.0), Qty::from_f64(2.0)),
                (Price::from_f64(60_090.0), Qty::from_f64(1.0)),
            ]
            .into_iter()
            .collect(),
            asks: BTreeMap::new(),
        };
        panel.regroup_from_depth(&depth);
        assert_eq!(
            panel.grouped_bids()[&Price::from_f64(60_000.0)],
            Qty::from_f64(180_110.0)
        );
        let price = Price::from_f64(60_010.0);
        assert_eq!(
            Ladder::convert_qty(
                QuantityUnit::Quote,
                QuantityUnit::Base,
                price,
                Qty::from_f64(120_020.0)
            ),
            Qty::from_f64(2.0)
        );
    }

    #[test]
    fn dom2_depth_range_is_symmetric_and_excludes_far_orders() {
        let mid = Price::from_f64(80_000.0);
        for (bps, _) in DEPTH_SCALES {
            let [lower, upper] = mid_band(mid, bps);
            let expected = f64::from(bps) / 100.0;
            assert!((distance_pct(mid, lower) - expected).abs() < 1e-12);
            assert!((distance_pct(mid, upper) - expected).abs() < 1e-12);
        }
        let [lower, upper] = mid_band(mid, DEFAULT_DEPTH_BPS);
        let book = BTreeMap::from([
            (Price::from_f64(1.0), Qty::from_f64(100.0)),
            (lower, Qty::from_f64(2.0)),
            (upper, Qty::from_f64(3.0)),
            (Price::from_f64(214_128.0), Qty::from_f64(100.0)),
        ]);
        assert_eq!(
            book.range(lower..=mid).map(|(_, q)| *q).collect::<Vec<_>>(),
            [Qty::from_f64(2.0)]
        );
        assert_eq!(
            book.range(mid..=upper).map(|(_, q)| *q).collect::<Vec<_>>(),
            [Qty::from_f64(3.0)]
        );
    }

    #[test]
    fn dom2_presets_use_raw_mid_and_stay_at_selected_prices() {
        let mut panel = ladder();
        panel.mid = Some(Price::from_f64(60_000.0));
        panel.ladder_control(Control::Preset(25));
        let band = [Price::from_f64(59_850.0), Price::from_f64(60_150.0)];
        assert_eq!(panel.ruler_band(), Some(band));
        panel.mid = Some(Price::from_f64(60_100.0));
        assert_eq!(panel.ruler_band(), Some(band));
    }

    #[test]
    fn dom2_ruler_prices_survive_scroll_compact_and_regroup() {
        let mut ladder = ladder();
        let band = ladder.ruler_band().unwrap();
        let grid = ladder.build_price_grid().unwrap();
        let before = ladder.price_to_screen_y(band[0], &grid, 600.0).unwrap();
        ladder.scroll(37.0);
        assert_eq!(
            ladder.price_to_screen_y(band[0], &grid, 600.0).unwrap(),
            before - 37.0
        );
        assert_eq!(ladder.ruler_band(), Some(band));
        ladder.set_config(Config {
            compact: true,
            ..ladder.config
        });
        assert_eq!(ladder.row_height(), 13.0);
        assert_eq!(ladder.ruler_band(), Some(band));
        ladder.set_tick_size(PriceStep {
            units: ladder.step.units * 2,
        });
        assert_eq!(ladder.ruler_band(), Some(band));
    }
    #[test]
    fn dom2_ruler_drag_cannot_invert_band() {
        let mut ladder = ladder();
        ladder.move_depth_ruler(true, Price::from_f64(1.0));
        let [lower, upper] = ladder.ruler_band().unwrap();
        assert!(upper > lower);
        assert_eq!(upper, ladder.best_price(Side::Ask).unwrap());
        ladder.move_depth_ruler(false, Price::from_f64(500.0));
        let [lower, upper] = ladder.ruler_band().unwrap();
        assert!(upper > lower);
        assert_eq!(lower, ladder.best_price(Side::Bid).unwrap());
    }
    #[test]
    fn dom2_unified_layout_leaves_room_for_volume_at_narrow_widths() {
        let mut ladder = ladder();
        let grid = ladder.build_price_grid().unwrap();
        for compact in [true, false] {
            ladder.config.compact = compact;
            for width in [150.0, 270.0, 380.0, 640.0] {
                let cols = ladder.column_ranges(width, &grid);
                assert!(cols.price_end < cols.volume.0);
                assert!(cols.volume.0 < cols.volume.1);
                assert_eq!(cols.sell.0, cols.volume.1);
                assert_eq!(cols.buy.1, width);
                assert_eq!(cols.volume.1 == width, compact || width < 450.0);
            }
        }
    }
}
