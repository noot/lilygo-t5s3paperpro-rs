#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use alloc::{format, string::String, vec::Vec};
use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
    IpEndpoint,
    StackResources,
};
use embassy_time::{with_timeout, Duration, Timer};
use embedded_graphics::{
    image::Image,
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{Circle, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    interrupt::software::SoftwareInterruptControl,
    rng::Rng,
    time::Instant,
    timer::timg::TimerGroup,
};
use esp_radio::wifi::{sta::StationConfig, Config, ControllerConfig, Interface, WifiController};
use t5s3_epaper_core::{
    display::DisplayRotation,
    lora::{Config as LoraConfig, Lora},
    pin_config,
    sdcard_pin_config,
    Clock,
    Display,
    DrawMode,
    FrontLight,
    SdCard,
};
#[cfg(feature = "gps")]
use t5s3_epaper_core::{gps::Gps, gps_pin_config};
use tinybmp::Bmp;

esp_bootloader_esp_idf::esp_app_desc!();

// ── wifi / ntp config (from .env at build time; see the justfile) ────
const SSID: &str = match option_env!("SSID") {
    Some(s) => s,
    None => "changeme",
};
const PASSWORD: &str = match option_env!("PASSWORD") {
    Some(s) => s,
    None => "changeme",
};
const NTP_SERVER: &str = "pool.ntp.org";
// seconds between the NTP epoch (1900-01-01) and the unix epoch (1970-01-01).
const NTP_UNIX_DELTA: u64 = 2_208_988_800;
// re-sync the clock over wifi this often. wifi is powered down between syncs so
// it doesn't interfere with gps reception; the RTC only drifts seconds per day.
const RESYNC_INTERVAL_SECS: u64 = 4 * 3600;

// ── layout constants ────────────────────────────────────────────────
const SCREEN_W: i32 = 540;
const STATUS_H: i32 = 55;

const COLS: usize = 2;
const ICON_W: u16 = 220;
const ICON_H: u16 = 240;
const GAP_X: u16 = 20;
const GAP_Y: u16 = 30;
const GRID_TOP_Y: i32 = STATUS_H + 40;

const BRIGHTNESS_STEP: u8 = 3;
const FL_BAR_X: i32 = 70;
const FL_BAR_Y: i32 = 380;
const FL_BAR_W: u32 = 400;
const FL_BAR_H: u32 = 50;
const FL_BTN_Y: i32 = 500;
const FL_BTN_W: u32 = 150;
const FL_BTN_H: u32 = 70;
const FL_MINUS_X: i32 = 80;
const FL_PLUS_X: i32 = 310;

const SLEEP_BTN_X: i32 = 160;
const SLEEP_BTN_Y: i32 = 420;
const SLEEP_BTN_W: u32 = 220;
const SLEEP_BTN_H: u32 = 90;

// folder of 540x960 grayscale .bmp wallpapers on the SD card; one is picked at
// random as the sleep screensaver. this must be a FAT 8.3 name (<=8 chars), as
// must the .bmp files inside it, so they can be opened by name.
const WALLPAPER_DIR: &str = "/WALLS";

// loop ticks between GPS readout refreshes (~50ms per tick)
#[cfg(feature = "gps")]
const GPS_REFRESH_TICKS: u16 = 30;

// ── formatting helper ───────────────────────────────────────────────
struct FmtBuf<const N: usize> {
    buf: [u8; N],
    pos: usize,
}

impl<const N: usize> FmtBuf<N> {
    fn new() -> Self {
        Self {
            buf: [0; N],
            pos: 0,
        }
    }

    #[cfg_attr(not(feature = "gps"), allow(dead_code))]
    fn reset(&mut self) {
        self.pos = 0;
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.pos]).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let len = bytes.len().min(N - self.pos);
        self.buf[self.pos..self.pos + len].copy_from_slice(&bytes[..len]);
        self.pos += len;
        Ok(())
    }
}

// ── coordinate helpers ──────────────────────────────────────────────
// Rotate270: screen(x,y) → native(y, 539-x)
// Inverse for touch: screen_x = 539 - native_y, screen_y = native_x
fn touch_to_screen(tx: u16, ty: u16) -> (i32, i32) {
    (539 - ty as i32, tx as i32)
}

fn screen_to_native_rect(
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
) -> t5s3_epaper_core::display::Rectangle {
    t5s3_epaper_core::display::Rectangle {
        x: sy as u16,
        y: (Display::HEIGHT as i32 - sx - sw) as u16,
        width: sh as u16,
        height: sw as u16,
    }
}

// ── screens ─────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Gps,
    Lora,
    Frontlight,
    Sleep,
}

impl Screen {
    fn to_index(self) -> u8 {
        match self {
            Screen::Home => 0,
            Screen::Gps => 1,
            Screen::Lora => 2,
            Screen::Frontlight => 3,
            Screen::Sleep => 4,
        }
    }

    // map a stored index back to a screen. the Sleep screen and any
    // unexpected value fall back to Home, so waking never lands on the sleep
    // menu or on garbage left by an interrupted persistent write.
    fn from_index(value: u8) -> Self {
        match value {
            1 => Screen::Gps,
            2 => Screen::Lora,
            3 => Screen::Frontlight,
            _ => Screen::Home,
        }
    }
}

// last visited screen, stored in RTC fast memory so it survives the reset that
// deep sleep performs. zeroed (Home) on first boot, then retained across sleep.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut LAST_SCREEN: u8 = 0;

struct Icon {
    label: &'static str,
    glyph: &'static str,
    screen: Screen,
}

const ICONS: [Icon; 4] = [
    Icon {
        label: "GPS",
        glyph: "GPS",
        screen: Screen::Gps,
    },
    Icon {
        label: "LoRa",
        glyph: "RF",
        screen: Screen::Lora,
    },
    Icon {
        label: "Light",
        glyph: "LIT",
        screen: Screen::Frontlight,
    },
    Icon {
        label: "Sleep",
        glyph: "ZZZ",
        screen: Screen::Sleep,
    },
];

fn grid_origin_x() -> i32 {
    (SCREEN_W - (COLS as i32 * ICON_W as i32 + (COLS as i32 - 1) * GAP_X as i32)) / 2
}

