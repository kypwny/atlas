//! Phase 4 of the stock-to-QA zone conversion (`convert_zone`): effects and the impact-effect table.
//!
//! Stock offsets are the IW4 structures' (ZoneTool `Structs.hpp`: `FxEffectDef`, `FxElemDef`, `FxElemVelStateSample`,
//! `FxElemVisStateSample`, `FxTrailDef`, `FxSparkFountainDef`, `FxImpactTable`, `FxImpactEntry`).

use super::convert_zone::{cstr_at, Hdr, Out, Rule, Src, INLINE};

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "FxEffectDef" => effect,
        "FxImpactTable" => impact_table,
        _ => return None,
    })
}

fn follows(s: &Src, off: u32) -> u32 {
    if s.has(off) { INLINE } else { 0 }
}

/// The QA element type number of a stock one: the QA enumeration has two more types, at 2 and 4 (sprite 0 and 1 and the types from
/// the trail on are numbered as in IW4 plus none or two; the stock tail is QA 3). Measured on 1,442 elements of 297 effects present
/// in both stock `common_mp` and QA `common_mp`: 0 -> 0, 1 -> 1, 2 -> 3, 3 -> 5, 5 -> 7, 6 -> 8, 7 -> 9, 8 -> 10, 10 -> 12, 11 -> 13,
/// 12 -> 14 (no stock type 4 or 9 occurred; they follow the same shift).
pub fn qa_elem_type(stock: u8) -> u8 {
    match stock {
        0 | 1 => stock,
        2 => 3,
        n => n + 2,
    }
}

/// The QA element flags: a flag was inserted at 0x100, so the stock flags 0x100..0x8000 (use collision, die on touch, draw past fog,
/// draw with viewmodel, block sight, thermal view only, trail orient by velocity, emit orient by element) sit one bit higher.
/// Measured on 1,442 element pairs: bits 8, 9, 10, 11, 12 and 14 of the stock flags are bits 9, 10, 11, 12, 13 and 15 of the QA ones,
/// bits 0 to 7 and 24 to 30 are unchanged.
pub fn qa_flags(stock: u32) -> u32 {
    (stock & 0xFFFF_00FF) | ((stock & 0xFF00) << 1)
}

