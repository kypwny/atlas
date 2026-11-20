//! Call of Duty 4 (IW3) <-> stock Modern Warfare 2 (IW4), the two hops of the hub-and-spoke conversion that involve IW3.

use super::convert_cli::{engine_hop, HopOut, Input};
use super::convert_engine::{Conv, Rules, R, S, T};
use super::schema_walk::Game;
use anyhow::Result;

/// Assets whose content is specific to the game (their fields have no counterpart that keeps their meaning).
fn game_specific(r: &mut Rules) {
    for (name, why) in [
        ("WeaponDef", "weapons are game specific"),
        ("WeaponCompleteDef", "weapons are game specific"),
        ("VehicleDef", "vehicles are game specific"),
        ("menuDef_t", "menus are game specific"),
        ("MenuList", "menus are game specific"),
        ("LeaderboardDef", "leaderboards are game specific"),
        ("StructuredDataDefSet", "structured data is game specific"),
    ] {
        r.skip.insert(name.to_string(), why.to_string());
    }
}

fn name_of(c: &Conv, s: S) -> String {
    c.sstr(s, "name").map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

/// IW3 keeps the surfaces of all levels of detail in one array of the model; IW4 has an asset (`XModelSurfs`) per level.
/// The bone translations are stored with a fourth float per bone in IW3.
fn model_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["trans"])?;
    let (Some(sk), Some(tk), Some(surfs_t)) = (c.ssid("XSurface"), c.tsid("XSurface"), c.tsid("XModelSurfs")) else { return Err("no XSurface".to_string()) };
    let (ssz, tsz) = (c.ssize(sk), c.tsize(tk));
    let surfs = c.sp(s, "surfs");
    let name = name_of(c, s);
    let lods = c.si(s, "numLods").clamp(0, 4) as usize;
    for i in 0..lods {
        let (Some(lod), Some(tlod)) = (c.s_elem(s, "lodInfo", i), c.t_elem(t, "lodInfo", i)) else { break };
        let (n, first) = (c.si(lod, "numsurfs").max(0) as u32, c.si(lod, "surfIndex").max(0) as u32);
        let hdr = T { sid: surfs_t, addr: c.heap.alloc(c.tsize(surfs_t) as usize, 4) };
        c.generic_members(lod, hdr)?;
        c.t_str(hdr, "name", format!("{name}_lod{i}").as_bytes());
        if surfs != 0 && n > 0 {
            let arr = c.heap.alloc((tsz * n) as usize, 4);
            for k in 0..n {
                c.generic(S { sid: sk, addr: surfs + (first + k) * ssz }, T { sid: tk, addr: arr + k * tsz })?;
            }
            c.t_set_ptr(hdr, "surfs", arr);
        }
        c.t_set_ptr(tlod, "modelSurfs", hdr.addr);
    }
    // the translations: three floats per bone
    let bones = (c.si(s, "numBones") - c.si(s, "numRootBones")).max(0) as usize;
    let trans = c.sp(s, "trans");
    c.t_repack_floats(t, "trans", trans, bones, 4, 3);
    // bounds from the box
    let (Some(mins), Some(maxs)) = (c.ssub(s, "mins"), c.ssub(s, "maxs")) else { return Ok(()) };
    if let Some(b) = c.t_sub(t, "bounds") {
        if let (Some(mid), Some(half)) = (c.t_sub(b, "midPoint"), c.t_sub(b, "halfSize")) {
            for (k, axis) in ["x", "y", "z"].iter().enumerate() {
                let _ = axis;
                let (lo, hi) = (c.sf_at(mins, k), c.sf_at(maxs, k));
                c.t_set_f_at(mid, k, (lo + hi) / 2.0);
                c.t_set_f_at(half, k, (hi - lo) / 2.0);
            }
        }
    }
    Ok(())
}