fn icon_rect(idx: usize) -> (i32, i32) {
    let col = idx % COLS;
    let row = idx / COLS;
    (
        grid_origin_x() + (col as i32) * (ICON_W as i32 + GAP_X as i32),
        GRID_TOP_Y + (row as i32) * (ICON_H as i32 + GAP_Y as i32),
    )
}

fn hit_test(sx: i32, sy: i32) -> Option<usize> {
    for i in 0..ICONS.len() {
        let (x, y) = icon_rect(i);
        if sx >= x && sx < x + ICON_W as i32 && sy >= y && sy < y + ICON_H as i32 {
            return Some(i);
        }
    }
    None
}

fn back_button_hit(sx: i32, sy: i32) -> bool {
    sx < 110 && sy < 50
}

fn minus_hit(sx: i32, sy: i32) -> bool {
    (FL_MINUS_X..FL_MINUS_X + FL_BTN_W as i32).contains(&sx)
        && (FL_BTN_Y..FL_BTN_Y + FL_BTN_H as i32).contains(&sy)
}

fn plus_hit(sx: i32, sy: i32) -> bool {
    (FL_PLUS_X..FL_PLUS_X + FL_BTN_W as i32).contains(&sx)
        && (FL_BTN_Y..FL_BTN_Y + FL_BTN_H as i32).contains(&sy)
}

fn sleep_now_hit(sx: i32, sy: i32) -> bool {
    (SLEEP_BTN_X..SLEEP_BTN_X + SLEEP_BTN_W as i32).contains(&sx)
        && (SLEEP_BTN_Y..SLEEP_BTN_Y + SLEEP_BTN_H as i32).contains(&sy)
}

// ── drawing: status bar ─────────────────────────────────────────────
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

fn draw_status_bar(
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
fn draw_statusbar_time(display: &mut Display, time: Option<(u32, u32)>) {
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

fn statusbar_time_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(215, 18, 110, 30)
}

// read the RTC and return (hours, minutes) of local time, or None if it has
// not been set to a real wall-clock time yet (i.e. no successful NTP sync).
fn status_time(clock: &mut Clock) -> Option<(u32, u32)> {
    let secs = clock.now_us() / 1_000_000;
    // ~year 2020; below this the RTC is just counting up from boot, unsynced.
    if secs > 1_600_000_000 {
        let sod = (secs % 86_400) as u32;
        Some((sod / 3600, (sod % 3600) / 60))
    } else {
        None
    }
}

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

// read the RTC and return (day-of-week, year, month, day) of local time, or
// None if it has not been synced to a real wall-clock time yet.
fn status_date(clock: &mut Clock) -> Option<(usize, i64, u32, u32)> {
    let secs = clock.now_us() / 1_000_000;
    if secs > 1_600_000_000 {
        let days = (secs / 86_400) as i64;
        let (year, month, day) = civil_from_days(days);
        let dow = ((days + 4) % 7) as usize; // 1970-01-01 was a Thursday; 0 = Sunday
        Some((dow, year, month, day))
    } else {
        None
    }
}

// gregorian (year, month, day) from days since the unix epoch.
// see http://howardhinnant.github.io/date_algorithms.html#civil_from_days
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (year + i64::from(month <= 2), month, day)
}

// ── drawing: home ───────────────────────────────────────────────────
fn draw_home(display: &mut Display, date: Option<(usize, i64, u32, u32)>) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));

    Text::with_alignment("T5 S3 Pro", Point::new(15, 37), bold, Alignment::Left)
        .draw(display)
        .ok();

    // date header above the icon grid, once the clock has been synced
    if let Some((dow, year, month, day)) = date {
        let mut buf = FmtBuf::<32>::new();
        write!(
            buf,
            "{}, {} {}, {}",
            DAY_NAMES[dow],
            MONTH_NAMES[(month - 1) as usize],
            day,
            year
        )
        .ok();
        Text::with_alignment(
            buf.as_str(),
            Point::new(SCREEN_W / 2, 82),
            bold,
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }

    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(2)
        .fill_color(Gray4::WHITE)
        .build();

    for (i, icon) in ICONS.iter().enumerate() {
        let (x, y) = icon_rect(i);

        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(x, y), Size::new(ICON_W as u32, ICON_H as u32)),
            Size::new(16, 16),
        )
        .into_styled(border)
        .draw(display)
        .ok();

        Text::with_alignment(
            icon.glyph,
            Point::new(x + ICON_W as i32 / 2, y + ICON_H as i32 / 2 - 15),
            bold,
            Alignment::Center,
        )
        .draw(display)
        .ok();

        Text::with_alignment(
            icon.label,
            Point::new(x + ICON_W as i32 / 2, y + ICON_H as i32 / 2 + 20),
            small,
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }
}

