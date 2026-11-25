//! Phase 3 of the stock-to-QA zone conversion (`convert_zone`): physics presets, physics collision maps, models with their surface
//! records and surfaces.
//!
//! Stock offsets are the IW4 structures' (ZoneTool `Structs.hpp`: `PhysPreset`, `PhysCollmap`, `PhysGeomInfo`, `BrushWrapper`,
//! `XModel`, `XModelLodInfo`, `XModelSurfs`, `XSurface`, `XModelCollSurf_s`, `XBoneInfo`; OpenAssetTools `IW4_Assets.h`). The QA
//! layouts are the ones the QA walker (`Replay`) consumes; the values of fields the stock structures lack come from real QA assets
//! (written next to each field).
//!
//! The model-surface records (`XModelSurfs`) are not assets of their own in either game. In a stock zone they are loaded in the
//! temporary block while their model loads, so the converter's walker announces each one as it is loaded (`convert_walk`) and
//! `surfs_event` converts it to the QA stream bytes; the first model that uses a record stores it inline (with a pointer slot), any
//! other model that names the same record refers to that slot.

use super::convert_zone::{Hdr, Out, Rule, Src, INLINE, INSERT};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "PhysPreset" => phys_preset,
        "PhysCollmap" => phys_collmap,
        "XModel" => model,
        _ => return None,
    })
}

thread_local! {
    /// Converted model-surface records by walker id: stream bytes.
    static SURFS: RefCell<HashMap<u32, (Vec<u8>, Option<Vec<u8>>)>> = RefCell::new(HashMap::new());
    /// Ids of the records already stored inline in a model.
    static EMITTED: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
}

/// Development aid: every converted model-surface record (stream bytes).
pub fn all_surfs() -> Vec<Vec<u8>> {
    SURFS.with(|m| m.borrow().values().map(|v| v.0.clone()).collect())
}

/// Forgets the records of the previous zone.
pub fn reset() {
    SURFS.with(|s| s.borrow_mut().clear());
    EMITTED.with(|s| s.borrow_mut().clear());
}

fn follows(s: &Src, off: u32) -> u32 {
    if s.has(off) { INLINE } else { 0 }
}

/// PhysPreset. Stock (44 bytes): name, type, mass, bounce, friction, bulletForceScale, explosiveForceScale, sndAliasPrefix,
/// piecesSpreadFraction, piecesUpwardVelocity, tempDefaultToCylinder, perSurfaceSndAlias.
///
/// QA (68 bytes, the IW5 layout of OpenAssetTools' `IW5_Assets.h`): the first ten members are the same; then minMomentum +40,
/// maxMomentum +44, minPitch +48, maxPitch +52, volumeType +56, pitchType +60 (the scaling enums), tempDefaultToCylinder +64 and
/// perSurfaceSndAlias +65. Read from 64 QA models' presets: the six new members are all zero unless the preset names a sound alias
/// prefix; then they are 250.0, 250.0, 1.0, 1.0, 0, 0 (23 of 23 presets with a prefix that were located).
fn phys_preset(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(68);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 24)); // type, mass, bounce, friction, bulletForceScale, explosiveForceScale
    h.u32(28, follows(s, 28));
    h.raw(32, &s.bytes(32, 8)); // piecesSpreadFraction, piecesUpwardVelocity
    if s.has(28) {
        h.raw(40, &250.0f32.to_le_bytes());
        h.raw(44, &250.0f32.to_le_bytes());
        h.raw(48, &1.0f32.to_le_bytes());
        h.raw(52, &1.0f32.to_le_bytes());
    }
    h.u8(64, s.u8(40)); // tempDefaultToCylinder
    h.u8(65, s.u8(41)); // perSurfaceSndAlias
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if let Some(prefix) = s.cstr(28) {
        o.put_str(&prefix);
    }
    Ok(())
}

