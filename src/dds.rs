//! DirectDraw Surface (.dds) import and export for IWI images.
//!
//! Supported on the way in: DXT1/3/5, ATI2/BC5, 8/16/24/32-bit uncompressed
//! (any channel order), and the DX10 header for BC1/2/3/5, B8G8R8A8,
//! R8G8B8A8, R8 and A8. Cubemaps need all six faces; volume textures and
//! texture arrays are rejected.

use crate::iwi::{self, Format, Raw};
use anyhow::{Result, bail};

const DDSD_CAPS: u32 = 0x1;
const DDSD_HEIGHT: u32 = 0x2;
const DDSD_WIDTH: u32 = 0x4;
const DDSD_PITCH: u32 = 0x8;
const DDSD_PIXELFORMAT: u32 = 0x1000;
const DDSD_MIPMAPCOUNT: u32 = 0x20000;
const DDSD_LINEARSIZE: u32 = 0x80000;

const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x20000;

const CAPS_COMPLEX: u32 = 0x8;
const CAPS_TEXTURE: u32 = 0x1000;
const CAPS_MIPMAP: u32 = 0x400000;
const CAPS2_CUBEMAP: u32 = 0x200;
const CAPS2_ALL_FACES: u32 = 0xFC00;
const CAPS2_VOLUME: u32 = 0x200000;

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*s)
}

/// How the stored bytes of a DDS map to an IWI format, plus the channel
/// shuffle needed to get to IWI's BGRA / BGR byte order.
#[derive(Clone, Copy, Debug)]
enum Layout {
    Plain(Format),
    /// `src_bpp` bytes per texel; `map[i]` = source byte for destination byte i
    /// (255 = constant 0xFF). Destination is BGRA (4) or BGR (3).
    Shuffle { src_bpp: usize, map: [u8; 4], out: Format },
}

pub struct Dds {
    pub width: u32,
    pub height: u32,
    layout: Layout,
    /// `data[face][mip]` as stored in the file (largest mip first).
    data: Vec<Vec<Vec<u8>>>,
}