// ── drawing: back button ────────────────────────────────────────────
fn draw_back_button(display: &mut Display) {
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

// ── drawing: frontlight ─────────────────────────────────────────────
fn draw_frontlight_screen(display: &mut Display, brightness: u8) {
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

fn draw_brightness_area(display: &mut Display, brightness: u8) {
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

fn brightness_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(40, 290, 460, 160)
}

// ── drawing: GPS ────────────────────────────────────────────────────
// a position fix worth keeping on screen after the live fix drops, so a brief
// signal loss shows the last known position instead of blanking to "--".
#[cfg(feature = "gps")]
#[derive(Clone, Copy)]
struct GpsFix {
    lat: f64,
    lng: f64,
    alt: f32,
    speed: f32,
    hdop: f32,
    vdop: f32,
    sats: u32,
}

// snapshot the current fix, or None if the receiver has no position right now.
#[cfg(feature = "gps")]
fn current_fix(gps: &Gps<'_>) -> Option<GpsFix> {
    let (lat, lng) = gps.location()?;
    Some(GpsFix {
        lat,
        lng,
        alt: gps.altitude().unwrap_or(0.0),
        speed: gps.speed_over_ground().unwrap_or(0.0),
        hdop: gps.hdop().unwrap_or(0.0),
        vdop: gps.vdop().unwrap_or(0.0),
        sats: gps.fix_satellites().unwrap_or(0),
    })
}

#[cfg(feature = "gps")]
fn draw_gps_data(display: &mut Display, gps: &Gps<'_>, last_fix: Option<GpsFix>) {
    let small = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    let x = 60;
    let line_h = 28;
    let mut y = 200;

    // fill the data area with white before drawing text, matching the
    // u8g2 WithBackground pattern used in the working gps example.
    // this ensures previous text is explicitly cleared in the framebuffer
    // so the DU waveform sees clean transitions.
    Rectangle::new(Point::new(30, 170), Size::new(480, 300))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    let module_name = match gps.module() {
        t5s3_epaper_core::gps::Module::L76K => "L76K",
        t5s3_epaper_core::gps::Module::MiaM10Q => "MIA-M10Q",
    };

    let in_view = gps.satellites_in_view();
    let current = current_fix(gps);
    // when the live fix drops, keep showing the last known position rather than
    // blanking it. the DU partial-refresh waveform is 1-bit, so we flag the
    // stale data with text ("re-acquiring" / "(last)") rather than a grey tone
    // it can't render.
    let stale = current.is_none() && last_fix.is_some();
    let shown = current.or(last_fix);
    let mark = if stale { "  (last)" } else { "" };

    let mut buf = FmtBuf::<48>::new();

    write!(buf, "Module: {}", module_name).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h;

    buf.reset();
    match current {
        Some(f) => {
            let fix_str = match gps.fix_type() {
                Some(nmea::sentences::FixType::Gps) => "GPS",
                Some(nmea::sentences::FixType::DGps) => "DGPS",
                Some(nmea::sentences::FixType::Rtk) => "RTK",
                Some(nmea::sentences::FixType::FloatRtk) => "float RTK",
                Some(_) => "other",
                None => "fix",
            };
            write!(
                buf,
                "Fix:    {} ({} used, {} in view)",
                fix_str, f.sats, in_view
            )
            .ok();
        }
        None if stale => {
            write!(buf, "Fix:    re-acquiring ({in_view} in view)").ok();
        }
        None => {
            write!(buf, "Fix:    no fix ({in_view} in view)").ok();
        }
    }
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h + 10;

    match shown {
        Some(f) => {
            buf.reset();
            write!(buf, "Lat:    {:.6}{}", f.lat, mark).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Lon:    {:.6}{}", f.lng, mark).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Alt:    {:.1} m", f.alt).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Speed:  {:.1} kn", f.speed).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "HDOP:   {:.1}   VDOP: {:.1}", f.hdop, f.vdop).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
        }
        None => {
            Text::new("Lat:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Lon:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Alt:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Speed:  --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("HDOP:   --   VDOP: --", Point::new(x, y), small)
                .draw(display)
                .ok();
        }
    }
}

#[cfg(feature = "gps")]
fn gps_data_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(30, 170, 480, 300)
}

// ── drawing: LoRa ───────────────────────────────────────────────────
// ── lora keyboard ───────────────────────────────────────────────────
// on-screen touch keyboard for composing a message to broadcast. the keys are
// static, so the whole board is painted once on entry (full refresh); only the
// message box / status line / keyboard repaint as you type (partial refresh).
const KB_KEY_W: i32 = 50;
const KB_KEY_H: i32 = 78;
const KB_GAP: i32 = 4;
const KB_GAP_Y: i32 = 8;
// keyboard sits at the bottom of the screen (rows end ~24px from the edge).
const KB_TOP: i32 = 600;
const KB_X: i32 = 2;
const KB_FULL_W: i32 = 536;
const KB_TOGGLE_W: i32 = 90;
const KB_SEND_W: i32 = 110;

const MSG_X: i32 = 30;
const MSG_Y: i32 = 150;
const MSG_W: u32 = 480;
const MSG_H: u32 = 170;
const LORA_STATUS_Y: i32 = 338;
const MSG_MAX: usize = 200;

// sent + received message logs, stacked between the status line and keyboard.
const SENT_Y: i32 = 368;
const RECV_Y: i32 = 476;
const LIST_H: u32 = 102;
const LIST_MAX: usize = 3;

const KB_LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const KB_SYMBOLS: [&str; 3] = ["1234567890", "@#$&-+()/", "*\"':;!?,"];

#[derive(Clone, Copy, PartialEq)]
enum Key {
    Char(char),
    Shift,
    Symbols,
    Backspace,
    Space,
    Send,
}

struct KeyBox {
    key: Key,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn kb_row_y(row: i32) -> i32 {
    KB_TOP + row * (KB_KEY_H + KB_GAP_Y)
}

// build the key boxes for the current layer. the letters layer puts a shift key
// at the left of the third row; the symbols layer fills that slot with a symbol
// instead, so both layers share the same nine-slot geometry.
fn keyboard(symbols: bool, shift: bool) -> Vec<KeyBox> {
    let rows = if symbols { KB_SYMBOLS } else { KB_LETTERS };
    let mut keys = Vec::new();

    for (row, &row_keys) in rows.iter().enumerate().take(2) {
        let n = row_keys.chars().count() as i32;
        let ox = (SCREEN_W - (n * KB_KEY_W + (n - 1) * KB_GAP)) / 2;
        let y = kb_row_y(row as i32);
        for (i, c) in row_keys.chars().enumerate() {
            let ch = if !symbols && shift {
                c.to_ascii_uppercase()
            } else {
                c
            };
            keys.push(KeyBox {
                key: Key::Char(ch),
                x: ox + i as i32 * (KB_KEY_W + KB_GAP),
                y,
                w: KB_KEY_W,
                h: KB_KEY_H,
            });
        }
    }

    // third row: nine slots. letters -> [shift][7 letters][del]; symbols ->
    // [8 symbols][del].
    let y = kb_row_y(2);
    let ox = (SCREEN_W - (9 * KB_KEY_W + 8 * KB_GAP)) / 2;
    let mut x = ox;
    if !symbols {
        keys.push(KeyBox {
            key: Key::Shift,
            x,
            y,
            w: KB_KEY_W,
            h: KB_KEY_H,
        });
        x += KB_KEY_W + KB_GAP;
    }
    for c in rows[2].chars() {
        let ch = if !symbols && shift {
            c.to_ascii_uppercase()
        } else {
            c
        };
        keys.push(KeyBox {
            key: Key::Char(ch),
            x,
            y,
            w: KB_KEY_W,
            h: KB_KEY_H,
        });
        x += KB_KEY_W + KB_GAP;
    }
    keys.push(KeyBox {
        key: Key::Backspace,
        x,
        y,
        w: KB_KEY_W,
        h: KB_KEY_H,
    });

    // bottom row: layer toggle, wide space bar, send.
    let y = kb_row_y(3);
    keys.push(KeyBox {
        key: Key::Symbols,
        x: KB_X,
        y,
        w: KB_TOGGLE_W,
        h: KB_KEY_H,
    });
    let send_x = KB_X + KB_FULL_W - KB_SEND_W;
    let space_x = KB_X + KB_TOGGLE_W + KB_GAP;
    keys.push(KeyBox {
        key: Key::Space,
        x: space_x,
        y,
        w: send_x - KB_GAP - space_x,
        h: KB_KEY_H,
    });
    keys.push(KeyBox {
        key: Key::Send,
        x: send_x,
        y,
        w: KB_SEND_W,
        h: KB_KEY_H,
    });

    keys
}

fn kb_hit(sx: i32, sy: i32, symbols: bool, shift: bool) -> Option<Key> {
    keyboard(symbols, shift)
        .into_iter()
        .find_map(|k| (sx >= k.x && sx < k.x + k.w && sy >= k.y && sy < k.y + k.h).then_some(k.key))
}

fn key_label(key: Key, symbols: bool, buf: &mut [u8; 4]) -> &str {
    match key {
        Key::Char(c) => c.encode_utf8(buf),
        Key::Shift => "shift",
        Key::Symbols => {
            if symbols {
                "abc"
            } else {
                "123"
            }
        }
        Key::Backspace => "del",
        Key::Space => "space",
        Key::Send => "SEND",
    }
}

fn draw_keyboard(display: &mut Display, symbols: bool, shift: bool) {
    for k in keyboard(symbols, shift) {
        // draw the active shift key inverted so its state is visible.
        let active = matches!(k.key, Key::Shift) && shift;
        let (fill, fg) = if active {
            (Gray4::BLACK, Gray4::WHITE)
        } else {
            (Gray4::WHITE, Gray4::BLACK)
        };
        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(k.x, k.y), Size::new(k.w as u32, k.h as u32)),
            Size::new(6, 6),
        )
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(1)
                .fill_color(fill)
                .build(),
        )
        .draw(display)
        .ok();

        let mut buf = [0u8; 4];
        let label = key_label(k.key, symbols, &mut buf);
        Text::with_alignment(
            label,
            Point::new(k.x + k.w / 2, k.y + k.h / 2 + 5),
            MonoTextStyle::new(&FONT_9X15, fg),
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }
}