/// The reverse: the surfaces of the levels of detail go into one array, the translations get their fourth float.
fn model_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["trans"])?;
    let (Some(sk), Some(tk)) = (c.ssid("XSurface"), c.tsid("XSurface")) else { return Err("no XSurface".to_string()) };
    let (ssz, tsz) = (c.ssize(sk), c.tsize(tk));
    let lods = c.si(s, "numLods").clamp(0, 4) as usize;
    // the surfaces of every level, converted once per surfaces asset (a later model that shares it reads the converted copy)
    let mut per_lod: Vec<(u32, u32)> = Vec::new();
    for i in 0..lods {
        let Some(lod) = c.s_elem(s, "lodInfo", i) else { break };
        let n = c.si(lod, "numsurfs").max(0) as u32;
        let id = c.s_reuse_key(lod, "modelSurfs");
        let ms = c.sp(lod, "modelSurfs");
        let cached = id.and_then(|id| c.user.get(&id).copied());
        let entry = match cached {
            Some(e) => e,
            None if id.is_some() && ms != 0 => {
                let base = c.sp(S { sid: c.ssid("XModelSurfs").unwrap_or(0), addr: ms }, "surfs");
                if base == 0 || n == 0 {
                    (0, 0)
                } else {
                    let arr = c.heap.alloc((tsz * n) as usize, 4);
                    for k in 0..n {
                        c.generic(S { sid: sk, addr: base + k * ssz }, T { sid: tk, addr: arr + k * tsz })?;
                    }
                    if let Some(id) = id {
                        c.user.insert(id, (arr, n));
                    }
                    (arr, n)
                }
            }
            None => {
                c.warn("the surfaces of a level of detail were lost with an earlier entry".to_string());
                (0, 0)
            }
        };
        per_lod.push(entry);
    }
    let total: u32 = per_lod.iter().map(|e| e.1).sum();
    if total > 0 {
        let arr = c.heap.alloc((tsz * total) as usize, 4);
        let mut at = 0u32;
        for (i, &(src, n)) in per_lod.iter().enumerate() {
            if n > 0 {
                let bytes = c.heap.get(src, (tsz * n) as usize).to_vec();
                c.heap.put(arr + at * tsz, &bytes);
            }
            if let Some(tlod) = c.t_elem(t, "lodInfo", i) {
                c.t_set_i(tlod, "surfIndex", at as i64);
                c.t_set_i(tlod, "numsurfs", n as i64);
            }
            at += n;
        }
        c.t_set_ptr(t, "surfs", arr);
        c.t_set_i(t, "numsurfs", total as i64);
    }
    let bones = (c.si(s, "numBones") - c.si(s, "numRootBones")).max(0) as usize;
    let trans = c.sp(s, "trans");
    if trans != 0 && bones > 0 {
        let a = c.heap.alloc(bones * 16, 4);
        for g in 0..bones {
            if let Some(b) = c.s_bytes(trans + (g * 12) as u32, 12).map(|b| b.to_vec()) {
                c.heap.put(a + (g * 16) as u32, &b);
            }
        }
        c.t_set_ptr(t, "trans", a);
    }
    if let (Some(b), Some(mins), Some(maxs)) = (c.ssub(s, "bounds"), c.t_sub(t, "mins"), c.t_sub(t, "maxs")) {
        if let (Some(mid), Some(half)) = (c.ssub(b, "midPoint"), c.ssub(b, "halfSize")) {
            for k in 0..3 {
                let (m, h) = (c.sf_at(mid, k), c.sf_at(half, k));
                c.t_set_f_at(mins, k, m - h);
                c.t_set_f_at(maxs, k, m + h);
            }
        }
    }
    Ok(())
}

/// Writes the box given by two corners (source addresses of three floats each) as a midpoint and half size into the `Bounds`
/// member `name` of a target structure.
fn corners_to_bounds(c: &mut Conv, t: T, name: &str, min: u32, max: u32) {
    let Some(b) = c.t_sub(t, name) else { return };
    let (Some(mid), Some(half)) = (c.t_sub(b, "midPoint"), c.t_sub(b, "halfSize")) else { return };
    for k in 0..3 {
        let (lo, hi) = (c.sf_at(S { sid: 0, addr: min }, k), c.sf_at(S { sid: 0, addr: max }, k));
        c.t_set_f_at(mid, k, (lo + hi) / 2.0);
        c.t_set_f_at(half, k, (hi - lo) / 2.0);
    }
}

