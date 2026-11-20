//! Call of Duty Online (QA 3.3.3.3, 3.19, 1.1.1.13) -> stock IW4.
//!
//! What is converted is what the two games lay out the same way: the types whose QA structure is byte for byte the stock one
//! (physics collision maps, animations, shaders, vertex declarations, sound curves, the common and effect worlds, light
//! definitions, fonts, localized strings, raw files and string tables). Each such asset is checked on its own: its stream is put
//! into a stock-layout zone and read by the stock reader, and it is kept only when that walk is exact (an asset that points at
//! data elsewhere in the QA zone fails there and is left out). The other types have different headers in QA (models, materials,
//! images, technique sets, ...) or different loaders (the collision map and the game world) and are not read yet.

use super::convert_cli::{engine_hop, verify, Fmt, HopOut, Input};
use super::convert_zone::TypeStat;
use super::schema_walk::{self, Game, ZoneImage};
use super::{u32_at, Zone};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

/// Asset types whose QA stream is the stock stream (the numbers are the same in both games).
const IDENTICAL: &[u32] = &[1, 2, 6, 7, 8, 12, 16, 20, 22, 24, 27, 36, 37];

/// A stock-layout zone image: the script strings of the QA zone, then the asset list with the given assets.
fn draft(zone: &Zone, assets: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = zone.data[..zone.table_offset].to_vec();
    // block sizes: roomy, the engine pass that follows writes the real ones
    let total: usize = assets.iter().map(|a| a.1.len()).sum::<usize>() + out.len() + 0x10_0000;
    let sizes = [0x40_0000u32, 0, 0, total as u32, 0, 0, 0, 0];
    for (i, s) in sizes.iter().enumerate() {
        out[8 + 4 * i..12 + 4 * i].copy_from_slice(&s.to_le_bytes());
    }
    out[0x30..0x34].copy_from_slice(&(assets.len() as u32).to_le_bytes());
    for (ty, _) in assets {
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    for (_, b) in assets {
        out.extend_from_slice(b);
    }
    let size = (out.len() - 0x28) as u32;
    out[0..4].copy_from_slice(&size.to_le_bytes());
    out
}

/// Walks a draft with the stock reader and writes the block sizes it comes to into the header; the result has to read exactly.
fn fit(name: &str, mut image: Vec<u8>) -> Result<Vec<u8>> {
    let zi = ZoneImage { name: name.to_string(), block_sizes: (0..8).map(|i| u32_at(&image, 8 + 4 * i)).collect(), image: image.clone() };
    let r = schema_walk::walk(Game::Iw4, &zi);
    if let Some(f) = &r.failure {
        bail!("the stock walker stops at asset {} (type {}) at {:#x}: {}", f.asset_index, f.ty, f.offset, f.reason);
    }
    if r.consumed != image.len() {
        bail!("the stock walker used {:#x} of {:#x} bytes", r.consumed, image.len());
    }
    for b in 0..8 {
        let v = if b == 0 { r.temp_peak } else { r.block_end[b] } as u32;
        image[8 + 4 * b..12 + 4 * b].copy_from_slice(&v.to_le_bytes());
    }
    verify(Fmt::Iw4, name, &image)?;
    Ok(image)
}

pub fn codol_to_iw4(input: &Input) -> Result<HopOut> {
    let zone = Zone::from_image(input.version, input.image.clone())?;
    if u32_at(&zone.data, 0x28) as usize != zone.string_count {
        bail!("zone header inconsistent");
    }
    let mut stats: BTreeMap<u32, TypeStat> = BTreeMap::new();
    let mut kept: Vec<(u32, &[u8])> = Vec::new();
    for a in &zone.assets {
        let st = stats.entry(a.asset_type).or_default();
        let bytes = &zone.data[a.start..a.start + a.extent];
        if !IDENTICAL.contains(&a.asset_type) {
            *st.left_out.entry(format!("{} is laid out differently in Call of Duty Online (not read yet)", Fmt::Iw4.type_name(a.asset_type))).or_default() += 1;
            continue;
        }
        if a.reference != u32::MAX && a.reference != u32::MAX - 1 {
            *st.left_out.entry("its entry refers to data elsewhere".to_string()).or_default() += 1;
            continue;
        }
        match fit(&input.name, draft(&zone, &[(a.asset_type, bytes)])) {
            Ok(_) => {
                st.converted += 1;
                kept.push((a.asset_type, bytes));
            }
            Err(e) => {
                let why = format!("{e:#}");
                let why = why.split(" at ").next().unwrap_or("").to_string();
                *st.left_out.entry(format!("does not read as a stock {} ({why})", Fmt::Iw4.type_name(a.asset_type))).or_default() += 1;
            }
        }
    }
    let image = fit(&input.name, draft(&zone, &kept))?;
    let stock = Input { fmt: Fmt::Iw4, version: 276, name: input.name.clone(), image };
    // the stock pass reads the draft and writes it again with its real block sizes
    let mut out = engine_hop(Game::Iw4, Game::Iw4, Default::default(), &stock)?;
    out.stats = stats;
    Ok(out)
}