fn rd(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Which byte (0..bpp) a single-byte channel mask selects.
fn mask_byte(mask: u32) -> Option<u8> {
    match mask {
        0xFF => Some(0),
        0xFF00 => Some(1),
        0xFF_0000 => Some(2),
        0xFF00_0000 => Some(3),
        _ => None,
    }
}

fn layout_from_pixel_format(flags: u32, four: u32, bits: u32, r: u32, g: u32, b: u32, a: u32) -> Result<Layout> {
    if flags & DDPF_FOURCC != 0 {
        return Ok(Layout::Plain(match four {
            f if f == fourcc(b"DXT1") => Format::Dxt1,
            f if f == fourcc(b"DXT3") => Format::Dxt3,
            f if f == fourcc(b"DXT5") => Format::Dxt5,
            f if f == fourcc(b"ATI2") || f == fourcc(b"BC5U") => Format::Dxn,
            f if f == fourcc(b"DXT2") || f == fourcc(b"DXT4") => {
                bail!("premultiplied-alpha DXT2/DXT4 is not supported; save as DXT3/DXT5")
            }
            f => bail!("unsupported DDS FourCC {:?}", String::from_utf8_lossy(&f.to_le_bytes())),
        }));
    }
    if flags & DDPF_LUMINANCE != 0 {
        return match (bits, flags & DDPF_ALPHAPIXELS != 0) {
            (8, false) if r == 0xFF => Ok(Layout::Plain(Format::Lum)),
            (16, true) if r == 0xFF && a == 0xFF00 => Ok(Layout::Plain(Format::LumAlpha)),
            _ => bail!("unsupported luminance layout ({bits} bits)"),
        };
    }
    if flags & DDPF_ALPHA != 0 && flags & DDPF_RGB == 0 {
        return if bits == 8 { Ok(Layout::Plain(Format::Alpha)) } else { bail!("unsupported alpha-only layout ({bits} bits)") };
    }
    if flags & DDPF_RGB != 0 {
        let (Some(rb), Some(gb), Some(bb)) = (mask_byte(r), mask_byte(g), mask_byte(b)) else {
            bail!("unsupported RGB masks r={r:#x} g={g:#x} b={b:#x} ({bits} bits); use 8 bits per channel");
        };
        let has_a = flags & DDPF_ALPHAPIXELS != 0;
        return match (bits, has_a) {
            (32, true) => {
                let Some(ab) = mask_byte(a) else { bail!("unsupported alpha mask {a:#x}") };
                if (bb, gb, rb, ab) == (0, 1, 2, 3) {
                    Ok(Layout::Plain(Format::Bgra))
                } else {
                    Ok(Layout::Shuffle { src_bpp: 4, map: [bb, gb, rb, ab], out: Format::Bgra })
                }
            }
            (32, false) => Ok(Layout::Shuffle { src_bpp: 4, map: [bb, gb, rb, 0], out: Format::Bgr }),
            (24, false) => {
                if (bb, gb, rb) == (0, 1, 2) {
                    Ok(Layout::Plain(Format::Bgr))
                } else {
                    Ok(Layout::Shuffle { src_bpp: 3, map: [bb, gb, rb, 0], out: Format::Bgr })
                }
            }
            _ => bail!("unsupported uncompressed layout ({bits} bits, alpha {has_a}); use 24- or 32-bit 8:8:8(:8)"),
        };
    }
    bail!("unrecognised DDS pixel format")
}

fn layout_from_dxgi(f: u32) -> Result<Layout> {
    Ok(match f {
        71 | 72 => Layout::Plain(Format::Dxt1),
        74 | 75 => Layout::Plain(Format::Dxt3),
        77 | 78 => Layout::Plain(Format::Dxt5),
        83 => Layout::Plain(Format::Dxn),
        87 | 91 => Layout::Plain(Format::Bgra),
        28 | 29 => Layout::Shuffle { src_bpp: 4, map: [2, 1, 0, 3], out: Format::Bgra },
        61 => Layout::Plain(Format::Lum),
        65 => Layout::Plain(Format::Alpha),
        other => bail!("unsupported DXGI format {other}"),
    })
}

fn src_bytes(layout: Layout, w: u32, h: u32) -> usize {
    match layout {
        Layout::Plain(f) => iwi::surface_bytes(f, w, h).unwrap(),
        Layout::Shuffle { src_bpp, .. } => w as usize * h as usize * src_bpp,
    }
}

pub fn parse(b: &[u8]) -> Result<Dds> {
    if b.len() < 128 || &b[0..4] != b"DDS " || rd(b, 4) != 124 {
        bail!("not a DDS file");
    }
    let flags = rd(b, 8);
    let (height, width) = (rd(b, 12), rd(b, 16));
    let mips = if flags & DDSD_MIPMAPCOUNT != 0 { rd(b, 28).max(1) as usize } else { 1 };
    let (pf_flags, four, bits, r, g, bm, a) = (rd(b, 80), rd(b, 84), rd(b, 88), rd(b, 92), rd(b, 96), rd(b, 100), rd(b, 104));
    let (caps2, mut offset) = (rd(b, 112), 128usize);
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        bail!("DDS size {width}x{height} is out of range");
    }
    if caps2 & CAPS2_VOLUME != 0 {
        bail!("volume textures are not supported");
    }
    let layout = if pf_flags & DDPF_FOURCC != 0 && four == fourcc(b"DX10") {
        if b.len() < 148 {
            bail!("truncated DX10 header");
        }
        if rd(b, 128 + 4) == 4 {
            bail!("3D DDS textures are not supported");
        }
        if rd(b, 128 + 12) > 1 {
            bail!("DDS texture arrays are not supported");
        }
        let l = layout_from_dxgi(rd(b, 128))?;
        offset = 148;
        l
    } else {
        layout_from_pixel_format(pf_flags, four, bits, r, g, bm, a)?
    };
    let faces = if caps2 & CAPS2_CUBEMAP != 0 {
        if caps2 & CAPS2_ALL_FACES != CAPS2_ALL_FACES {
            bail!("cubemap DDS must contain all six faces");
        }
        6
    } else {
        1
    };
    let mut data = Vec::with_capacity(faces);
    for _ in 0..faces {
        let mut chain = Vec::with_capacity(mips);
        for l in 0..mips {
            let size = src_bytes(layout, (width >> l).max(1), (height >> l).max(1));
            let Some(src) = b.get(offset..offset + size) else { bail!("DDS file truncated in mip level {l}") };
            chain.push(src.to_vec());
            offset += size;
        }
        data.push(chain);
    }
    Ok(Dds { width, height, layout, data })
}