/// The reverse: the corners of the `Bounds` member `name` of a source structure, written at heap addresses.
fn bounds_to_corners(c: &mut Conv, s: S, name: &str, min: u32, max: u32) {
    let Some(b) = c.ssub(s, name) else { return };
    let (Some(mid), Some(half)) = (c.ssub(b, "midPoint"), c.ssub(b, "halfSize")) else { return };
    for k in 0..3 {
        let (m, h) = (c.sf_at(mid, k), c.sf_at(half, k));
        c.t_set_f_at(T { sid: 0, addr: min }, k, m - h);
        c.t_set_f_at(T { sid: 0, addr: max }, k, m + h);
    }
}

fn bone_info_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["bounds"])?;
    let base = c.s_at(s, "bounds");
    corners_to_bounds(c, t, "bounds", base, base + 12);
    Ok(())
}

fn bone_info_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["bounds"])?;
    let base = c.t_at(t, "bounds");
    bounds_to_corners(c, s, "bounds", base, base + 12);
    Ok(())
}

fn brush_model_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["bounds"])?;
    let base = c.s_at(s, "bounds");
    corners_to_bounds(c, t, "bounds", base, base + 12);
    Ok(())
}

fn brush_model_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["bounds"])?;
    let base = c.t_at(t, "bounds");
    bounds_to_corners(c, s, "bounds", base, base + 12);
    Ok(())
}

fn static_model_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let (min, max) = (c.s_at(s, "absmin"), c.s_at(s, "absmax"));
    corners_to_bounds(c, t, "bounds", min, max);
    Ok(())
}

fn static_model_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let (min, max) = (c.t_at(t, "absmin"), c.t_at(t, "absmax"));
    bounds_to_corners(c, s, "bounds", min, max);
    Ok(())
}

fn cmodel_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let (min, max) = (c.s_at(s, "mins"), c.s_at(s, "maxs"));
    corners_to_bounds(c, t, "bounds", min, max);
    Ok(())
}

fn cmodel_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let (min, max) = (c.t_at(t, "mins"), c.t_at(t, "maxs"));
    bounds_to_corners(c, s, "bounds", min, max);
    Ok(())
}

/// The hash a string table cell carries in IW4 (case-insensitive, 31 times the hash so far plus the character).
fn cell_hash(s: &[u8]) -> i64 {
    let mut h: i32 = 0;
    for &ch in s {
        h = (ch.to_ascii_lowercase() as i32).wrapping_add(h.wrapping_mul(31));
    }
    h as i64
}

/// IW3 string tables are lists of string pointers, IW4's are cells of a string and its hash.
fn string_table_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["values"])?;
    let n = (c.si(s, "columnCount") * c.si(s, "rowCount")).max(0) as usize;
    let sp = c.sp(s, "values");
    if sp == 0 || n == 0 {
        return Ok(());
    }
    c.t_alloc(t, "values", n);
    let spsz = c.s_psz();
    for i in 0..n {
        let Some(cell) = c.t_elem(t, "values", i) else { break };
        let p = c.s_ptr(sp + i as u32 * spsz);
        let bytes = if p == 0 { Vec::new() } else { c.s_cstr(p) };
        c.t_str(cell, "string", &bytes);
        c.t_set_i(cell, "hash", cell_hash(&bytes));
    }
    Ok(())
}

fn string_table_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["values"])?;
    let n = (c.si(s, "columnCount") * c.si(s, "rowCount")).max(0) as usize;
    if c.sp(s, "values") == 0 || n == 0 {
        return Ok(());
    }
    let psz = c.heap.psz as u32;
    let arr = c.heap.alloc(n * psz as usize, 4);
    c.t_set_ptr(t, "values", arr);
    for i in 0..n {
        let Some(cell) = c.s_elem(s, "values", i) else { break };
        let bytes = c.sstr(cell, "string").unwrap_or_default();
        let a = c.heap.alloc_cstr(&bytes);
        c.heap.set_ptr(arr + i as u32 * psz, a);
    }
    Ok(())
}

/// MW2 added two effect element types (spark cloud and spark fountain) after the cloud: CoD4's model, lights, sound, decal and runner
/// are two higher there.
fn elem_type_to_iw4(t: i64) -> i64 {
    if t >= 5 { t + 2 } else { t }
}

