//! The render world, Call of Duty 4 (IW3) -> Modern Warfare 2 (IW4).
//!
//! IW4 regrouped the world: the vertex, index, lightmap and reflection probe data sit in an embedded `draw`, the sky is a record of
//! its own, each cell's bounding-box tree moved out of the cell (`aabbTreeCounts`, `aabbTrees`), surfaces lost their box to a
//! parallel array (`surfacesBounds`), and boxes are stored as mid point and half size. Members with the same name convert by name
//! (the generic conversion); the rest is done here. Without these rules the by-name copy alone left a world with no vertices,
//! indices, lightmaps, sky or surface boxes (the QA client crashed sorting the surfaces: the box array was null).

use super::convert_engine::{Conv, Rules, R, S, T};

thread_local! {
    /// Source address and element size of the world's cells (a portal names its cell by pointer in IW3, by index in IW4).
    static CELLS: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 56)) };
}

fn f32_at(c: &Conv, addr: u32) -> f32 {
    c.s_bytes(addr, 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0.0)
}

fn i32_at(c: &Conv, addr: u32) -> i32 {
    c.s_bytes(addr, 4).map(|b| i32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0)
}

/// Writes the box with corners at the source addresses `min` and `max` as mid point and half size at the heap address `at`.
fn put_bounds(c: &mut Conv, at: u32, min: u32, max: u32) {
    for k in 0..3u32 {
        let (lo, hi) = (f32_at(c, min + 4 * k), f32_at(c, max + 4 * k));
        c.heap.put(at + 4 * k, &((lo + hi) / 2.0).to_le_bytes());
        c.heap.put(at + 12 + 4 * k, &((hi - lo) / 2.0).max(0.0).to_le_bytes());
    }
}

fn gfx_world_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    let cells = c.sp(s, "cells");
    let cell_size = c.ssid("GfxCell").map(|k| c.ssize(k)).unwrap_or(56);
    CELLS.with(|x| x.set((cells, cell_size)));
    c.generic_members_except(s, t, &["bounds", "skies", "aabbTreeCounts", "aabbTrees", "cellHasSunLitSurfsBits", "heroOnlyLights"])?;
    let (min, max) = (c.s_at(s, "mins"), c.s_at(s, "maxs"));
    let at = c.t_at(t, "bounds");
    put_bounds(c, at, min, max);
    let sun = c.si(s, "sunPrimaryLightIndex");
    c.t_set_i(t, "lastSunPrimaryLightIndex", sun);
    if let Some(grid) = c.t_sub(t, "lightGrid") {
        c.t_set_i(grid, "lastSunPrimaryLightIndex", sun);
    }
    // the sort keys IW4's linker writes
    c.t_set_i(t, "sortKeyLitDecal", 0x6);
    c.t_set_i(t, "sortKeyEffectDecal", 0x27);
    c.t_set_i(t, "sortKeyEffectAuto", 0x30);
    c.t_set_i(t, "sortKeyDistortion", 0x2b);
    c.t_set_i(t, "fogTypesAllowed", 1);
    // the sky: one record
    c.t_set_i(t, "skyCount", 1);
    if let Some(sky) = c.t_alloc(t, "skies", 1) {
        c.generic_members(s, sky)?;
    }
    // draw: lightmaps, vertices, layer data, indices by name; the reflection probes split into images and origins
    if let Some(draw) = c.t_sub(t, "draw") {
        c.generic_members_except(s, draw, &["reflectionProbes", "reflectionProbeOrigins", "lightmapOverridePrimary", "lightmapOverrideSecondary"])?;
        let n = c.si(s, "reflectionProbeCount").max(0) as usize;
        let probes = c.sp(s, "reflectionProbes");
        if n > 0 && probes != 0 {
            let origins = c.heap.alloc(12 * n, 4);
            let images = c.heap.alloc(4 * n, 4);
            for i in 0..n as u32 {
                c.copy_raw(probes + 16 * i, origins + 12 * i, 12);
                let a = c.asset_at_slot(probes + 16 * i + 12);
                c.heap.set_ptr(images + 4 * i, a);
            }
            c.t_set_ptr(draw, "reflectionProbeOrigins", origins);
            c.t_set_ptr(draw, "reflectionProbes", images);
        }
    }
    // the cells' trees
    let count = c.ssub(s, "dpvsPlanes").map(|p| c.si(p, "cellCount")).unwrap_or(0).max(0) as usize;
    if count > 0 && cells != 0 {
        let (Some(sk), Some(tk)) = (c.ssid("GfxAabbTree"), c.tsid("GfxAabbTree")) else { return Err("no GfxAabbTree".to_string()) };
        let (ssz, tsz) = (c.ssize(sk), c.tsize(tk));
        let counts = c.heap.alloc(4 * count, 4);
        let trees = c.heap.alloc(4 * count, 4);
        for i in 0..count as u32 {
            let cell = cells + cell_size * i;
            let n = i32_at(c, cell + 24).max(0) as u32;
            let src = c.s_ptr(cell + 28);
            c.heap.set_u32(counts + 4 * i, n);
            if n > 0 && src != 0 {
                let arr = c.heap.alloc((tsz * n) as usize, 4);
                for k in 0..n {
                    c.generic(S { sid: sk, addr: src + k * ssz }, T { sid: tk, addr: arr + k * tsz })?;
                }
                c.heap.set_ptr(trees + 4 * i, arr);
            }
        }
        c.t_set_ptr(t, "aabbTreeCounts", counts);
        c.t_set_ptr(t, "aabbTrees", trees);
        // every cell may hold sunlit surfaces (IW3 has no such table)
        let words = count.div_ceil(32);
        let bits = c.heap.alloc(4 * words, 4);
        for w in 0..words as u32 {
            c.heap.set_u32(bits + 4 * w, u32::MAX);
        }
        c.t_set_ptr(t, "cellHasSunLitSurfsBits", bits);
    }
    surface_indices_in_order(c, t);
    Ok(())
}