impl Dds {
    pub fn mip_count(&self) -> usize {
        self.data[0].len()
    }

    pub fn faces(&self) -> usize {
        self.data.len()
    }

    pub fn describe(&self) -> String {
        let f = match self.layout {
            Layout::Plain(f) => f,
            Layout::Shuffle { out, .. } => out,
        };
        format!("{}x{} {} {} mip(s){}", self.width, self.height, f.name(), self.mip_count(), if self.faces() == 6 { ", cubemap" } else { "" })
    }

    /// Converts to stored IWI form with the given flags. DDS stores each face's
    /// mip chain on its own; `Raw` is level-major, so this reorders.
    pub fn into_raw(self, flags: u32) -> Result<Raw> {
        let (width, height, layout) = (self.width, self.height, self.layout);
        let out_fmt = match layout {
            Layout::Plain(f) => f,
            Layout::Shuffle { out, .. } => out,
        };
        let n = self.mip_count();
        let mut mips: Vec<Vec<Vec<u8>>> = vec![Vec::new(); n];
        for face in self.data {
            for (l, surface) in face.into_iter().enumerate() {
                mips[l].push(match layout {
                    Layout::Plain(_) => surface,
                    Layout::Shuffle { src_bpp, map, out } => {
                        let obpp = if out == Format::Bgra { 4 } else { 3 };
                        let mut v = Vec::with_capacity(surface.len() / src_bpp * obpp);
                        for px in surface.chunks_exact(src_bpp) {
                            for &m in &map[..obpp] {
                                v.push(if m == 255 { 255 } else { px[m as usize] });
                            }
                        }
                        v
                    }
                });
            }
        }
        Ok(Raw { flags, format: out_fmt, width, height, mips })
    }
}

