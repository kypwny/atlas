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
/// Stand-ins for external model references the QA zones do not define (the leading comma marks an external reference).
fn stand_in_model(name: &str) -> Option<&'static str> {
    match name {
        ",weapon_m40a3" | ",weapon_remington700" | ",weapon_desert_eagle_gold" => Some(",weapon_m67_grenade"),
        _ => None,
    }
}

fn model_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["trans"])?;
    let (Some(sk), Some(tk), Some(surfs_t)) = (c.ssid("XSurface"), c.tsid("XSurface"), c.tsid("XModelSurfs")) else { return Err("no XSurface".to_string()) };
    let (ssz, tsz) = (c.ssize(sk), c.tsize(tk));
    let surfs = c.sp(s, "surfs");
    let name = name_of(c, s);
    // CoD4's common-zone weapon props (referenced by name, defined in no QA zone) stand in as a QA model every mount has
    if let Some(stand_in) = stand_in_model(&name) {
        c.t_str(t, "name", stand_in.as_bytes());
    }
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
                c.t_set_f_at(half, k, ((hi - lo) / 2.0).max(0.0)); // a box that is a hair inside out (rounding) must not get a negative half size
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
        c.t_set_f_at(half, k, ((hi - lo) / 2.0).max(0.0)); // a box that is a hair inside out (rounding) must not get a negative half size
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

/// IW3 has no static model tree; IW4 walks `smodelNodes` from node 0 without a null check (the QA client crashed there, sub_58D080,
/// tracing against static models). One root node holding every static model is written: a node whose first child is below
/// numStaticModels lists static models [first, first + count).
fn static_model_root_node(c: &mut Conv, s: S, t: T) {
    let n = c.si(s, "numStaticModels").max(0) as u32;
    let list = c.sp(s, "staticModelList");
    let size = c.ssid("cStaticModel_s").map(|k| c.ssize(k)).unwrap_or(80);
    let f = |c: &Conv, a: u32| c.s_bytes(a, 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0.0);
    let (mut lo, mut hi) = ([0f32; 3], [0f32; 3]);
    if list != 0 && n > 0 {
        (lo, hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for i in 0..n {
            let e = list + i * size;
            for k in 0..3usize {
                lo[k] = lo[k].min(f(c, e + 56 + 4 * k as u32));
                hi[k] = hi[k].max(f(c, e + 68 + 4 * k as u32));
            }
        }
    }
    let node = c.heap.alloc(28, 4);
    for k in 0..3usize {
        c.heap.put(node + 4 * k as u32, &((lo[k] + hi[k]) / 2.0).to_le_bytes());
        c.heap.put(node + 12 + 4 * k as u32, &((hi[k] - lo[k]) / 2.0).max(0.0).to_le_bytes());
    }
    c.heap.put(node + 24, &0u16.to_le_bytes());
    c.heap.put(node + 26, &(n.min(0xffff) as u16).to_le_bytes());
    c.t_set_i(t, "smodelNodeCount", 1);
    c.t_set_ptr(t, "smodelNodes", node);
}

/// IW3 keeps each brush's box and contents inside the brush (80 bytes: mins, contents, maxs, ...); IW4 has the brush without them
/// (36 bytes) and two parallel arrays, `brushBounds` (midpoint and half size) and `brushContents`. The members of the brushes
/// themselves map by name; the two arrays are built here from the source brushes.
fn clip_map_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["brushBounds", "brushContents", "smodelNodeCount", "smodelNodes"])?;
    static_model_root_node(c, s, t);
    let Some(bk) = c.ssid("cbrush_t") else { return Ok(()) };
    let n = c.si(s, "numBrushes").max(0) as u32;
    let base = c.sp(s, "brushes");
    if n == 0 || base == 0 {
        return Ok(());
    }
    let size = c.ssize(bk);
    let bounds = c.heap.alloc(24 * n as usize, 4);
    let contents = c.heap.alloc(4 * n as usize, 4);
    for i in 0..n {
        let e = S { sid: bk, addr: base + i * size };
        let (mn, mx) = (c.s_at(e, "mins"), c.s_at(e, "maxs"));
        for k in 0..3usize {
            let (lo, hi) = (c.sf_at(S { sid: 0, addr: mn }, k) as f32, c.sf_at(S { sid: 0, addr: mx }, k) as f32);
            // a half size is never negative (a box a hair inside out from rounding is refused by the QA loader)
            let (mid, half) = ((lo + hi) / 2.0, ((hi - lo) / 2.0).max(0.0));
            c.heap.put(bounds + 24 * i + 4 * k as u32, &mid.to_le_bytes());
            c.heap.put(bounds + 24 * i + 12 + 4 * k as u32, &half.to_le_bytes());
        }
        c.heap.put(contents + 4 * i, &(c.si(e, "contents") as i32).to_le_bytes());
    }
    c.t_set_ptr(t, "brushBounds", bounds);
    c.t_set_ptr(t, "brushContents", contents);
    Ok(())
}

