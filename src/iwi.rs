//! IWI image files as shipped by Call of Duty Online (version byte 9), plus Black Ops II's version 27.
//!
//! The header layout is the IW4/IW5 one (OpenAssetTools' "IWI 8"): after the
//! 4-byte tag/version come `u32 flags`, `i8 format`, one unused byte, three
//! `u16` dimensions and four `u32` "file size for picmip" entries. Mip levels
//! follow from the smallest to the largest; cubemaps store all six faces of a
//! level back to back.

use crate::bc::{self, Bc};
use crate::wavelet;
use anyhow::{Result, bail};

/// Header length of the CoDOL (and IW4/IW5) layout.
pub const HEADER_LEN: usize = 32;
/// Longest header of any supported version (Black Ops II's is 64 bytes); read this much to parse any.
pub const MAX_HEADER_LEN: usize = 64;

mod io;
pub use io::*;

/// The game (engine) an IWI version belongs to, from the version byte in the file header.
#[derive(Clone, Copy, Debug)]
pub struct Engine {
    pub game: &'static str,
    pub engine: &'static str,
    /// Whether this build's layout was checked against real files on this machine (otherwise the label is
    /// from published format notes only).
    pub checked: bool,
}

/// Names the game for an IWI version byte. Versions 6 (Call of Duty 4), 8 (Modern Warfare 2), 9 (Call of Duty Online) and 27 (Black Ops II) are checked against files here; the others follow the OpenAssetTools and community format notes.
pub fn engine(version: u8) -> Engine {
    let (game, engine, checked) = match version {
        5 => ("Call of Duty 2", "IW 2.0", false),
        6 => ("Call of Duty 4", "IW 3.0 (World at War uses version 6 too, per format notes)", true),
        8 => ("Modern Warfare 2", "IW 4.0 (Modern Warfare 3 uses version 8 too, per format notes)", true),
        9 => ("Call of Duty Online", "CoDOL engine (Tencent)", true),
        13 => ("Black Ops", "Treyarch T5", false),
        27 => ("Black Ops II", "Treyarch T6", true),
        _ => ("unknown game", "unknown engine", false),
    };
    Engine { game, engine, checked }
}

impl Engine {
    /// One line for the interface: `Call of Duty Online (IWI v9)`.
    pub fn label(&self, version: u8) -> String {
        format!("{} (IWI v{version})", self.game)
    }
}

