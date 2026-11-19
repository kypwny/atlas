//! Block-compressed texture decoders (DXT1/3/5 = BC1/2/3, DXN = BC5) to RGBA8.
//!
//! Done on the CPU so the result is identical on every platform and GPU; GPU
//! decompression differs slightly between vendors, and Direct3D 9 hardware did
//! too. The maths follows the S3TC / D3D9 documentation (565 expansion by bit
//! replication, 1/3 and 2/3 interpolation).

mod encode;
pub use encode::encode;

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [((r << 3) | (r >> 2)) as u8, ((g << 2) | (g >> 4)) as u8, ((b << 3) | (b >> 2)) as u8]
}

/// Decodes one 8-byte colour block into 16 RGBA texels.
/// `allow_punch` enables DXT1's 3-colour + transparent mode when c0 <= c1.
fn color_block(b: &[u8], allow_punch: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let p0 = rgb565(c0);
    let p1 = rgb565(c1);
    let mut pal = [[0u8; 4]; 4];
    pal[0] = [p0[0], p0[1], p0[2], 255];
    pal[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || !allow_punch {
        for i in 0..3 {
            pal[2][i] = ((2 * p0[i] as u32 + p1[i] as u32) / 3) as u8;
            pal[3][i] = ((p0[i] as u32 + 2 * p1[i] as u32) / 3) as u8;
        }
        pal[2][3] = 255;
        pal[3][3] = 255;
    } else {
        for i in 0..3 {
            pal[2][i] = ((p0[i] as u32 + p1[i] as u32) / 2) as u8;
        }
        pal[2][3] = 255;
        pal[3] = [0, 0, 0, 0];
    }
    let idx = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, px) in out.iter_mut().enumerate() {
        *px = pal[((idx >> (2 * i)) & 3) as usize];
    }
    out
}

/// Decodes one 8-byte BC4-style block (also DXT5 alpha) into 16 values.
fn alpha_block(b: &[u8]) -> [u8; 16] {
    let a0 = b[0] as u32;
    let a1 = b[1] as u32;
    let mut pal = [0u8; 8];
    pal[0] = a0 as u8;
    pal[1] = a1 as u8;
    if a0 > a1 {
        for i in 1..7u32 {
            pal[1 + i as usize] = (((7 - i) * a0 + i * a1) / 7) as u8;
        }
    } else {
        for i in 1..5u32 {
            pal[1 + i as usize] = (((5 - i) * a0 + i * a1) / 5) as u8;
        }
        pal[6] = 0;
        pal[7] = 255;
    }
    let mut bits = 0u64;
    for (i, v) in b[2..8].iter().enumerate() {
        bits |= (*v as u64) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = pal[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Bc {
    Dxt1,
    Dxt3,
    Dxt5,
    Dxn,
}

impl Bc {
    pub fn block_bytes(self) -> usize {
        match self {
            Bc::Dxt1 => 8,
            _ => 16,
        }
    }
}

/// Bytes needed for a width x height surface (at least one block).
pub fn surface_size(kind: Bc, w: u32, h: u32) -> usize {
    let bw = ((w + 3) / 4).max(1) as usize;
    let bh = ((h + 3) / 4).max(1) as usize;
    bw * bh * kind.block_bytes()
}

/// Decodes a surface to tightly packed RGBA8.
pub fn decode(kind: Bc, data: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let bw = ((w + 3) / 4).max(1);
    let bh = ((h + 3) / 4).max(1);
    let mut out = vec![0u8; w * h * 4];
    for by in 0..bh {
        for bx in 0..bw {
            let off = (by * bw + bx) * kind.block_bytes();
            let blk = &data[off..off + kind.block_bytes()];
            let texels: [[u8; 4]; 16] = match kind {
                Bc::Dxt1 => color_block(blk, true),
                Bc::Dxt3 => {
                    let mut t = color_block(&blk[8..], false);
                    for (i, px) in t.iter_mut().enumerate() {
                        let byte = blk[i / 2];
                        let nib = if i % 2 == 0 { byte & 15 } else { byte >> 4 };
                        px[3] = nib * 17;
                    }
                    t
                }
                Bc::Dxt5 => {
                    let mut t = color_block(&blk[8..], false);
                    let a = alpha_block(&blk[..8]);
                    for (px, a) in t.iter_mut().zip(a) {
                        px[3] = a;
                    }
                    t
                }
                Bc::Dxn => {
                    let r = alpha_block(&blk[..8]);
                    let g = alpha_block(&blk[8..]);
                    let mut t = [[0u8, 0, 0, 255]; 16];
                    for i in 0..16 {
                        t[i][0] = r[i];
                        t[i][1] = g[i];
                    }
                    t
                }
            };
            for ty in 0..4 {
                for tx in 0..4 {
                    let (x, y) = (bx * 4 + tx, by * 4 + ty);
                    if x < w && y < h {
                        let o = (y * w + x) * 4;
                        out[o..o + 4].copy_from_slice(&texels[ty * 4 + tx]);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxt1_solid_and_punch() {
        // c0 = pure red 565 (0xF800), c1 = 0, all indices 0 => red.
        let blk = [0x00, 0xF8, 0x00, 0x00, 0, 0, 0, 0];
        let px = decode(Bc::Dxt1, &blk, 4, 4);
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
        // c0 <= c1 and index 3 => transparent black.
        let blk = [0x00, 0x00, 0x00, 0xF8, 0xFF, 0xFF, 0xFF, 0xFF];
        let px = decode(Bc::Dxt1, &blk, 4, 4);
        assert_eq!(&px[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn dxt5_alpha_modes() {
        let mut blk = [0u8; 16];
        blk[0] = 255;
        blk[1] = 0; // 8-value mode, index 0 everywhere => 255
        let px = decode(Bc::Dxt5, &blk, 4, 4);
        assert_eq!(px[3], 255);
        blk[0] = 0;
        blk[1] = 255; // a0 <= a1: 6-value mode
        blk[2] = 0b0000_0110; // texel 0 -> index 6 => 0, texel 1 -> index 0
        let px = decode(Bc::Dxt5, &blk, 4, 4);
        assert_eq!(px[3], 0);
    }

    #[test]
    fn partial_block_sizes() {
        assert_eq!(surface_size(Bc::Dxt1, 1, 1), 8);
        assert_eq!(surface_size(Bc::Dxt5, 5, 2), 32);
    }
}
