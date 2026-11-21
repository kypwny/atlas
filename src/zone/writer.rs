//! Zone image writer: the inverse of the reader. A zone is taken apart into its prefix (script strings) and assets, the
//! assets are edited (replaced, removed, inserted), and the image is put back together with a new directory, block sizes
//! and relocated references.
//!
//! The stream addresses things in "block 3" memory: script strings, the directory and every persistent allocation, in the
//! order the loader makes them. Words that point at earlier memory (names, asset references) therefore change whenever
//! something before them grows or shrinks. The walker records every such word (`Replay::alias_words`) and every block-3
//! allocation; the old address of each unchanged allocation is paired with its new one and the words are rewritten.
//! Assets supplied by the caller are taken as they are: they may point at script strings (which never move) but at
//! nothing whose address depends on the edit.

use super::replay::Replay;
use super::{AssetEntry, Zone, u32_at};
use anyhow::{Result, bail};
use std::collections::HashMap;

#[derive(Clone)]
pub struct BuilderAsset {
    pub asset_type: u32,
    pub reference: u32,
    pub bytes: Vec<u8>,
    /// Index in the zone this was taken from; `None` for assets the caller made.
    pub origin: Option<usize>,
    /// Set when the caller changed the bytes: the walker's allocations for it are not paired with the old ones.
    pub fresh: bool,
}

pub struct ZoneBuilder {
    version: u32,
    /// The first 0x28 bytes (block sizes and size are recomputed).
    header: Vec<u8>,
    /// From the string count at 0x28 up to the directory.
    prefix: Vec<u8>,
    original_block_sizes: [u32; 8],
    pub assets: Vec<BuilderAsset>,
}

/// A finished image.
pub struct Built {
    pub image: Vec<u8>,
    pub block_sizes: [u32; 8],
    /// References the final walk could not resolve; compare with the original zone's count.
    pub dangling: usize,
    /// Words rewritten to a new address.
    pub relocated: usize,
}

struct Block3 {
    /// (address, length, index of the asset in its own zone's list)
    starts: Vec<(u32, usize)>,
    per_asset: HashMap<usize, Vec<usize>>,
}

/// Walks every asset with tracing on; returns the walk's block peaks, the allocations and the alias words.
struct Walked {
    peaks: [usize; 8],
    block3: Block3,
    aliases: Vec<super::replay::AliasWord>,
    dangling: usize,
    directory_reference: u32,
}

fn walk(zone: &Zone) -> Result<Walked> {
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("zone prefix: {} at {}", f.what, f.at))?;
    r.enable_tracing();
    for i in 0..zone.assets.len() {
        r.replay_asset(zone, i).map_err(|f| anyhow::anyhow!("asset {i} (type {}): {} at {:#x}", zone.assets[i].asset_type, f.what, f.at))?;
    }
    let mut block3 = Block3 { starts: Vec::new(), per_asset: HashMap::new() };
    for &(asset, address, length) in r.block3_allocations() {
        block3.per_asset.entry(asset).or_default().push(block3.starts.len());
        block3.starts.push((address, length));
    }
    Ok(Walked { peaks: r.block_positions(), block3, aliases: r.alias_words(), dangling: r.dangling_references().len(), directory_reference: r.directory_reference })
}

impl ZoneBuilder {
    pub fn from_zone(z: &Zone) -> ZoneBuilder {
        ZoneBuilder {
            version: z.version,
            header: z.data[..0x28].to_vec(),
            prefix: z.data[0x28..z.table_offset].to_vec(),
            original_block_sizes: z.block_sizes,
            assets: z
                .assets
                .iter()
                .enumerate()
                .map(|(i, e)| BuilderAsset { asset_type: e.asset_type, reference: e.reference, bytes: z.data[e.start..e.start + e.extent].to_vec(), origin: Some(i), fresh: false })
                .collect(),
        }
    }

    pub fn replace(&mut self, index: usize, bytes: Vec<u8>) {
        let a = &mut self.assets[index];
        a.bytes = bytes;
        a.fresh = true;
    }

    /// Replaces an asset's bytes with an edit that keeps its structure (the same arrays in the same order, only some of them
    /// longer or shorter, such as a text nested in it). The asset stays "not fresh": its own pointers move with whatever it
    /// grew past, and what points into it follows its allocations. `build` refuses the result when the number of block-3
    /// allocations of the asset changed.
    pub fn replace_same_layout(&mut self, index: usize, bytes: Vec<u8>) {
        self.assets[index].bytes = bytes;
    }

    pub fn remove(&mut self, index: usize) {
        self.assets.remove(index);
    }

    pub fn insert(&mut self, at: usize, asset_type: u32, reference: u32, bytes: Vec<u8>) {
        self.assets.insert(at, BuilderAsset { asset_type, reference, bytes, origin: None, fresh: true });
    }

