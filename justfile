set dotenv-load

# flash the wifi clock example (SSID/PASSWORD/TZ_OFFSET_HOURS from .env)
clock:
    # re-touch so the env-baked credentials pick up any .env changes
    touch crates/t5s3-epaper-core/examples/clock.rs
    SSID="$SSID" PASSWORD="$PASSWORD" TZ_OFFSET_HOURS="$TZ_OFFSET_HOURS" cargo run -p t5s3-epaper-core --example clock --features wifi

# flash the touchscreen ui (gps + lora keyboard + wifi status-bar clock; creds/tz from .env)
ui:
    # re-touch so the env-baked credentials pick up any .env changes
    touch crates/t5s3-epaper-ui/src/main.rs
    SSID="$SSID" PASSWORD="$PASSWORD" TZ_OFFSET_HOURS="$TZ_OFFSET_HOURS" cargo run -p t5s3-epaper-ui --features gps

# check that everything compiles
check:
    cargo c -p t5s3-epaper-core --examples --all-features
    cargo c -p t5s3-epaper-ui --all-features

# format and lint
lint:
    cargo fmt
    cargo clippy --workspace --all-features
