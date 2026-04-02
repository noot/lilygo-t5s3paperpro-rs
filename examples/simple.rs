#![no_std]
#![no_main]

extern crate alloc;
extern crate lilygo_t5s3paperpro;

use embedded_graphics::{
    prelude::*,
    primitives::{Circle, PrimitiveStyle},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use lilygo_t5s3paperpro::{pin_config, Display, DrawMode};
use log::*;

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();

    info!("Create PSRAM allocator");
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    info!("Initialise the display");
    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.I2C0,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    info!("Turn the display on");
    display.power_on().expect("to power on display");
    delay.delay_millis(10);

    info!("clear the screen");
    display.clear().expect("to clear screen");

    info!("Draw a circle with a 3px wide stroke in the center of the screen");
    Circle::new(display.bounding_box().center() - Point::new(100, 100), 200)
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 3))
        .draw(&mut display)
        .expect("to draw in the framebuffer");
    info!("Flush the framebuffer to the screen");
    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush to display");

    info!("Turn the display off again");
    display.power_off().expect("to power off display");

    info!("do nothing");
    loop {
        core::hint::spin_loop();
    }
}
