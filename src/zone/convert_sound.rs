//! Phase 2 (continued) of the stock-to-QA zone conversion (`convert_zone`): sound aliases, falloff curves and loaded sounds.

use super::convert_zone::{Hdr, Out, Rule, Src, INLINE};
use std::collections::HashMap;

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "snd_alias_list_t" => sound_list,
        "SndCurve" => sound_curve,
        "LoadedSound" => loaded_sound,
        _ => return None,
    })
}

fn follows(s: &Src, off: u32) -> u32 {
    if s.has(off) { INLINE } else { 0 }
}

/// SndCurve: file name, knot count, 16 knots of two floats. Identical in QA (136 bytes).
fn sound_curve(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(136);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 132));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    Ok(())
}

/// LoadedSound: stock 44 bytes (name, the Miles sound info in 36 bytes, the data pointer at +40); QA 48 with one word more in front
/// of the data pointer (+44). The info keeps its place, so the data length is at +12 in both. The new word is zero: no QA zone
/// of this build holds a loaded sound to read it from.
fn loaded_sound(s: &Src, o: &mut Out) -> Result<(), String> {
    let n = s.u32(12) as usize;
    let mut h = Hdr::new(48);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 36));
    h.u32(44, follows(s, 40));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(40) {
        let data = s.array(40, n, 1);
        if data.len() != n {
            return Err("sound data outside the loaded memory".into());
        }
        o.put(&data);
    }
    Ok(())
}

/// Sound alias list: name, the aliases, their count. A stock alias is 100 bytes, a QA one 108: the same fields, with two words
/// inserted after the flags (+60: 50 in 10,089 of 10,768 QA aliases, the default; +64: 0 in nearly all), so the fields from the
/// stock `slavePercentage` union on sit 8 bytes later (probability +72, ..., volume falloff curve +88, speaker map +104). The
/// master and slave flag bits (0x2, 0x4) of the stock flags are not set in QA (checked on 127 + 76 + ... paired aliases).
fn sound_list(s: &Src, o: &mut Out) -> Result<(), String> {
    let count = s.u32(8) as usize;
    let mut h = Hdr::new(12);
    h.u32(0, follows(s, 0));
    h.u32(4, follows(s, 4));
    h.u32(8, count as u32);
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if !s.has(4) {
        return Ok(());
    }
    let base = s.u32(4);
    let mut records = Vec::with_capacity(count * 108);
    for i in 0..count {
        let a = s.at(base + 100 * i as u32);
        let mut r = Hdr::new(108);
        for off in [0u32, 4, 8, 12, 16] {
            r.u32(off as usize, follows(&a, off));
        }
        r.u32(20, follows(&a, 20));
        r.raw(24, &a.bytes(24, 32)); // sequence .. velocityMin
        r.u32(56, a.u32(56) & !0x6);
        r.u32(60, 50);
        r.raw(68, &a.bytes(60, 20)); // slave/master percentage, probability, lfe, center, startDelay
        r.u32(88, 0); // curve: a reference, set below
        r.raw(92, &a.bytes(84, 12)); // envelope min, max, percentage
        r.u32(104, follows(&a, 96));
        records.extend_from_slice(&r.0);
    }
    let at = o.put(&records);
    let mut speakers_at: HashMap<u32, usize> = HashMap::new();
    for i in 0..count {
        let a = s.at(base + 100 * i as u32);
        for off in [0u32, 4, 8, 12, 16] {
            if let Some(t) = a.cstr(off) {
                o.put_str(&t);
            }
        }
        if a.has(20) {
            let f = s.at(a.u32(20));
            let kind = f.u8(0);
            let mut rec = Hdr::new(12);
            rec.raw(0, &f.bytes(0, 4));
            if kind == 1 {
                let p = o.put(&rec.0);
                o.reference(p + 4, f.asset(4));
            } else {
                rec.u32(4, follows(&f, 4));
                rec.u32(8, follows(&f, 8));
                o.put(&rec.0);
                for off in [4u32, 8] {
                    if let Some(t) = f.cstr(off) {
                        o.put_str(&t);
                    }
                }
            }
        }
        o.reference(at + 108 * i + 88, a.asset(80));
        if a.has(96) {
            let sp = a.u32(96);
            if let Some(&first) = speakers_at.get(&sp) {
                o.inner(at + 108 * i + 104, first);
            } else {
                let m = s.at(sp);
                let mut rec = m.bytes(0, 408);
                if rec.len() != 408 {
                    return Err("speaker map outside the loaded memory".into());
                }
                let has_name = m.has(4);
                rec[4..8].copy_from_slice(&(if has_name { INLINE } else { 0 }).to_le_bytes());
                speakers_at.insert(sp, o.pos());
                o.put(&rec);
                if has_name {
                    o.put_str(&m.cstr(4).unwrap_or_default());
                }
            }
        }
    }
    Ok(())
}