/// PhysCollmap (72 bytes in both): name, geomCount, geoms, mass (a 7-word `PhysMass`), bounds. Each of the `geomCount` geometries
/// (68 bytes: brush pointer, type, orientation, offset, half lengths) may carry a brush (68 bytes) with its sides (8 bytes each,
/// plane pointer first), the plane behind each side (20 bytes), the base adjacent side bytes (`totalEdgeCount` of them) and the
/// brush's own plane array (20 bytes each). Every pointer is "data follows" in QA; the stock zone's shared planes are copied.
fn phys_collmap(s: &Src, o: &mut Out) -> Result<(), String> {
    let count = s.u32(4) as usize;
    let mut h = Hdr::new(72);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 68));
    h.u32(8, follows(s, 8));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if !s.has(8) {
        return Ok(());
    }
    let geoms = s.array(8, count, 68);
    if geoms.len() != count * 68 {
        return Err("collision geometry outside the loaded memory".into());
    }
    let base = s.u32(8);
    let mut out = geoms.clone();
    for i in 0..count {
        let g = s.at(base + 68 * i as u32);
        out[68 * i..68 * i + 4].copy_from_slice(&follows(&g, 0).to_le_bytes());
    }
    o.put(&out);
    for i in 0..count {
        let g = s.at(base + 68 * i as u32);
        if !g.has(0) {
            continue;
        }
        let b = s.at(g.u32(0));
        let n = b.u16(24) as usize;
        let mut brush = b.bytes(0, 68);
        if brush.len() != 68 {
            return Err("brush outside the loaded memory".into());
        }
        let (sides_p, adj_p, planes_p, edges) = (b.has(28), b.has(32), b.has(64), b.u32(60) as usize);
        for (off, set) in [(28usize, sides_p), (32, adj_p), (64, planes_p)] {
            brush[off..off + 4].copy_from_slice(&(if set { INLINE } else { 0 }).to_le_bytes());
        }
        o.put(&brush);
        if sides_p {
            let sides = b.array(28, n, 8);
            if sides.len() != n * 8 {
                return Err("brush sides outside the loaded memory".into());
            }
            let sbase = b.u32(28);
            let mut out = sides.clone();
            let mut planes = Vec::new();
            for j in 0..n {
                let sd = s.at(sbase + 8 * j as u32);
                if sd.has(0) {
                    out[8 * j..8 * j + 4].copy_from_slice(&INLINE.to_le_bytes());
                    planes.push(s.v.bytes(sd.u32(0), 20).ok_or("brush plane outside the loaded memory")?.to_vec());
                } else {
                    out[8 * j..8 * j + 4].copy_from_slice(&0u32.to_le_bytes());
                }
            }
            o.put(&out);
            for p in planes {
                o.put(&p);
            }
        }
        if adj_p {
            let a = b.array(32, edges, 1);
            if a.len() != edges {
                return Err("brush edge data outside the loaded memory".into());
            }
            o.put(&a);
        }
        if planes_p {
            let p = b.array(64, n, 20);
            if p.len() != n * 20 {
                return Err("brush planes outside the loaded memory".into());
            }
            o.put(&p);
        }
    }
    Ok(())
}

/// XModelSurfs. Stock (36 bytes): name, surfs, numsurfs (u16, then 2 bytes of padding), partBits[6]. QA (48): the same 36 bytes, then
/// three more part-bit words (zero in every QA record). Behind the header: the name, the surfaces (QA 84 bytes each) and, for each
/// surface in turn, its blend data, vertices, rigid vertex lists with their collision trees, and triangle indices.
///
/// XSurface, stock (64 bytes): tileMode +0, deformed +1, vertCount +2, triCount +4, streamHandle +6, zoneHandle +7, baseTriIndex +8,
/// baseVertIndex +10, triIndices +12, vertexInfo {vertCount[4] +16, vertsBlend +24}, verticies +28, vertListCount +32,
/// rigidVertLists +36, partBits[6] +40.
/// QA (84): tileMode +0, deformed +1, a flag word +2 (0x20; 0x24 for a deformed surface: 889 of 889 QA surfaces), vertCount +4,
/// triCount +6, baseTriIndex +8 and baseVertIndex +12 as words (cumulative over the surfaces of a record, as in the stock ones),
/// triIndices +16, vertCount[4] +20, vertsBlend +28, verticies +32, a zero word +36, vertListCount +40, rigidVertLists +44,
/// a zero word +48 (889 of 889), partBits[6] +52, two zero words (the part-bit arrays are wider; the extra words are zero in every QA
/// surface checked).
pub fn surfs_event(s: &Src, id: u32) {
    let bytes = surfs_bytes(s).unwrap_or_default();
    SURFS.with(|m| m.borrow_mut().insert(id, (bytes, s.cstr(0))));
}