#[derive(Clone, Debug)]
pub struct Header {
    pub version: u8,
    pub flags: u32,
    pub format: u8,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub sizes: [u32; 4],
    /// Bytes before the first mip level (32, or 64 for version 27).
    pub len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapType {
    Tex2D,
    Cube,
    Volume,
    Tex1D,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Format {
    Bgra,
    Bgr,
    LumAlpha,
    Lum,
    Alpha,
    /// One red channel (Black Ops II's format code 5); shown as grey.
    R8,
    WaveletRgba,
    WaveletRgb,
    WaveletLumAlpha,
    WaveletLum,
    WaveletAlpha,
    Dxt1,
    Dxt3,
    Dxt5,
    Dxn,
}

impl Format {
    pub fn from_code(code: u8) -> Option<Format> {
        Some(match code {
            1 => Format::Bgra,
            2 => Format::Bgr,
            3 => Format::LumAlpha,
            4 => Format::Lum,
            5 => Format::Alpha,
            6 => Format::WaveletRgba,
            7 => Format::WaveletRgb,
            8 => Format::WaveletLumAlpha,
            9 => Format::WaveletLum,
            10 => Format::WaveletAlpha,
            0xB => Format::Dxt1,
            0xC => Format::Dxt3,
            0xD => Format::Dxt5,
            0xE => Format::Dxn,
            _ => return None,
        })
    }

    pub fn code(self) -> u8 {
        match self {
            Format::Bgra => 1,
            Format::Bgr => 2,
            Format::LumAlpha => 3,
            Format::Lum => 4,
            Format::Alpha | Format::R8 => 5,
            Format::WaveletRgba => 6,
            Format::WaveletRgb => 7,
            Format::WaveletLumAlpha => 8,
            Format::WaveletLum => 9,
            Format::WaveletAlpha => 10,
            Format::Dxt1 => 0xB,
            Format::Dxt3 => 0xC,
            Format::Dxt5 => 0xD,
            Format::Dxn => 0xE,
        }
    }

    pub fn is_wavelet(self) -> bool {
        self.wavelet_channels().is_some()
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Bgra => "BGRA8",
            Format::Bgr => "BGR8",
            Format::LumAlpha => "L8A8",
            Format::Lum => "L8",
            Format::Alpha => "A8",
            Format::R8 => "R8",
            Format::WaveletRgba => "Wavelet RGBA",
            Format::WaveletRgb => "Wavelet RGB",
            Format::WaveletLumAlpha => "Wavelet L+A",
            Format::WaveletLum => "Wavelet L",
            Format::WaveletAlpha => "Wavelet A",
            Format::Dxt1 => "DXT1",
            Format::Dxt3 => "DXT3",
            Format::Dxt5 => "DXT5",
            Format::Dxn => "DXN",
        }
    }

    /// Whether the stored data carries an alpha channel.
    pub fn has_alpha(self) -> bool {
        matches!(
            self,
            Format::Bgra
                | Format::LumAlpha
                | Format::Alpha
                | Format::WaveletRgba
                | Format::WaveletLumAlpha
                | Format::WaveletAlpha
                | Format::Dxt1 // one-bit alpha is possible
                | Format::Dxt3
                | Format::Dxt5
        )
    }

    fn bc(self) -> Option<Bc> {
        match self {
            Format::Dxt1 => Some(Bc::Dxt1),
            Format::Dxt3 => Some(Bc::Dxt3),
            Format::Dxt5 => Some(Bc::Dxt5),
            Format::Dxn => Some(Bc::Dxn),
            _ => None,
        }
    }

    /// Wavelet channel count, or None when not a wavelet format.
    fn wavelet_channels(self) -> Option<usize> {
        match self {
            Format::WaveletRgba => Some(4),
            Format::WaveletRgb => Some(3),
            Format::WaveletLumAlpha => Some(2),
            Format::WaveletLum | Format::WaveletAlpha => Some(1),
            _ => None,
        }
    }

    fn raw_bytes_per_pixel(self) -> Option<usize> {
        match self {
            Format::Bgra => Some(4),
            Format::Bgr => Some(3),
            Format::LumAlpha => Some(2),
            Format::Lum | Format::Alpha | Format::R8 => Some(1),
            _ => None,
        }
    }
}

impl Header {
    pub fn parse(b: &[u8]) -> Result<Header> {
        if b.len() < 4 {
            bail!("file too short for an IWI header");
        }
        if &b[0..3] != b"IWi" {
            bail!("not an IWI file");
        }
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u32;
        match b[3] {
            // 8 and 9 share the 32-byte layout
            8 | 9 => {
                if b.len() < HEADER_LEN {
                    bail!("file too short for an IWI header");
                }
                Ok(Header {
                    version: b[3],
                    flags: u32_at(4),
                    format: b[8],
                    width: u16_at(10),
                    height: u16_at(12),
                    depth: u16_at(14),
                    sizes: [u32_at(16), u32_at(20), u32_at(24), u32_at(28)],
                    len: HEADER_LEN,
                })
            }
            // Call of Duty 4: 28 bytes; format and a one-byte flag field, three u16 dimensions, four picmip sizes.
            // The flag bits that decide the data layout (1 no picmip, 2 no mipmaps, 4 cubemap) were checked against
            // every uncompressed/DXT image in the installed game's archives.
            6 => {
                if b.len() < 28 {
                    bail!("file too short for an IWI header");
                }
                let raw = b[5] as u32;
                Ok(Header {
                    version: 6,
                    flags: (raw & 3) | if raw & 4 != 0 { 1 << 16 } else { 0 },
                    format: b[4],
                    width: u16_at(6),
                    height: u16_at(8),
                    depth: u16_at(10),
                    sizes: [u32_at(12), u32_at(16), u32_at(20), u32_at(24)],
                    len: 28,
                })
            }
            // Black Ops II: format and a one-byte flag field first, then the dimensions, and eight picmip sizes.
            // Flag bits seen to matter for the data layout (checked against 13,366 + 9,767 files): 1 no picmip,
            // 2 no mipmaps, 4 cubemap. They are translated into the version 9 bit positions used elsewhere.
            27 => {
                if b.len() < 64 {
                    bail!("file too short for an IWI header");
                }
                let raw = b[5] as u32;
                let flags = (raw & 3) | if raw & 4 != 0 { 1 << 16 } else { 0 };
                Ok(Header {
                    version: 27,
                    flags,
                    format: b[4],
                    width: u16_at(6),
                    height: u16_at(8),
                    depth: u16_at(10),
                    sizes: [u32_at(32), u32_at(36), u32_at(40), u32_at(44)],
                    len: 64,
                })
            }
            v => bail!("IWI version {v} ({}) is not supported", engine(v).game),
        }
    }

