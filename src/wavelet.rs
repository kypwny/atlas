//! IWI wavelet (formats 6..=10) decoder.
//!
//! Port of OpenAssetTools' `IwiWaveletDecoder.cpp` (GPL-3.0,
//! https://github.com/Laupetin/OpenAssetTools); see ATTRIBUTION.md. The Huffman
//! tables are generated from that file by `scripts/gen_wavelet_tables.py`.

use crate::wavelet_tables::{ALPHA, BLUE, ESCAPE, RED_GREEN};
use anyhow::{Result, bail};
use std::sync::OnceLock;

const LOOKUP_BITS: u32 = 12;
const LOOKUP_SIZE: usize = 1 << LOOKUP_BITS;

#[derive(Clone, Copy, Default, Debug)]
struct Decode {
    value: i16,
    bits: u8,
}

type Lookup = Box<[Decode; LOOKUP_SIZE]>;

fn build(codes: &[(u16, u8, i16)]) -> Lookup {
    let mut t: Lookup = vec![Decode::default(); LOOKUP_SIZE].into_boxed_slice().try_into().unwrap();
    for &(code, bits, value) in codes {
        let step = 1usize << bits;
        let mut i = code as usize;
        while i < LOOKUP_SIZE {
            debug_assert_eq!(t[i].bits, 0, "overlapping Huffman codewords");
            t[i] = Decode { value, bits };
            i += step;
        }
    }
    debug_assert!(t.iter().all(|e| e.bits != 0), "incomplete Huffman lookup");
    t
}

struct Tables {
    blue: Lookup,
    red_green: Lookup,
    alpha: Lookup,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| Tables { blue: build(&BLUE), red_green: build(&RED_GREEN), alpha: build(&ALPHA) })
}

struct Bits<'a> {
    data: &'a [u8],
    byte: usize,
    bit: usize,
    started: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, byte: 0, bit: 0, started: false }
    }

    fn start(&mut self) {
        if !self.started {
            self.bit = self.byte * 8;
            self.started = true;
        }
    }

    fn raw_byte(&mut self) -> Option<u8> {
        if self.started || self.byte >= self.data.len() {
            return None;
        }
        let v = self.data[self.byte];
        self.byte += 1;
        Some(v)
    }

    /// Next `n` (<= 32) bits LSB-first, zero-padded past the end.
    fn peek(&mut self, n: u32) -> u32 {
        self.start();
        let first = self.bit >> 3;
        let shift = (self.bit & 7) as u32;
        let mut acc = 0u64;
        for i in 0..5 {
            acc |= (*self.data.get(first + i).unwrap_or(&0) as u64) << (8 * i);
        }
        ((acc >> shift) & ((1u64 << n) - 1)) as u32
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        self.start();
        if n > 32 || self.bit + n as usize > self.data.len() * 8 {
            return None;
        }
        let v = self.peek(n);
        self.bit += n as usize;
        Some(v)
    }
}

fn decode_value(lookup: &Lookup, escape_bits: u32, escape_bias: i32, r: &mut Bits) -> Option<i32> {
    let e = lookup[r.peek(LOOKUP_BITS) as usize];
    if e.bits == 0 {
        return None;
    }
    r.read(e.bits as u32)?;
    if e.value == ESCAPE {
        let v = r.read(escape_bits)?;
        Some(v as i32 - escape_bias)
    } else {
        Some(e.value as i32)
    }
}

fn decode_coefficients(lookup: &Lookup, escape_bits: u32, escape_bias: i32, r: &mut Bits) -> Option<[i32; 3]> {
    let mut c = [0i32; 3];
    for v in c.iter_mut() {
        *v = decode_value(lookup, escape_bits, escape_bias, r)?;
    }
    Some(c)
}

fn reconstruct(src: &[u8], dst: &mut [u8], bpp: usize, stride: usize, channel: usize, parity: u32, c: [i32; 3]) {
    let base = 2 * src[channel] as i32;
    let (h, v, d) = (c[0], c[1], c[2]);
    dst[channel] = (parity as i32 + ((d + v + h + base) >> 1)) as u8;
    dst[bpp + channel] = ((h + base - d - v) >> 1) as u8;
    dst[stride + channel] = ((v - d + base - h) >> 1) as u8;
    dst[stride + bpp + channel] = ((base - h - v + d) >> 1) as u8;
}

