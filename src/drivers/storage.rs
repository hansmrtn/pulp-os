//! High-level storage operations for reading files from SD card.
use embedded_sdmmc::{Error, Mode, SdCardError, VolumeIdx};

use crate::board::sdcard::SdStorage;

pub fn list_root_dir<SPI>(
    sd: &mut SdStorage<SPI>,
    mut f: impl FnMut(&str, u32, bool),
) -> Result<u32, Error<SdCardError>>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd.volume_mgr.open_volume(VolumeIdx(0))?;
    let root = volume.open_root_dir()?;

    let mut count = 0u32;
    root.iterate_dir(|entry| {
        let name = entry.name.base_name();
        let ext = entry.name.extension();
        let is_dir = entry.attributes.is_directory();
        let size = entry.size;

        // Format "NAME.EXT" into a stack buffer (8.3 = max 12 chars)
        let mut buf = [0u8; 13];
        let mut pos = 0;

        for &b in name {
            if b == b' ' {
                break;
            }
            if pos < buf.len() {
                buf[pos] = b;
                pos += 1;
            }
        }

        if ext[0] != b' ' {
            if pos < buf.len() {
                buf[pos] = b'.';
                pos += 1;
            }
            for &b in ext {
                if b == b' ' {
                    break;
                }
                if pos < buf.len() {
                    buf[pos] = b;
                    pos += 1;
                }
            }
        }

        if let Ok(formatted) = core::str::from_utf8(&buf[..pos]) {
            f(formatted, size, is_dir);
        }

        count += 1;
    })?;

    Ok(count)
}

pub fn file_size<SPI>(
    sd: &mut SdStorage<SPI>,
    name: &str,
) -> Result<u32, Error<SdCardError>>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd.volume_mgr.open_volume(VolumeIdx(0))?;
    let root = volume.open_root_dir()?;
    let file = root.open_file_in_dir(name, Mode::ReadOnly)?;
    Ok(file.length())
}

/// Read an entire file into a buffer. Returns bytes read.
pub fn read_file<SPI>(
    sd: &mut SdStorage<SPI>,
    name: &str,
    buf: &mut [u8],
) -> Result<usize, Error<SdCardError>>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd.volume_mgr.open_volume(VolumeIdx(0))?;
    let root = volume.open_root_dir()?;
    let file = root.open_file_in_dir(name, Mode::ReadOnly)?;

    let mut total = 0;
    while !file.is_eof() && total < buf.len() {
        let n = file.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }

    Ok(total)
}

/// Read a chunk of a file starting at `offset`. Returns bytes read.
pub fn read_file_chunk<SPI>(
    sd: &mut SdStorage<SPI>,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> Result<usize, Error<SdCardError>>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd.volume_mgr.open_volume(VolumeIdx(0))?;
    let root = volume.open_root_dir()?;
    let file = root.open_file_in_dir(name, Mode::ReadOnly)?;
    file.seek_from_start(offset)?;

    let mut total = 0;
    while !file.is_eof() && total < buf.len() {
        let n = file.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }

    Ok(total)
}

/// Write data to a file (create or truncate).
/// File name must be in 8.3 format.
pub fn write_file<SPI>(
    sd: &mut SdStorage<SPI>,
    name: &str,
    data: &[u8],
) -> Result<(), Error<SdCardError>>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd.volume_mgr.open_volume(VolumeIdx(0))?;
    let root = volume.open_root_dir()?;
    let file = root.open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)?;
    file.write(data)?;
    file.flush()?;
    Ok(())
}