fn surfs_bytes(s: &Src) -> Option<Vec<u8>> {
    let mut o = Out::new(3, 0);
    let count = s.u16(8) as usize;
    let mut h = Hdr::new(48);
    h.u32(0, follows(s, 0));
    h.u32(4, follows(s, 4));
    h.u16(8, count as u16);
    h.raw(12, &s.bytes(12, 24));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if !s.has(4) {
        return Some(o.f.bytes);
    }
    let base = s.u32(4);
    let stock = s.array(4, count, 64);
    if stock.len() != count * 64 {
        return None;
    }
    let mut table = vec![0u8; 84 * count];
    for i in 0..count {
        let x = s.at(base + 64 * i as u32);
        let q = &mut table[84 * i..84 * (i + 1)];
        let put32 = |q: &mut [u8], at: usize, v: u32| q[at..at + 4].copy_from_slice(&v.to_le_bytes());
        q[0] = x.u8(0);
        q[1] = x.u8(1);
        let flags: u16 = if x.u8(1) != 0 { 0x24 } else { 0x20 };
        q[2..4].copy_from_slice(&flags.to_le_bytes());
        q[4..6].copy_from_slice(&x.u16(2).to_le_bytes());
        q[6..8].copy_from_slice(&x.u16(4).to_le_bytes());
        put32(q, 8, x.u16(8) as u32);
        put32(q, 12, x.u16(10) as u32);
        put32(q, 16, follows(&x, 12));
        q[20..28].copy_from_slice(&x.bytes(16, 8));
        put32(q, 28, follows(&x, 24));
        put32(q, 32, follows(&x, 28));
        put32(q, 40, x.u32(32));
        put32(q, 44, follows(&x, 36));
        q[52..76].copy_from_slice(&x.bytes(40, 24));
    }
    o.put(&table);
    for i in 0..count {
        let x = s.at(base + 64 * i as u32);
        let (verts, tris) = (x.u16(2) as usize, x.u16(4) as usize);
        if x.has(24) {
            let c: Vec<usize> = (0..4).map(|j| x.u16(16 + 2 * j) as usize).collect();
            let total = c[0] + 3 * c[1] + 5 * c[2] + 7 * c[3];
            let a = x.array(24, total, 2);
            if a.len() != total * 2 {
                return None;
            }
            o.put(&a);
        }
        if x.has(28) {
            let a = x.array(28, verts, 32);
            if a.len() != verts * 32 {
                return None;
            }
            o.put(&a);
        }
        if x.has(36) {
            let n = x.u32(32) as usize;
            let lists = x.array(36, n, 12);
            if lists.len() != n * 12 {
                return None;
            }
            let lbase = x.u32(36);
            let mut out = lists.clone();
            for j in 0..n {
                let l = s.at(lbase + 12 * j as u32);
                out[12 * j + 8..12 * j + 12].copy_from_slice(&follows(&l, 8).to_le_bytes());
            }
            o.put(&out);
            for j in 0..n {
                let l = s.at(lbase + 12 * j as u32);
                if !l.has(8) {
                    continue;
                }
                let t = s.at(l.u32(8));
                let mut tree = t.bytes(0, 40);
                if tree.len() != 40 {
                    return None;
                }
                tree[28..32].copy_from_slice(&follows(&t, 28).to_le_bytes());
                tree[36..40].copy_from_slice(&follows(&t, 36).to_le_bytes());
                o.put(&tree);
                if t.has(28) {
                    let n = t.u32(24) as usize;
                    let a = t.array(28, n, 16);
                    if a.len() != n * 16 {
                        return None;
                    }
                    o.put(&a);
                }
                if t.has(36) {
                    let n = t.u32(32) as usize;
                    let a = t.array(36, n, 2);
                    if a.len() != n * 2 {
                        return None;
                    }
                    o.put(&a);
                }
            }
        }
        if x.has(12) {
            let a = x.array(12, tris, 6);
            if a.len() != tris * 6 {
                return None;
            }
            o.put(&a);
        }
    }
    Some(o.f.bytes)
}

