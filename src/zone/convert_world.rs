//! Phase 6 of the stock-to-QA zone conversion (`convert_zone`): the map's render world (`GfxWorld`, type 21).
//!
//! Stock offsets are IW4's (ZoneTool `IW4/Structs.hpp`: `GfxWorld` 628 bytes, `GfxWorldDraw`, `GfxLightGrid`, `sunflare_t`,
//! `GfxWorldDpvsStatic`, `GfxWorldDpvsDynamic`, `GfxCell`, `GfxReflectionProbe`; the load order follows OpenAssetTools'
//! `IW4/XAssets/GfxWorld.txt`). The QA record (1,608 bytes) is IW4's with these differences (all of them measured by walking real QA
//! maps):
//!
//! | stock | QA |
//! |---|---|
//! | 0..80 header (names, counts, sky list, dpvs planes, cell trees, cells) | the same |
//! | draw block `GfxWorldDraw` +80, 72 bytes | 84 bytes: after the reflection probes (count, images, origins, textures) three words: a count +96, a list of 12-byte records +100 and a list of 1-byte records +104 (both null here); then the lightmaps and the world vertices, layer data and indices, 12 bytes later |
//! | light grid +152 (56 bytes) | +164 |
//! | model count and models, bounds, checksum, material memory (44 bytes at +208) | +220 |
//! | sun flare +252: flag, sprite and flare materials, 84 bytes of sizes and fades | +264, 104 bytes: two more material references after the first two (null here), the rest as in stock |
//! | (nothing) | 960 bytes at +368 (the "insert", `zone::world::world_insert` decodes it): a count of 24 nine-float records, the records and some map-wide factors |
//! | outdoor lookup matrix +348, outdoor image +412, the rest of the header | +1328 (matrix), +1392 (image), +1396 .. (the rest, 980 bytes later than stock) |
//!
//! Cells are 48 bytes (stock 40: a count byte at +40 and a list of 1-byte records at +44, null here), reflection probes 20 bytes (stock
//! 12: a list pointer +12 and a count +16, null here). The insert region takes its map-wide factors from a real QA map (`mp_nuked`'s:
//! the same in 5 maps checked: a flag, a small float, a larger float, three zero words, a distance and a fraction; the record count is
//! set to 0 and the records are zero), because the converter cannot know them.

use super::convert_zone::{cstr_at, Hdr, Out, Rule, Src, INLINE};

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "GfxWorld" => gfx_world,
        "GameWorldMp" | "GameWorldSp" => no_payload,
        _ => return None,
    })
}

/// The stock game world (the glass data; in single-player maps the path nodes) has no loader in the QA build: its directory entry
/// carries no payload, and nothing refers to it, so the asset is left out.
fn no_payload(_s: &Src, _o: &mut Out) -> Result<(), String> {
    Err("QA has no loader for this type".into())
}

/// The 960 bytes the QA world has between the sun flare and the outdoor lookup matrix, as `mp_nuked` has them with the record count
/// zeroed: words 0..4 zero, +16 a flag (1), +20 0.03125, +24 2.0, +28..+44 zero, +44 a distance (512.0), +48 a fraction (0.25), +56 the
/// record count (0), then the 24 nine-float records (zero).
fn insert_defaults() -> Vec<u8> {
    let mut b = vec![0u8; 960];
    let mut put = |at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(16, 1);
    put(20, 0x3d00_0000);
    put(24, 0x4000_0000);
    put(44, 0x4400_0000);
    put(48, 0x3e80_0000);
    b
}

fn need(s: &Src, ptr: u32, count: usize, stride: usize, what: &str) -> Result<Vec<u8>, String> {
    if count == 0 || !s.has(ptr) {
        return Ok(Vec::new());
    }
    let a = s.array(ptr, count, stride);
    if a.len() != count * stride {
        return Err(format!("{what} outside the loaded memory"));
    }
    Ok(a)
}

/// Sets the words at `offs` of every `stride`-byte record of `data` to "data follows" when they are set in the stock record.
fn inline_words(data: &mut [u8], stride: usize, offs: &[usize]) {
    for rec in data.chunks_mut(stride) {
        for &o in offs {
            let set = u32::from_le_bytes(rec[o..o + 4].try_into().unwrap()) != 0;
            rec[o..o + 4].copy_from_slice(&(if set { INLINE } else { 0 }).to_le_bytes());
        }
    }
}

