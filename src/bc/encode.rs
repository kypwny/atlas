//! Block-compressed encoders (DXT1/3/5, DXN) for mip generation.
//!
//! Colour blocks: principal-axis endpoint fit, then two rounds of least-squares
//! refinement, keeping whichever candidate has the lowest squared error. Alpha
//! blocks: both interpolation modes of the BC4 scheme, best one wins. This is
//! not a high-end compressor, but it is deterministic and good enough for the
//! lower mip levels (level 0 of an existing image is never recompressed).

use super::{Bc, rgb565};

type Rgb = [f32; 3];

/// The 4-entry colour palette for the endpoint pair, as the decoder builds it.
fn palette(c0: u16, c1: u16, punch_allowed: bool) -> [[u8; 4]; 4] {
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mut pal = [[0u8; 4]; 4];
    pal[0] = [p0[0], p0[1], p0[2], 255];
    pal[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || !punch_allowed {
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
    pal
}

fn quant565(c: Rgb) -> u16 {
    let r = ((c[0].clamp(0.0, 255.0) * 31.0 / 255.0) + 0.5) as u16;
    let g = ((c[1].clamp(0.0, 255.0) * 63.0 / 255.0) + 0.5) as u16;
    let b = ((c[2].clamp(0.0, 255.0) * 31.0 / 255.0) + 0.5) as u16;
    (r << 11) | (g << 5) | b
}

fn dist2(a: [u8; 4], p: Rgb) -> f32 {
    let d = [a[0] as f32 - p[0], a[1] as f32 - p[1], a[2] as f32 - p[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
}

struct Fit {
    c0: u16,
    c1: u16,
    indices: u32,
    err: f32,
}

/// Orders the endpoints for the chosen mode, builds the palette and assigns
/// the nearest index to every opaque pixel (transparent ones get index 3 in
/// 3-colour mode).
fn finish(c0: u16, c1: u16, px: &[[u8; 4]; 16], opaque: &[bool; 16], mode3: bool) -> Fit {
    let (mut a, mut b) = (c0, c1);
    if mode3 {
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
    } else if a < b {
        std::mem::swap(&mut a, &mut b);
    }
    let pal = palette(a, b, mode3);
    let usable = if mode3 { 3 } else { 4 };
    let mut indices = 0u32;
    let mut err = 0.0;
    for i in 0..16 {
        let idx = if !opaque[i] {
            3
        } else {
            let p = [px[i][0] as f32, px[i][1] as f32, px[i][2] as f32];
            let mut best = (f32::MAX, 0usize);
            for (k, e) in pal.iter().enumerate().take(usable) {
                let d = dist2(*e, p);
                if d < best.0 {
                    best = (d, k);
                }
            }
            err += best.0;
            best.1
        };
        indices |= (idx as u32) << (2 * i);
    }
    Fit { c0: a, c1: b, indices, err }
}

/// Least-squares endpoints for fixed indices. Returns None when degenerate.
fn refine(px: &[[u8; 4]; 16], opaque: &[bool; 16], indices: u32, mode3: bool) -> Option<(Rgb, Rgb)> {
    let (mut a2, mut ab, mut b2) = (0.0f32, 0.0f32, 0.0f32);
    let (mut ax, mut bx) = ([0.0f32; 3], [0.0f32; 3]);
    for i in 0..16 {
        if !opaque[i] {
            continue;
        }
        let idx = (indices >> (2 * i)) & 3;
        // Position along c0 -> c1 for each index; in 4-colour mode c0 is index 0.
        let t = match (mode3, idx) {
            (_, 0) => 0.0,
            (_, 1) => 1.0,
            (true, _) => 0.5,
            (false, 2) => 1.0 / 3.0,
            _ => 2.0 / 3.0,
        };
        let w0 = 1.0 - t;
        a2 += w0 * w0;
        ab += w0 * t;
        b2 += t * t;
        for c in 0..3 {
            ax[c] += w0 * px[i][c] as f32;
            bx[c] += t * px[i][c] as f32;
        }
    }
    let det = a2 * b2 - ab * ab;
    if det.abs() < 1e-4 {
        return None;
    }
    let mut c0 = [0.0; 3];
    let mut c1 = [0.0; 3];
    for c in 0..3 {
        c0[c] = (ax[c] * b2 - bx[c] * ab) / det;
        c1[c] = (bx[c] * a2 - ax[c] * ab) / det;
    }
    Some((c0, c1))
}

/// Encodes one 8-byte colour block. `punch` allows DXT1's 3-colour mode with a
/// transparent index when any pixel has alpha < 128.
fn color_block(px: &[[u8; 4]; 16], punch: bool) -> [u8; 8] {
    let mut opaque = [true; 16];
    if punch {
        for i in 0..16 {
            opaque[i] = px[i][3] >= 128;
        }
    }
    let mode3 = punch && opaque.iter().any(|o| !o);
    let cols: Vec<Rgb> = (0..16).filter(|&i| opaque[i]).map(|i| [px[i][0] as f32, px[i][1] as f32, px[i][2] as f32]).collect();
    if cols.is_empty() {
        return [0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF];
    }
    let n = cols.len() as f32;
    let mean = [0, 1, 2].map(|c| cols.iter().map(|p| p[c]).sum::<f32>() / n);
    let mut cov = [[0.0f32; 3]; 3];
    for p in &cols {
        let d = [p[0] - mean[0], p[1] - mean[1], p[2] - mean[2]];
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    // Power iteration for the principal axis.
    let mut axis = [0.57735f32; 3];
    for _ in 0..8 {
        let v = [0, 1, 2].map(|i| cov[i][0] * axis[0] + cov[i][1] * axis[1] + cov[i][2] * axis[2]);
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if len < 1e-6 {
            break;
        }
        axis = [v[0] / len, v[1] / len, v[2] / len];
    }
    let (mut tmin, mut tmax) = (f32::MAX, f32::MIN);
    for p in &cols {
        let t = (p[0] - mean[0]) * axis[0] + (p[1] - mean[1]) * axis[1] + (p[2] - mean[2]) * axis[2];
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    let end = |t: f32| [mean[0] + axis[0] * t, mean[1] + axis[1] * t, mean[2] + axis[2] * t];
    let mut best = finish(quant565(end(tmin)), quant565(end(tmax)), px, &opaque, mode3);
    for _ in 0..2 {
        let Some((e0, e1)) = refine(px, &opaque, best.indices, mode3) else { break };
        // refine() solves for the endpoints in the *current* order; finish() may reorder, so map back by recomputing.
        let cand = finish(quant565(e0), quant565(e1), px, &opaque, mode3);
        if cand.err < best.err {
            best = cand;
        } else {
            break;
        }
    }
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&best.c0.to_le_bytes());
    out[2..4].copy_from_slice(&best.c1.to_le_bytes());
    out[4..8].copy_from_slice(&best.indices.to_le_bytes());
    out
}

fn alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let (x0, x1) = (a0 as u32, a1 as u32);
    let mut pal = [a0, a1, 0, 0, 0, 0, 0, 0];
    if x0 > x1 {
        for i in 1..7u32 {
            pal[1 + i as usize] = (((7 - i) * x0 + i * x1) / 7) as u8;
        }
    } else {
        for i in 1..5u32 {
            pal[1 + i as usize] = (((5 - i) * x0 + i * x1) / 5) as u8;
        }
        pal[6] = 0;
        pal[7] = 255;
    }
    pal
}

fn alpha_try(v: &[u8; 16], a0: u8, a1: u8) -> (u32, [u8; 8], u64) {
    let pal = alpha_palette(a0, a1);
    let mut err = 0u32;
    let mut bits = 0u64;
    for (i, &x) in v.iter().enumerate() {
        let mut best = (u32::MAX, 0usize);
        for (k, &p) in pal.iter().enumerate() {
            let d = (x as i32 - p as i32).unsigned_abs();
            let d = d * d;
            if d < best.0 {
                best = (d, k);
            }
        }
        err += best.0;
        bits |= (best.1 as u64) << (3 * i);
    }
    (err, [a0, a1, 0, 0, 0, 0, 0, 0], bits)
}

/// Encodes 16 single-channel values into an 8-byte BC4-style block.
fn alpha_block(v: &[u8; 16]) -> [u8; 8] {
    let (lo, hi) = (*v.iter().min().unwrap(), *v.iter().max().unwrap());
    // Mode A: 8-step ramp from hi to lo.
    let mut best = alpha_try(v, hi, lo);
    if hi == lo {
        best = alpha_try(v, hi, lo);
    } else {
        // Mode B: 6-step ramp plus exact 0 and 255, endpoints from the inner values.
        let inner: Vec<u8> = v.iter().copied().filter(|&x| x != 0 && x != 255).collect();
        if inner.len() < 16 && !inner.is_empty() {
            let (ilo, ihi) = (*inner.iter().min().unwrap(), *inner.iter().max().unwrap());
            let cand = alpha_try(v, ilo, ihi);
            if cand.0 < best.0 {
                best = cand;
            }
        }
    }
    let mut out = [0u8; 8];
    out[0] = best.1[0];
    out[1] = best.1[1];
    for i in 0..6 {
        out[2 + i] = (best.2 >> (8 * i)) as u8;
    }
    out
}

/// Gathers the 4x4 block at (bx, by), clamping at the image edge.
fn gather(rgba: &[u8], w: usize, h: usize, bx: usize, by: usize) -> [[u8; 4]; 16] {
    let mut px = [[0u8; 4]; 16];
    for (i, p) in px.iter_mut().enumerate() {
        let x = (bx * 4 + i % 4).min(w - 1);
        let y = (by * 4 + i / 4).min(h - 1);
        p.copy_from_slice(&rgba[(y * w + x) * 4..(y * w + x) * 4 + 4]);
    }
    px
}

/// Encodes tightly packed RGBA8 into the block format.
pub fn encode(kind: Bc, rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let (bw, bh) = (w.div_ceil(4).max(1), h.div_ceil(4).max(1));
    let mut out = Vec::with_capacity(bw * bh * kind.block_bytes());
    for by in 0..bh {
        for bx in 0..bw {
            let px = gather(rgba, w, h, bx, by);
            match kind {
                Bc::Dxt1 => out.extend_from_slice(&color_block(&px, true)),
                Bc::Dxt3 => {
                    for i in 0..8 {
                        let q = |a: u8| ((a as u32 * 15 + 127) / 255) as u8;
                        out.push(q(px[2 * i][3]) | (q(px[2 * i + 1][3]) << 4));
                    }
                    out.extend_from_slice(&color_block(&px, false));
                }
                Bc::Dxt5 => {
                    out.extend_from_slice(&alpha_block(&px.map(|p| p[3])));
                    out.extend_from_slice(&color_block(&px, false));
                }
                Bc::Dxn => {
                    out.extend_from_slice(&alpha_block(&px.map(|p| p[0])));
                    out.extend_from_slice(&alpha_block(&px.map(|p| p[1])));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bc::decode;

    fn psnr(a: &[u8], b: &[u8], channels: usize) -> f64 {
        let mut se = 0.0f64;
        let mut n = 0.0f64;
        for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            for c in 0..channels {
                let d = pa[c] as f64 - pb[c] as f64;
                se += d * d;
                n += 1.0;
            }
        }
        if se == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / (se / n)).log10() }
    }

    fn gradient(w: usize, h: usize) -> Vec<u8> {
        gradient_alpha(w, h, true)
    }

    fn gradient_alpha(w: usize, h: usize, vary_alpha: bool) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.extend([(x * 255 / (w - 1)) as u8, (y * 255 / (h - 1)) as u8, ((x + y) * 255 / (w + h - 2)) as u8, if vary_alpha { (255 - x * 255 / (w - 1)) as u8 } else { 255 }]);
            }
        }
        v
    }

    #[test]
    fn smooth_gradient_roundtrips_closely() {
        for (kind, ch, min) in [(Bc::Dxt1, 3, 36.0), (Bc::Dxt3, 3, 36.0), (Bc::Dxt5, 4, 36.0)] {
            let src = gradient_alpha(64, 48, kind != Bc::Dxt1);
            let back = decode(kind, &encode(kind, &src, 64, 48), 64, 48);
            let p = psnr(&src, &back, ch);
            assert!(p > min, "{:?}: PSNR {p:.1}", kind.block_bytes());
        }
    }

    #[test]
    fn solid_colours_are_nearly_exact() {
        let mut src = Vec::new();
        for _ in 0..16 {
            src.extend([200u8, 100, 50, 255]);
        }
        let back = decode(Bc::Dxt5, &encode(Bc::Dxt5, &src, 4, 4), 4, 4);
        for (a, b) in src.iter().zip(&back) {
            assert!(a.abs_diff(*b) <= 4, "{a} vs {b}");
        }
    }

    #[test]
    fn dxt1_punch_through_keeps_transparency() {
        let mut src = Vec::new();
        for i in 0..16 {
            src.extend(if i % 2 == 0 { [255u8, 0, 0, 255] } else { [0, 0, 0, 0] });
        }
        let back = decode(Bc::Dxt1, &encode(Bc::Dxt1, &src, 4, 4), 4, 4);
        for i in 0..16 {
            assert_eq!(back[i * 4 + 3], if i % 2 == 0 { 255 } else { 0 }, "texel {i}");
        }
    }

    #[test]
    fn few_alpha_levels_are_exact() {
        let vals: [u8; 16] = [0, 255, 0, 255, 255, 255, 0, 0, 255, 0, 255, 0, 0, 0, 255, 255];
        let blk = alpha_block(&vals);
        let mut full = [0u8; 16];
        full[..8].copy_from_slice(&blk);
        let dec = decode(Bc::Dxt5, &full, 4, 4);
        for i in 0..16 {
            assert_eq!(dec[i * 4 + 3], vals[i], "texel {i}");
        }
    }

    #[test]
    fn dxn_keeps_two_channels() {
        let src = gradient(8, 8);
        let back = decode(Bc::Dxn, &encode(Bc::Dxn, &src, 8, 8), 8, 8);
        for (a, b) in src.chunks_exact(4).zip(back.chunks_exact(4)) {
            assert!(a[0].abs_diff(b[0]) <= 8 && a[1].abs_diff(b[1]) <= 8, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn odd_sizes_work() {
        let src = gradient_alpha(61, 45, false);
        let enc = encode(Bc::Dxt1, &src, 61, 45);
        assert_eq!(enc.len(), super::super::surface_size(Bc::Dxt1, 61, 45));
        let back = decode(Bc::Dxt1, &enc, 61, 45);
        let p = psnr(&src, &back, 3);
        assert!(p > 34.0, "PSNR {p:.1}");
    }
}
