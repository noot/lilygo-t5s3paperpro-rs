#![no_std]
#![no_main]

extern crate alloc;
extern crate lilygo_t5s3paperpro;

use embassy_executor::Spawner;
use embassy_net::{
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
    IpEndpoint,
    Runner,
    StackResources,
};
use embassy_time::{Duration, Timer};
use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    interrupt::software::SoftwareInterruptControl,
    rng::Rng,
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::wifi::{sta::StationConfig, Config, ControllerConfig, Interface, WifiController};
use lilygo_t5s3paperpro::{display::Rectangle, pin_config, Clock, Display, DrawMode};
use static_cell::StaticCell;
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

esp_bootloader_esp_idf::esp_app_desc!();

// supplied at build time (e.g. `SSID=... PASSWORD=... cargo run --example clock
// --features wifi`); fall back to placeholders so `cargo c` works without them.
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

static TIME_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_spleen32x64_mr>();
static LABEL_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_spleen16x32_mr>();

// region (native, unrotated coords) covering the big HH:MM readout, used for
// fast partial refreshes once per minute.
fn time_rect() -> Rectangle {
    Rectangle {
        x: 340,
        y: 210,
        width: 280,
        height: 120,
    }
}

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    // internal-RAM heaps for the wifi stack (its DMA buffers must not live in
    // PSRAM), plus a PSRAM heap for the display's large framebuffers.
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

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.I2C0,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    let delay = Delay::new();
    display.power_on().expect("to power on");
    delay.delay_millis(10);
    display.clear().expect("to clear");
    draw_status(&mut display, "connecting to wifi...");
    display.flush(DrawMode::BlackOnWhite).expect("to flush");

    // ── wifi ─────────────────────────────────────────────────────────
    println!("wifi: connecting to {SSID}");
    let station_config = Config::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );
    let wifi_interface = Interface::station();
    let controller = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default().with_initial_config(station_config),
    )
    .expect("wifi config valid");

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, runner) = embassy_net::new(
        wifi_interface,
        net_config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    spawner.spawn(connection(controller).expect("spawn connection task"));
    spawner.spawn(net_task(runner).expect("spawn net task"));

    println!("wifi: waiting for dhcp...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        println!("wifi: ip {}", cfg.address);
    }

    // ── sntp time sync ───────────────────────────────────────────────
    draw_status(&mut display, "syncing time (ntp)...");
    display.flush(DrawMode::BlackOnWhite).expect("to flush");

    let unix = loop {
        match sntp_unix_time(stack).await {
            Some(t) => break t,
            None => {
                println!("sntp: query failed, retrying");
                Timer::after(Duration::from_secs(2)).await;
            }
        }
    };
    // timezone offset (hours from UTC) from the TZ_OFFSET_HOURS build env (see
    // .env); defaults to pacific daylight time.
    let offset_hours: i64 = option_env!("TZ_OFFSET_HOURS")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(-7);
    let local = (unix as i64 + offset_hours * 3600).max(0) as u64;
    println!("sntp: unix={unix} local={local} (utc{offset_hours:+})");

    let mut clock = Clock::new(peripherals.LPWR);
    clock.set_now_us(local * 1_000_000);

    // ── clock display loop ───────────────────────────────────────────
    display.clear().expect("to clear");
    let (mut h, mut m, _) = hms(clock.now_us() / 1_000_000);
    draw_time(&mut display, h, m);
    display.flush(DrawMode::BlackOnWhite).expect("to flush");
    let mut last_minute = m;

    loop {
        let (nh, nm, ns) = hms(clock.now_us() / 1_000_000);
        if nm != last_minute {
            last_minute = nm;
            h = nh;
            m = nm;
            draw_time(&mut display, h, m);
            display.flush_partial_fast(time_rect()).ok();
        }
        // sleep until the next minute boundary
        let until_next_minute = 60u64.saturating_sub(ns as u64).max(1);
        Timer::after(Duration::from_secs(until_next_minute)).await;
    }
}

// convert unix seconds into (hours, minutes, seconds) of the day.
fn hms(unix_secs: u64) -> (u32, u32, u32) {
    let secs_of_day = (unix_secs % 86_400) as u32;
    (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    )
}

fn draw_status(display: &mut Display, text: &str) {
    LABEL_FONT
        .render_aligned(
            text,
            Point::new(Display::WIDTH as i32 / 2, Display::HEIGHT as i32 / 2),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::WithBackground {
                fg: Gray4::BLACK,
                bg: Gray4::WHITE,
            },
            display,
        )
        .ok();
}

fn draw_time(display: &mut Display, hours: u32, minutes: u32) {
    TIME_FONT
        .render_aligned(
            format_args!("{hours:02}:{minutes:02}"),
            Point::new(Display::WIDTH as i32 / 2, Display::HEIGHT as i32 / 2),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::WithBackground {
                fg: Gray4::BLACK,
                bg: Gray4::WHITE,
            },
            display,
        )
        .ok();
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

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                println!("wifi: connected");
                controller.wait_for_disconnect_async().await.ok();
                println!("wifi: disconnected");
            }
            Err(e) => {
                println!("wifi: connect failed: {e:?}");
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}