/// A leaf-brush node is either a leaf (a pointer to its brush indices) or an inner node (split distance, range and two child
/// offsets, 12 plain bytes in the same place). Copying the union by member left the inner nodes' data zero, which the QA loader's
/// reader refuses, so the plain bytes are copied as they are.
fn leaf_brush_node_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["data"])?;
    let (Some(ds), Some(dt)) = (c.ssub(s, "data"), c.t_sub(t, "data")) else { return Ok(()) };
    if c.si(s, "leafBrushCount") > 0 {
        c.generic_members(ds, dt)?;
    } else if let Some(b) = c.s_bytes(ds.addr, 12).map(|b| b.to_vec()) {
        c.heap.put(dt.addr, &b);
    }
    Ok(())
}

/// IW3's clip material holds its name inline (`char material[64]`, flags after it); IW4's has a name pointer and `contents`.
fn clip_material_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    let name = c.s_cstr(c.s_at(s, "material"));
    c.t_str(t, "name", &name);
    c.t_set_i(t, "surfaceFlags", c.si(s, "surfaceFlags"));
    c.t_set_i(t, "contents", c.si(s, "contentFlags"));
    Ok(())
}

/// IW3 RawFile: name, len, buffer (len + 1 bytes, never compressed). IW4: name, compressedLen, len, a union of the compressed and the
/// plain buffer. The members have different names, so the generic copy dropped the text (a script came out with a null buffer).
/// IW3's map entities have no stages; IW4 looks up the stage of a position in `stages` without a null check (the QA client
/// crashed there, sub_ACD420). Every stock IW4 map has at least "stage 0"; the same one is written here.
fn map_ents_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    c.t_set_i(t, "stageCount", 1);
    if let Some(st) = c.t_alloc(t, "stages", 1) {
        let name = c.heap.alloc_cstr(b"stage 0");
        c.heap.set_ptr(st.addr, name);
        c.heap.put(st.addr + 16, &0x400u16.to_le_bytes());
        c.heap.put(st.addr + 18, &[1u8]);
    }
    Ok(())
}

fn raw_file_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    if let Some(name) = c.sstr(s, "name") {
        c.t_str(t, "name", &name);
    }
    let len = c.si(s, "len").max(0) as usize;
    c.t_set_i(t, "compressedLen", 0);
    c.t_set_i(t, "len", len as i64);
    let src = c.sp(s, "buffer");
    if src != 0 {
        let bytes = c.s_bytes(src, len).map(|b| b.to_vec()).ok_or_else(|| "raw file data outside the loaded memory".to_string())?;
        let a = c.heap.alloc_cstr(&bytes);
        c.heap.set_ptr(t.addr + 12, a);
    }
    Ok(())
}

fn static_model_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let (min, max) = (c.s_at(s, "absmin"), c.s_at(s, "absmax"));
    corners_to_bounds(c, t, "absBounds", min, max);
    Ok(())
}

fn static_model_to_iw3(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let (min, max) = (c.t_at(t, "absmin"), c.t_at(t, "absmax"));
    bounds_to_corners(c, s, "absBounds", min, max);
    Ok(())
}

/// Where an IW3 technique slot (34 of them) sits in an IW4 technique set (48), by name (OpenAssetTools `IW3_Assets.h` /
/// `IW4_Assets.h`, `MaterialTechniqueType`). IW4 gave the emissive and every lit technique a depth-fog twin right behind it
/// (EMISSIVE 5, EMISSIVE_DFOG 6, ..., LIT 9, LIT_DFOG 10, LIT_SUN 11, ...), so IW3's emissive shadow and lit slots land on every
/// other slot from 7; the light, fake light, preview, case texture and wireframe slots (21..=29) move up by 16; the shadow cookie
/// slots (30, 31) have no counterpart; the bump-map debug ones (32, 33) are 46 and 47. (An earlier version moved the lit ones up by
/// 2, which put LIT_SUN in LIT_DFOG and left IW4's sun slots empty: the QA client drew no world surface.)
fn technique_slot_to_iw4(i: usize) -> Option<usize> {
    match i {
        0..=5 => Some(i),
        6..=20 => Some(7 + 2 * (i - 6)),
        21..=29 => Some(i + 16),
        32 | 33 => Some(i + 14),
        _ => None,
    }
}

