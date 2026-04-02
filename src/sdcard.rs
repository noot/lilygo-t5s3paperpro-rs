use alloc::{format, string::String, vec::Vec};

use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{DirEntry, LfnBuffer, Mode, SdCardError, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_hal::{
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    peripherals,
    spi::{
        master::{Config as SpiConfig, ConfigError as SpiConfigError, Spi},
        Mode as SpiMode,
    },
    time::Rate,
    Blocking,
};

type SpiBusType<'d> = Spi<'d, Blocking>;
type CsPin<'d> = Output<'d>;
type SpiDevice<'d> = ExclusiveDevice<SpiBusType<'d>, CsPin<'d>, Delay>;
type BlockDevice<'d> = embedded_sdmmc::SdCard<SpiDevice<'d>, Delay>;
type VolumeManagerType<'d> = VolumeManager<BlockDevice<'d>, SdTimeSource, 4, 4, 1>;

#[derive(Clone, Debug)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_directory: bool,
    pub size: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct SdTimeSource {
    timestamp: Timestamp,
}

impl SdTimeSource {
    pub fn new(timestamp: Timestamp) -> Self {
        Self { timestamp }
    }
}

impl Default for SdTimeSource {
    fn default() -> Self {
        Self {
            timestamp: Timestamp::from_calendar(2026, 1, 1, 0, 0, 0).unwrap(),
        }
    }
}

impl TimeSource for SdTimeSource {
    fn get_timestamp(&self) -> Timestamp {
        self.timestamp
    }
}

#[derive(Debug)]
pub enum Error {
    SpiConfig(SpiConfigError),
    Spi(esp_hal::spi::Error),
    Filesystem(embedded_sdmmc::Error<SdCardError>),
    Card(SdCardError),
}

pub type Result<T> = core::result::Result<T, Error>;

pub struct SdCard<'d> {
    card_size_bytes: u64,
    volume_mgr: VolumeManagerType<'d>,
}

pub struct PinConfig<'d> {
    pub miso: peripherals::GPIO21<'d>,
    pub mosi: peripherals::GPIO13<'d>,
    pub sclk: peripherals::GPIO14<'d>,
    pub cs: peripherals::GPIO12<'d>,
}

impl<'d> SdCard<'d> {
    pub fn new(
        pins: PinConfig<'d>,
        spi: peripherals::SPI2<'d>,
    ) -> Result<Self> {
        Self::new_with_time_source(pins, spi, SdTimeSource::default())
    }

    pub fn new_with_time_source(
        pins: PinConfig<'d>,
        spi: peripherals::SPI2<'d>,
        time_source: SdTimeSource,
    ) -> Result<Self> {
        let sd_bus = Spi::new(
            spi,
            SpiConfig::default()
                .with_frequency(Rate::from_khz(400))
                .with_mode(SpiMode::_0),
        )
        .map_err(Error::SpiConfig)?
        .with_sck(pins.sclk)
        .with_mosi(pins.mosi)
        .with_miso(pins.miso);

        let sd_cs = Output::new(pins.cs, Level::High, OutputConfig::default());
        let mut sd_bus = sd_bus;
        sd_bus.write(&[0xFF; 10]).map_err(Error::Spi)?;

        let sd_device = ExclusiveDevice::new(sd_bus, sd_cs, Delay::new()).unwrap();
        let sd_card = embedded_sdmmc::SdCard::new(sd_device, Delay::new());
        let card_size_bytes = sd_card.num_bytes().map_err(Error::Card)?;
        let volume_mgr = VolumeManager::new(sd_card, time_source);

        Ok(Self {
            card_size_bytes,
            volume_mgr,
        })
    }

    pub fn card_size_bytes(&self) -> Result<u64> {
        Ok(self.card_size_bytes)
    }

    pub fn write_root_file(&self, name: &str, contents: &[u8]) -> Result<()> {
        let volume = self
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(Error::Filesystem)?;
        let root_dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        let file = root_dir
            .open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)
            .map_err(Error::Filesystem)?;
        file.write(contents).map_err(Error::Filesystem)?;
        file.flush().map_err(Error::Filesystem)?;
        Ok(())
    }

    pub fn list_root(&self) -> Result<Vec<DirectoryEntry>> {
        let volume = self
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(Error::Filesystem)?;
        let root_dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        let mut lfn_storage = [0u8; 260];
        let mut lfn_buffer = LfnBuffer::new(&mut lfn_storage);
        let mut entries = Vec::new();

        root_dir
            .iterate_dir_lfn(&mut lfn_buffer, |entry, long_name| {
                entries.push(build_directory_entry(entry, long_name));
            })
            .map_err(Error::Filesystem)?;

        Ok(entries)
    }

    pub fn read_root_file(&self, name: &str) -> Result<Vec<u8>> {
        let volume = self
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(Error::Filesystem)?;
        let root_dir = volume.open_root_dir().map_err(Error::Filesystem)?;
        let file = root_dir
            .open_file_in_dir(name, Mode::ReadOnly)
            .map_err(Error::Filesystem)?;
        let mut data = Vec::new();

        while !file.is_eof() {
            let mut buffer = [0u8; 64];
            let count = file.read(&mut buffer).map_err(Error::Filesystem)?;
            data.extend_from_slice(&buffer[..count]);
        }

        Ok(data)
    }
}

fn build_directory_entry(entry: &DirEntry, long_name: Option<&str>) -> DirectoryEntry {
    DirectoryEntry {
        name: long_name
            .map(String::from)
            .unwrap_or_else(|| format!("{}", entry.name)),
        is_directory: entry.attributes.is_directory(),
        size: entry.size,
    }
}