    pub fn format(&self) -> Option<Format> {
        // Black Ops II files only use these codes (others are not seen, so not guessed at)
        if self.version == 27 {
            return match self.format {
                1 | 0xB..=0xE => Format::from_code(self.format),
                5 => Some(Format::R8),
                _ => None,
            };
        }
        Format::from_code(self.format)
    }

    pub fn map_type(&self) -> MapType {
        match (self.flags >> 16) & 3 {
            0 => MapType::Tex2D,
            1 => MapType::Cube,
            2 => MapType::Volume,
            _ => MapType::Tex1D,
        }
    }

    pub fn has_mipmaps(&self) -> bool {
        self.flags & 2 == 0
    }

    pub fn is_normal_map(&self) -> bool {
        self.flags & (1 << 18) != 0
    }

    pub fn face_count(&self) -> usize {
        if self.map_type() == MapType::Cube { 6 } else { 1 }
    }

    pub fn mip_count(&self) -> usize {
        if !self.has_mipmaps() {
            return 1;
        }
        let m = self.width.max(self.height).max(1);
        (32 - m.leading_zeros()) as usize
    }

    pub fn mip_dims(&self, level: usize) -> (u32, u32) {
        ((self.width >> level).max(1), (self.height >> level).max(1))
    }

    /// Stored bytes of one face of one mip level.
    fn level_bytes(&self, fmt: Format, level: usize) -> usize {
        let (w, h) = self.mip_dims(level);
        if let Some(bc) = fmt.bc() {
            bc::surface_size(bc, w, h)
        } else if let Some(bpp) = fmt.raw_bytes_per_pixel() {
            w as usize * h as usize * bpp
        } else {
            0
        }
    }

    /// Short flag list for display.
    pub fn flag_names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        let f = self.flags;
        let table: [(u32, &str); 9] = [
            (1, "nopicmip"),
            (2, "nomipmaps"),
            (4, "streaming"),
            (8, "legacy normals"),
            (0x10, "clamp U"),
            (0x20, "clamp V"),
            (1 << 18, "normal map"),
            (1 << 19, "intensity->alpha"),
            (1 << 24, "dynamic"),
        ];
        for (bit, name) in table {
            if f & bit != 0 {
                v.push(name);
            }
        }
        match (f >> 8) & 3 {
            1 => v.push("gamma sRGB"),
            2 => v.push("gamma PWL"),
            3 => v.push("gamma 2.0"),
            _ => {}
        }
        v
    }
}

/// One decoded mip level: `faces` RGBA8 pixel buffers of `width` x `height`.
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub faces: Vec<Vec<u8>>,
}

/// Converts stored bytes of a raw (or wavelet) format to RGBA8, following the
/// Direct3D 9 sampling convention for the narrow formats (A8 -> 0,0,0,a;
/// L8 -> l,l,l,1; A8L8 -> l,l,l,a).
fn to_rgba(fmt: Format, src: &[u8], pixels: usize) -> Vec<u8> {
    let mut out = vec![0u8; pixels * 4];
    match fmt {
        Format::Bgra | Format::WaveletRgba | Format::WaveletRgb => {
            for i in 0..pixels {
                out[i * 4] = src[i * 4 + 2];
                out[i * 4 + 1] = src[i * 4 + 1];
                out[i * 4 + 2] = src[i * 4];
                out[i * 4 + 3] = src[i * 4 + 3];
            }
        }
        Format::Bgr => {
            for i in 0..pixels {
                out[i * 4] = src[i * 3 + 2];
                out[i * 4 + 1] = src[i * 3 + 1];
                out[i * 4 + 2] = src[i * 3];
                out[i * 4 + 3] = 255;
            }
        }
        Format::LumAlpha | Format::WaveletLumAlpha => {
            for i in 0..pixels {
                let l = src[i * 2];
                out[i * 4..i * 4 + 4].copy_from_slice(&[l, l, l, src[i * 2 + 1]]);
            }
        }
        Format::Lum | Format::WaveletLum | Format::R8 => {
            for i in 0..pixels {
                let l = src[i];
                out[i * 4..i * 4 + 4].copy_from_slice(&[l, l, l, 255]);
            }
        }
        Format::Alpha | Format::WaveletAlpha => {
            for i in 0..pixels {
                out[i * 4..i * 4 + 4].copy_from_slice(&[0, 0, 0, src[i]]);
            }
        }
        _ => unreachable!("block formats are decoded by bc::decode"),
    }
    out
}

