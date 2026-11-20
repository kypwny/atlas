//! CoDOL zone images: the container (`fastfile`), the directory (this file) and the stream replay.

pub mod convert;
pub mod convert_basic;
pub mod convert_cli;
pub mod convert_clip;
pub mod convert_codol;
pub mod convert_dev;
pub mod convert_engine;
pub mod convert_fx;
pub mod convert_from_codol;
pub mod convert_gfx;
pub mod convert_iw3;
pub mod convert_iw5;
pub mod convert_model;
pub mod convert_sound;
pub mod convert_walk;
pub mod convert_world;
pub mod convert_zone;
pub mod export;
pub mod fastfile;
pub mod hq;
pub mod replay;
pub mod stock;
pub mod t6;
pub mod t6_anims;
pub mod t6_models;
pub mod t6_files;
pub mod t6_rebuild;
pub mod t6_write;
pub mod t6_world;
pub mod schema_walk;
pub mod world;
pub mod writer;
mod replay_more;
mod replay_world;

use anyhow::{Result, bail};

pub fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub fn u16_at(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}

/// One directory entry.
#[derive(Clone, Copy, Debug)]
pub struct AssetEntry {
    pub asset_type: u32,
    /// Offset of the asset's first byte in the zone image.
    pub start: usize,
    pub extent: usize,
    pub reference: u32,
}

/// An inflated zone: 40-byte header, block sizes, script-string pool, asset directory, then the asset stream.
pub struct Zone {
    pub version: u32,
    pub data: Vec<u8>,
    pub block_sizes: [u32; 8],
    pub string_count: usize,
    pub table_offset: usize,
    pub assets: Vec<AssetEntry>,
}

impl Zone {
    /// Validates the layout like the server loader (`CODOL_OpenZoneImage`): the directory's extents must tile the
    /// stream exactly.
    pub fn from_image(version: u32, data: Vec<u8>) -> Result<Zone> {
        let size = data.len();
        if size < 0x38 || u32_at(&data, 0) as usize != size - 0x28 {
            bail!("zone size field does not match the image ({size} bytes)");
        }
        let string_count = u32_at(&data, 0x28) as usize;
        let asset_count = u32_at(&data, 0x30) as usize;
        let mut block_sizes = [0u32; 8];
        for (i, b) in block_sizes.iter_mut().enumerate() {
            *b = u32_at(&data, 8 + 4 * i);
        }
        let pointers = 0x38;
        if string_count > (size - pointers) / 4 {
            bail!("string count out of range");
        }
        let mut pos = pointers + string_count * 4;
        for i in 0..string_count {
            if u32_at(&data, pointers + 4 * i) == u32::MAX {
                let Some(n) = data[pos..].iter().position(|&c| c == 0) else { bail!("unterminated script string") };
                pos += n + 1;
            }
        }
        let table_offset = pos;
        if asset_count > (size - pos) / 16 {
            bail!("asset count out of range");
        }
        let mut end = pos + asset_count * 16;
        let mut assets = Vec::with_capacity(asset_count);
        for i in 0..asset_count {
            let e = pos + 16 * i;
            let (offset, extent) = (u32_at(&data, e + 4) as usize, u32_at(&data, e + 12) as usize);
            if offset > size - 0x28 || offset + 0x28 != end || extent > size - end {
                bail!("asset {i}: extent does not continue the stream");
            }
            assets.push(AssetEntry { asset_type: u32_at(&data, e), start: offset + 0x28, extent, reference: u32_at(&data, e + 8) });
            end += extent;
        }
        if end != size {
            bail!("asset extents end at {end}, image is {size} bytes");
        }
        Ok(Zone { version, data, block_sizes, string_count, table_offset, assets })
    }

    pub fn open(path: &std::path::Path, key: &fastfile::ArchiveKey) -> Result<Zone> {
        let o = fastfile::open(path, key)?;
        Zone::from_image(o.version, o.image)
    }

    /// Script strings in order; `None` for entries that reference another zone's pool.
    pub fn script_strings(&self) -> Vec<Option<String>> {
        let mut pos = 0x38 + self.string_count * 4;
        (0..self.string_count)
            .map(|i| {
                if u32_at(&self.data, 0x38 + 4 * i) == u32::MAX {
                    let n = self.data[pos..].iter().position(|&c| c == 0).unwrap();
                    let s = String::from_utf8_lossy(&self.data[pos..pos + n]).into_owned();
                    pos += n + 1;
                    Some(s)
                } else {
                    None
                }
            })
            .collect()
    }
}
