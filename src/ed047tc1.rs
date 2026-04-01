use esp_hal::{
    dma::DmaTxBuf,
    dma_buffers,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    lcd_cam::{
        lcd::{i8080, i8080::Command},
        LcdCam,
    },
    peripherals,
    rmt::PulseCode,
    time::Rate,
    Blocking,
};

use crate::rmt;

macro_rules! pulse {
    ($high:expr, $low:expr) => {
        if $high > 0 {
            [
                PulseCode::new(Level::High, $high, Level::Low, $low),
                PulseCode::end_marker(),
            ]
        } else {
            [
                PulseCode::new(Level::High, $low, Level::Low, 0),
                PulseCode::end_marker(),
            ]
        }
    };
}

const DMA_BUFFER_SIZE: usize = 248;
const I2C_FREQUENCY_KHZ: u32 = 100;
const PCA9555_ADDR: u8 = 0x20;
const TPS65185_ADDR: u8 = 0x68;
const PCA9555_REG_INPUT_PORT1: u8 = 1;
const PCA9555_REG_OUTPUT_PORT0: u8 = 2;
const PCA9555_REG_OUTPUT_PORT1: u8 = 3;
const PCA9555_REG_INVERT_PORT0: u8 = 4;
const PCA9555_REG_INVERT_PORT1: u8 = 5;
const PCA9555_REG_CONFIG_PORT0: u8 = 6;
const PCA9555_REG_CONFIG_PORT1: u8 = 7;
const TPS_REG_ENABLE: u8 = 0x01;
const TPS_REG_VCOM1: u8 = 0x03;
const TPS_REG_VCOM2: u8 = 0x04;
const TPS_REG_PG: u8 = 0x0F;
const BQ27220_ADDR: u8 = 0x55;
const BQ27220_REG_VOLTAGE: u8 = 0x08;
const BQ27220_REG_STATE_OF_CHARGE: u8 = 0x2C;
const VCOM_MV: u16 = 1600;
const PCA_BIT_OE: u8 = 1 << 0;
const PCA_BIT_MODE: u8 = 1 << 1;
const PCA_BIT_BUTTON: u8 = 1 << 2;
const PCA_BIT_PWRUP: u8 = 1 << 3;
const PCA_BIT_VCOM_CTRL: u8 = 1 << 4;
const PCA_BIT_WAKEUP: u8 = 1 << 5;
const PCA_BIT_PWRGOOD: u8 = 1 << 6;
const PCA_BIT_INT: u8 = 1 << 7;

#[derive(Default)]
struct ConfigRegister {
    mode: bool,
    output_enable: bool,
    pwrup: bool,
    vcom_ctrl: bool,
    wakeup: bool,
}

struct ConfigWriter<'a> {
    i2c: I2c<'a, Blocking>,
    leh: Output<'a>,
    stv: Output<'a>,
    output_port1: u8,
    config: ConfigRegister,
}