fn gfx_world(s: &Src, o: &mut Out) -> Result<(), String> {
    let n = |off: u32| s.u32(off) as usize;
    let (planes, nodes, surfaces, skies, lights, cells) = (n(8), n(12), n(16), n(20), n(32), n(52));
    let nz = |off: u32| s.has(off);
    // ---- the QA header
    let mut q = Hdr::new(1608);
    q.raw(0, &s.bytes(0, 80));
    q.raw(80, &s.bytes(80, 16));
    q.raw(108, &s.bytes(96, 56)); // lightmaps .. indices
    q.raw(164, &s.bytes(152, 56)); // light grid
    q.raw(220, &s.bytes(208, 44)); // models, bounds, checksum, material memory
    q.raw(264, &s.bytes(252, 4)); // sun: hasValidData
    q.raw(284, &s.bytes(264, 84)); // sun: the sizes and fades
    q.raw(368, &insert_defaults());
    q.raw(1328, &s.bytes(348, 64)); // outdoor lookup matrix
    q.raw(1392, &s.bytes(412, 216)); // outdoor image .. fogTypesAllowed
    // the world's sort keys (lit decal, effect decal, effect auto, distortion): QA numbers sort keys one higher than IW4 from 30 on,
    // like the materials' (stock QA mp_crash_tropical: 6, 0x28, 0x31, 0x2c)
    for off in [36usize, 40, 44, 48] {
        let k = u32::from_le_bytes(q.0[off..off + 4].try_into().unwrap());
        if k >= 30 {
            q.u32(off, k + 1);
        }
    }
    // pointers: "data follows" where the stock word is set (asset references are patched below)
    for (stock, qa) in [(0u32, 0usize), (4, 4), (24, 24), (56, 56), (60, 60), (64, 64), (68, 68), (72, 72), (76, 76)] {
        q.u32(qa, if nz(stock) { INLINE } else { 0 });
    }
    for (stock, qa) in [(84u32, 84usize), (88, 88), (92, 92), (100, 112), (104, 116), (108, 120), (124, 136), (136, 148), (148, 160)] {
        q.u32(qa, if nz(stock) { INLINE } else { 0 });
    }
    for off in [112u32, 116] {
        q.u32(off as usize + 12, 0); // overrides: references, set below
    }
    q.u32(140, 0);
    q.u32(152, 0);
    for (stock, qa) in [(180u32, 192usize), (188, 200), (196, 208), (204, 216), (212, 224), (248, 260)] {
        q.u32(qa, if nz(stock) { INLINE } else { 0 });
    }
    for k in 0..8u32 {
        q.u32(1396 + 4 * k as usize, if nz(416 + 4 * k) { INLINE } else { 0 });
    }
    for (stock, qa) in [(448u32, 1428usize), (452, 1432)] {
        q.u32(qa, if nz(stock) { INLINE } else { 0 });
    }
    // dpvs (+456, 108 bytes): the runtime arrays +508..+532, the data arrays +532..+552, two more runtime ones
    for off in (508..560u32).step_by(4) {
        if off >= 508 + 24 + 4 && off < 532 {
            continue;
        }
        q.u32(off as usize + 980, if nz(off) { INLINE } else { 0 });
    }
    for off in [580u32, 584, 588, 592, 596, 600, 604, 608] {
        q.u32(off as usize + 980, if nz(off) { INLINE } else { 0 });
    }
    q.u32(620 + 980, if nz(620) { INLINE } else { 0 });
    // the cell count and the sort keys are plain values: already copied
    // 3.19 (1,612 bytes): one more word at +52 behind the sort keys (0x35 in the stock 3.19 mp_crash_tropical, absent in QA's; the
    // rest of the header is QA's four bytes later); header references below add `sh`
    let sh = if super::convert_zone::target_319() {
        let mut v = q.0[..52].to_vec();
        v.extend_from_slice(&0x35u32.to_le_bytes());
        v.extend_from_slice(&q.0[52..]);
        q.0 = v;
        4
    } else {
        0
    };
    let at = o.put(&q.0);

    // ---- stream, in the order the QA loader reads it
    for off in [0u32, 4] {
        if let Some(t) = s.cstr(off) {
            o.put_str(&t);
        }
    }
    // sky list: 16-byte records (surface count, start-surface pointer, image, sampler state)
    if nz(24) && skies > 0 {
        let a = need(s, 24, skies, 16, "sky list")?;
        let base = s.u32(24);
        let mut out = a.clone();
        inline_words(&mut out, 16, &[4]);
        let st = o.put(&out);
        for i in 0..skies {
            let r = s.at(base + 16 * i as u32);
            if r.has(4) {
                let c = r.u32(0) as usize;
                o.put(&need(&r, 4, c, 4, "sky surfaces")?);
            }
            o.reference(st + 16 * i + 8, r.asset(8));
        }
    }
    // dpvs planes, node plane indices
    if nz(56) {
        let bytes = need(s, 56, planes, 20, "dpvs planes")?;
        let at = o.put(&bytes);
        // the collision map's planes are these (the QA loader reads that array once and the map aliases it)
        super::convert_clip::world_planes(o.f.id, at, bytes);
    }
    if nz(60) {
        o.put(&need(s, 60, nodes, 2, "dpvs nodes")?);
    }
    // cell trees: counts, the tree pointers, the nodes of each tree with their static-model index lists
    let counts = if nz(68) { need(s, 68, cells, 4, "tree counts")? } else { Vec::new() };
    o.put(&counts);
    if nz(72) {
        let ptrs = need(s, 72, cells, 4, "cell trees")?;
        let mut out = ptrs.clone();
        inline_words(&mut out, 4, &[0]);
        o.put(&out);
        for i in 0..cells {
            let p = u32::from_le_bytes(ptrs[4 * i..4 * i + 4].try_into().unwrap());
            if p == 0 {
                continue;
            }
            let c = u32::from_le_bytes(counts[4 * i..4 * i + 4].try_into().unwrap()) as usize;
            let t = s.at(p);
            let a = s.v.bytes(p, 44 * c).ok_or("tree nodes outside the loaded memory")?.to_vec();
            let mut out = a.clone();
            inline_words(&mut out, 44, &[36]);
            o.put(&out);
            for j in 0..c {
                let nd = t.sub(44 * j as u32);
                if nd.has(36) {
                    let m = nd.u16(34) as usize;
                    o.put(&need(&nd, 36, m, 2, "tree surfaces")?);
                }
            }
        }
    }
    // cells: 40 bytes in stock, 48 in QA
    if nz(76) {
        let a = need(s, 76, cells, 40, "cells")?;
        let base = s.u32(76);
        let mut out = Vec::with_capacity(48 * cells);
        for rec in a.chunks(40) {
            out.extend_from_slice(rec);
            out.extend_from_slice(&[0u8; 8]);
        }
        inline_words(&mut out, 48, &[28, 36]);
        o.put(&out);
        for i in 0..cells {
            let c = s.at(base + 40 * i as u32);
            if c.has(28) {
                let np = c.u32(24) as usize;
                let pa = need(&c, 28, np, 60, "portals")?;
                let pbase = c.u32(28);
                let mut out = pa.clone();
                inline_words(&mut out, 60, &[28]);
                o.put(&out);
                for j in 0..np {
                    let pt = s.at(pbase + 60 * j as u32);
                    if pt.has(28) {
                        let m = pt.u8(34) as usize;
                        o.put(&need(&pt, 28, m, 12, "portal vertices")?);
                    }
                }
            }
            if c.has(36) {
                let m = c.u8(32) as usize;
                o.put(&need(&c, 36, m, 1, "cell probes")?);
            }
        }
    }

    // ---- draw block: probe images, probe origins (12 -> 20 bytes), lightmaps, vertices, layer data, indices
    let probes = n(80);
    if nz(84) {
        let a = need(s, 84, probes, 4, "reflection probe images")?;
        let base = s.u32(84);
        let st = o.put(&vec![0u8; a.len()]);
        for i in 0..probes {
            o.reference(st + 4 * i, s.at(base + 4 * i as u32).asset(0));
        }
    }
    if nz(88) {
        let a = need(s, 88, probes, 12, "reflection probe origins")?;
        let mut out = Vec::with_capacity(20 * probes);
        for rec in a.chunks(12) {
            out.extend_from_slice(rec);
            out.extend_from_slice(&[0u8; 8]);
        }
        o.put(&out);
    }
    let lightmaps = n(96);
    if nz(100) {
        let a = need(s, 100, lightmaps, 8, "lightmaps")?;
        let base = s.u32(100);
        let st = o.put(&vec![0u8; a.len()]);
        for i in 0..lightmaps {
            for j in 0..2 {
                o.reference(st + 8 * i + 4 * j, s.at(base + 8 * i as u32 + 4 * j as u32).asset(0));
            }
        }
    }
    o.reference(at + sh + 124, s.asset(112));
    o.reference(at + sh + 128, s.asset(116));
    let (vc, lsz, ic) = (n(120), n(132), n(144));
    if nz(124) {
        o.put(&need(s, 124, vc, 44, "world vertices")?);
    }
    if nz(136) {
        o.put(&need(s, 136, lsz, 1, "world vertex layer data")?);
    }
    if nz(148) {
        o.put(&need(s, 148, ic, 2, "world indices")?);
    }
    // ---- light grid (the pointers were set above)
    if nz(180) {
        let axis = s.u32(172) as usize;
        if axis > 2 {
            return Err("light grid axis".into());
        }
        let (lo, hi) = (s.u16(160 + 2 * axis as u32) as usize, s.u16(166 + 2 * axis as u32) as usize);
        if hi < lo {
            return Err("light grid bounds".into());
        }
        o.put(&need(s, 180, hi - lo + 1, 2, "light grid rows")?);
    }
    if nz(188) {
        o.put(&need(s, 188, n(184), 1, "light grid raw rows")?);
    }
    if nz(196) {
        o.put(&need(s, 196, n(192), 4, "light grid entries")?);
    }
    if nz(204) {
        o.put(&need(s, 204, n(200), 168, "light grid colours")?);
    }
    // ---- brush models, material memory
    if nz(212) {
        o.put(&need(s, 212, n(208), 60, "brush models")?);
    }
    if nz(248) {
        let m = n(244);
        let a = need(s, 248, m, 8, "material memory")?;
        let base = s.u32(248);
        let st = o.put(&a);
        for i in 0..m {
            o.reference(st + 8 * i, s.at(base + 8 * i as u32).asset(0));
        }
    }
    // sun flare materials (sprite, flare), outdoor image
    o.reference(at + sh + 268, s.asset(256));
    o.reference(at + sh + 272, s.asset(260));
    o.reference(at + sh + 1392, s.asset(412));
    // ---- shadow geometry (12 bytes per primary light), light regions (8 bytes per light with 80-byte hulls)
    if nz(448) {
        let a = need(s, 448, lights, 12, "shadow geometry")?;
        let base = s.u32(448);
        let mut out = a.clone();
        inline_words(&mut out, 12, &[4, 8]);
        o.put(&out);
        for i in 0..lights {
            let r = s.at(base + 12 * i as u32);
            if r.has(4) {
                o.put(&need(&r, 4, r.u16(0) as usize, 2, "shadow surfaces")?);
            }
            if r.has(8) {
                o.put(&need(&r, 8, r.u16(2) as usize, 2, "shadow models")?);
            }
        }
    }
    if nz(452) {
        let a = need(s, 452, lights, 8, "light regions")?;
        let base = s.u32(452);
        let mut out = a.clone();
        inline_words(&mut out, 8, &[4]);
        o.put(&out);
        for i in 0..lights {
            let r = s.at(base + 8 * i as u32);
            if r.has(4) {
                let hc = r.u32(0) as usize;
                let ha = need(&r, 4, hc, 80, "light region hulls")?;
                let hbase = r.u32(4);
                let mut out = ha.clone();
                inline_words(&mut out, 80, &[76]);
                o.put(&out);
                for j in 0..hc {
                    let hl = s.at(hbase + 80 * j as u32);
                    if hl.has(76) {
                        o.put(&need(&hl, 76, hl.u32(72) as usize, 20, "light region axes")?);
                    }
                }
            }
        }
    }
    // ---- static geometry: sorted surface indices, static model instances, surfaces, bounds, static model draw instances
    let (models, sorted) = (n(456), n(460) + n(464));
    if nz(532) {
        o.put(&need(s, 532, sorted, 2, "sorted surface indices")?);
    }
    if nz(536) {
        o.put(&need(s, 536, models, 36, "static model instances")?);
    }
    if nz(540) {
        let a = need(s, 540, surfaces, 24, "surfaces")?;
        let base = s.u32(540);
        let st = o.put(&a);
        for i in 0..surfaces {
            o.reference(st + 24 * i + 16, s.at(base + 24 * i as u32).asset(16));
        }
    }
    if nz(544) {
        o.put(&need(s, 544, surfaces, 24, "surface bounds")?);
    }
    if nz(548) {
        let a = need(s, 548, models, 76, "static model draw instances")?;
        if std::env::var_os("ATLAS_CHECK_TREES").is_some() {
            for v in [0usize, 1, 2] {
                let f: Vec<f32> = (0..3).map(|k| f32::from_le_bytes(a[76 * v + 4 * k..76 * v + 4 * k + 4].try_into().unwrap())).collect();
                eprintln!("iw4 -> codol smodel {v}: origin {f:?}");
            }
        }
        let base = s.u32(548);
        let st = o.put(&a);
        for i in 0..models {
            o.reference(st + 76 * i + 52, s.at(base + 76 * i as u32).asset(52));
        }
    }
    // ---- hero lights
    if nz(620) {
        o.put(&need(s, 620, n(616), 56, "hero lights")?);
    }
    let _ = cstr_at;
    Ok(())
}
