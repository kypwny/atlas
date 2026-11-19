//! Loading an image by name as a GPU-ready mip chain.

use crate::iwi;
use crate::source::Catalog;
use anyhow::{Context, Result, bail};

pub struct MipLevel {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8.
    pub rgba: Vec<u8>,
}

pub struct TexImage {
    /// Level 0 is the largest.
    pub mips: Vec<MipLevel>,
    pub clamp_u: bool,
    pub clamp_v: bool,
}

/// Decodes the named image (case-insensitive) from the catalogue. When the
/// name exists in several sources (for example `main` and `hires`), the
/// largest version is used.
pub fn load_image(catalog: &Catalog, name: &str) -> Result<TexImage> {
    // A leading comma marks a name defined in another zone; the image itself is stored without it.
    let key = name.strip_prefix(',').unwrap_or(name).to_ascii_lowercase();
    let i = catalog.entries.binary_search_by(|e| e.name.as_str().cmp(key.as_str())).map_err(|_| anyhow::anyhow!("image {name:?} is not installed"))?;
    let entry = &catalog.entries[i];
    let variant = entry
        .variants
        .iter()
        .filter(|v| v.header.is_some())
        .max_by_key(|v| {
            let h = v.header.as_ref().unwrap();
            h.width as u64 * h.height as u64
        })
        .with_context(|| format!("{name}: no readable version"))?;
    let header = variant.header.as_ref().unwrap();
    let data = catalog.providers[variant.provider].read(&variant.key)?;
    if header.map_type() != iwi::MapType::Tex2D && header.map_type() != iwi::MapType::Cube {
        bail!("{name}: unsupported texture type");
    }
    let mut mips = Vec::new();
    for level in 0..header.mip_count() {
        let l = iwi::decode_level(&data, header, level)?;
        mips.push(MipLevel { width: l.width, height: l.height, rgba: l.faces.into_iter().next().unwrap() });
    }
    Ok(TexImage { mips, clamp_u: header.flags & 0x10 != 0, clamp_v: header.flags & 0x20 != 0 })
}
