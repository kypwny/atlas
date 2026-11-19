//! Reading IWI files into stored form and writing them back (DDS import/export
//! and conversions build on this).

use super::*;

/// An image in its stored (not RGBA) form, independent of the file layout:
/// `mips[level][face]`, level 0 largest.
#[derive(Clone, Debug)]
pub struct Raw {
    pub flags: u32,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub mips: Vec<Vec<Vec<u8>>>,
}

/// Bytes of one stored surface; None for wavelet formats.
pub fn surface_bytes(fmt: Format, w: u32, h: u32) -> Option<usize> {
    if let Some(bc) = fmt.bc() {
        Some(bc::surface_size(bc, w, h))
    } else {
        fmt.raw_bytes_per_pixel().map(|bpp| w as usize * h as usize * bpp)
    }
}

/// Length of a full mip chain down to 1x1.
pub fn full_mip_count(w: u32, h: u32) -> usize {
    (32 - w.max(h).max(1).leading_zeros()) as usize
}

/// Reads every level in stored form. Wavelet images are decompressed into the
/// matching plain format (RGBA -> BGRA8, RGB -> BGR8, L+A -> L8A8, ...).
pub fn read_raw(data: &[u8], h: &Header) -> Result<Raw> {
    let fmt = check(h)?;
    let n = h.mip_count();
    let faces = h.face_count();
    if let Some(ch) = fmt.wavelet_channels() {
        let all = wavelet::decode(&data[h.len.min(data.len())..], h.width, h.height, n, faces, ch)?;
        let (out_fmt, drop4) = match fmt {
            Format::WaveletRgba => (Format::Bgra, false),
            Format::WaveletRgb => (Format::Bgr, true),
            Format::WaveletLumAlpha => (Format::LumAlpha, false),
            Format::WaveletLum => (Format::Lum, false),
            _ => (Format::Alpha, false),
        };
        let mips = all
            .into_iter()
            .map(|lvl| {
                lvl.into_iter()
                    .map(|f| if drop4 { f.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect() } else { f })
                    .collect()
            })
            .collect();
        return Ok(Raw { flags: h.flags, format: out_fmt, width: h.width, height: h.height, mips });
    }
    let mut mips: Vec<Vec<Vec<u8>>> = vec![Vec::new(); n];
    let mut offset = h.len;
    for l in (0..n).rev() {
        let size = h.level_bytes(fmt, l);
        for _ in 0..faces {
            let Some(src) = data.get(offset..offset + size) else { bail!("file truncated in mip level {l}") };
            mips[l].push(src.to_vec());
            offset += size;
        }
    }
    Ok(Raw { flags: h.flags, format: fmt, width: h.width, height: h.height, mips })
}