fn decode_raw_level(dst: &mut [u8], pixels: usize, channels: usize, bpp: usize, r: &mut Bits) -> Option<()> {
    for p in 0..pixels {
        for c in 0..channels {
            dst[p * bpp + c] = r.raw_byte()?;
        }
        for c in channels..bpp {
            dst[p * bpp + c] = 0xFF;
        }
    }
    Some(())
}

fn decode_level(
    src: Option<&[u8]>,
    dst: &mut [u8],
    width: usize,
    height: usize,
    channels: usize,
    bpp: usize,
    r: &mut Bits,
) -> Option<()> {
    let t = tables();
    if width <= 1 || height <= 1 {
        return decode_raw_level(dst, width * height, channels, bpp, r);
    }
    let src = src?;
    let needs_delta = r.read(1)?;
    let adjusted;
    let src: &[u8] = if needs_delta != 0 {
        let count = width * height / 4;
        let mut s = src[..count * bpp].to_vec();
        for p in 0..count {
            for c in 0..channels {
                let d = decode_value(&t.alpha, 9, 255, r)?;
                s[p * bpp + c] = (s[p * bpp + c] as i32 + d) as u8;
            }
        }
        adjusted = s;
        &adjusted
    } else {
        src
    };

    let stride = width * bpp;
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let sp = &src[((y / 2) * (width / 2) + x / 2) * bpp..];
            let dp_off = (y * width + x) * bpp;
            let dp = &mut dst[dp_off..];

            if channels != 1 {
                let parity = r.read(1)?;
                let blue = decode_coefficients(&t.blue, 9, 0xFF, r)?;
                reconstruct(sp, dp, bpp, stride, 0, parity, blue);
                if channels >= 3 {
                    for channel in 1..=2 {
                        let parity = r.read(1)?;
                        let mut c = decode_coefficients(&t.red_green, 10, 0x1FE, r)?;
                        for i in 0..3 {
                            c[i] += blue[i];
                        }
                        reconstruct(sp, dp, bpp, stride, channel, parity, c);
                    }
                }
            }

            if channels == 3 {
                dp[3] = 0xFF;
                dp[bpp + 3] = 0xFF;
                dp[stride + 3] = 0xFF;
                dp[stride + bpp + 3] = 0xFF;
            } else {
                let parity = r.read(1)?;
                let c = decode_coefficients(&t.alpha, 9, 0xFF, r)?;
                reconstruct(sp, dp, bpp, stride, channels - 1, parity, c);
            }
        }
    }
    Some(())
}

/// Output layout of a wavelet format: (channel count, bytes per pixel).
/// 4-channel and 3-channel are stored B,G,R,A / B,G,R,X; 2-channel is L,A.
pub fn layout(channels: usize) -> (usize, usize) {
    match channels {
        4 => (4, 4),
        3 => (3, 4),
        2 => (2, 2),
        _ => (1, 1),
    }
}

/// Decodes every mip level and face. `levels[0]` is the largest. Pixel data is
/// in the layout described by [`layout`].
pub fn decode(data: &[u8], width: u32, height: u32, mip_count: usize, faces: usize, channels: usize) -> Result<Vec<Vec<Vec<u8>>>> {
    if width == 0 || height == 0 || !width.is_power_of_two() || !height.is_power_of_two() {
        bail!("wavelet dimensions must be powers of two");
    }
    if mip_count == 1 && width > 1 && height > 1 {
        bail!("wavelet textures larger than 1x1 need mipmaps");
    }
    let (ch, bpp) = layout(channels);
    let mut reader = Bits::new(data);
    let mut levels: Vec<Vec<Vec<u8>>> = Vec::with_capacity(mip_count);
    for mip in 0..mip_count {
        let (w, h) = ((width >> mip).max(1) as usize, (height >> mip).max(1) as usize);
        levels.push((0..faces).map(|_| vec![0u8; w * h * bpp]).collect());
    }
    for mip in (0..mip_count).rev() {
        let (w, h) = ((width >> mip).max(1) as usize, (height >> mip).max(1) as usize);
        for face in 0..faces {
            let (lo, hi) = levels.split_at_mut(mip + 1);
            let src = hi.first().map(|l| l[face].as_slice());
            let dst = &mut lo[mip][face];
            if decode_level(src, dst, w, h, ch, bpp, &mut reader).is_none() {
                bail!("unexpected end of wavelet data in mip level {mip}");
            }
        }
    }
    Ok(levels)
}