/// FxEffectDef (32 bytes in both): name, flags, totalSize, msecLoopingLife, the three element counts (looping, one-shot, emission)
/// and the element array.
///
/// FxElemDef, stock (252 bytes): flags +0, spawn +4, spawnRange +12, fadeInRange +20, fadeOutRange +28, spawnFrustumCullRadius +36,
/// spawnDelayMsec +40, lifeSpanMsec +48, spawnOrigin[3] +56, spawnOffsetRadius +80, spawnOffsetHeight +88, spawnAngles[3] +96,
/// angularVelocity[3] +120, initialRotation +144, gravity +152, reflectionFactor +160, atlas +168, elemType +176, visualCount +177,
/// velIntervalCount +178, visStateIntervalCount +179, velSamples +180, visSamples +184, visuals +188, collBounds +192, effectOnImpact
/// +216, effectOnDeath +220, effectEmitted +224, emitDist +228, emitDistVariance +236, extended +244, sortOrder +248, lightingFrac
/// +249, useItemClip +250, fadeInfo +251.
/// QA (260): the same up to +251 (element types from the trail on are two higher, see `qa_elem_type`), then a word +252 and a
/// string pointer +256, both zero in every QA element looked at (the three sampled zones' 6 effects and the food-prop ones).
/// The effect references (+216, +220, +224) are names in both: a string follows.
fn effect(s: &Src, o: &mut Out) -> Result<(), String> {
    let (looping, one_shot, emission) = (s.u32(16) as usize, s.u32(20) as usize, s.u32(24) as usize);
    let count = looping + one_shot + emission;
    let mut h = Hdr::new(32);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 24));
    h.u32(28, follows(s, 28));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if !s.has(28) {
        return Ok(());
    }
    let stock = s.array(28, count, 252);
    if stock.len() != count * 252 {
        return Err("effect elements outside the loaded memory".into());
    }
    let base = s.u32(28);
    // the QA element table first
    let mut table = vec![0u8; 260 * count];
    for i in 0..count {
        let e = s.at(base + 252 * i as u32);
        let q = &mut table[260 * i..260 * (i + 1)];
        q[..252].copy_from_slice(&stock[252 * i..252 * (i + 1)]);
        q[0..4].copy_from_slice(&qa_flags(e.u32(0)).to_le_bytes());
        q[176] = qa_elem_type(e.u8(176));
        // pointer words: sample tables and the extended definition follow as data; the effect names are strings
        for off in [180usize, 184, 216, 220, 224, 244] {
            let v = if e.has(off as u32) { INLINE } else { 0 };
            q[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
        q[188..192].copy_from_slice(&0u32.to_le_bytes());
    }
    let at = o.put(&table);
    for i in 0..count {
        let e = s.at(base + 252 * i as u32);
        let (ty_stock, n) = (e.u8(176), e.u8(177) as usize);
        let ty = qa_elem_type(ty_stock);
        let q = at + 260 * i;
        // velocity samples (96 bytes each, velIntervalCount + 1)
        if e.has(180) {
            let c = e.u8(178) as usize + 1;
            let a = e.array(180, c, 96);
            if a.len() != c * 96 {
                return Err("effect velocity samples outside the loaded memory".into());
            }
            o.put(&a);
        }
        // visual-state samples (48 bytes in the stock element, 64 in QA)
        if e.has(184) {
            let c = e.u8(179) as usize + 1;
            let a = e.array(184, c, 48);
            if a.len() != c * 48 {
                return Err("effect visual samples outside the loaded memory".into());
            }
            o.put(&vis_samples(&a, ty_stock <= 3));
        }
        // visuals
        match ty {
            13 => {
                // decals: an array of material pairs
                if e.has(188) {
                    o.set(q + 188, INLINE);
                    let v = o.put(&vec![0u8; 8 * n]);
                    let arr = e.u32(188);
                    for j in 0..2 * n {
                        o.reference(v + 4 * j, s.at(arr + 4 * j as u32).asset(0));
                    }
                }
            }
            10 | 11 => {}
            _ if n <= 1 => single_visual(s, &e, ty, q, o),
            _ => {
                if e.has(188) {
                    o.set(q + 188, INLINE);
                    let v = o.put(&vec![0u8; 4 * n]);
                    let arr = e.u32(188);
                    for j in 0..n {
                        let w = s.at(arr + 4 * j as u32);
                        visual(s, &w, 0, ty, v + 4 * j, o);
                    }
                }
            }
        }
        // names of the effects spawned on impact, on death and by emission
        for off in [216u32, 220, 224] {
            if e.has(off) {
                o.put_str(&cstr_at(s.v, e.u32(off)));
            }
        }
        // extended definition
        if e.has(244) {
            let x = e.u32(244);
            match ty {
                5 => {
                    let t = s.at(x);
                    let mut def = t.bytes(0, 36);
                    for off in [24usize, 32] {
                        let set = if t.has(off as u32) { INLINE } else { 0 };
                        def[off..off + 4].copy_from_slice(&set.to_le_bytes());
                    }
                    o.put(&def);
                    if t.has(24) {
                        let n = t.u32(20) as usize;
                        let a = t.array(24, n, 20);
                        if a.len() != n * 20 {
                            return Err("trail vertices outside the loaded memory".into());
                        }
                        o.put(&a);
                    }
                    if t.has(32) {
                        let n = t.u32(28) as usize;
                        let a = t.array(32, n, 2);
                        if a.len() != n * 2 {
                            return Err("trail indices outside the loaded memory".into());
                        }
                        o.put(&a);
                    }
                }
                8 => {
                    let t = s.at(x);
                    let d = t.bytes(0, 52);
                    if d.len() != 52 {
                        return Err("spark fountain definition outside the loaded memory".into());
                    }
                    o.put(&d);
                }
                _ => {
                    let d = s.v.bytes(x, 1).ok_or("extended definition outside the loaded memory")?.to_vec();
                    o.put(&d);
                }
            }
        }
    }
    Ok(())
}

/// Visual-state samples: the stock record is 48 bytes (the base visual state of 24 bytes, then the variance, 24 bytes), the QA one
/// 64: each state is followed by two more words. After the base state they are (0, 0x80000000) for the sprite, oriented-sprite, tail
/// and trail elements (stock types 0 to 3) and zero for the others; after the variance state they are zero (every sample of the 297
/// effects compared, except where the QA effect differs from the stock one). `a` holds stock records.
fn vis_samples(a: &[u8], sprite: bool) -> Vec<u8> {
    let n = a.len() / 48;
    let mut out = vec![0u8; 64 * n];
    for i in 0..n {
        out[64 * i..64 * i + 24].copy_from_slice(&a[48 * i..48 * i + 24]);
        if sprite {
            out[64 * i + 28..64 * i + 32].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        }
        out[64 * i + 32..64 * i + 56].copy_from_slice(&a[48 * i + 24..48 * (i + 1)]);
    }
    out
}

/// One visual that is not in an array (visualCount of 0 or 1): the element's own word at `q + 188`.
fn single_visual(s: &Src, e: &Src, ty: u8, q: usize, o: &mut Out) {
    visual(s, e, 188, ty, q + 188, o);
}

/// A visual of an element of QA type `ty` at `w + off` in the stock zone, written as the word at stream offset `at`.
fn visual(s: &Src, w: &Src, off: u32, ty: u8, at: usize, o: &mut Out) {
    if ty == 12 || ty == 14 {
        // a sound or effect name
        if w.has(off) {
            o.set(at, INLINE);
            o.put_str(&cstr_at(s.v, w.u32(off)));
        }
        return;
    }
    o.reference(at, w.asset(off));
}

/// FxImpactTable: name and the table pointer. The table has one record per impact type, 35 effect references each (31 surface
/// types and 4 flesh ones): 15 records in a stock table, 19 in a QA table. The QA record order starts with the stock one; the four
/// QA records the stock table lacks are left empty (no effect).
fn impact_table(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(8);
    h.u32(0, follows(s, 0));
    h.u32(4, follows(s, 4));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(4) {
        let stock = s.array(4, 15, 140);
        if stock.len() != 15 * 140 {
            return Err("impact table outside the loaded memory".into());
        }
        let base = s.u32(4);
        let at = o.put(&vec![0u8; 19 * 140]);
        for r in 0..15usize {
            for k in 0..35usize {
                let id = s.at(base + 140 * r as u32 + 4 * k as u32).asset(0);
                o.reference(at + 140 * r + 4 * k, id);
            }
        }
    }
    Ok(())
}