fn draw_message(display: &mut Display, message: &str) {
    Rectangle::new(Point::new(MSG_X, MSG_Y), Size::new(MSG_W, MSG_H))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .fill_color(Gray4::WHITE)
                .build(),
        )
        .draw(display)
        .ok();

    let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let x = MSG_X + 12;
    let mut y = MSG_Y + 28;
    if message.is_empty() {
        Text::new("type a message...", Point::new(x, y), font)
            .draw(display)
            .ok();
        return;
    }

    // wrap on a character count; the font is fixed width and the text is ascii.
    let per_line = ((MSG_W as i32 - 24) / 9) as usize;
    let bytes = message.len();
    let mut start = 0;
    while start < bytes {
        let end = (start + per_line).min(bytes);
        Text::new(&message[start..end], Point::new(x, y), font)
            .draw(display)
            .ok();
        y += 20;
        start = end;
    }
}

fn draw_lora_status(display: &mut Display, status: &str) {
    Rectangle::new(Point::new(MSG_X, LORA_STATUS_Y), Size::new(MSG_W, 26))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Text::with_alignment(
        status,
        Point::new(SCREEN_W / 2, LORA_STATUS_Y + 18),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// a titled message log (newest first), each entry truncated to one line. used
// for both the sent and received lists; `y` is the top of its section.
fn draw_list(display: &mut Display, y: i32, header: &str, items: &[String]) {
    Rectangle::new(Point::new(MSG_X, y), Size::new(MSG_W, LIST_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Text::new(
        header,
        Point::new(MSG_X + 4, y + 16),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
    )
    .draw(display)
    .ok();

    let font = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    let mut ey = y + 40;
    for msg in items.iter().rev() {
        let line = if msg.len() > 66 {
            format!("> {}...", &msg[..66])
        } else {
            format!("> {msg}")
        };
        Text::new(&line, Point::new(MSG_X + 10, ey), font)
            .draw(display)
            .ok();
        ey += 20;
    }
}

fn draw_lora_screen(
    display: &mut Display,
    message: &str,
    status: &str,
    sent: &[String],
    received: &[String],
    symbols: bool,
    shift: bool,
) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);
    Text::with_alignment(
        "LoRa  915 MHz",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_message(display, message);
    draw_lora_status(display, status);
    draw_list(display, SENT_Y, "sent", sent);
    draw_list(display, RECV_Y, "received", received);
    draw_keyboard(display, symbols, shift);
}

fn message_box_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, MSG_Y, MSG_W as i32, MSG_H as i32)
}

fn lora_status_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, LORA_STATUS_Y, MSG_W as i32, 26)
}

fn sent_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, SENT_Y, MSG_W as i32, LIST_H as i32)
}

fn received_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, RECV_Y, MSG_W as i32, LIST_H as i32)
}

fn keyboard_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(0, KB_TOP - 6, SCREEN_W, 4 * (KB_KEY_H + KB_GAP_Y) + 12)
}