fn elem_type_to_iw3(t: i64) -> i64 {
    match t {
        5 | 6 => 4,
        t if t >= 7 => t - 2,
        t => t,
    }
}

fn elem_def_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    let ty = c.si(s, "elemType");
    c.t_set_i(t, "elemType", elem_type_to_iw4(ty));
    c.generic_members_except(s, t, &["elemType"])?;
    let (min, max) = (c.s_at(s, "collMins"), c.s_at(s, "collMaxs"));
    corners_to_bounds(c, t, "bounds", min, max);
    // a trail's definition lives in the extended member
    let trail = c.sp(s, "trailDef");
    if let (true, Some(ext), Some(sk), Some(tk)) = (trail != 0, c.t_sub(t, "extended"), c.ssid("FxTrailDef"), c.tsid("FxTrailDef")) {
        let a = c.heap.alloc(c.tsize(tk) as usize, 4);
        c.generic(S { sid: sk, addr: trail }, T { sid: tk, addr: a })?;
        c.t_set_ptr(ext, "trailDef", a);
    }
    Ok(())
}

fn elem_def_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    let ty = c.si(s, "elemType");
    c.t_set_i(t, "elemType", elem_type_to_iw3(ty));
    c.generic_members_except(s, t, &["elemType"])?;
    let (min, max) = (c.t_at(t, "collMins"), c.t_at(t, "collMaxs"));
    bounds_to_corners(c, s, "bounds", min, max);
    if let (Some(ext), Some(sk), Some(tk)) = (c.ssub(s, "extended"), c.ssid("FxTrailDef"), c.tsid("FxTrailDef")) {
        let trail = c.sp(ext, "trailDef");
        if trail != 0 && ty == 3 {
            let a = c.heap.alloc(c.tsize(tk) as usize, 4);
            c.generic(S { sid: sk, addr: trail }, T { sid: tk, addr: a })?;
            c.t_set_ptr(t, "trailDef", a);
        }
    }
    Ok(())
}

pub fn rules_to_iw4() -> Rules {
    let mut r = Rules::default();
    r.replace.insert("XModel".to_string(), model_to_iw4);
    r.replace.insert("FxElemDef".to_string(), elem_def_to_iw4);
    r.replace.insert("XBoneInfo".to_string(), bone_info_to_iw4);
    r.replace.insert("GfxBrushModel".to_string(), brush_model_to_iw4);
    r.replace.insert("cStaticModel_s".to_string(), static_model_to_iw4);
    r.replace.insert("cmodel_t".to_string(), cmodel_to_iw4);
    r.replace.insert("StringTable".to_string(), string_table_to_iw4);
    game_specific(&mut r);
    r.type_map.insert("CLIPMAP".to_string(), "CLIPMAP_SP".to_string());
    r.type_map.insert("CLIPMAP_PVS".to_string(), "CLIPMAP_MP".to_string());
    r
}

pub fn rules_from_iw4() -> Rules {
    let mut r = Rules::default();
    r.replace.insert("XModel".to_string(), model_to_iw3);
    r.replace.insert("FxElemDef".to_string(), elem_def_to_iw3);
    r.replace.insert("XBoneInfo".to_string(), bone_info_to_iw3);
    r.replace.insert("GfxBrushModel".to_string(), brush_model_to_iw3);
    r.replace.insert("cStaticModel_s".to_string(), static_model_to_iw3);
    r.replace.insert("cmodel_t".to_string(), cmodel_to_iw3);
    r.replace.insert("StringTable".to_string(), string_table_to_iw3);
    game_specific(&mut r);
    r.type_map.insert("CLIPMAP_SP".to_string(), "CLIPMAP".to_string());
    r.type_map.insert("CLIPMAP_MP".to_string(), "CLIPMAP_PVS".to_string());
    r
}

pub fn iw3_to_iw4(input: &Input) -> Result<HopOut> {
    engine_hop(Game::Iw3, Game::Iw4, rules_to_iw4(), input)
}

pub fn iw4_to_iw3(input: &Input) -> Result<HopOut> {
    engine_hop(Game::Iw4, Game::Iw3, rules_from_iw4(), input)
}
