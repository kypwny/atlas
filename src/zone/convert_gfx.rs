//! Phase 2 of the stock-to-QA zone conversion (`convert_zone`): the render assets. Materials, technique sets, images, pixel and
//! vertex shaders, vertex declarations.
//!
//! Stock offsets are the IW4 structures' (ZoneTool `Structs.hpp`, OpenAssetTools `IW4_Assets.h`); the QA layouts were identified by
//! pairing the headers of assets that both games hold under the same name (769 materials, 204 technique sets, 856 images in
//! `common_mp`) and are described next to each rule. Fields the QA record has and the stock one lacks are noted as such and take the
//! value the QA assets hold (almost always the same one).

use super::convert_zone::{cstr_at, Hdr, Out, Rule, Src, INLINE};
use std::collections::HashMap;

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "Material" => material,
        "MaterialTechniqueSet" => technique_set,
        "GfxImage" => image,
        "MaterialPixelShader" | "MaterialVertexShader" => shader,
        "MaterialVertexDeclaration" => vertex_declaration,
        _ => return None,
    })
}

fn follows(s: &Src, off: u32) -> u32 {
    if s.has(off) { INLINE } else { 0 }
}

/// Material. Stock (96 bytes): `info` {name, gameFlags, sortKey, atlas rows, atlas columns, drawSurf (8), surfaceTypeBits,
/// hashIndex}, `stateBitsEntry[48]` at +24, the four counts at +72, cameraRegion +76, the technique set +80 and the texture,
/// constant and state-bits tables +84, +88, +92.
///
/// QA (104 bytes): drawSurf (16, runtime, zero) at +0, name +16, surfaceTypeBits +20, a word at +24 whose third byte is the game
/// flags, then sortKey +28, atlas rows +29, atlas columns +30; `stateBitsEntry[48]` at +32, the counts at +80, cameraRegion +84,
/// technique set +88, tables +92, +96, +100. The QA sort-key numbering has one more key than IW4's from 30 on (stock 33 is 34, 47 is
/// 48; 0 to 29 are the same in all 1,966 pairs checked).
fn material(s: &Src, o: &mut Out) -> Result<(), String> {
    let (ntex, nconst, nbits) = (s.u8(72) as usize, s.u8(73) as usize, s.u8(74) as usize);
    // textures: 12 bytes each; a water texture (semantic 11) has a water record behind it: not converted
    let textures = s.array(84, ntex, 12);
    if s.has(84) && textures.len() != ntex * 12 {
        return Err("texture table outside the loaded memory".into());
    }
    let mut h = Hdr::new(104);
    h.u32(16, INLINE);
    h.u32(20, s.u32(16)); // surfaceTypeBits
    h.u8(26, s.u8(4)); // gameFlags
    let sort = s.u8(5);
    h.u8(28, if sort >= 30 { sort + 1 } else { sort });
    h.u8(29, s.u8(6));
    h.u8(30, s.u8(7));
    h.raw(32, &s.bytes(24, 48)); // stateBitsEntry
    h.raw(80, &s.bytes(72, 4)); // textureCount, constantCount, stateBitsCount, stateFlags
    h.u32(84, s.u32(76)); // cameraRegion
    h.u32(92, follows(s, 84));
    h.u32(96, follows(s, 88));
    h.u32(100, follows(s, 92));
    let at = o.put(&h.0);
    // name, the technique set (a reference), the texture table with the images' references, constants, state bits
    let name = s.cstr(0).unwrap_or_default();
    o.put_str(&name);
    o.reference(at + 88, s.asset(80));
    if s.has(84) {
        let t = o.put(&textures);
        let base = s.u32(84);
        for i in 0..ntex {
            if textures[12 * i + 7] == 11 {
                // a water texture: a `water_t` record follows (see `water`); its word is "data follows"
                o.set(t + 12 * i + 8, INLINE);
                continue;
            }
            let id = s.at(base + 12 * i as u32).asset(8);
            o.reference(t + 12 * i + 8, id);
        }
        for i in 0..ntex {
            if textures[12 * i + 7] == 11 {
                water(&s.at(base + 12 * i as u32), o)?;
            }
        }
    }
    if s.has(88) {
        let c = s.array(88, nconst, 32);
        if c.len() != nconst * 32 {
            return Err("constant table outside the loaded memory".into());
        }
        o.put(&c);
    }
    if s.has(92) {
        let b = s.array(92, nbits, 8);
        if b.len() != nbits * 8 {
            return Err("state bits outside the loaded memory".into());
        }
        o.put(&b);
    }
    Ok(())
}

