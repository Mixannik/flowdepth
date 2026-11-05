pub mod ladder;
pub mod timeandsales;

use iced::{
    Element, padding,
    widget::{center, container, text},
};
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub enum Message {
    Scrolled(f32),
    ResetScroll,
    LadderControl(ladder::Control),
    DepthRulerMoved(bool, exchange::unit::Price),
    Invalidate(Option<Instant>),
}

pub enum Action {}

pub trait Panel {
    fn ladder_control(&mut self, _control: ladder::Control) {}
    fn move_depth_ruler(&mut self, _upper: bool, _price: exchange::unit::Price) {}
    fn scroll(&mut self, scroll: f32);

    fn reset_scroll(&mut self);

    fn invalidate(&mut self, now: Option<Instant>) -> Option<Action>;

    fn is_empty(&self) -> bool;

    fn view<'a>(&'a self, timezone: data::UserTimezone) -> Element<'a, Message>;
}

pub fn view<T: Panel>(panel: &'_ T, timezone: data::UserTimezone) -> Element<'_, Message> {
    if panel.is_empty() {
        return center(text("Waiting for data...").size(crate::style::text_size::TITLE)).into();
    }

    container(panel.view(timezone))
        .padding(padding::left(1).right(1).bottom(1))
        .into()
}

pub fn update<T: Panel>(panel: &mut T, message: Message) {
    match message {
        Message::LadderControl(control) => panel.ladder_control(control),
        Message::DepthRulerMoved(upper, price) => panel.move_depth_ruler(upper, price),
        Message::Scrolled(delta) => {
            panel.scroll(delta);
        }
        Message::ResetScroll => {
            panel.reset_scroll();
        }
        Message::Invalidate(now) => {
            panel.invalidate(now);
        }
    }
}