// build the lora radio used by the send/receive page. it shares SPI2 with the
// SD card, which is only touched at sleep (after the main loop), so the bus is
// free while this page is open. steal the bus + radio pins (mirroring the wifi
// re-sync); dropping the returned radio releases them. the 3.3v rail powered up
// at boot, so no settle delay is needed.
fn make_radio() -> Result<Lora<'static>, t5s3_epaper_core::lora::Error> {
    let pins = t5s3_epaper_core::lora::PinConfig {
        sclk: unsafe { esp_hal::peripherals::GPIO14::steal() },
        mosi: unsafe { esp_hal::peripherals::GPIO13::steal() },
        miso: unsafe { esp_hal::peripherals::GPIO21::steal() },
        cs: unsafe { esp_hal::peripherals::GPIO46::steal() },
        rst: unsafe { esp_hal::peripherals::GPIO1::steal() },
        busy: unsafe { esp_hal::peripherals::GPIO47::steal() },
        dio1: unsafe { esp_hal::peripherals::GPIO10::steal() },
    };
    let spi = unsafe { esp_hal::peripherals::SPI2::steal() };
    // match the t3-s3 receiver, whose Config::default() uses SF7. every other
    // parameter (915 MHz, BW125, CR4/5, preamble 8, private sync word) already
    // agrees; only the spreading factor differed.
    let config = LoraConfig {
        spreading_factor: t5s3_epaper_core::lora::SpreadingFactor::Sf7,
        ..LoraConfig::default()
    };
    Lora::new(pins, spi, &config)
}

// ── drawing: sleep ──────────────────────────────────────────────────
fn draw_sleep_screen(display: &mut Display) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    draw_back_button(display);

    Text::with_alignment(
        "Deep Sleep",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    Text::with_alignment(
        "Sleep to save power.",
        Point::new(SCREEN_W / 2, 300),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    Text::with_alignment(
        "Wake with the BOOT button.",
        Point::new(SCREEN_W / 2, 330),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    let btn_border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(3)
        .fill_color(Gray4::WHITE)
        .build();
    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(SLEEP_BTN_X, SLEEP_BTN_Y),
            Size::new(SLEEP_BTN_W, SLEEP_BTN_H),
        ),
        Size::new(12, 12),
    )
    .into_styled(btn_border)
    .draw(display)
    .ok();
    Text::with_alignment(
        "Sleep Now",
        Point::new(
            SLEEP_BTN_X + SLEEP_BTN_W as i32 / 2,
            SLEEP_BTN_Y + SLEEP_BTN_H as i32 / 2 + 6,
        ),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// ── drawing: screensaver (shown while in deep sleep) ─────────────────
fn draw_screensaver(display: &mut Display, pct: u16) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);

    let cx = SCREEN_W / 2;
    let cy = 350;
    let r = 90;

    // crescent moon: a black disc with an offset white disc carving it
    Circle::new(Point::new(cx - r, cy - r), (r * 2) as u32)
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
    Circle::new(Point::new(cx - r + 40, cy - r - 30), (r * 2) as u32)
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    // a scattering of stars
    for (star_x, star_y, star_r) in [
        (150, 230, 5u32),
        (415, 300, 7),
        (385, 470, 4),
        (165, 500, 5),
    ] {
        Circle::new(
            Point::new(star_x - star_r as i32, star_y - star_r as i32),
            star_r * 2,
        )
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
    }

    Text::with_alignment("Sleeping", Point::new(cx, 580), bold, Alignment::Center)
        .draw(display)
        .ok();
    Text::with_alignment(
        "Press the BOOT button to wake",
        Point::new(cx, 630),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    let mut buf = FmtBuf::<24>::new();
    write!(buf, "Battery {}%", pct.min(100)).ok();
    Text::with_alignment(buf.as_str(), Point::new(cx, 700), small, Alignment::Center)
        .draw(display)
        .ok();
}

// load the wallpaper bitmap from the SD card and draw it full-screen. returns
// false if the card, file, or bitmap is missing or unreadable so the caller can
// fall back to the drawn screensaver.
fn show_wallpaper<'d>(
    display: &mut Display,
    spi: esp_hal::peripherals::SPI2<'d>,
    pins: t5s3_epaper_core::sdcard::PinConfig<'d>,
    lora_cs: esp_hal::peripherals::GPIO46<'d>,
) -> bool {
    // the SD card shares the SPI bus (sclk/mosi/miso) with the LoRa SX1262
    // radio. drive the radio's chip-select high so it releases MISO; otherwise
    // it corrupts SD init and the card comes back as CardNotFound. held for the
    // duration of the SD access below.
    let _lora_cs = Output::new(lora_cs, Level::High, OutputConfig::default());

    let sdcard = match SdCard::new(pins, spi) {
        Ok(sdcard) => sdcard,
        Err(e) => {
            esp_println::println!("wallpaper: sd init failed: {e:?}");
            return false;
        }
    };
    let entries = match sdcard.list_dir(WALLPAPER_DIR) {
        Ok(entries) => entries,
        Err(e) => {
            esp_println::println!("wallpaper: list_dir {WALLPAPER_DIR} failed: {e:?}");
            return false;
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        if !entry.is_directory && is_bmp(&entry.name) {
            paths.push(entry.path);
        }
    }
    if paths.is_empty() {
        esp_println::println!("wallpaper: no .bmp files in {WALLPAPER_DIR}");
        return false;
    }

    // mix the hardware RNG with a microsecond timer reading. with the radios
    // off the RNG alone is biased (it kept picking the same file); the instant
    // at which sleep is triggered adds real entropy. then fall through the rest
    // so an unreadable file (e.g. a long name the FAT layer can't open by its
    // 8.3 short name) is skipped rather than aborting.
    let seed = Rng::new().random() ^ Instant::now().duration_since_epoch().as_micros() as u32;
    let start = seed as usize % paths.len();
    for offset in 0..paths.len() {
        let path = &paths[(start + offset) % paths.len()];
        let bytes = match sdcard.read_file(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                esp_println::println!("wallpaper: read {path} failed: {e:?}");
                continue;
            }
        };
        let Ok(bmp) = Bmp::<Gray4>::from_slice(&bytes) else {
            esp_println::println!("wallpaper: parse {path} failed");
            continue;
        };
        if Image::new(&bmp, Point::zero()).draw(display).is_ok() {
            esp_println::println!("wallpaper: drew {path}");
            return true;
        }
    }
    false
}

fn is_bmp(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("bmp"))
}