/// Technique set. Stock (204 bytes): name, worldVertFormat/hasBeenUploaded/unused (4), remappedTechniqueSet (runtime), 48 technique
/// pointers at +12. QA (208): the same with a word inserted at +4 (zero in all 1,683 QA sets), so the format word is at +8, the
/// remapped pointer at +12 and the techniques at +16. A technique that several slots share is stored once; the other slots point at it.
fn technique_set(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(208);
    h.u32(0, follows(s, 0));
    h.raw(8, &s.bytes(4, 4));
    let slots: Vec<u32> = (0..48).map(|i| s.u32(12 + 4 * i)).collect();
    for (i, &p) in slots.iter().enumerate() {
        h.u32(16 + 4 * i as usize, if p != 0 { INLINE } else { 0 });
    }
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    let mut first_at: HashMap<u32, usize> = HashMap::new();
    for (i, &p) in slots.iter().enumerate() {
        if p == 0 {
            continue;
        }
        if let Some(&target) = first_at.get(&p) {
            o.inner(16 + 4 * i, target);
            continue;
        }
        first_at.insert(p, o.pos());
        technique(&s.at(p), o)?;
    }
    Ok(())
}

/// A technique: {name, flags, passCount} then `passCount` passes of 20 bytes {vertex declaration, vertex shader, pixel shader,
/// three argument counts, custom sampler flags, arguments}; QA is the same. Literal constants (argument types 1 and 7) are
/// stored behind their arguments.
fn technique(t: &Src, o: &mut Out) -> Result<(), String> {
    let passes = t.u16(6) as usize;
    let mut head = Hdr::new(8);
    head.u32(0, follows(t, 0));
    head.u16(4, t.u16(4));
    head.u16(6, passes as u16);
    o.put(&head.0);
    let pass_base = t.hdr + 8;
    let mut records = Vec::with_capacity(20 * passes);
    for j in 0..passes {
        let p = t.at(pass_base + 20 * j as u32);
        let mut r = [0u8; 20];
        r[12..16].copy_from_slice(&p.bytes(12, 4));
        if p.has(16) {
            r[16..20].copy_from_slice(&INLINE.to_le_bytes());
        }
        records.extend_from_slice(&r);
    }
    let at = o.put(&records);
    for j in 0..passes {
        let p = t.at(pass_base + 20 * j as u32);
        for (k, off) in [0u32, 4, 8].into_iter().enumerate() {
            o.reference(at + 20 * j + 4 * k, p.asset(off));
        }
    }
    for j in 0..passes {
        let p = t.at(pass_base + 20 * j as u32);
        if !p.has(16) {
            continue;
        }
        let n = p.u8(12) as usize + p.u8(13) as usize + p.u8(14) as usize;
        let args = p.array(16, n, 8);
        if args.len() != n * 8 {
            return Err("shader arguments outside the loaded memory".into());
        }
        let base = p.u32(16);
        let mut out = args.clone();
        let mut literals: Vec<Vec<u8>> = Vec::new();
        for k in 0..n {
            let ty = u16::from_le_bytes([args[8 * k], args[8 * k + 1]]);
            if ty == 1 || ty == 7 {
                let lp = p.at(base + 8 * k as u32).u32(4);
                let lit = t.v.bytes(lp, 16).ok_or("literal constant outside the loaded memory")?;
                literals.push(lit.to_vec());
                out[8 * k + 4..8 * k + 8].copy_from_slice(&INLINE.to_le_bytes());
            }
        }
        o.put(&out);
        for l in literals {
            o.put(&l);
        }
    }
    if t.has(0) {
        o.put_str(&t.cstr(0).unwrap_or_default());
    }
    Ok(())
}

/// GfxImage. Stock (32 bytes): the load definition pointer, mapType/semantic/category/useSrgbReads (4 bytes), picmip (2), noPicmip,
/// track, cardMemory (8), width, height, depth, delayLoadPixels, name at +28.
///
/// QA (52): load definition +0, the four type bytes +4 (same values), +8 a word (0x00010200 for an image that may be reduced in
/// quality, 0 otherwise) and +12 the no-picmip flag, +16 four card-memory words, width and height +32, depth and delay-load +36, two
/// words +40 (2, the usual value, and 0), the name +48. The four card-memory words are the stock total for the first and last and the
/// totals without the top one and the top two mip levels for the other two (equal for single-level images and for
/// images that may not be reduced).
fn image(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(52);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 4));
    if s.u8(10) != 0 {
        h.u32(12, 1);
    } else {
        h.u32(8, 0x0001_0200);
    }
    let total = s.u32(16);
    let (def, levels, format) = if s.has(0) { (s.u32(0), s.v.u8(s.u32(0)).unwrap_or(1) as u32, s.v.u32(s.u32(0) + 8).unwrap_or(0)) } else { (0, 1, 0) };
    let _ = def;
    let (w, hgt) = (s.u16(20) as u32, s.u16(22) as u32);
    let (d1, d2) = if s.u8(10) != 0 { (total, total) } else { card_memory(total, levels, format, w, hgt) };
    h.u32(16, total);
    h.u32(20, d2);
    h.u32(24, d1);
    h.u32(28, total);
    h.u32(32, s.u32(20));
    h.u32(36, s.u32(24));
    h.u32(40, 2);
    h.u32(48, INLINE);
    o.put(&h.0);
    o.put_str(&s.cstr(28).unwrap_or_default());
    if s.has(0) {
        let def = s.u32(0);
        let head = s.v.bytes(def, 16).ok_or("image load definition outside the loaded memory")?.to_vec();
        let size = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
        let data = s.v.bytes(def + 16, size).ok_or("image data outside the loaded memory")?.to_vec();
        o.put(&head);
        o.put(&data);
    }
    Ok(())
}

