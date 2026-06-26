set dotenv-load

# flash the wifi clock example (SSID/PASSWORD/TZ_OFFSET_HOURS from .env)
clock:
    # re-touch so the env-baked credentials pick up any .env changes
    touch examples/clock.rs
    SSID="$SSID" PASSWORD="$PASSWORD" TZ_OFFSET_HOURS="$TZ_OFFSET_HOURS" cargo run --example clock --features wifi

# flash the touchscreen ui (gps + wifi status-bar clock; creds/tz from .env)
ui:
    # re-touch so the env-baked credentials pick up any .env changes
    touch examples/ui.rs
    SSID="$SSID" PASSWORD="$PASSWORD" TZ_OFFSET_HOURS="$TZ_OFFSET_HOURS" cargo run --example ui --features gps,wifi

# check that everything compiles
check:
    cargo c --examples --all-features

# format and lint
lint:
    cargo fmt
    cargo clippy --all-features
