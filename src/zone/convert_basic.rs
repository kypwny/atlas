//! Phase 1 of the stock-to-QA zone conversion (`convert_zone`): the assets whose layouts are equal or differ by a field or
//! two: raw files, string tables, localised strings, map entities, the common and effect worlds, light definitions, tracers.
//! Offsets are the stock (IW4) structure's, named after ZoneTool's `Structs.hpp` and OpenAssetTools' `IW4_Assets.h`.

use super::convert_zone::{Hdr, Out, Rule, Src, INLINE};

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "RawFile" => rawfile,
        "StringTable" => string_table,
        "LocalizeEntry" => localize,
        "MapEnts" => map_ents,
        "ComWorld" => com_world,
        "FxWorld" => fx_world,
        "GfxLightDef" => light_def,
        "TracerDef" => tracer,
        "XAnimParts" => xanim,
        "Font_s" => font,
        _ => return None,
    })
}

/// `-1` ("data follows") when the stock pointer field is set, else null.
fn follows(s: &Src, off: u32) -> u32 {
    if s.has(off) { INLINE } else { 0 }
}

/// RawFile: name, compressedLen, len, data. Identical in QA; the data is `compressedLen` bytes when compressed, else `len + 1`.
fn rawfile(s: &Src, o: &mut Out) -> Result<(), String> {
    let (compressed, len) = (s.u32(4), s.u32(8));
    let n = if compressed != 0 { compressed } else { len.wrapping_add(1) } as usize;
    let mut h = Hdr::new(16);
    h.u32(0, follows(s, 0));
    h.u32(4, compressed);
    h.u32(8, len);
    h.u32(12, follows(s, 12));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(12) {
        let data = s.array(12, n, 1);
        if data.len() != n {
            return Err("raw file data outside the loaded memory".into());
        }
        o.put(&data);
    }
    Ok(())
}

/// StringTable: name, columnCount, rowCount, values (cells of a string pointer and a hash). Identical in QA.
fn string_table(s: &Src, o: &mut Out) -> Result<(), String> {
    let (columns, rows) = (s.u32(4) as usize, s.u32(8) as usize);
    let count = columns * rows;
    let mut h = Hdr::new(16);
    h.u32(0, follows(s, 0));
    h.u32(4, columns as u32);
    h.u32(8, rows as u32);
    h.u32(12, follows(s, 12));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(12) {
        let cells = s.array(12, count, 8);
        if cells.len() != count * 8 {
            return Err("string table cells outside the loaded memory".into());
        }
        let mut out = cells.clone();
        let mut strings = Vec::new();
        for i in 0..count {
            let p = u32::from_le_bytes(cells[8 * i..8 * i + 4].try_into().unwrap());
            if p != 0 {
                out[8 * i..8 * i + 4].copy_from_slice(&INLINE.to_le_bytes());
                strings.push(super::convert_zone::cstr_at(s.v, p));
            }
        }
        o.put(&out);
        for t in strings {
            o.put_str(&t);
        }
    }
    Ok(())
}

/// LocalizeEntry: value, name (two strings). Identical in QA.
fn localize(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(8);
    h.u32(0, follows(s, 0));
    h.u32(4, follows(s, 4));
    o.put(&h.0);
    // the stock zone's value is the first member, the key the second; the QA walker reads them in the same order
    if let Some(v) = s.cstr(0) {
        o.put_str(&v);
    }
    if let Some(k) = s.cstr(4) {
        o.put_str(&k);
    }
    Ok(())
}

/// MapEnts: stock 44 bytes (name, entityString, numEntityChars, MapTriggers {models, hulls, slabs}, stages, stageCount); the QA
/// record is the first 36 bytes of it. The QA collision map holds the triggers and the stages (IW5 moved them there), so they are
/// handed over to `convert_clip` and the record carries none; the collision map also stores the record itself inline, and the
/// asset is dropped when it did (`convert_clip::consumed`).
fn map_ents(s: &Src, o: &mut Out) -> Result<(), String> {
    let chars = s.u32(8) as usize;
    let mut h = Hdr::new(36);
    h.u32(0, follows(s, 0));
    h.u32(4, follows(s, 4));
    h.u32(8, chars as u32);
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(4) {
        let text = s.array(4, chars, 1);
        if text.len() != chars {
            return Err("entity string outside the loaded memory".into());
        }
        o.put(&text);
    }
    let mut stash = super::convert_clip::MapEntsStash { record: o.f.bytes.clone(), ..Default::default() };
    // MapTriggers: modelCount, models, hullCount, hulls, slabCount, slabs
    for (k, stride) in [8usize, 32, 20].into_iter().enumerate() {
        let off = 16 + 8 * k as u32;
        let n = s.u32(12 + 8 * k as u32) as usize;
        stash.triggers[k].0 = n as u32;
        if s.has(off) {
            let a = s.array(off, n, stride);
            if a.len() != n * stride {
                return Err("map trigger data outside the loaded memory".into());
            }
            stash.triggers[k].1 = a;
        }
    }
    // stages: 20 bytes each, a name first
    let count = s.u8(40) as usize;
    if s.has(36) && count > 0 {
        stash.stages = s.array(36, count, 20);
        if stash.stages.len() != count * 20 {
            return Err("stages outside the loaded memory".into());
        }
        let base = s.u32(36);
        for i in 0..count {
            stash.stage_names.push(s.at(base + 20 * i as u32).cstr(0));
        }
    }
    super::convert_clip::stash_map_ents(o.f.id, stash);
    Ok(())
}