/// Checks that the header describes something we can decode.
fn check(h: &Header) -> Result<Format> {
    let Some(fmt) = h.format() else { bail!("unknown IWI format code {}", h.format) };
    match h.map_type() {
        MapType::Tex2D | MapType::Cube => {}
        MapType::Volume => bail!("volume textures are not supported"),
        MapType::Tex1D => bail!("1D textures are not supported"),
    }
    if h.width == 0 || h.height == 0 {
        bail!("empty image");
    }
    Ok(fmt)
}

/// Total file size implied by the header for raw / block formats (None for wavelet).
pub fn expected_size(h: &Header) -> Option<usize> {
    let fmt = h.format()?;
    fmt.wavelet_channels().is_none().then(|| {
        h.len + (0..h.mip_count()).map(|l| h.level_bytes(fmt, l) * h.face_count()).sum::<usize>()
    })
}

/// Decodes a single mip level (0 = largest). Cheap for raw and block formats
/// (only that level is touched); wavelet files must decode every level.
pub fn decode_level(data: &[u8], h: &Header, level: usize) -> Result<Level> {
    let fmt = check(h)?;
    let n = h.mip_count();
    if level >= n {
        bail!("mip level {level} out of range (0..{n})");
    }
    let (w, hgt) = h.mip_dims(level);
    let faces = h.face_count();
    let pixels = w as usize * hgt as usize;

    if let Some(ch) = fmt.wavelet_channels() {
        let mut all = wavelet::decode(&data[h.len.min(data.len())..], h.width, h.height, n, faces, ch)?;
        let lvl = all.swap_remove(level);
        let faces = lvl.into_iter().map(|f| to_rgba(fmt, &f, pixels)).collect();
        return Ok(Level { width: w, height: hgt, faces });
    }

    // Levels are stored smallest first, so level L starts after all smaller ones.
    let mut offset = h.len;
    for l in (level + 1..n).rev() {
        offset += h.level_bytes(fmt, l) * faces;
    }
    let size = h.level_bytes(fmt, level);
    let mut out = Vec::with_capacity(faces);
    for f in 0..faces {
        let start = offset + f * size;
        let Some(src) = data.get(start..start + size) else { bail!("file truncated in mip level {level}") };
        out.push(match fmt.bc() {
            Some(bc) => bc::decode(bc, src, w, hgt),
            None => to_rgba(fmt, src, pixels),
        });
    }
    Ok(Level { width: w, height: hgt, faces: out })
}

/// Picks the smallest mip level whose longest side is at least `target`
/// (or level 0 when the image is smaller than that).
pub fn level_for_size(h: &Header, target: u32) -> usize {
    let mut best = 0;
    for l in 0..h.mip_count() {
        let (w, hh) = h.mip_dims(l);
        if w.max(hh) >= target {
            best = l;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(flags: u32, format: u8, w: u16, h: u16, sizes: [u32; 4]) -> Vec<u8> {
        let mut b = b"IWi\x09".to_vec();
        b.extend(flags.to_le_bytes());
        b.push(format);
        b.push(0);
        b.extend(w.to_le_bytes());
        b.extend(h.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        for s in sizes {
            b.extend(s.to_le_bytes());
        }
        b
    }

    #[test]
    fn parses_real_header() {
        // 512x256 DXT1 header from a real file.
        let b = hex("49576909400300000b0000020001010088550100885500008815000088050000");
        let h = Header::parse(&b).unwrap();
        assert_eq!((h.width, h.height, h.depth), (512, 256, 1));
        assert_eq!(h.format(), Some(Format::Dxt1));
        assert_eq!(h.mip_count(), 10);
        assert_eq!(expected_size(&h), Some(h.sizes[0] as usize));
    }

    #[test]
    fn mip_order_is_smallest_first() {
        // 2x2 BGRA with mips 2x2 (16 B) and 1x1 (4 B): the 1x1 level comes first.
        let mut b = header_bytes(0, 1, 2, 2, [HEADER_LEN as u32 + 20, HEADER_LEN as u32 + 4, 0, 0]);
        b.extend([10, 20, 30, 40]); // 1x1: B,G,R,A
        for _ in 0..4 {
            b.extend([1, 2, 3, 4]);
        }
        let h = Header::parse(&b).unwrap();
        let l0 = decode_level(&b, &h, 0).unwrap();
        assert_eq!(&l0.faces[0][0..4], &[3, 2, 1, 4]);
        let l1 = decode_level(&b, &h, 1).unwrap();
        assert_eq!(&l1.faces[0][0..4], &[30, 20, 10, 40]);
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }
}
