use embassy_futures::select::{select, Either};
use embassy_net::{
    dns::DnsQueryType,
    udp::{PacketMetadata, UdpSocket},
    IpEndpoint,
    StackResources,
};
use embassy_time::{with_timeout, Duration, Timer};
use esp_hal::rng::Rng;
use esp_radio::wifi::{sta::StationConfig, Config, ControllerConfig, Interface, WifiController};
use t5s3_epaper_core::Clock;

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
pub(crate) const RESYNC_INTERVAL_SECS: u64 = 4 * 3600;

// connect to wifi, fetch the current unix time via SNTP, then power the radio
// back down. self-contained: it drives the network stack (`runner`) alongside
// the connect+query work via `select`, so when the query finishes everything
// here drops — and WifiController's Drop deinitialises wifi (radio off), which
// frees the 2.4 GHz band for gps and saves power. returns UTC unix seconds, or
// None on timeout. re-callable for periodic re-sync (steal WIFI again).
pub(crate) async fn sync_time(wifi: esp_hal::peripherals::WIFI<'static>) -> Option<u64> {
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
pub(crate) fn set_local_time(clock: &mut Clock, utc_unix: u64, offset_hours: i64) {
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