impl<'a> ConfigWriter<'a> {
    fn new(
        i2c: peripherals::I2C0<'a>,
        sda: peripherals::GPIO39<'a>,
        scl: peripherals::GPIO40<'a>,
        leh: peripherals::GPIO42<'a>,
        stv: peripherals::GPIO45<'a>,
    ) -> crate::Result<Self> {
        let i2c = I2c::new(
            i2c,
            I2cConfig::default().with_frequency(Rate::from_khz(I2C_FREQUENCY_KHZ)),
        )
        .map_err(crate::Error::I2cConfig)?
        .with_sda(sda)
        .with_scl(scl);

        let mut writer = ConfigWriter {
            i2c,
            leh: Output::new(leh, Level::Low, OutputConfig::default()),
            stv: Output::new(stv, Level::High, OutputConfig::default()),
            output_port1: 0,
            config: ConfigRegister::default(),
        };

        writer.write_register(
            PCA9555_ADDR,
            &[PCA9555_REG_CONFIG_PORT1, PCA_BIT_BUTTON | PCA_BIT_PWRGOOD | PCA_BIT_INT],
        )?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_INVERT_PORT0, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_INVERT_PORT1, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_CONFIG_PORT0, 0x00])?;
        writer.write_register(PCA9555_ADDR, &[PCA9555_REG_OUTPUT_PORT0, 0xFF])?;
        writer.write()?;

        Ok(writer)
    }

    fn write(&mut self) -> crate::Result<()> {
        let mut value = 0;
        if self.config.output_enable {
            value |= PCA_BIT_OE;
        }
        if self.config.mode {
            value |= PCA_BIT_MODE;
        }
        if self.config.pwrup {
            value |= PCA_BIT_PWRUP;
        }
        if self.config.vcom_ctrl {
            value |= PCA_BIT_VCOM_CTRL;
        }
        if self.config.wakeup {
            value |= PCA_BIT_WAKEUP;
        }
        self.output_port1 = value;
        self.write_register(PCA9555_ADDR, &[PCA9555_REG_OUTPUT_PORT1, value])
    }

    fn set_stv(&mut self, level: bool) {
        self.stv.set_level(if level { Level::High } else { Level::Low });
    }

    fn pulse_leh(&mut self) {
        self.leh.set_high();
        busy_delay(64);
        self.leh.set_low();
        busy_delay(64);
    }

    fn pwrgood(&mut self) -> crate::Result<bool> {
        Ok(self.read_register(PCA9555_ADDR, PCA9555_REG_INPUT_PORT1)? & PCA_BIT_PWRGOOD != 0)
    }

    fn enable_tps(&mut self) -> crate::Result<()> {
        self.write_register(TPS65185_ADDR, &[TPS_REG_ENABLE, 0x3F])?;
        self.set_vcom(VCOM_MV)
    }

    fn set_vcom(&mut self, mv: u16) -> crate::Result<()> {
        let value = mv / 10;
        self.write_register(TPS65185_ADDR, &[TPS_REG_VCOM2, ((value & 0x100) >> 8) as u8])?;
        self.write_register(TPS65185_ADDR, &[TPS_REG_VCOM1, (value & 0xFF) as u8])
    }

    fn tps_power_good(&mut self) -> crate::Result<bool> {
        Ok(self.read_register(TPS65185_ADDR, TPS_REG_PG)? & 0xFA == 0xFA)
    }

    fn read_register(&mut self, device: u8, reg: u8) -> crate::Result<u8> {
        let mut value = [0u8; 1];
        self.i2c
            .write_read(device, &[reg], &mut value)
            .map_err(crate::Error::I2c)?;
        Ok(value[0])
    }

    fn read_register_u16(&mut self, device: u8, reg: u8) -> crate::Result<u16> {
        let mut value = [0u8; 2];
        self.i2c
            .write_read(device, &[reg], &mut value)
            .map_err(crate::Error::I2c)?;
        Ok(u16::from_le_bytes(value))
    }

    fn write_register(&mut self, device: u8, payload: &[u8]) -> crate::Result<()> {
        self.i2c.write(device, payload).map_err(crate::Error::I2c)
    }

    fn battery_voltage_mv(&mut self) -> crate::Result<u16> {
        self.read_register_u16(BQ27220_ADDR, BQ27220_REG_VOLTAGE)
    }

    fn battery_state_of_charge(&mut self) -> crate::Result<u16> {
        self.read_register_u16(BQ27220_ADDR, BQ27220_REG_STATE_OF_CHARGE)
    }
}

