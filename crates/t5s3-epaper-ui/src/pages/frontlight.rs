use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{ascii::FONT_9X18_BOLD, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::draw_back_button,
};

pub(crate) const BRIGHTNESS_STEP: u8 = 3;
const FL_BAR_X: i32 = 70;
const FL_BAR_Y: i32 = 380;
const FL_BAR_W: u32 = 400;
const FL_BAR_H: u32 = 50;
const FL_BTN_Y: i32 = 500;
const FL_BTN_W: u32 = 150;
const FL_BTN_H: u32 = 70;
const FL_MINUS_X: i32 = 80;
const FL_PLUS_X: i32 = 310;

pub(crate) fn minus_hit(sx: i32, sy: i32) -> bool {
    (FL_MINUS_X..FL_MINUS_X + FL_BTN_W as i32).contains(&sx)
        && (FL_BTN_Y..FL_BTN_Y + FL_BTN_H as i32).contains(&sy)
}

pub(crate) fn plus_hit(sx: i32, sy: i32) -> bool {
    (FL_PLUS_X..FL_PLUS_X + FL_BTN_W as i32).contains(&sx)
        && (FL_BTN_Y..FL_BTN_Y + FL_BTN_H as i32).contains(&sy)
}

pub(crate) fn draw_frontlight_screen(display: &mut Display, brightness: u8) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);

    Text::with_alignment(
        "Frontlight",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    draw_brightness_area(display, brightness);

    let btn_border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(3)
        .fill_color(Gray4::WHITE)
        .build();

    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(FL_MINUS_X, FL_BTN_Y),
            Size::new(FL_BTN_W, FL_BTN_H),
        ),
        Size::new(12, 12),
    )
    .into_styled(btn_border)
    .draw(display)
    .ok();

    Text::with_alignment(
        "-",
        Point::new(
            FL_MINUS_X + FL_BTN_W as i32 / 2,
            FL_BTN_Y + FL_BTN_H as i32 / 2 + 6,
        ),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(FL_PLUS_X, FL_BTN_Y),
            Size::new(FL_BTN_W, FL_BTN_H),
        ),
        Size::new(12, 12),
    )
    .into_styled(btn_border)
    .draw(display)
    .ok();

    Text::with_alignment(
        "+",
        Point::new(
            FL_PLUS_X + FL_BTN_W as i32 / 2,
            FL_BTN_Y + FL_BTN_H as i32 / 2 + 6,
        ),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

pub(crate) fn draw_brightness_area(display: &mut Display, brightness: u8) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);

    let mut buf = FmtBuf::<8>::new();
    write!(buf, "{}%", brightness).ok();
    Text::with_alignment(
        buf.as_str(),
        Point::new(SCREEN_W / 2, 330),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    Rectangle::new(
        Point::new(FL_BAR_X, FL_BAR_Y),
        Size::new(FL_BAR_W, FL_BAR_H),
    )
    .into_styled(
        PrimitiveStyleBuilder::new()
            .stroke_color(Gray4::BLACK)
            .stroke_width(2)
            .build(),
    )
    .draw(display)
    .ok();

    let fill_w = FL_BAR_W * brightness.min(100) as u32 / 100;
    if fill_w > 4 {
        Rectangle::new(
            Point::new(FL_BAR_X + 3, FL_BAR_Y + 3),
            Size::new(fill_w - 4, FL_BAR_H - 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(6)))
        .draw(display)
        .ok();
    }
}

pub(crate) fn brightness_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(40, 290, 460, 160)
}
