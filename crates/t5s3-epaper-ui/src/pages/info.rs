use alloc::{format, string::String};
use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_hal::time::Instant;
use t5s3_epaper_core::{Clock, Display};

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::draw_back_button,
};

const INFO_TOP: i32 = 210;
const INFO_H: u32 = 320;

// shown on the info page.
const MODEL_NAME: &str = "LilyGo T5 S3 Paper Pro";
// loop ticks between info-page refreshes (~50ms per tick), so uptime/temp tick.
pub(crate) const INFO_REFRESH_TICKS: u16 = 40;

// format a duration in seconds as a compact "1h 23m" / "5m 12s" / "8s".
fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

// read the live system stats for the info page: battery voltage, panel
// temperature, uptime since boot, and time since the last clock sync (None if
// it has not synced this power cycle).
pub(crate) fn read_info(display: &mut Display, clock: &mut Clock) -> (f32, i8, u64, Option<u64>) {
    let voltage = display.battery_voltage().unwrap_or(0.0);
    let temp = display.panel_temperature().unwrap_or(0);
    let uptime = Instant::now().duration_since_epoch().as_micros() / 1_000_000;
    let now_secs = clock.now_us() / 1_000_000;
    let last_sync = unsafe { crate::LAST_SYNC_UNIX };
    let since_sync = if last_sync > 0 && now_secs >= last_sync {
        Some(now_secs - last_sync)
    } else {
        None
    };
    (voltage, temp, uptime, since_sync)
}

// the label/value rows, drawn over a white fill so the periodic refresh cleanly
// replaces the previous values.
pub(crate) fn draw_info_values(
    display: &mut Display,
    voltage: f32,
    temp: i8,
    uptime: u64,
    since_sync: Option<u64>,
) {
    Rectangle::new(Point::new(40, INFO_TOP), Size::new(460, INFO_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    // both black: this area is repainted with the 1-bit DU waveform on the
    // periodic refresh, which can't render a grey tone. the bold value font is
    // what sets values apart from labels.
    let label = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let value = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let v_int = voltage as u32;
    let v_frac = ((voltage - v_int as f32) * 100.0) as u32;

    let mut volt = FmtBuf::<16>::new();
    write!(volt, "{v_int}.{v_frac:02} V").ok();
    let mut tmp = FmtBuf::<16>::new();
    write!(tmp, "{temp} C").ok();
    let uptime = format_duration(uptime);
    let synced = match since_sync {
        Some(s) => {
            let mut b = FmtBuf::<24>::new();
            write!(b, "{} ago", format_duration(s)).ok();
            b
        }
        None => {
            let mut b = FmtBuf::<24>::new();
            write!(b, "never").ok();
            b
        }
    };

    let rows = [
        ("Model", MODEL_NAME),
        ("Battery", volt.as_str()),
        ("Temp", tmp.as_str()),
        ("Uptime", uptime.as_str()),
        ("Synced", synced.as_str()),
    ];
    let mut y = INFO_TOP + 36;
    for (name, val) in rows {
        Text::new(name, Point::new(70, y), label).draw(display).ok();
        Text::new(val, Point::new(250, y), value).draw(display).ok();
        y += 58;
    }
}

pub(crate) fn draw_info_screen(
    display: &mut Display,
    voltage: f32,
    temp: i8,
    uptime: u64,
    since_sync: Option<u64>,
) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);
    Text::with_alignment(
        "Info",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_info_values(display, voltage, temp, uptime, since_sync);
}

pub(crate) fn info_values_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(40, INFO_TOP, 460, INFO_H as i32)
}