pub struct PinConfig<'a> {
    pub data0: peripherals::GPIO5<'a>,
    pub data1: peripherals::GPIO6<'a>,
    pub data2: peripherals::GPIO7<'a>,
    pub data3: peripherals::GPIO15<'a>,
    pub data4: peripherals::GPIO16<'a>,
    pub data5: peripherals::GPIO17<'a>,
    pub data6: peripherals::GPIO18<'a>,
    pub data7: peripherals::GPIO8<'a>,
    pub i2c_sda: peripherals::GPIO39<'a>,
    pub i2c_scl: peripherals::GPIO40<'a>,
    pub leh: peripherals::GPIO42<'a>,
    pub lcd_dc: peripherals::GPIO41<'a>,
    pub lcd_wrx: peripherals::GPIO4<'a>,
    pub rmt: peripherals::GPIO48<'a>,
    pub stv: peripherals::GPIO45<'a>,
}

pub(crate) struct ED047TC1<'a> {
    i8080: Option<i8080::I8080<'a, Blocking>>,
    cfg_writer: ConfigWriter<'a>,
    rmt: rmt::Rmt<'a>,
    dma_buf: Option<DmaTxBuf>,
}

impl<'a> ED047TC1<'a> {
    pub(crate) fn new(
        pins: PinConfig<'a>,
        i2c: peripherals::I2C0<'a>,
        dma: peripherals::DMA_CH0<'a>,
        lcd_cam: peripherals::LCD_CAM<'a>,
        rmt: peripherals::RMT<'a>,
    ) -> crate::Result<Self> {
        let lcd_cam = LcdCam::new(lcd_cam);

        let mut cfg_writer =
            ConfigWriter::new(i2c, pins.i2c_sda, pins.i2c_scl, pins.leh, pins.stv)?;
        cfg_writer.write()?;

        let (_, _, tx_buffer, tx_descriptors) = dma_buffers!(0, DMA_BUFFER_SIZE);
        let dma_buf =
            Some(DmaTxBuf::new(tx_descriptors, tx_buffer).map_err(crate::Error::DmaBuffer)?);

        let config = i8080::Config::default()
            .with_frequency(Rate::from_mhz(20))
            .with_cd_idle_edge(false)
            .with_cd_cmd_edge(true)
            .with_cd_dummy_edge(false)
            .with_cd_data_edge(false);
        let ctrl = ED047TC1 {
            i8080: Some(
                i8080::I8080::new(lcd_cam.lcd, dma, config)
                    .expect("to create i8080 device")
                    .with_dc(pins.lcd_dc)
                    .with_wrx(pins.lcd_wrx)
                    .with_data0(pins.data6)
                    .with_data1(pins.data7)
                    .with_data2(pins.data4)
                    .with_data3(pins.data5)
                    .with_data4(pins.data2)
                    .with_data5(pins.data3)
                    .with_data6(pins.data0)
                    .with_data7(pins.data1),
            ),
            cfg_writer,
            rmt: rmt::Rmt::new(rmt, pins.rmt),
            dma_buf,
        };
        Ok(ctrl)
    }

    pub(crate) fn power_on(&mut self) -> crate::Result<()> {
        self.cfg_writer.set_stv(true);
        self.cfg_writer.config.output_enable = true;
        self.cfg_writer.config.mode = false;
        self.cfg_writer.config.wakeup = true;
        self.cfg_writer.write()?;
        self.cfg_writer.config.pwrup = true;
        self.cfg_writer.write()?;
        self.cfg_writer.config.vcom_ctrl = true;
        self.cfg_writer.write()?;
        busy_delay(240_000);
        while !self.cfg_writer.pwrgood()? {}
        self.cfg_writer.enable_tps()?;
        let mut tries = 0;
        while !self.cfg_writer.tps_power_good()? {
            tries += 1;
            if tries >= 500 {
                return Err(crate::Error::PowerTimeout);
            }
            busy_delay(240_000);
        }
        Ok(())
    }

    pub(crate) fn power_off(&mut self) -> crate::Result<()> {
        self.cfg_writer.config.vcom_ctrl = false;
        self.cfg_writer.config.pwrup = false;
        self.cfg_writer.config.output_enable = false;
        self.cfg_writer.config.mode = false;
        self.cfg_writer.write()?;
        busy_delay(240_000);
        self.cfg_writer.config.wakeup = false;
        self.cfg_writer.write()?;
        self.cfg_writer.set_stv(false);
        Ok(())
    }