/// Vertex and pixel shader programs: CoD4 has two renderers and marks each program with the one it is for (0 shader model 2,
/// 1 shader model 3); MW2 and QA have one, 0, and create only programs marked 0. CoD4's lit world shaders are all _sm3 (1), so
/// QA never created them and drew the world with no shader bound. Every program is marked 0.
fn shader_load_def_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    c.t_set_i(t, "loadForRenderer", 0);
    Ok(())
}

/// MaterialVertexDeclaration: IW4 added STREAM_DST_DEPTH (4) to the stream destinations, so every texture-coordinate
/// destination (IW3 4..11) is one higher in IW4 (5..12). Copied unchanged, the world's texture coordinates went to the depth
/// input and the client drew no world pixels at all.
fn vertex_declaration_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let n = c.si(s, "streamCount").clamp(0, 13) as u32;
    let data = c.t_at(t, "routing");
    for i in 0..n {
        let at = data + 2 * i + 1;
        let d = c.heap.get(at, 1)[0];
        if d >= 4 {
            c.heap.put(at, &[d + 1]);
        }
    }
    Ok(())
}

/// Material: `stateBitsEntry` is indexed by technique type like the technique set's slots, so it is permuted the same way
/// (unused slots 0xFF; IW4's depth-fog slots take the plain technique's entry).
fn material_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let (from, to) = (c.s_at(s, "stateBitsEntry"), c.t_at(t, "stateBitsEntry"));
    let old: Vec<u8> = c.s_bytes(from, 34).map(|b| b.to_vec()).unwrap_or_default();
    if old.len() < 34 {
        return Ok(());
    }
    let mut new = [0xFFu8; 48];
    for (i, &e) in old.iter().enumerate() {
        if let Some(j) = technique_slot_to_iw4(i) {
            new[j] = e;
        }
    }
    for j in (6..=36).step_by(2) {
        if new[j] == 0xFF {
            new[j] = new[j - 1];
        }
    }
    c.heap.put(to, &new);
    // the sort key: CoD4 0 distortion, 1 opaque water, 2 boat hull, 3 opaque ambient, 4 opaque, 5 sky, 6..8 skybox, 9..16 decals;
    // MW2 0 opaque ambient, 1 opaque, 2 sky, 3 skybox, 4/5 dynamic decals, 6..13 decals, 43 distortion, 48 effect auto sort.
    // Copied unchanged, the world's opaque surfaces (4) went to a decal list and drew only scraps.
    if let (Some(si), Some(ti)) = (c.ssub(s, "info"), c.t_sub(t, "info")) {
        let k = c.si(si, "sortKey");
        c.t_set_i(ti, "sortKey", sort_key_to_iw4(k));
    }
    Ok(())
}

/// CoD4 material sort key -> MW2. Keys above the decals keep their order three lower (the CoD4 list has distortion, opaque water
/// and boat hull in front); 44 and up (CoD4's late translucent keys, e.g. glass) become MW2's effect auto sort.
fn sort_key_to_iw4(k: i64) -> i64 {
    match k {
        0 => 43,
        1 | 2 | 4 => 1,
        3 => 0,
        5 => 2,
        6..=8 => 3,
        9..=43 => k - 3,
        _ => 48,
    }
}

/// MaterialTechniqueSet: the generic copy puts technique `i` in slot `i`; the slots are permuted afterwards (the pointers, so a
/// technique several slots share stays shared).
fn technique_set_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let base = t.addr + 12; // `techniques` of the IW4 structure
    let old: Vec<u32> = (0..48).map(|i| c.heap.ptr(base + 4 * i)).collect();
    let mut new = vec![0u32; 48];
    for (i, &p) in old.iter().enumerate().take(34) {
        if let Some(j) = technique_slot_to_iw4(i) {
            new[j] = p;
        }
    }
    // the depth-fog twins (6, 8, 10, ..., 36): IW3 has none; the plain technique stands in (no depth fog on those surfaces)
    for j in (6..=36).step_by(2) {
        if new[j] == 0 {
            new[j] = new[j - 1];
        }
    }
    for (j, &p) in new.iter().enumerate() {
        c.heap.set_ptr(base + 4 * j as u32, p);
    }
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