/// Serialises to an IWI version 9 file.
///
/// Cubemap and no-mipmap flags are derived from the data; every other flag is
/// taken from `raw.flags`. The four "file size for picmip" fields are the full
/// file size when the no-picmip flag is set (what 99% of shipped files do) and
/// the size without the largest N levels otherwise.
pub fn write(raw: &Raw) -> Result<Vec<u8>> {
    if raw.format.is_wavelet() {
        bail!("cannot write wavelet formats; convert to a plain format first");
    }
    if raw.width == 0 || raw.height == 0 || raw.width > 65535 || raw.height > 65535 {
        bail!("image size {}x{} is out of range", raw.width, raw.height);
    }
    let n = raw.mips.len();
    let full = full_mip_count(raw.width, raw.height);
    if n == 0 || (n != 1 && n != full) {
        bail!("{n} mip levels, but an IWI needs either 1 or the full chain of {full} for {}x{}", raw.width, raw.height);
    }
    let faces = raw.mips[0].len();
    if faces != 1 && faces != 6 {
        bail!("{faces} faces; only plain 2D images and 6-face cubemaps are supported");
    }
    let mut flags = raw.flags & !(3 << 16);
    if faces == 6 {
        flags |= 1 << 16;
    }
    if n > 1 {
        flags &= !2;
    } else if full > 1 {
        flags |= 2;
    }
    let mut level_total = vec![0usize; n];
    for (l, level) in raw.mips.iter().enumerate() {
        if level.len() != faces {
            bail!("mip level {l} has {} faces, level 0 has {faces}", level.len());
        }
        let (w, h) = ((raw.width >> l).max(1), (raw.height >> l).max(1));
        let want = surface_bytes(raw.format, w, h).unwrap();
        for f in level {
            if f.len() != want {
                bail!("mip level {l} ({w}x{h}) has {} bytes, {} needs {want}", f.len(), raw.format.name());
            }
        }
        level_total[l] = want * faces;
    }
    let total = HEADER_LEN + level_total.iter().sum::<usize>();
    let mut sizes = [0u32; 4];
    for (i, s) in sizes.iter_mut().enumerate() {
        *s = if flags & 1 != 0 { total } else { HEADER_LEN + level_total[i.min(n - 1)..].iter().sum::<usize>() } as u32;
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"IWi\x09");
    out.extend_from_slice(&flags.to_le_bytes());
    out.push(raw.format.code());
    out.push(0);
    out.extend_from_slice(&(raw.width as u16).to_le_bytes());
    out.extend_from_slice(&(raw.height as u16).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    for s in sizes {
        out.extend_from_slice(&s.to_le_bytes());
    }
    for level in raw.mips.iter().rev() {
        for f in level {
            out.extend_from_slice(f);
        }
    }
    Ok(out)
}

/// One box-filtered RGBA level from the previous one: a plain 2x2 average of
/// every channel. (Weighting colour by alpha was tried and agrees *less* with
/// the mip chains shipped in the game files.)
fn downsample_rgba(src: &[u8], pw: usize, ph: usize, w: usize, h: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (x0, y0) = ((2 * x).min(pw - 1), (2 * y).min(ph - 1));
            let (x1, y1) = ((2 * x + 1).min(pw - 1), (2 * y + 1).min(ph - 1));
            for c in 0..4 {
                let sum = src[(y0 * pw + x0) * 4 + c] as u32 + src[(y0 * pw + x1) * 4 + c] as u32 + src[(y1 * pw + x0) * 4 + c] as u32 + src[(y1 * pw + x1) * 4 + c] as u32;
                dst[(y * w + x) * 4 + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    dst
}

/// Builds the mip chain of a block-compressed image. Level 0 is kept exactly as
/// it is; every lower level is box-filtered from the decoded level above and
/// re-encoded.
fn generate_block_mips(raw: &mut Raw, kind: bc::Bc) {
    let mut current: Vec<Vec<u8>> = raw.mips[0].iter().map(|f| bc::decode(kind, f, raw.width, raw.height)).collect();
    for l in 1..full_mip_count(raw.width, raw.height) {
        let (pw, ph) = ((raw.width >> (l - 1)).max(1) as usize, (raw.height >> (l - 1)).max(1) as usize);
        let (w, h) = ((raw.width >> l).max(1) as usize, (raw.height >> l).max(1) as usize);
        let next: Vec<Vec<u8>> = current.iter().map(|f| downsample_rgba(f, pw, ph, w, h)).collect();
        raw.mips.push(next.iter().map(|f| bc::encode(kind, f, w as u32, h as u32)).collect());
        current = next;
    }
}

/// Builds a full mip chain from level 0 with a 2x2 box filter. Plain formats
/// are averaged directly; block formats are decoded, filtered and re-encoded
/// (level 0 is left untouched).
pub fn generate_mips(raw: &mut Raw) -> Result<()> {
    raw.mips.truncate(1);
    if let Some(kind) = raw.format.bc() {
        generate_block_mips(raw, kind);
        return Ok(());
    }
    let Some(bpp) = raw.format.raw_bytes_per_pixel() else {
        bail!("{} cannot be mip-mapped", raw.format.name());
    };
    for l in 1..full_mip_count(raw.width, raw.height) {
        let (pw, ph) = ((raw.width >> (l - 1)).max(1) as usize, (raw.height >> (l - 1)).max(1) as usize);
        let (w, h) = ((raw.width >> l).max(1) as usize, (raw.height >> l).max(1) as usize);
        let mut level = Vec::new();
        for src in &raw.mips[l - 1] {
            let mut dst = vec![0u8; w * h * bpp];
            for y in 0..h {
                for x in 0..w {
                    let (x0, y0) = ((2 * x).min(pw - 1), (2 * y).min(ph - 1));
                    let (x1, y1) = ((2 * x + 1).min(pw - 1), (2 * y + 1).min(ph - 1));
                    for c in 0..bpp {
                        let sum = src[(y0 * pw + x0) * bpp + c] as u32
                            + src[(y0 * pw + x1) * bpp + c] as u32
                            + src[(y1 * pw + x0) * bpp + c] as u32
                            + src[(y1 * pw + x1) * bpp + c] as u32;
                        dst[(y * w + x) * bpp + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
            level.push(dst);
        }
        raw.mips.push(level);
    }
    Ok(())
}

/// Flag presets taken from how shipped files are flagged (QA build survey).
pub fn usage_flags(name: &str) -> Option<u32> {
    Some(match name {
        // nopicmip | alpha-weighted | gamma 2.0 - 4,675 of the `_col` images
        "color" => 0x341,
        // nopicmip | normal map - 2,837 of the `_nml` images
        "normal" => 0x40001,
        // nopicmip | no mipmaps | clamp U/V | alpha-weighted | sRGB - the most common UI flags
        "ui" => 0x173,
        // like ui but with mipmaps
        "clamp" => 0x171,
        "raw" => 0,
        _ => return None,
    })
}