    pub(crate) fn battery_voltage_mv(&mut self) -> crate::Result<u16> {
        self.cfg_writer.battery_voltage_mv()
    }

    pub(crate) fn battery_state_of_charge(&mut self) -> crate::Result<u16> {
        self.cfg_writer.battery_state_of_charge()
    }

    pub(crate) fn frame_start(&mut self) -> crate::Result<()> {
        self.cfg_writer.config.mode = true;
        self.cfg_writer.write()?;

        let data = pulse!(10, 10);
        self.rmt.pulse(&data, true)?;

        self.cfg_writer.set_stv(false);

        busy_delay(240);
        let data = pulse!(100, 100);
        let rmt_tx = self.rmt.pulse(&data, false)?;

        self.cfg_writer.set_stv(true);

        if let Some(rmt_tx) = rmt_tx {
            self.rmt.reclaim_channel(rmt_tx)?;
        }

        let data = pulse!(0, 100);
        self.rmt.pulse(&data, true)?;

        self.cfg_writer.config.output_enable = true;
        self.cfg_writer.write()?;

        let data = pulse!(10, 10);
        self.rmt.pulse(&data, true)?;

        Ok(())
    }

    pub(crate) fn latch_row(&mut self) {
        self.cfg_writer.pulse_leh();
    }

    pub(crate) fn skip(&mut self) -> crate::Result<()> {
        let data = pulse!(45, 5);
        if let Some(rmt_tx) = self.rmt.pulse(&data, false)? {
            self.rmt.reclaim_channel(rmt_tx)?;
        }
        Ok(())
    }

    pub(crate) fn output_row(&mut self, output_time: u16) -> crate::Result<()> {
        self.latch_row();

        let data = pulse!(output_time, 50);
        let rmt_tx = self.rmt.pulse(&data, false)?;
        let i8080 = self.i8080.take().ok_or(crate::Error::MissingI8080)?;
        let dma_buf = self.dma_buf.take().ok_or(crate::Error::MissingDmaBuffer)?;
        let tx = i8080
            .send(Command::<u8>::One(0), 0, dma_buf)
            .map_err(|(err, i8080, buf)| {
                self.dma_buf = Some(buf);
                self.i8080 = Some(i8080);
                crate::Error::Dma(err)
            })?;
        let (r, i8080, dma_buf) = tx.wait();
        if let Some(rmt_tx) = rmt_tx {
            self.rmt.reclaim_channel(rmt_tx)?;
        }
        r.map_err(crate::Error::Dma)?;
        self.i8080 = Some(i8080);
        self.dma_buf = Some(dma_buf);

        Ok(())
    }

    pub(crate) fn frame_end(&mut self) -> crate::Result<()> {
        self.cfg_writer.config.output_enable = false;
        self.cfg_writer.write()?;
        self.cfg_writer.config.mode = false;
        self.cfg_writer.write()?;
        let data = pulse!(10, 10);
        self.rmt.pulse(&data, true)?;
        self.rmt.pulse(&data, true)?;

        Ok(())
    }

    pub(crate) fn set_buffer(&mut self, data: &[u8]) -> crate::Result<()> {
        let mut dma_buf = self.dma_buf.take().ok_or(crate::Error::MissingDmaBuffer)?;
        dma_buf.as_mut_slice().fill(0);
        dma_buf.as_mut_slice()[..data.len()].copy_from_slice(data);
        self.dma_buf = Some(dma_buf);
        Ok(())
    }
}

#[inline(always)]
fn busy_delay(wait_cycles: u32) {
    let target = cycles() + wait_cycles as u64;
    while cycles() < target {}
}

#[inline(always)]
fn cycles() -> u64 {
    esp_hal::xtensa_lx::timer::get_cycle_count() as u64
}
