//! LZO1X decompression (the format used by IPAK data blocks).
//!
//! Written from the published stream format (the same bitstream as `lzo1x_decompress_safe`): a run of
//! literals, then matches with 1-4 byte distance codes, ended by a distance-zero long match. Bounds are
//! checked everywhere, so malformed input gives an error rather than a panic.

use anyhow::{Result, bail};

struct Rd<'a> {
    s: &'a [u8],
    i: usize,
}

impl Rd<'_> {
    fn u8(&mut self) -> Result<usize> {
        let Some(&b) = self.s.get(self.i) else { bail!("LZO input ended early") };
        self.i += 1;
        Ok(b as usize)
    }

    /// Length extension: zero bytes each add 255, then one more byte.
    fn extend(&mut self, base: usize) -> Result<usize> {
        let mut n = base;
        loop {
            let b = self.u8()?;
            if b == 0 {
                n += 255;
                if n > 1 << 28 {
                    bail!("LZO length overflow");
                }
            } else {
                return Ok(n + b);
            }
        }
    }

    fn u16le(&mut self) -> Result<usize> {
        let lo = self.u8()?;
        let hi = self.u8()?;
        Ok(lo | hi << 8)
    }
}

fn literals(r: &mut Rd, out: &mut Vec<u8>, n: usize, limit: usize) -> Result<()> {
    let Some(src) = r.s.get(r.i..r.i + n) else { bail!("LZO literal run past the input") };
    if out.len() + n > limit {
        bail!("LZO output larger than expected");
    }
    out.extend_from_slice(src);
    r.i += n;
    Ok(())
}

fn copy_match(out: &mut Vec<u8>, dist: usize, n: usize, limit: usize) -> Result<()> {
    if dist == 0 || dist > out.len() {
        bail!("LZO match points before the start of the output");
    }
    if out.len() + n > limit {
        bail!("LZO output larger than expected");
    }
    let from = out.len() - dist;
    for k in 0..n {
        let b = out[from + k];
        out.push(b);
    }
    Ok(())
}

/// Decompresses one LZO1X stream. `limit` is the largest output accepted.
pub fn decompress(src: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut r = Rd { s: src, i: 0 };
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(1 << 20));
    // `first_run` is set right after a literal run of 4 or more: a short code then means a 3-byte far match.
    let mut t: usize;
    let mut first_run;

    let head = r.u8()?;
    if head > 17 {
        // the first instruction is a short literal run (3 or fewer literals leave no "first run" state)
        t = head - 17;
        literals(&mut r, &mut out, t, limit)?;
        first_run = t >= 4;
        t = r.u8()?;
    } else if head < 16 {
        let n = if head == 0 { r.extend(15)? } else { head };
        literals(&mut r, &mut out, n + 3, limit)?;
        first_run = true;
        t = r.u8()?;
    } else {
        bail!("LZO stream starts with a match");
    }

    loop {
        // t holds the next opcode
        let trailing;
        if t >= 16 {
            let dist;
            let len;
            if t >= 64 {
                len = (t >> 5) - 1 + 2;
                dist = 1 + ((t >> 2) & 7) + (r.u8()? << 3);
                trailing = t & 3;
            } else if t >= 32 {
                let mut l = t & 31;
                if l == 0 {
                    l = r.extend(31)?;
                }
                len = l + 2;
                let v = r.u16le()?;
                dist = 1 + (v >> 2);
                trailing = v & 3;
            } else {
                let far = (t & 8) << 11;
                let mut l = t & 7;
                if l == 0 {
                    l = r.extend(7)?;
                }
                len = l + 2;
                let v = r.u16le()?;
                let d = far + (v >> 2);
                if d == 0 {
                    // distance zero is the end-of-stream marker
                    return Ok(out);
                }
                dist = d + 0x4000;
                trailing = v & 3;
            }
            copy_match(&mut out, dist, len, limit)?;
            first_run = false;
            if trailing == 0 {
                // a plain literal run (or match) comes next
                t = r.u8()?;
                if t < 16 {
                    // literal run
                    let n = if t == 0 { r.extend(15)? } else { t };
                    literals(&mut r, &mut out, n + 3, limit)?;
                    first_run = true;
                    t = r.u8()?;
                }
                continue;
            }
            literals(&mut r, &mut out, trailing, limit)?;
            t = r.u8()?;
            continue;
        }

        // t < 16 right after literals or a short match
        let b = r.u8()?;
        if first_run {
            // 3-byte match with a distance above 2 KiB
            let dist = 1 + 0x0800 + (t >> 2) + (b << 2);
            copy_match(&mut out, dist, 3, limit)?;
        } else {
            let dist = 1 + (t >> 2) + (b << 2);
            copy_match(&mut out, dist, 2, limit)?;
        }
        first_run = false;
        let trailing = t & 3;
        if trailing == 0 {
            t = r.u8()?;
            if t < 16 {
                let n = if t == 0 { r.extend(15)? } else { t };
                literals(&mut r, &mut out, n + 3, limit)?;
                first_run = true;
                t = r.u8()?;
            }
            continue;
        }
        literals(&mut r, &mut out, trailing, limit)?;
        t = r.u8()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_only_stream() {
        // 3 literals via the initial-run shortcut, then the end marker
        assert_eq!(decompress(&[20, b'a', b'b', b'c', 0x11, 0, 0], 16).unwrap(), b"abc");
    }

    #[test]
    fn long_literal_run_and_overlapping_match() {
        // 5 literals "abcab" encoded as first byte 17+5, then M2 match (len 3, dist 2... overlapping), then end
        let mut s = vec![22, b'a', b'b', b'c', b'a', b'b'];
        // M2: t = 0b01_000_000 | (len-1)<<5... len 4 -> (t>>5)-1+2 = 4 -> t>>5 = 3 -> t = 96; dist = 1 + ((t>>2)&7) + (b<<3) = 1+0+0
        s.extend_from_slice(&[96, 0]);
        s.extend_from_slice(&[0x11, 0, 0]);
        assert_eq!(decompress(&s, 32).unwrap(), b"abcabbbbb");
    }

    #[test]
    fn malformed_input_is_an_error() {
        assert!(decompress(&[], 8).is_err());
        assert!(decompress(&[20, b'a'], 8).is_err());
        assert!(decompress(&[20, b'a', b'b', b'c', 0x11, 0, 0], 2).is_err());
        // match before any output
        assert!(decompress(&[17, 0x40, 0], 8).is_err());
    }
}