// ── main ────────────────────────────────────────────────────────────
#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    // internal-RAM heaps for the wifi stack (its DMA buffers can't live in
    // PSRAM), plus a PSRAM heap for the display's large framebuffers. esp-hal
    // 1.1 dropped ESP_HAL_CONFIG_PSRAM_MODE, so request octal mode explicitly.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 64 * 1024);
    esp_alloc::psram_allocator!(
        peripherals.PSRAM,
        esp_hal::psram,
        esp_hal::psram::PsramConfig {
            mode: esp_hal::psram::PsramMode::OctalSpi,
            ..Default::default()
        }
    );

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // a cold boot needs a fresh time sync; a wake from deep sleep keeps the RTC.
    let woke = t5s3_epaper_core::power::wake_status().woke_from_deep_sleep();
    let mut clock = Clock::new(peripherals.LPWR);

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.I2C0,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    display.set_rotation(DisplayRotation::Rotate270);

    let mut light =
        FrontLight::new(peripherals.LEDC, peripherals.GPIO11).expect("to initialize front light");

    let delay = Delay::new();
    display.power_on().expect("to power on");
    delay.delay_millis(10);
    display.clear().expect("to clear");

    // detect the GPS module BEFORE bringing up wifi. the L76K probe is a plain
    // UART exchange; doing it first keeps it in the same quiet, no-radio state
    // it was validated in (running it after the ~20s wifi sync was failing to
    // detect). the panel power-on + clear above gives the module time to boot.
    #[cfg(feature = "gps")]
    let mut gps: Option<Gps<'_>> = {
        let mut detect_delay = Delay::new();
        match Gps::detect(
            peripherals.UART1,
            gps_pin_config!(peripherals),
            &mut detect_delay,
        ) {
            Ok(g) => {
                esp_println::println!("detected GPS module: {:?}", g.module());
                Some(g)
            }
            Err(e) => {
                esp_println::println!("gps detection failed: {}", e);
                None
            }
        }
    };

    #[cfg(feature = "gps")]
    if let Some(ref mut g) = gps {
        for _ in 0..50 {
            g.update().ok();
            delay.delay_millis(20);
        }
    }

    // timezone offset (hours from UTC) from the TZ_OFFSET_HOURS build env (see
    // .env); defaults to pacific daylight time. used for the initial sync and
    // each periodic re-sync.
    let offset_hours: i64 = option_env!("TZ_OFFSET_HOURS")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(-7);

    // on a cold boot, sync the clock over wifi (best effort, with a timeout so
    // it still boots when offline), then the radio powers down. on wake the RTC
    // already holds the time, so we skip wifi for a fast resume.
    if !woke {
        Text::with_alignment(
            "syncing clock over wifi...",
            Point::new(SCREEN_W / 2, 400),
            MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
            Alignment::Center,
        )
        .draw(&mut display)
        .ok();
        display.flush(DrawMode::BlackOnWhite).expect("to flush");

        match sync_time(peripherals.WIFI).await {
            Some(unix) => set_local_time(&mut clock, unix, offset_hours),
            None => esp_println::println!("clock: wifi/ntp sync failed, time unavailable"),
        }
    }

    // restore the screen we slept on (only after a real deep-sleep wake; any
    // other reset starts at Home). reading the RTC-backed static is sound as
    // the UI is single-threaded.
    let mut current_screen = if woke {
        Screen::from_index(unsafe { LAST_SCREEN })
    } else {
        Screen::Home
    };
    let mut needs_redraw = true;
    let mut brightness: u8 = 0;
    // whether a finger is currently down, so each tap is handled once on press.
    let mut touch_active = false;
    let mut last_status_minute: u32 = 60;
    // time of the last clock sync, used to schedule periodic re-syncs.
    let mut last_resync_secs = clock.now_us() / 1_000_000;

    // lora send/receive state: the message being typed, a status line, the last
    // few sent and received messages, and the keyboard's symbol/shift toggles.
    // `radio` is live only while the lora screen is open (see the loop).
    let mut lora_message = String::new();
    let mut lora_status = String::from("type a message, then SEND");
    let mut lora_sent: Vec<String> = Vec::new();
    let mut lora_recv: Vec<String> = Vec::new();
    let mut radio: Option<Lora<'static>> = None;
    let mut radio_tried = false;
    let mut kb_symbols = false;
    let mut kb_shift = false;

    #[cfg(feature = "gps")]
    let mut gps_refresh: u16 = 0;
    // last good position, kept so a dropped fix shows the previous coordinates
    // (marked stale) instead of blanking out.
    #[cfg(feature = "gps")]
    let mut last_fix: Option<GpsFix> = None;

    loop {
        if needs_redraw {
            let voltage = display.battery_voltage().unwrap_or(0.0);
            let pct = display.battery_percentage().unwrap_or(0);
            let temp = display.panel_temperature().unwrap_or(0);
            let now = status_time(&mut clock);

            display.clear().ok();
            draw_status_bar(&mut display, voltage, pct, temp, now);
            match current_screen {
                Screen::Home => draw_home(&mut display, status_date(&mut clock)),
                Screen::Gps => {
                    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
                    draw_back_button(&mut display);
                    Text::with_alignment(
                        "GPS",
                        Point::new(SCREEN_W / 2, 120),
                        bold,
                        Alignment::Center,
                    )
                    .draw(&mut display)
                    .ok();

                    #[cfg(feature = "gps")]
                    match &gps {
                        Some(g) => draw_gps_data(&mut display, g, last_fix),
                        None => {
                            let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
                            Text::with_alignment(
                                "no module detected",
                                Point::new(SCREEN_W / 2, 400),
                                small,
                                Alignment::Center,
                            )
                            .draw(&mut display)
                            .ok();
                        }
                    }

                    #[cfg(not(feature = "gps"))]
                    {
                        let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
                        Text::with_alignment(
                            "compile with --features gps",
                            Point::new(SCREEN_W / 2, 400),
                            small,
                            Alignment::Center,
                        )
                        .draw(&mut display)
                        .ok();
                    }
                }
                Screen::Lora => draw_lora_screen(
                    &mut display,
                    &lora_message,
                    &lora_status,
                    &lora_sent,
                    &lora_recv,
                    kb_symbols,
                    kb_shift,
                ),
                Screen::Frontlight => draw_frontlight_screen(&mut display, brightness),
                Screen::Sleep => draw_sleep_screen(&mut display),
            }
            display.flush(DrawMode::BlackOnWhite).expect("to flush");
            needs_redraw = false;
            last_status_minute = now.map_or(60, |(_, m)| m);

            #[cfg(feature = "gps")]
            {
                gps_refresh = 0;
            }
        }

        // tick the status-bar clock once a minute via a fast partial refresh.
        if !needs_redraw {
            if let Some((h, m)) = status_time(&mut clock) {
                if m != last_status_minute {
                    last_status_minute = m;
                    draw_statusbar_time(&mut display, Some((h, m)));
                    display.flush_partial_fast(statusbar_time_rect()).ok();
                }
            }
        }

        // periodically bring wifi back up briefly to re-sync the clock, then it
        // powers down again. correct against RTC drift without leaving the radio
        // on to interfere with gps. (steal WIFI: the previous controller was
        // dropped, so the peripheral is free to re-init.)
        if clock.now_us() / 1_000_000 >= last_resync_secs + RESYNC_INTERVAL_SECS {
            esp_println::println!("clock: periodic re-sync");
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            if let Some(unix) = sync_time(wifi).await {
                set_local_time(&mut clock, unix, offset_hours);
            }
            last_resync_secs = clock.now_us() / 1_000_000;
            needs_redraw = true;
        }

        // poll touch/buttons every pass so input stays responsive. the GPS
        // work below is non-blocking, so it never stalls this poll.
        let input = display.input().expect("to read input");

        if input.buttons.home && current_screen != Screen::Home {
            current_screen = Screen::Home;
            needs_redraw = true;
        }

        // the auxiliary button sleeps from any screen; the current screen is
        // restored on wake.
        if input.buttons.auxiliary {
            break;
        }

        // edge-detect touches: act only on the press (untouched -> touched) and
        // wait for release before accepting the next, so a tap held longer than
        // one poll doesn't register repeatedly (double letters).
        match input.touch.and_then(|s| s.first_point()) {
            Some(point) if !touch_active => {
                touch_active = true;
                let (sx, sy) = touch_to_screen(point.x, point.y);

                match current_screen {
                    Screen::Home => {
                        if let Some(idx) = hit_test(sx, sy) {
                            current_screen = ICONS[idx].screen;
                            needs_redraw = true;
                        }
                    }
                    Screen::Frontlight => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if minus_hit(sx, sy) {
                            brightness = brightness.saturating_sub(BRIGHTNESS_STEP);
                            light.set_brightness(brightness);
                            draw_brightness_area(&mut display, brightness);
                            display.flush_partial_fast(brightness_native_rect()).ok();
                        } else if plus_hit(sx, sy) {
                            brightness = brightness.saturating_add(BRIGHTNESS_STEP).min(100);
                            light.set_brightness(brightness);
                            draw_brightness_area(&mut display, brightness);
                            display.flush_partial_fast(brightness_native_rect()).ok();
                        }
                    }
                    Screen::Sleep => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if sleep_now_hit(sx, sy) {
                            // leave the loop to draw the screensaver and enter
                            // deep sleep below
                            break;
                        }
                    }
                    Screen::Lora => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if let Some(key) = kb_hit(sx, sy, kb_symbols, kb_shift) {
                            match key {
                                Key::Shift => {
                                    kb_shift = !kb_shift;
                                    draw_keyboard(&mut display, kb_symbols, kb_shift);
                                    display.flush_partial_fast(keyboard_native_rect()).ok();
                                }
                                Key::Symbols => {
                                    kb_symbols = !kb_symbols;
                                    draw_keyboard(&mut display, kb_symbols, kb_shift);
                                    display.flush_partial_fast(keyboard_native_rect()).ok();
                                }
                                Key::Send => {
                                    if lora_message.is_empty() {
                                        lora_status = String::from("nothing to send");
                                    } else if let Some(r) = &mut radio {
                                        match r.transmit(lora_message.as_bytes()) {
                                            Ok(()) => {
                                                esp_println::println!("lora tx: {lora_message}");
                                                lora_status =
                                                    format!("sent {} bytes", lora_message.len());
                                                lora_sent.push(lora_message.clone());
                                                if lora_sent.len() > LIST_MAX {
                                                    lora_sent.remove(0);
                                                }
                                                lora_message.clear();
                                            }
                                            Err(e) => {
                                                esp_println::println!("lora tx error: {e}");
                                                lora_status = format!("tx error: {e}");
                                            }
                                        }
                                        // resume listening after transmitting.
                                        r.start_receive().ok();
                                        draw_message(&mut display, &lora_message);
                                        display.flush_partial_fast(message_box_native_rect()).ok();
                                        draw_list(&mut display, SENT_Y, "sent", &lora_sent);
                                        display.flush_partial_fast(sent_native_rect()).ok();
                                    } else {
                                        lora_status = String::from("radio not ready");
                                    }
                                    draw_lora_status(&mut display, &lora_status);
                                    display.flush_partial_fast(lora_status_native_rect()).ok();
                                }
                                other => {
                                    match other {
                                        Key::Char(c) if lora_message.len() < MSG_MAX => {
                                            lora_message.push(c)
                                        }
                                        Key::Space if lora_message.len() < MSG_MAX => {
                                            lora_message.push(' ')
                                        }
                                        Key::Backspace => {
                                            lora_message.pop();
                                        }
                                        _ => {}
                                    }
                                    draw_message(&mut display, &lora_message);
                                    display.flush_partial_fast(message_box_native_rect()).ok();
                                }
                            }
                        }
                    }
                    _ => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        }
                    }
                }
            }
            Some(_) => {}
            None => touch_active = false,
        }

        // the radio listens only while the lora screen is open: bring it up in
        // receive mode on entry, set it to standby and drop it on leave (frees
        // SPI2 for the SD wallpaper at sleep and avoids drawing rx current).
        // `radio_tried` keeps a failed init from re-resetting the chip every
        // pass; it re-arms when the screen is left.
        if current_screen == Screen::Lora {
            if radio.is_none() && !radio_tried {
                radio_tried = true;
                match make_radio() {
                    Ok(mut r) => {
                        if let Err(e) = r.start_receive() {
                            esp_println::println!("lora: start rx failed: {e}");
                        }
                        radio = Some(r);
                    }
                    Err(e) => {
                        esp_println::println!("lora: init failed: {e}");
                        lora_status = String::from("radio init failed");
                    }
                }
            }
        } else {
            radio_tried = false;
            if let Some(mut r) = radio.take() {
                r.standby().ok();
            }
        }

        // poll for an incoming packet (cheap: just a dio1 read until one lands)
        // and append it to the received log.
        if current_screen == Screen::Lora {
            if let Some(r) = &mut radio {
                let mut rx = [0u8; 255];
                if let Ok(Some(n)) = r.poll_receive(&mut rx) {
                    let rssi = r.rssi();
                    let text = core::str::from_utf8(&rx[..n]).unwrap_or("<binary>");
                    esp_println::println!("lora rx: {text} ({rssi} dBm)");
                    lora_recv.push(String::from(text));
                    if lora_recv.len() > LIST_MAX {
                        lora_recv.remove(0);
                    }
                    lora_status = format!("received {n} bytes ({rssi} dBm)");
                    draw_list(&mut display, RECV_Y, "received", &lora_recv);
                    display.flush_partial_fast(received_native_rect()).ok();
                    draw_lora_status(&mut display, &lora_status);
                    display.flush_partial_fast(lora_status_native_rect()).ok();
                }
            }
        }

        // keep the GPS UART drained with a single non-blocking read per pass
        // and refresh the readout periodically. one read every ~50ms keeps
        // the 128-byte FIFO (~133ms at 9600 baud) from overflowing without
        // blocking the touch poll above.
        #[cfg(feature = "gps")]
        if let Some(ref mut g) = gps {
            g.update().ok();
            if let Some(f) = current_fix(g) {
                last_fix = Some(f);
            }

            if current_screen == Screen::Gps && !needs_redraw {
                gps_refresh += 1;
                if gps_refresh >= GPS_REFRESH_TICKS {
                    gps_refresh = 0;
                    draw_gps_data(&mut display, g, last_fix);
                    display.flush_partial_fast(gps_data_native_rect()).ok();
                }
            }
        }

        delay.delay_millis(50);
    }

    // sleep was requested from the Sleep screen. turn the front light off,
    // paint the screensaver (e-ink retains it with the panel unpowered), then
    // enter deep sleep. the boot button wakes the chip, which resets and
    // re-runs main() from the top.
    light.set_brightness(0);
    // if we slept straight from the lora screen the radio still holds SPI2 and
    // the lora CS; standby and release them so the SD wallpaper can use the bus.
    if let Some(mut r) = radio.take() {
        r.standby().ok();
    }
    // remember where we were so wake lands on the same screen. single-threaded,
    // so writing the RTC-backed static is sound.
    unsafe {
        LAST_SCREEN = current_screen.to_index();
    }
    display.clear().ok();
    // pick a random wallpaper from the SD card; fall back to the drawn
    // screensaver if the folder is missing or has no usable .bmp files.
    if !show_wallpaper(
        &mut display,
        peripherals.SPI2,
        sdcard_pin_config!(peripherals),
        peripherals.GPIO46,
    ) {
        let pct = display.battery_percentage().unwrap_or(0);
        draw_screensaver(&mut display, pct);
    }
    display.flush(DrawMode::BlackOnWhite).expect("to flush");
    // hand LPWR back from the clock for the deep-sleep path.
    display.deep_sleep(clock.into_inner(), None)
}