/// ComWorld: name, isInUse, primaryLightCount, primaryLights (68 bytes each, the last word a name). Identical in QA.
fn com_world(s: &Src, o: &mut Out) -> Result<(), String> {
    let n = s.u32(8) as usize;
    let mut h = Hdr::new(16);
    h.u32(0, follows(s, 0));
    h.u32(4, s.u32(4));
    h.u32(8, n as u32);
    h.u32(12, follows(s, 12));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(12) {
        let lights = s.array(12, n, 68);
        if lights.len() != n * 68 {
            return Err("primary lights outside the loaded memory".into());
        }
        let mut out = lights.clone();
        let mut names = Vec::new();
        for i in 0..n {
            let p = u32::from_le_bytes(lights[68 * i + 64..68 * i + 68].try_into().unwrap());
            if p != 0 {
                out[68 * i + 64..68 * i + 68].copy_from_slice(&INLINE.to_le_bytes());
                names.push(super::convert_zone::cstr_at(s.v, p));
            }
        }
        o.put(&out);
        for t in names {
            o.put_str(&t);
        }
    }
    Ok(())
}

/// FxWorld: name and the glass system (112 bytes at +4). Identical in QA: the glass definitions (36 bytes, referencing a physics
/// preset and two materials), nine zero-filled runtime arrays that take no stream bytes, then the initial piece data.
fn fx_world(s: &Src, o: &mut Out) -> Result<(), String> {
    let g = 4u32;
    let defs = s.u32(g + 8) as usize;
    let init_pieces = s.u32(g + 20) as usize;
    let mut h = Hdr::new(116);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 112));
    // pointers in the glass system: defs at +48, nine runtime arrays +52..+88, three arrays +88, +92, +96
    for off in [g + 48, g + 52, g + 56, g + 60, g + 64, g + 68, g + 72, g + 76, g + 80, g + 84, g + 88, g + 92, g + 96] {
        h.u32(off as usize, follows(s, off));
    }
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(g + 48) {
        let a = s.array(g + 48, defs, 36);
        if a.len() != defs * 36 {
            return Err("glass definitions outside the loaded memory".into());
        }
        let base = s.u32(g + 48);
        let at = o.put(&a);
        for i in 0..defs {
            let d = s.at(base + 36 * i as u32);
            // physPreset +32, material +24, materialShattered +28
            for off in [32u32, 24, 28] {
                let id = d.asset(off);
                o.reference(at + 36 * i + off as usize, id);
            }
        }
    }
    if s.has(g + 88) {
        let a = s.array(g + 88, init_pieces, 2);
        o.put(&a);
    }
    if s.has(g + 92) {
        let a = s.array(g + 92, init_pieces, 52);
        o.put(&a);
    }
    if s.has(g + 96) {
        let n = s.u32(g + 44) as usize;
        let a = s.array(g + 96, n, 4);
        o.put(&a);
    }
    Ok(())
}

/// GfxLightDef: name, attenuation (image, sampler state), lmapLookupStart. Identical in QA.
fn light_def(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(16);
    h.u32(0, follows(s, 0));
    h.u32(8, s.u32(8));
    h.u32(12, s.u32(12));
    let at = o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    o.reference(at + 4, s.asset(4));
    Ok(())
}

/// TracerDef: stock 112 bytes; QA 116 with an effect reference inserted at +8 (the rest moves up by 4).
fn tracer(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(116);
    h.u32(0, follows(s, 0));
    h.raw(12, &s.bytes(8, 104));
    let at = o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    o.reference(at + 4, s.asset(4));
    // the QA record's effect (+8) has no stock counterpart: none
    Ok(())
}

/// XAnimParts (88 bytes in both games): a name, the delta tracks, the bone-name list, the notifications and the frame data. None of
/// its pointers is a reusable one, so the stock stream of the asset is the QA stream.
fn xanim(s: &Src, o: &mut Out) -> Result<(), String> {
    o.put(s.v.entry_stream());
    Ok(())
}

/// Font_s (24 bytes in both): name, pixelHeight, glyphCount, material, glowMaterial, glyphs (24 bytes each; a reusable pointer in
/// stock zones).
fn font(s: &Src, o: &mut Out) -> Result<(), String> {
    let count = s.u32(8) as usize;
    let mut h = Hdr::new(24);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 8));
    h.u32(20, if count > 0 && s.has(20) { INLINE } else { 0 });
    let at = o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    o.reference(at + 12, s.asset(12));
    o.reference(at + 16, s.asset(16));
    if count > 0 && s.has(20) {
        let g = s.array(20, count, 24);
        if g.len() != count * 24 {
            return Err("glyphs outside the loaded memory".into());
        }
        o.put(&g);
    }
    Ok(())
}
