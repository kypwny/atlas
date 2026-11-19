//! Stock Infinity Ward (IW4 / Modern Warfare 2) fastfiles: the container (plain, signed or unsigned, version 276) and a
//! probe that walks the asset stream with the Call of Duty Online layouts to show where the two games differ.
//!
//! A stock zone has the same 0x28-byte header and script-string pool as a CoDOL one, but its asset list is 8 bytes per
//! entry (type, header pointer) with no per-asset extents, so an asset ends where its walk ends.

use super::replay::Replay;
use super::{AssetEntry, Zone, u32_at};
use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;
use std::collections::BTreeMap;
use std::io::Read;

const BLOCK: usize = 0x2000;
pub const VERSION_IW4_PC: u32 = 276;
/// Call of Duty Online Alpha 2 (0.0.2.2): the same plain container as stock MW2.
pub const VERSION_ALPHA2: u32 = 316;

/// Opens a stock IW4 PC fastfile (`IWffu100` unsigned, or `IWff0100` signed: SHA-256 hash tables between the data blocks,
/// nothing encrypted). Returns the version and the zone image.
pub fn open(raw: &[u8]) -> Result<(u32, Vec<u8>)> {
    if raw.len() < 0x15 + 16 {
        bail!("too short for a fastfile");
    }
    let signed = match &raw[..8] {
        b"IWff0100" => true,
        b"IWffu100" => false,
        other => bail!("not a stock IW fastfile ({:?})", String::from_utf8_lossy(other)),
    };
    let version = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    if version != VERSION_IW4_PC && version != VERSION_ALPHA2 {
        bail!("fastfile version {version} is not a plain IW zone ({VERSION_IW4_PC} or {VERSION_ALPHA2})");
    }
    let body = &raw[0x15..];
    let compressed: Vec<u8> = if signed {
        if &body[..8] != b"IWffs100" {
            bail!("signed fastfile without its authentication block");
        }
        let mut out = Vec::with_capacity(body.len());
        let mut index = 0usize;
        for chunk in body.chunks(BLOCK).skip(1) {
            if index == 0 {
                index = 1; // a hash table
            } else {
                out.extend_from_slice(chunk);
                index += 1;
                if index == 257 {
                    index = 0;
                }
            }
        }
        out
    } else {
        body.to_vec()
    };
    let mut image = Vec::with_capacity(compressed.len() * 4);
    ZlibDecoder::new(&compressed[..]).read_to_end(&mut image).context("inflating the zone")?;
    Ok((version, image))
}