/// XModel. Stock (304 bytes): name +0, numBones +4, numRootBones +5, numSurfaces +6, lodRampType +7, scale +8, noScalePartBits[6] +12,
/// boneNames +36, parentList +40, tagAngles +44, tagPositions +48, partClassification +52, animMatrix +56, materials +60,
/// lodInfo[4] +64 (44 bytes each: dist, numSurfacesInLod, surfIndex, surfaces, partBits[6], surfs, lod, smcBaseIndexPlusOne,
/// smcSubIndexMask, smcBucket), maxLoadedLod +240, numLods +241, collLod +242, flags +243, colSurf +244, numColSurfs +248, contents
/// +252, boneInfo +256, radius +260, bounds +264, memUsage +288, bad +292, physPreset +296, physCollmap +300.
///
/// QA (360 bytes): noScalePartBits are eight words (the pointers from boneNames on sit 8 bytes later: boneNames +44 ... materials
/// +68, lodInfo +72); a lod record is 56 bytes: dist +0, numSurfacesInLod +4, surfIndex +6, surfaces (an inserted record) +8, the
/// record's name word +12 (the address of the surface record's name), partBits[9] +16, lod / smcBaseIndexPlusOne / smcSubIndexMask /
/// smcBucket +52; everything after the lods is 56 bytes later than in the stock model (colSurf +300, ..., physCollmap +356).
///
/// The stock lods carry no static-model-cache bytes (smcBase..smcBucket are zero outside `lod`): the QA ones do not either unless
/// the linker precomputed them; they are left as the stock zone has them.
fn model(s: &Src, o: &mut Out) -> Result<(), String> {
    let (bones, roots, surfs) = (s.u8(4) as usize, s.u8(5) as usize, s.u8(6) as usize);
    if roots > bones {
        return Err("more root bones than bones".into());
    }
    if let Ok(filter) = std::env::var("ATLAS_MODEL_PROBE") {
        let name = String::from_utf8_lossy(&s.cstr(0).unwrap_or_default()).into_owned();
        if name.contains(&filter) {
            let b = s.bytes(240, 4);
            eprintln!("model probe {name}: numLods {} collLod {} flags {:#x} colSurf {} numColSurfs {} contents {:#x}",
                b[1], b[2] as i8, b[3], if s.has(244) { "yes" } else { "null" }, s.u32(248), s.u32(252));
        }
    }
    let mut h = Hdr::new(360);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 8)); // numBones .. scale
    h.raw(12, &s.bytes(12, 24)); // noScalePartBits[0..6]; words 6 and 7 are zero
    // boneNames, parentList, tagAngles, tagPositions, partClassification, animMatrix, materials: stock +36.. -> QA +44..
    for k in 0..7u32 {
        h.u32(44 + 4 * k as usize, follows(s, 36 + 4 * k));
    }
    h.raw(296, &s.bytes(240, 4)); // maxLoadedLod, numLods, collLod, flags
    h.u32(300, follows(s, 244)); // colSurf
    h.raw(304, &s.bytes(248, 8)); // numColSurfs, contents
    h.u32(312, follows(s, 256)); // boneInfo
    h.raw(316, &s.bytes(260, 32)); // radius, bounds, memUsage
    h.raw(348, &s.bytes(292, 4)); // bad + padding
    let at = o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    // skeleton: names (2 bytes per bone), parents (1 per non-root bone), angles (8), positions (12), part classification (1), base
    // matrices (32 per bone)
    let non_root = bones - roots;
    for (off, count, stride) in [(36u32, bones, 2usize), (40, non_root, 1), (44, non_root, 8), (48, non_root, 12), (52, bones, 1), (56, bones, 32)] {
        if s.has(off) {
            let a = s.array(off, count, stride);
            if a.len() != count * stride {
                return Err("skeleton outside the loaded memory".into());
            }
            o.put(&a);
        }
    }
    // materials: one reference per surface
    if s.has(60) {
        let m = o.put(&vec![0u8; 4 * surfs]);
        let base = s.u32(60);
        for i in 0..surfs {
            o.reference(m + 4 * i, s.at(base + 4 * i as u32).asset(0));
        }
    }
    // lods: the surface record (inline the first time a model uses it) and the lod's name word
    for i in 0..4u32 {
        let l = s.sub(64 + 44 * i);
        let q = at + 72 + 56 * i as usize;
        o.f.bytes[q..q + 8].copy_from_slice(&l.bytes(0, 8)); // dist, numSurfacesInLod, surfIndex
        o.f.bytes[q + 16..q + 40].copy_from_slice(&l.bytes(12, 24)); // partBits[0..6]
        o.f.bytes[q + 52..q + 56].copy_from_slice(&l.bytes(40, 4)); // lod, smc bytes
        let Some(id) = l.asset(8) else { continue };
        let Some((bytes, name)) = SURFS.with(|m| m.borrow().get(&id).cloned()) else { return Err("a model surface record was not seen".into()) };
        if EMITTED.with(|e| e.borrow_mut().insert(id)) {
            o.set(q + 8, INSERT);
            o.anchor(id);
            o.put(&bytes);
        } else {
            o.reference(q + 8, Some(id));
        }
        if let Some(n) = name {
            o.set(q + 12, INLINE);
            o.put_str(&n);
        }
    }
    // collision surfaces (44 bytes: triangles, triangle count, bounds, boneIdx, contents, surfFlags; triangles are 48 bytes)
    if s.has(244) {
        let n = s.u32(248) as usize;
        let a = s.array(244, n, 44);
        if a.len() != n * 44 {
            return Err("collision surfaces outside the loaded memory".into());
        }
        let base = s.u32(244);
        let mut out = a.clone();
        for j in 0..n {
            out[44 * j..44 * j + 4].copy_from_slice(&follows(&s.at(base + 44 * j as u32), 0).to_le_bytes());
        }
        o.put(&out);
        for j in 0..n {
            let c = s.at(base + 44 * j as u32);
            if c.has(0) {
                let t = c.u32(4) as usize;
                let tris = c.array(0, t, 48);
                if tris.len() != t * 48 {
                    return Err("collision triangles outside the loaded memory".into());
                }
                o.put(&tris);
            }
        }
    }
    // bone info (28 bytes per bone: bounds and radius squared)
    if s.has(256) {
        let a = s.array(256, bones, 28);
        if a.len() != bones * 28 {
            return Err("bone info outside the loaded memory".into());
        }
        o.put(&a);
    }
    o.reference(at + 352, s.asset(296)); // physPreset
    o.reference(at + 356, s.asset(300)); // physCollmap
    Ok(())
}
