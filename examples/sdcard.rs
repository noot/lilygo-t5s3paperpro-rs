#![no_std]
#![no_main]

extern crate alloc;
extern crate lilygo_t5s3paperpro;

use alloc::{format, string::String};
use core::{fmt::Write as _, format_args};

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use lilygo_t5s3paperpro::{pin_config, sdcard_pin_config, Display, DrawMode, SdCard};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen16x32_mr>();

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);
    let delay = Delay::new();

    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.I2C0,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    let sdcard = SdCard::new(sdcard_pin_config!(peripherals), peripherals.SPI2)
        .expect("to initialize sd card");
    let card_size = sdcard.card_size_bytes().expect("to query card size");

    let contents = format!("Hello from lilygo-t5s3paperpro\nCard size: {} bytes\n", card_size);
    sdcard
        .write_root_file("TEST.TXT", contents.as_bytes())
        .expect("to write test file");

    let mut listing = sdcard.list_root().expect("to list root directory");
    listing.sort_by(|a, b| a.name.cmp(&b.name));

    let mut body = String::new();
    let _ = writeln!(body, "SD: {} MB", card_size / (1024 * 1024));
    let _ = writeln!(body, "Wrote /TEST.TXT");
    let _ = writeln!(body);

    for entry in listing.iter().take(10) {
        let kind = if entry.is_directory { 'D' } else { 'F' };
        let _ = writeln!(body, "{} {:<16} {}", kind, entry.name, entry.size);
    }

    display.power_on().expect("to power on display");
    delay.delay_millis(20);
    display.clear().expect("to clear display");

    FONT.render_aligned(
        format_args!("{}", body),
        Point::new(24, 48),
        u8g2_fonts::types::VerticalPosition::Top,
        u8g2_fonts::types::HorizontalAlignment::Left,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        &mut display,
    )
    .expect("to render text");

    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush to display");
    display.power_off().expect("to power off display");

    loop {
        core::hint::spin_loop();
    }
}