/// A shader argument's `u` is a union chosen by `type`: a pointer to four floats for the literal constants (1 and 7), and plain
/// four bytes otherwise (a material constant's or texture's name hash for 0, 2 and 6, a code constant's index and rows for 3 and
/// 5, a code sampler's index for 4). The by-name copy read every one of them as the pointer, which zeroed the hashes (the QA client
/// then found no texture of any converted material: the world drew black). IW3 and IW4 number the code constants and code
/// samplers differently (the world matrices moved from 58 to 96, view-projection from 74 to 84): both are remapped by name.
fn shader_argument_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members_except(s, t, &["u"])?;
    let ty = c.si(s, "type");
    if ty == 1 || ty == 7 {
        if let (Some(su), Some(tu)) = (c.ssub(s, "u"), c.t_sub(t, "u")) {
            c.generic_members(su, tu)?;
        }
        return Ok(());
    }
    let (from, to) = (c.s_at(s, "u"), c.t_at(t, "u"));
    c.copy_raw(from, to, 4);
    match ty {
        // code constant: {index u16, firstRow u8, rowCount u8}
        3 | 5 => {
            let i = c.s_bytes(from, 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0) as usize;
            let n = IW3_TO_IW4_CODE_CONST.get(i).copied().unwrap_or(i as u16);
            c.heap.put(to, &n.to_le_bytes());
        }
        // code sampler: a 32-bit index
        4 => {
            let i = c.s_bytes(from, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0) as usize;
            let n = IW3_TO_IW4_CODE_SAMPLER.get(i).copied().unwrap_or(i as u32);
            c.heap.put(to, &n.to_le_bytes());
        }
        _ => {}
    }
    Ok(())
}

/// `ShaderCodeConstants` IW3 -> IW4, by name (OpenAssetTools `IW3_Assets.h` / `IW4_Assets.h`; the QA client uses the IW4
/// numbering). IW3's single world matrices become IW4's `*_MATRIX0`; the sun constants (IW4 lights the sun as the primary light)
/// become the light ones, FOG_COLOR becomes FOG_COLOR_GAMMA and SHADOW_PARMS (gone in IW4) SHADOWMAP_SCALE.
const IW3_TO_IW4_CODE_CONST: [u16; 92] = [
    0, 1, 2, 3, 4, 60, 61, 62, 32, 21, 22, 5, 23, 24, 25, 26, 27, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 31, 32, 33,
    0, 1, 2, 34, 35, 36, 37, 39, 44, 45, 46, 47, 48, 50, 51, 53, 63, 64, 65, 73, 74, 75, 58, 96, 97, 98, 99, 76, 77, 78, 79, 80,
    81, 82, 83, 100, 101, 102, 103, 84, 85, 86, 87, 104, 105, 106, 107, 88, 89, 90, 91, 92, 93, 94, 95, 132, 133,
];

/// `MaterialTextureSource` IW3 -> IW4 by name (same sources). SHADOWCOOKIE and DYNAMIC_SHADOWS (gone in IW4) become WHITE, SKY
/// BLACK. The QA client's sampler name table (QA 3.3.3.3, filled at 0xA98A30) matches IW4's numbering and adds two at the end.
const IW3_TO_IW4_CODE_SAMPLER: [u32; 27] = [0, 1, 2, 3, 4, 5, 1, 6, 7, 8, 9, 10, 11, 12, 0, 13, 1, 14, 15, 16, 17, 20, 21, 22, 23, 24, 25];

pub fn rules_to_iw4() -> Rules {
    let mut r = Rules::default();
    r.replace.insert("MaterialShaderArgument".to_string(), shader_argument_to_iw4);
    r.replace.insert("XModel".to_string(), model_to_iw4);
    r.replace.insert("FxElemDef".to_string(), elem_def_to_iw4);
    r.replace.insert("XBoneInfo".to_string(), bone_info_to_iw4);
    r.replace.insert("GfxBrushModel".to_string(), brush_model_to_iw4);
    r.replace.insert("cStaticModel_s".to_string(), static_model_to_iw4);
    r.replace.insert("clipMap_t".to_string(), clip_map_to_iw4);
    r.replace.insert("cLeafBrushNode_s".to_string(), leaf_brush_node_to_iw4);
    r.replace.insert("ClipMaterial".to_string(), clip_material_to_iw4);
    r.replace.insert("cmodel_t".to_string(), cmodel_to_iw4);
    r.replace.insert("StringTable".to_string(), string_table_to_iw4);
    r.replace.insert("MapEnts".to_string(), map_ents_to_iw4);
    r.replace.insert("RawFile".to_string(), raw_file_to_iw4);
    r.replace.insert("MaterialTechniqueSet".to_string(), technique_set_to_iw4);
    r.replace.insert("MaterialVertexDeclaration".to_string(), vertex_declaration_to_iw4);
    r.replace.insert("Material".to_string(), material_to_iw4);
    r.replace.insert("GfxVertexShaderLoadDef".to_string(), shader_load_def_to_iw4);
    r.replace.insert("GfxPixelShaderLoadDef".to_string(), shader_load_def_to_iw4);
    super::convert_iw3_world::rules(&mut r);
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