    /// Lays the parts out: header, script strings, directory, asset bytes.
    fn assemble(&self, sizes: [u32; 8]) -> Vec<u8> {
        let mut out = self.header.clone();
        for (i, s) in sizes.iter().enumerate() {
            out[8 + 4 * i..12 + 4 * i].copy_from_slice(&s.to_le_bytes());
        }
        let mut prefix = self.prefix.clone();
        prefix[8..12].copy_from_slice(&(self.assets.len() as u32).to_le_bytes());
        out.extend_from_slice(&prefix);
        let mut offset = out.len() + 16 * self.assets.len() - 0x28;
        for a in &self.assets {
            out.extend_from_slice(&a.asset_type.to_le_bytes());
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            out.extend_from_slice(&a.reference.to_le_bytes());
            out.extend_from_slice(&(a.bytes.len() as u32).to_le_bytes());
            offset += a.bytes.len();
        }
        for a in &self.assets {
            out.extend_from_slice(&a.bytes);
        }
        let size = (out.len() - 0x28) as u32;
        out[0..4].copy_from_slice(&size.to_le_bytes());
        out
    }

    /// Builds the image. `original` is the zone this builder was made from (its walk gives the old addresses).
    pub fn build(&self, original: &Zone) -> Result<Built> {
        // pass 1: lay out with the old block sizes plus room, walk it, learn the new footprint
        let mut roomy = self.original_block_sizes;
        for s in roomy.iter_mut() {
            *s = (*s + 0x80_0000).min(0x0fff_ffff);
        }
        let mut image = self.assemble(roomy);
        let first = Zone::from_image(self.version, image.clone())?;
        let old = walk(original)?;
        let new = walk(&first)?;

        // old address -> new address, from the allocations of assets whose bytes were not touched
        let mut pairs: Vec<(u32, usize, u32)> = Vec::new(); // (old start, length, new start)
        for (j, a) in self.assets.iter().enumerate() {
            let (Some(o), false) = (a.origin, a.fresh) else { continue };
            let (Some(olds), Some(news)) = (old.block3.per_asset.get(&o), new.block3.per_asset.get(&j)) else { continue };
            if olds.len() != news.len() {
                bail!("asset {j}: {} block-3 allocations before, {} after", olds.len(), news.len());
            }
            for (&x, &y) in olds.iter().zip(news) {
                pairs.push((old.block3.starts[x].0, old.block3.starts[x].1, new.block3.starts[y].0));
            }
        }
        pairs.sort_by_key(|p| p.0);
        // old directory slot -> new slot (an asset's own address is a slot in the directory)
        let mut slots: HashMap<u32, u32> = HashMap::new();
        for (j, a) in self.assets.iter().enumerate() {
            if let Some(o) = a.origin {
                slots.insert(old.directory_reference + 16 * o as u32 + 8, new.directory_reference + 16 * j as u32 + 8);
            }
        }
        let first_alloc = old.block3.starts.first().map(|s| s.0).unwrap_or(u32::MAX);
        let fresh: Vec<bool> = self.assets.iter().map(|a| a.fresh).collect();
        let mut relocated = 0usize;
        let mut lost = Vec::new();
        for w in &new.aliases {
            if fresh.get(w.asset).copied().unwrap_or(true) {
                continue;
            }
            let v = w.value;
            let target = if let Some(n) = slots.get(&v) {
                *n
            } else if v < first_alloc && v >= old.directory_reference && v < old.directory_reference + 16 * original.assets.len() as u32 {
                lost.push((w.asset, v));
                continue;
            } else if v < first_alloc {
                v // script strings sit in front of everything that can change
            } else {
                let k = pairs.partition_point(|p| p.0 <= v);
                match k.checked_sub(1).map(|k| pairs[k]) {
                    Some((start, len, to)) if (v - start) as usize <= len => to + (v - start),
                    _ => {
                        lost.push((w.asset, v));
                        continue;
                    }
                }
            };
            if target != v {
                image[w.at..w.at + 4].copy_from_slice(&target.to_le_bytes());
                relocated += 1;
            }
        }
        if !lost.is_empty() {
            bail!("{} references point at memory that no longer exists, e.g. asset {} -> {:#x}", lost.len(), lost[0].0, lost[0].1);
        }

        // pass 2: record the real sizes and check the finished image
        // a few zones declare more room than the walk uses (a type-42 record whose tail is not decoded): keep that margin
        let mut sizes = [0u32; 8];
        for (b, s) in sizes.iter_mut().enumerate() {
            let own = old.peaks[b] as u32 + if b == 0 { 16 } else { 0 };
            let slack = self.original_block_sizes[b].saturating_sub(own);
            *s = new.peaks[b] as u32 + if b == 0 { 16 } else { 0 } + slack;
        }
        for (i, s) in sizes.iter().enumerate() {
            image[8 + 4 * i..12 + 4 * i].copy_from_slice(&s.to_le_bytes());
        }
        let done = Zone::from_image(self.version, image)?;
        let check = walk(&done)?;
        for b in 0..8 {
            if check.peaks[b] != new.peaks[b] {
                bail!("block {b} footprint changed from {} to {} after relocation", new.peaks[b], check.peaks[b]);
            }
        }
        Ok(Built { image: done.data, block_sizes: sizes, dangling: check.dangling, relocated })
    }
}

/// How many references the zone's own walk could not resolve (the baseline for `Built::dangling`).
pub fn baseline_dangling(zone: &Zone) -> Result<usize> {
    Ok(walk(zone)?.dangling)
}

/// The assets of an image as plain entries (for callers that only want the directory).
pub fn directory(image: &[u8], version: u32) -> Result<Vec<AssetEntry>> {
    let _ = u32_at(image, 0);
    Ok(Zone::from_image(version, image.to_vec())?.assets)
}