/// The QA renderer draws a run of consecutive surfaces with one copy of the index range from the first surface's baseIndex to the
/// end of the last one's (sub_AC6950), which takes the indices to be laid out in surface order, as IW4's linker writes them. IW3
/// maps do not keep that order (mp_brony: 222 of 1599 surfaces start before the previous one ends; the copy ran off the buffer).
/// Every surface's triangles are copied, in surface order, behind the original indices and its baseIndex moved there; the
/// original indices stay where they were, unused.
fn surface_indices_in_order(c: &mut Conv, t: T) {
    let (Some(dpvs), Some(draw), Some(gk)) = (c.t_sub(t, "dpvs"), c.t_sub(t, "draw"), c.tsid("GfxSurface")) else { return };
    let n = c.t_get_i(dpvs, "staticSurfaceCount").max(0) as u32;
    let surfs = c.t_ptr(dpvs, "surfaces");
    let old = c.t_ptr(draw, "indices");
    let old_count = c.t_get_i(draw, "indexCount").max(0) as u32;
    if n == 0 || surfs == 0 || old == 0 {
        return;
    }
    let gsz = c.tsize(gk);
    let mut out: Vec<u8> = c.heap.get(old, 2 * old_count as usize).to_vec();
    for i in 0..n {
        let e = surfs + i * gsz;
        let tri = c.heap.u16(e + 10) as u32;
        let base = c.heap.u32(e + 12);
        if base + 3 * tri > old_count {
            continue;
        }
        let at = (out.len() / 2) as u32;
        let src = c.heap.get(old + 2 * base, 6 * tri as usize).to_vec();
        out.extend_from_slice(&src);
        c.heap.set_u32(e + 12, at);
    }
    let count = (out.len() / 2) as i64;
    let a = c.heap.alloc(out.len(), 16);
    c.heap.put(a, &out);
    c.t_set_ptr(draw, "indices", a);
    c.t_set_i(draw, "indexCount", count);
}

fn box_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["bounds"])?;
    let (min, max) = (c.s_at(s, "mins"), c.s_at(s, "maxs"));
    let at = c.t_at(t, "bounds");
    put_bounds(c, at, min, max);
    Ok(())
}

fn static_model_inst_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    box_to_iw4(c, s, t)?;
    // the lighting origin: the middle of the box
    let (min, max) = (c.s_at(s, "mins"), c.s_at(s, "maxs"));
    let at = c.t_at(t, "lightingOrigin");
    for k in 0..3u32 {
        let v = (f32_at(c, min + 4 * k) + f32_at(c, max + 4 * k)) / 2.0;
        c.heap.put(at + 4 * k, &v.to_le_bytes());
    }
    Ok(())
}

fn static_model_draw_inst_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["cullDist", "cacheId"])?;
    let dist = f32_at(c, c.s_at(s, "cullDist"));
    c.t_set_i(t, "cullDist", dist.clamp(0.0, 65535.0) as i64);
    let (from, to) = (c.s_at(s, "smodelCacheIndex"), c.t_at(t, "cacheId"));
    c.copy_raw(from, to, 8);
    Ok(())
}