/// Writes any IWI (wavelet images are decompressed first) as a DDS file.
/// Cubemap faces keep their stored order (+X,-X,+Y,-Y,+Z,-Z slots).
pub fn write(raw: &Raw) -> Result<Vec<u8>> {
    let n = raw.mips.len();
    let faces = raw.mips[0].len();
    let (pf_flags, four, bits, masks): (u32, u32, u32, [u32; 4]) = match raw.format {
        Format::Dxt1 => (DDPF_FOURCC, fourcc(b"DXT1"), 0, [0; 4]),
        Format::Dxt3 => (DDPF_FOURCC, fourcc(b"DXT3"), 0, [0; 4]),
        Format::Dxt5 => (DDPF_FOURCC, fourcc(b"DXT5"), 0, [0; 4]),
        Format::Dxn => (DDPF_FOURCC, fourcc(b"ATI2"), 0, [0; 4]),
        Format::Bgra => (DDPF_RGB | DDPF_ALPHAPIXELS, 0, 32, [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000]),
        Format::Bgr => (DDPF_RGB, 0, 24, [0xFF_0000, 0xFF00, 0xFF, 0]),
        Format::Lum | Format::R8 => (DDPF_LUMINANCE, 0, 8, [0xFF, 0, 0, 0]),
        Format::LumAlpha => (DDPF_LUMINANCE | DDPF_ALPHAPIXELS, 0, 16, [0xFF, 0, 0, 0xFF00]),
        Format::Alpha => (DDPF_ALPHA, 0, 8, [0, 0, 0, 0xFF]),
        other => bail!("{} cannot be written to DDS", other.name()),
    };
    let top = iwi::surface_bytes(raw.format, raw.width, raw.height).unwrap() as u32;
    let compressed = pf_flags & DDPF_FOURCC != 0;
    let mut flags = DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT;
    flags |= if compressed { DDSD_LINEARSIZE } else { DDSD_PITCH };
    if n > 1 {
        flags |= DDSD_MIPMAPCOUNT;
    }
    let mut caps = CAPS_TEXTURE;
    if n > 1 {
        caps |= CAPS_COMPLEX | CAPS_MIPMAP;
    }
    let mut caps2 = 0;
    if faces == 6 {
        caps |= CAPS_COMPLEX;
        caps2 = CAPS2_CUBEMAP | CAPS2_ALL_FACES;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"DDS ");
    let put = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    put(&mut out, 124);
    put(&mut out, flags);
    put(&mut out, raw.height);
    put(&mut out, raw.width);
    put(&mut out, if compressed { top } else { raw.width * bits / 8 });
    put(&mut out, 0);
    put(&mut out, if n > 1 { n as u32 } else { 0 });
    for _ in 0..11 {
        put(&mut out, 0);
    }
    put(&mut out, 32);
    put(&mut out, pf_flags);
    put(&mut out, four);
    put(&mut out, bits);
    for m in masks {
        put(&mut out, m);
    }
    put(&mut out, caps);
    put(&mut out, caps2);
    put(&mut out, 0);
    put(&mut out, 0);
    put(&mut out, 0);
    for f in 0..faces {
        for l in 0..n {
            out.extend_from_slice(&raw.mips[l][f]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_bgra(w: u32, h: u32) -> Raw {
        let mut r = Raw { flags: 0x341, format: Format::Bgra, width: w, height: h, mips: vec![vec![(0..w * h * 4).map(|i| i as u8).collect()]] };
        iwi::generate_mips(&mut r).unwrap();
        r
    }

    #[test]
    fn iwi_dds_iwi_roundtrip() {
        for raw in [raw_bgra(8, 4), raw_bgra(5, 3), raw_bgra(1, 1)] {
            let dds = write(&raw).unwrap();
            let back = parse(&dds).unwrap().into_raw(raw.flags).unwrap();
            assert_eq!(back.mips, raw.mips);
            assert_eq!(iwi::write(&back).unwrap(), iwi::write(&raw).unwrap());
        }
    }

    #[test]
    fn rgba_channel_order_is_swapped() {
        // DX10 R8G8B8A8 pixel (1,2,3,4) must become BGRA (3,2,1,4).
        let mut b = vec![0u8; 148];
        b[0..4].copy_from_slice(b"DDS ");
        b[4..8].copy_from_slice(&124u32.to_le_bytes());
        b[12..16].copy_from_slice(&1u32.to_le_bytes());
        b[16..20].copy_from_slice(&1u32.to_le_bytes());
        b[80..84].copy_from_slice(&DDPF_FOURCC.to_le_bytes());
        b[84..88].copy_from_slice(b"DX10");
        b[128..132].copy_from_slice(&28u32.to_le_bytes());
        b[140..144].copy_from_slice(&1u32.to_le_bytes());
        b.extend([1, 2, 3, 4]);
        let raw = parse(&b).unwrap().into_raw(0).unwrap();
        assert_eq!(raw.format, Format::Bgra);
        assert_eq!(raw.mips[0][0], vec![3, 2, 1, 4]);
    }

    #[test]
    fn partial_mip_chain_is_rejected_by_writer() {
        let mut r = raw_bgra(8, 8);
        r.mips.truncate(2);
        assert!(iwi::write(&r).is_err());
    }
}
