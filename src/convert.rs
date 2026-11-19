//! DDS <-> IWI conversion shared by the command line and the window.

use crate::{dds, iwi};
use anyhow::{Context, Result, bail};

#[derive(Clone)]
pub struct Options {
    /// Flag preset: color, normal, ui, clamp or raw (see `iwi::usage_flags`).
    pub usage: String,
    /// Explicit flags; overrides the preset.
    pub flags: Option<u32>,
    /// Build the mip chain when the DDS has only one level.
    pub genmips: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { usage: "color".into(), flags: None, genmips: false }
    }
}

/// Parses `0x341`, `341` (hex) or `833` (decimal is not accepted: flags are hex).
pub fn parse_flags(s: &str) -> Result<u32> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(t, 16).with_context(|| format!("flags must be hexadecimal, got {s:?}"))
}

/// Converts a DDS file to an IWI file. Returns the bytes and a one-line summary.
pub fn dds_to_iwi(dds_bytes: &[u8], opts: &Options) -> Result<(Vec<u8>, String)> {
    let parsed = dds::parse(dds_bytes)?;
    let described = parsed.describe();
    let flags = match opts.flags {
        Some(f) => f,
        None => iwi::usage_flags(&opts.usage).with_context(|| format!("unknown usage {:?} (color, normal, ui, clamp, raw)", opts.usage))?,
    };
    let mut raw = parsed.into_raw(flags)?;
    let full = iwi::full_mip_count(raw.width, raw.height);
    if opts.genmips && raw.mips.len() == 1 && full > 1 {
        iwi::generate_mips(&mut raw)?;
    }
    let n = raw.mips.len();
    if n != 1 && n != full {
        bail!("the DDS has {n} mip levels but {}x{} needs the full chain of {full} (or just 1); re-export it with all mipmaps", raw.width, raw.height);
    }
    let out = iwi::write(&raw)?;
    let flags_written = u32::from_le_bytes([out[4], out[5], out[6], out[7]]);
    let summary = format!("{described} -> IWI {} flags {flags_written:#x}, {} bytes", raw.format.name(), out.len());
    Ok((out, summary))
}

/// Converts an IWI file to DDS. Wavelet images are decompressed on the way.
pub fn iwi_to_dds(iwi_bytes: &[u8]) -> Result<Vec<u8>> {
    let header = iwi::Header::parse(iwi_bytes)?;
    let raw = iwi::read_raw(iwi_bytes, &header)?;
    dds::write(&raw)
}

/// Converts an IWI to a plain-format IWI (wavelet -> uncompressed); other
/// files come back unchanged apart from header normalisation.
pub fn iwi_to_plain(iwi_bytes: &[u8]) -> Result<Vec<u8>> {
    let header = iwi::Header::parse(iwi_bytes)?;
    iwi::write(&iwi::read_raw(iwi_bytes, &header)?)
}
