use esp_hal::{
    ledc::{
        channel::{self, ChannelIFace as _},
        timer::{self, TimerIFace as _},
        LSGlobalClkSource,
        Ledc,
        LowSpeed,
    },
    peripherals,
    time::Rate,
};

const PWM_FREQUENCY_KHZ: u32 = 5;
const DUTY_RESOLUTION: u32 = 8;
const DUTY_RANGE: u32 = 1 << DUTY_RESOLUTION;

/// Front light controller using the PT4103B23F LED driver on GPIO11.
///
/// Uses the LEDC peripheral (timer 0, channel 0) to drive a hardware PWM
/// signal. After construction the LEDC hardware runs autonomously; brightness
/// changes are applied by writing directly to the LEDC registers.
pub struct FrontLight {
    brightness_pct: u8,
}

impl FrontLight {
    /// Create a new front light controller.
    ///
    /// Configures the LEDC peripheral with an 8-bit PWM at 5 kHz and starts
    /// with the light off.
    pub fn new(
        ledc_peripheral: peripherals::LEDC<'_>,
        pin: peripherals::GPIO11<'_>,
    ) -> Result<Self, Error> {
        let mut ledc = Ledc::new(ledc_peripheral);
        ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

        let mut timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
        timer
            .configure(timer::config::Config {
                duty: timer::config::Duty::Duty8Bit,
                clock_source: timer::LSClockSource::APBClk,
                frequency: Rate::from_khz(PWM_FREQUENCY_KHZ),
            })
            .map_err(Error::TimerConfig)?;

        let mut channel = ledc.channel(channel::Number::Channel0, pin);
        channel
            .configure(channel::config::Config {
                timer: &timer,
                duty_pct: 0,
                drive_mode: esp_hal::gpio::DriveMode::PushPull,
            })
            .map_err(Error::ChannelConfig)?;

        Ok(Self { brightness_pct: 0 })
    }

    /// Set brightness as a percentage (0–100). Values above 100 are clamped.
    pub fn set_brightness(&mut self, pct: u8) {
        let pct = pct.min(100);
        let duty_value = DUTY_RANGE * pct as u32 / 100;
        set_duty_hw(duty_value);
        self.brightness_pct = pct;
    }

    /// Return the current brightness percentage.
    pub fn brightness(&self) -> u8 {
        self.brightness_pct
    }

    /// Turn the front light off.
    pub fn off(&mut self) {
        self.set_brightness(0);
    }

    /// Turn the front light on at full brightness.
    pub fn on(&mut self) {
        self.set_brightness(100);
    }
}

fn set_duty_hw(duty: u32) {
    let regs = peripherals::LEDC::regs();
    regs.ch(0)
        .duty()
        .write(|w| unsafe { w.duty().bits(duty << 4) });
    regs.ch(0).conf1().write(|w| {
        w.duty_start().set_bit();
        w.duty_inc().set_bit();
        unsafe {
            w.duty_num().bits(0x1);
            w.duty_cycle().bits(0x1);
            w.duty_scale().bits(0x0)
        }
    });
    regs.ch(0).conf0().modify(|_, w| w.para_up().set_bit());
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    TimerConfig(timer::Error),
    ChannelConfig(channel::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TimerConfig(e) => write!(f, "front light timer configuration failed: {e:?}"),
            Self::ChannelConfig(e) => write!(f, "front light channel configuration failed: {e:?}"),
        }
    }
}