/// Bytes the top mip level takes in the stock card-memory total (a guess from the format: Direct3D format codes DXT1, DXT3, DXT5
/// and the uncompressed 32-bit and 8-bit formats), used for the QA record's reduced-quality totals.
fn level_bytes(format: u32, w: u32, h: u32) -> u32 {
    let blocks = |bytes: u32| w.div_ceil(4).max(1) * h.div_ceil(4).max(1) * bytes;
    match format {
        0x3154_5844 => blocks(8),
        0x3354_5844 | 0x3554_5844 | 0x3254_5844 | 0x3454_5844 => blocks(16),
        0x15 | 0x16 | 0x14 => w.max(1) * h.max(1) * 4,
        0x32 | 0x1c | 0x29 => w.max(1) * h.max(1),
        0x33 | 0x34 => w.max(1) * h.max(1) * 2,
        _ => w.max(1) * h.max(1),
    }
}

/// (total without the top level, total without the top two levels) in the image's own card-memory units.
fn card_memory(total: u32, levels: u32, format: u32, w: u32, h: u32) -> (u32, u32) {
    // a level count of 0 means the full chain
    let levels = if levels == 0 { 32 - w.max(h).max(1).leading_zeros() } else { levels };
    if levels <= 1 {
        return (total, total);
    }
    let top = level_bytes(format, w, h);
    let second = level_bytes(format, (w / 2).max(1), (h / 2).max(1));
    (total.saturating_sub(top), total.saturating_sub(top + second))
}

/// Pixel and vertex shaders: {name, runtime pointer, program pointer, program size in words, renderer}. Identical in QA.
fn shader(s: &Src, o: &mut Out) -> Result<(), String> {
    let words = s.u16(12) as usize;
    let mut h = Hdr::new(16);
    h.u32(0, follows(s, 0));
    h.u32(8, follows(s, 8));
    h.raw(12, &s.bytes(12, 4));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(8) {
        let code = s.array(8, words, 4);
        if code.len() != words * 4 {
            return Err("shader program outside the loaded memory".into());
        }
        o.put(&code);
    }
    Ok(())
}

/// Vertex declaration: name, stream count, optional-source flag and the routing table (92 bytes, the last pointer is runtime).
/// Identical in QA.
fn vertex_declaration(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(100);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 96));
    h.u32(36, 0);
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    let _ = cstr_at;
    Ok(())
}

/// `water_t` behind a water texture (semantic 11): 68 bytes, the same in both games: floatTime +0, the two wave-spectrum arrays
/// (`H0X`, `H0Y` at +4 and +8: M * N records of 8 and 4 bytes; M +12, N +16), the animation values, and the water image at +64
/// (a reference). `t` is the texture record (the record's +8 word is the water pointer).
fn water(t: &Src, o: &mut Out) -> Result<(), String> {
    if !t.has(8) {
        return Ok(());
    }
    let w = t.at(t.u32(8));
    let (m, n) = (w.u32(12) as usize, w.u32(16) as usize);
    let cells = m * n;
    let mut h = Hdr::new(68);
    h.raw(0, &w.bytes(0, 68));
    h.u32(4, follows(&w, 4));
    h.u32(8, follows(&w, 8));
    h.u32(64, 0);
    let at = o.put(&h.0);
    o.reference(at + 64, w.asset(64));
    if w.has(4) {
        let a = w.array(4, cells, 8);
        if a.len() != cells * 8 {
            return Err("water spectrum outside the loaded memory".into());
        }
        o.put(&a);
    }
    if w.has(8) {
        let a = w.array(8, cells, 4);
        if a.len() != cells * 4 {
            return Err("water spectrum outside the loaded memory".into());
        }
        o.put(&a);
    }
    Ok(())
}