fn portal_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["cellIndex"])?;
    let (base, size) = CELLS.with(|x| x.get());
    let cell = c.sp(s, "cell");
    if cell >= base && size > 0 {
        c.t_set_i(t, "cellIndex", ((cell - base) / size) as i64);
    }
    Ok(())
}

fn surface_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["laf"])?;
    // lightmapIndex, reflectionProbeIndex, primaryLightIndex, flags: the same four bytes
    let (from, to) = (c.s_at(s, "lightmapIndex"), c.t_at(t, "laf"));
    c.copy_raw(from, to, 4);
    Ok(())
}

fn dpvs_static_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["surfacesBounds"])?;
    // IW3: lit, decal, emissive. IW4: lit opaque, lit translucent, shadow casters (none here), emissive.
    let (lit0, lit1) = (c.si(s, "litSurfsBegin"), c.si(s, "litSurfsEnd"));
    let (dec0, dec1) = (c.si(s, "decalSurfsBegin"), c.si(s, "decalSurfsEnd"));
    let em0 = c.si(s, "emissiveSurfsBegin");
    c.t_set_i(t, "litOpaqueSurfsBegin", lit0);
    c.t_set_i(t, "litOpaqueSurfsEnd", lit1);
    c.t_set_i(t, "litTransSurfsBegin", dec0);
    c.t_set_i(t, "litTransSurfsEnd", dec1);
    c.t_set_i(t, "shadowCasterSurfsBegin", em0);
    c.t_set_i(t, "shadowCasterSurfsEnd", em0);
    // the surfaces' boxes
    let n = c.si(s, "staticSurfaceCount").max(0) as u32;
    let surfs = c.sp(s, "surfaces");
    let (Some(sk), Some(bk)) = (c.ssid("GfxSurface"), c.tsid("GfxSurfaceBounds")) else { return Err("no GfxSurface".to_string()) };
    let (ssz, bsz) = (c.ssize(sk), c.tsize(bk));
    if n > 0 && surfs != 0 {
        let arr = c.heap.alloc((bsz * n) as usize, 4);
        let (mut prev_end, mut out_of_order, mut max_end) = (0i64, 0u32, 0i64);
        for i in 0..n {
            let e = surfs + i * ssz;
            let b = c.s_at(S { sid: sk, addr: e }, "bounds");
            put_bounds(c, arr + i * bsz, b, b + 12);
            let tri = c.s_bytes(e + 10, 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as i64).unwrap_or(0);
            let base = i32_at(c, e + 12) as i64;
            if base < prev_end {
                out_of_order += 1;
            }
            prev_end = base + 3 * tri;
            max_end = max_end.max(prev_end);
        }
        if std::env::var_os("ATLAS_DEBUG_SURFS").is_some() {
            eprintln!("surfaces {n}: {out_of_order} start before the previous one ends; last index used {max_end}");
        }
        c.t_set_ptr(t, "surfacesBounds", arr);
    }
    Ok(())
}

/// IW3 names the box tree's mid point `origin`; IW4 `midPoint` (the half size keeps its name).
fn collision_aabb_tree_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["midPoint"])?;
    let (from, to) = (c.s_at(s, "origin"), c.t_at(t, "midPoint"));
    c.copy_raw(from, to, 12);
    Ok(())
}

pub fn rules(r: &mut Rules) {
    // collision: boxes stored as corners in IW3; the by-name copy left these zero (a leaf with an empty box is never entered by a
    // trace: the player fell through the map)
    r.replace.insert("cLeaf_t".to_string(), box_to_iw4);
    r.replace.insert("XModelCollSurf_s".to_string(), box_to_iw4);
    r.replace.insert("GfxBrushModelWritable".to_string(), box_to_iw4);
    r.replace.insert("CollisionAabbTree".to_string(), collision_aabb_tree_to_iw4);
    r.replace.insert("GfxWorld".to_string(), gfx_world_to_iw4);
    r.replace.insert("GfxWorldDpvsStatic".to_string(), dpvs_static_to_iw4);
    r.replace.insert("GfxCell".to_string(), box_to_iw4);
    r.replace.insert("GfxAabbTree".to_string(), box_to_iw4);
    r.replace.insert("GfxStaticModelInst".to_string(), static_model_inst_to_iw4);
    r.replace.insert("GfxStaticModelDrawInst".to_string(), static_model_draw_inst_to_iw4);
    r.replace.insert("GfxPortal".to_string(), portal_to_iw4);
    r.replace.insert("GfxSurface".to_string(), surface_to_iw4);
}