// connect to wifi, fetch the current unix time via SNTP, then power the radio
// back down. self-contained: it drives the network stack (`runner`) alongside
// the connect+query work via `select`, so when the query finishes everything
// here drops — and WifiController's Drop deinitialises wifi (radio off), which
// frees the 2.4 GHz band for gps and saves power. returns UTC unix seconds, or
// None on timeout. re-callable for periodic re-sync (steal WIFI again).
async fn sync_time(wifi: esp_hal::peripherals::WIFI<'static>) -> Option<u64> {
    let station_config = Config::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );
    let mut controller = WifiController::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    )
    .ok()?;

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let mut resources = StackResources::<3>::new();
    let (stack, mut runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        &mut resources,
        seed,
    );

    let outcome = select(runner.run(), async {
        controller.connect_async().await.ok()?;
        with_timeout(Duration::from_secs(15), stack.wait_config_up())
            .await
            .ok()?;
        esp_println::println!("wifi: connected");
        for _ in 0..3 {
            if let Some(unix) = sntp_unix_time(stack).await {
                return Some(unix);
            }
            Timer::after(Duration::from_secs(2)).await;
        }
        None
    })
    .await;

    match outcome {
        Either::First(_) => None,
        Either::Second(unix) => unix,
    }
}

// set the RTC to local time from a UTC unix timestamp plus the configured
// offset.
fn set_local_time(clock: &mut Clock, utc_unix: u64, offset_hours: i64) {
    let local = (utc_unix as i64 + offset_hours * 3600).max(0) as u64;
    clock.set_now_us(local * 1_000_000);
    esp_println::println!("clock: set local unix={local} (utc{offset_hours:+})");
}

// query an NTP server over UDP and return the current unix time in seconds.
async fn sntp_unix_time(stack: embassy_net::Stack<'_>) -> Option<u64> {
    let addrs = stack.dns_query(NTP_SERVER, DnsQueryType::A).await.ok()?;
    let server = IpEndpoint::new(*addrs.first()?, 123);

    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 128];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(50123).ok()?;

    // minimal SNTP client request: LI=0, VN=3, Mode=3 (client).
    let mut request = [0u8; 48];
    request[0] = 0x1B;
    socket.send_to(&request, server).await.ok()?;

    let mut response = [0u8; 48];
    let (n, _) = socket.recv_from(&mut response).await.ok()?;
    if n < 44 {
        return None;
    }
    // transmit timestamp (seconds since 1900) is at bytes 40..44, big-endian.
    let ntp_secs = u32::from_be_bytes([response[40], response[41], response[42], response[43]]);
    Some((ntp_secs as u64).saturating_sub(NTP_UNIX_DELTA))
}