/// What walking a stock zone with CoDOL layouts found.
pub struct Probe {
    pub block_sizes: [u32; 8],
    pub string_count: usize,
    /// (type, header pointer) for every asset in the list.
    pub list: Vec<(u32, u32)>,
    /// Per type: assets walked, assets in the zone.
    pub by_type: BTreeMap<u32, (usize, usize)>,
    /// Index, type, reason and stream offset of the first asset that did not walk (the walk stops there).
    pub failure: Option<(usize, u32, &'static str, usize)>,
    /// Stream offset the walk reached.
    pub reached: usize,
    /// The last few assets walked: (index, type, start, end).
    pub recent: Vec<(usize, u32, usize, usize)>,
    /// How far the walk filled each memory block (compare with `block_sizes`).
    pub peaks: [usize; 8],
    /// String tables, localised strings and raw files found before the walk stopped.
    pub data: Vec<super::replay::DataRec>,
    /// The map's geometry, when the walk got through its world.
    pub world: Option<super::replay::WorldRec>,
    /// Addresses registered for later references to materials (for diagnosis).
    pub material_aliases: Vec<u32>,
    /// How many distinct -2 pointer words the walk read (the loader reserves a slot for each).
    pub insert_words: usize,
    /// Image names of every material walked, by material name.
    pub material_textures: std::collections::HashMap<String, crate::modelindex::SurfaceTextures>,
    /// The Com world's primary lights and the GfxWorld's hero lights the walk met.
    pub lights: super::world::MapLights,
}

/// The map a stock zone holds, as a viewer model with its material names.
pub fn model_of_image(image: &[u8]) -> Option<(crate::model::Model, Vec<String>, Vec<crate::modelindex::SurfaceTextures>)> {
    map_of_image(image).map(|(m, n, t, _)| (m, n, t))
}

/// Like [`model_of_image`], with the map's lights.
pub fn map_of_image(image: &[u8]) -> Option<(crate::model::Model, Vec<String>, Vec<crate::modelindex::SurfaceTextures>, super::world::MapLights)> {
    let p = probe(image, 334).ok()?;
    let (model, names) = super::world::world_model(image, p.world.as_ref()?);
    let textures = super::world::textures_for(&names, &p.material_textures);
    Some((model, names, textures, p.lights))
}

/// The data records of a stock zone (what the walk reached when it stopped, if it stopped).
pub fn probe_data(image: &[u8]) -> Result<Vec<super::replay::DataRec>> {
    Ok(probe(image, 334)?.data)
}

/// Walks `image` as `layout` (a CoDOL fastfile version choosing the structure sizes: 334 is the 1.1.1.13 client, which is
/// closest to IW4; 423 is QA 3.3.3.3) until an asset does not fit.
pub fn probe(image: &[u8], layout: u32) -> Result<Probe> {
    if image.len() < 0x38 || u32_at(image, 0) as usize != image.len() - 0x28 {
        bail!("zone size field does not match the image");
    }
    let mut block_sizes = [0u32; 8];
    for (i, b) in block_sizes.iter_mut().enumerate() {
        *b = u32_at(image, 8 + 4 * i);
    }
    let string_count = u32_at(image, 0x28) as usize;
    let asset_count = u32_at(image, 0x30) as usize;
    // a 64-bit zone has a zero word where the 32-bit one has the string pointer
    if string_count > 0 && u32_at(image, 0x2c) == 0 {
        bail!("a 64-bit zone: only the 32-bit layouts are read");
    }
    let mut pos = 0x38 + 4 * string_count;
    for i in 0..string_count {
        if u32_at(image, 0x38 + 4 * i) == u32::MAX {
            pos += image[pos..].iter().position(|&c| c == 0).context("unterminated script string")? + 1;
        }
    }
    let table_offset = pos;
    if table_offset + 8 * asset_count > image.len() {
        bail!("asset list runs past the image");
    }
    let list: Vec<(u32, u32)> = (0..asset_count).map(|i| (u32_at(image, table_offset + 8 * i), u32_at(image, table_offset + 8 * i + 4))).collect();
    let mut by_type: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    for (t, _) in &list {
        by_type.entry(*t).or_default().1 += 1;
    }
    let assets = list.iter().map(|&(t, r)| AssetEntry { asset_type: t, start: 0, extent: 0, reference: r }).collect();
    let zone = Zone { version: layout, data: image.to_vec(), block_sizes, string_count, table_offset, assets };
    let mut r = Replay::new_stock(&zone).map_err(|f| anyhow::anyhow!("prefix: {} at {}", f.what, f.at))?;
    let mut failure = None;
    let mut recent: Vec<(usize, u32, usize, usize)> = Vec::new();
    for (i, &(t, reference)) in list.iter().enumerate() {
        let start = r.pos;
        match r.walk_stream_asset(i, t, reference) {
            Ok(end) => {
                recent.push((i, t, start, end));
                if recent.len() > 6 {
                    recent.remove(0);
                }
                r.pos = end;
                by_type.entry(t).or_default().0 += 1;
            }
            Err(f) => {
                failure = Some((i, t, f.what, f.at));
                break;
            }
        }
    }
    let reached = r.pos;
    let data = std::mem::take(&mut r.rec.data);
    let world = r.rec.world.take();
    let lights = std::mem::take(&mut r.rec.lights);
    let material_textures = super::world::material_textures(&r.rec);
    let mut material_aliases: Vec<u32> = r.rec.aliases.keys().filter(|k| k.0 == 5).map(|k| k.1).collect();
    material_aliases.sort();
    Ok(Probe { block_sizes, string_count, list, by_type, failure, reached, recent, peaks: r.block_positions(), data, world, material_aliases, insert_words: r.insert_words.borrow().len(), material_textures, lights })
}
