use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W, STATUS_H},
};

fn draw_battery_icon(display: &mut Display, x: i32, y: i32, pct: u16) {
    let body_w: u32 = 30;
    let body_h: u32 = 20;
    let nub_w: u32 = 3;
    let nub_h: u32 = 10;

    // body outline
    Rectangle::new(Point::new(x, y), Size::new(body_w, body_h))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .build(),
        )
        .draw(display)
        .ok();

    // nub
    Rectangle::new(
        Point::new(x + body_w as i32, y + (body_h as i32 - nub_h as i32) / 2),
        Size::new(nub_w, nub_h),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
    .draw(display)
    .ok();

    // fill
    let inner_w = body_w - 6;
    let fill_w = inner_w * pct.min(100) as u32 / 100;
    if fill_w > 0 {
        Rectangle::new(Point::new(x + 3, y + 3), Size::new(fill_w, body_h - 6))
            .into_styled(PrimitiveStyle::with_fill(Gray4::new(4)))
            .draw(display)
            .ok();
    }
}

pub(crate) fn draw_status_bar(
    display: &mut Display,
    voltage: f32,
    pct: u16,
    temp: i8,
    time: Option<(u32, u32)>,
) {
    let status_font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);

    let v_int = voltage as u32;
    let v_frac = ((voltage - v_int as f32) * 100.0) as u32;
    let mut buf = FmtBuf::<48>::new();
    write!(
        buf,
        "{}%  {}.{:02}V  {}C",
        pct.min(100),
        v_int,
        v_frac,
        temp
    )
    .ok();
    Text::with_alignment(
        buf.as_str(),
        Point::new(491, 35),
        status_font,
        Alignment::Right,
    )
    .draw(display)
    .ok();

    draw_battery_icon(display, 497, 20, pct);
    draw_statusbar_time(display, time);

    Rectangle::new(Point::new(0, STATUS_H - 2), Size::new(SCREEN_W as u32, 2))
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(8)))
        .draw(display)
        .ok();
}

// the clock (HH:MM, or --:-- before an NTP sync) shown centered in the status
// bar. drawn over a white fill so the once-a-minute partial refresh cleanly
// replaces the previous value.
pub(crate) fn draw_statusbar_time(display: &mut Display, time: Option<(u32, u32)>) {
    Rectangle::new(Point::new(215, 18), Size::new(110, 30))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let mut buf = FmtBuf::<8>::new();
    match time {
        Some((h, m)) => write!(buf, "{h:02}:{m:02}").ok(),
        None => write!(buf, "--:--").ok(),
    };
    Text::with_alignment(buf.as_str(), Point::new(270, 37), bold, Alignment::Center)
        .draw(display)
        .ok();
}

pub(crate) fn statusbar_time_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(215, 18, 110, 30)
}

pub(crate) fn draw_back_button(display: &mut Display) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(2)
        .fill_color(Gray4::WHITE)
        .build();

    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(10, 10), Size::new(100, 40)),
        Size::new(8, 8),
    )
    .into_styled(border)
    .draw(display)
    .ok();

    Text::with_alignment("< Back", Point::new(60, 36), bold, Alignment::Center)
        .draw(display)
        .ok();
}

pub(crate) fn back_button_hit(sx: i32, sy: i32) -> bool {
    sx < 110 && sy < 50
}
