//! Black Ops II models read out of a zone with the schema walker: the decoded `XModel` (bones, LODs, surfaces), its
//! `XSurface` geometry (vertices, triangles, skin influences) and the materials the surfaces use.
//!
//! The walker (`schema_walk`) loads a zone with the loader's own rules and hands each asset to a hook as soon as it is
//! loaded; this file is that hook for `XModel`, `Material` and `GfxImage`, reading the structures through the schema's member
//! offsets. The layouts follow OpenAssetTools' Black Ops II headers (`XModel`, `XSurface`, `GfxPackedVertex`,
//! `DObjAnimMat`), the vertex decoding its `Pack` and T6 `Common` code (GPL-3.0, <https://github.com/Laupetin/OpenAssetTools>);
//! this is an independent implementation.
//!
//! Vertex format (32 bytes, `GfxPackedVertex`): position as three floats, the binormal sign, an RGBA colour, the texture
//! coordinates as two 16-bit floats (U in the low half, V in the high half), and the normal and tangent as 10:10:10 packed
//! vectors (each component a float whose top mantissa bits are stored, see [`unpack_unit_vec`]).
//!
//! Skinning: a surface's vertices come in two groups, in this order: first the rigid runs (`XRigidVertList`, each bound
//! entirely to one bone), then the vertices with 1, 2, 3 and 4 influences (`XSurfaceVertexInfo::vertCount`), whose words in
//! `vertsBlend` are the main bone, then (bone, weight) pairs for the others; the main bone takes the weight left over.
//! Bones are byte offsets into the 64-byte-per-bone skeleton matrix array, so the bone index is `offset / 64`; weights are
//! `u16 / 65535`.

use super::schema_walk::{Game, View, WalkReport, ZoneImage, walk_with};
use anyhow::{Context, Result};
use std::collections::HashMap;

/// One material as the models use it: its name and the images it samples.
#[derive(Clone, Debug, Default)]
pub struct T6Material {
    pub name: String,
    /// (texture semantic, image name) of every texture of the material.
    pub textures: Vec<(u8, String)>,
    pub color: Option<String>,
    pub normal: Option<String>,
    pub spec: Option<String>,
    /// The archive (IPAK) key of each image by its name: the image's `hash` member.
    pub image_hashes: HashMap<String, u32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct T6Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 3],
    pub binormal_sign: f32,
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

/// A run of vertices bound to one bone (`XRigidVertList`).
#[derive(Clone, Copy, Debug, Default)]
pub struct T6Rigid {
    pub bone_offset: u16,
    pub vert_count: u16,
    pub tri_offset: u16,
    pub tri_count: u16,
}

#[derive(Clone, Debug, Default)]
pub struct T6Surface {
    pub flags: u16,
    pub tile_mode: i8,
    pub base_vert_index: u16,
    pub vertices: Vec<T6Vertex>,
    /// Triangles as stored (indices into this surface's vertices).
    pub triangles: Vec<[u16; 3]>,
    pub rigid: Vec<T6Rigid>,
    /// Vertices with 1, 2, 3 and 4 influences.
    pub blend_counts: [u16; 4],
    /// The raw blend words.
    pub blend: Vec<u16>,
}

impl T6Surface {
    /// Bone influences of each vertex as (bone, weight), weights summing to 1; empty for a vertex nothing binds.
    pub fn skin_weights(&self) -> Vec<Vec<(u32, f32)>> {
        let mut out: Vec<Vec<(u32, f32)>> = Vec::with_capacity(self.vertices.len());
        for r in &self.rigid {
            for _ in 0..r.vert_count {
                out.push(vec![((r.bone_offset / 64) as u32, 1.0)]);
            }
        }
        let mut w = 0usize;
        for extra in 0..4usize {
            for _ in 0..self.blend_counts[extra] {
                let word = |k: usize| self.blend.get(w + k).copied().unwrap_or(0);
                let main = (word(0) / 64) as u32;
                let mut list = Vec::with_capacity(extra + 1);
                let mut left = 1.0f32;
                for k in 0..extra {
                    let (bone, weight) = ((word(1 + 2 * k) / 64) as u32, word(2 + 2 * k) as f32 / 65535.0);
                    list.push((bone, weight));
                    left -= weight;
                }
                list.insert(0, (main, left.max(0.0)));
                out.push(list);
                w += 2 * extra + 1;
            }
        }
        out.resize(self.vertices.len(), Vec::new());
        out
    }
}

#[derive(Clone, Debug, Default)]
pub struct T6Bone {
    pub name: String,
    /// -1 for a root bone.
    pub parent: i32,
    /// Position and rotation (x, y, z, w) relative to the parent.
    pub local: ([f32; 3], [f32; 4]),
    /// The same in model space (`DObjAnimMat`).
    pub global: ([f32; 3], [f32; 4]),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct T6Lod {
    pub dist: f32,
    /// First surface of the LOD in [`T6Model::surfaces`] (also the index of its first material handle) and their count.
    pub surf_index: u16,
    pub num_surfs: u16,
}

#[derive(Clone, Debug, Default)]
pub struct T6Model {
    pub name: String,
    pub num_root_bones: u8,
    pub bones: Vec<T6Bone>,
    pub lods: Vec<T6Lod>,
    /// Every surface of every LOD.
    pub surfaces: Vec<T6Surface>,
    /// The material of each surface (same indexing as `surfaces`); `None` when the link could not be followed.
    pub materials: Vec<Option<T6Material>>,
    pub radius: f32,
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub flags: u32,
}

impl T6Model {
    /// Surfaces of a LOD with their indices.
    pub fn lod_surfaces(&self, lod: usize) -> &[T6Surface] {
        match self.lods.get(lod) {
            Some(l) => self.surfaces.get(l.surf_index as usize..(l.surf_index as usize + l.num_surfs as usize).min(self.surfaces.len())).unwrap_or(&[]),
            None => &[],
        }
    }

    pub fn lod_triangle_count(&self, lod: usize) -> usize {
        self.lod_surfaces(lod).iter().map(|s| s.triangles.len()).sum()
    }
}

/// What reading a zone's models gives.
#[derive(Debug, Default)]
pub struct T6Models {
    pub models: Vec<T6Model>,
    pub materials: usize,
    /// The map's geometry, when asked for and the zone has a `GfxWorld` (see `t6_world`).
    pub world: Option<super::t6_world::T6World>,
    /// The map's lights (the Com world's primary lights, the GfxWorld's hero lights), when the world was asked for.
    pub lights: super::world::MapLights,
}

/// A 16-bit float as the game stores texture coordinates.
pub fn half_to_f32(half: u16) -> f32 {
    if half == 0 {
        return 0.0;
    }
    let h = half as u32;
    let bits = ((h << 16) & 0x8000_0000) | (((((h << 14) & 0x0FFF_C000).wrapping_sub(!(h << 14) & 0x1000_0000)) ^ 0x8000_0000) >> 1);
    f32::from_bits(bits)
}

/// A 10:10:10 packed unit vector (normals, tangents): each 10-bit field is a signed offset to the mantissa of a float near 3.0.
pub fn unpack_unit_vec(v: u32) -> [f32; 3] {
    let c = |shift: u32| {
        let x = (v >> shift) & 0x3FF;
        let bits = x.wrapping_sub(2 * (x & 0x200)).wrapping_add(0x4040_0000);
        (f32::from_bits(bits) - 3.0) * 8208.0312
    };
    [c(0), c(10), c(20)]
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-6 { [v[0] / l, v[1] / l, v[2] / l] } else { v }
}

/// Member offsets the decoding needs, looked up once in the schema.
struct Offsets {
    xmodel: [u32; 22],
    surf: [u32; 12],
    mat: [u32; 5],
}

const XM: [&str; 22] = [
    "name", "numBones", "numRootBones", "numsurfs", "boneNames", "parentList", "quats", "trans", "baseMat", "surfs", "materialHandles", "lodInfo", "numLods", "radius", "mins", "maxs", "flags", "partClassification", "collLod", "boneInfo", "himipInvSqRadii", "memUsage",
];
const XS: [&str; 12] = ["tileMode", "vertListCount", "flags", "vertCount", "triCount", "baseVertIndex", "triIndices", "vertInfo", "verts0", "vertList", "partBits", "vb0"];
const MT: [&str; 5] = ["info", "textureCount", "textureTable", "stateBitsCount", "constantCount"];

impl Offsets {
    fn new(v: &View) -> Option<Offsets> {
        let get = |s: &str, names: &[&str]| -> Option<Vec<u32>> { names.iter().map(|n| v.member(s, n)).collect() };
        let a = get("XModel", &XM)?;
        let b = get("XSurface", &XS)?;
        let c = get("Material", &MT)?;
        Some(Offsets { xmodel: a.try_into().ok()?, surf: b.try_into().ok()?, mat: c.try_into().ok()? })
    }
}

/// Reads the models of a zone (all of them, with every LOD).
pub fn read_models(zone: &ZoneImage) -> (T6Models, WalkReport) {
    read_zone(zone, true, false)
}

/// Reads the models and/or the map geometry of a zone.
pub fn read_zone(zone: &ZoneImage, want_models: bool, want_world: bool) -> (T6Models, WalkReport) {
    read_zone_with(zone, want_models, want_world, false)
}

/// Like [`read_zone`]; with `want_static` the map's placed static models are decoded too and kept in `T6World::models` and
/// `T6World::instances` (it implies `want_world`).
pub fn read_zone_with(zone: &ZoneImage, want_models: bool, want_world: bool, want_static: bool) -> (T6Models, WalkReport) {
    let want_world = want_world || want_static;
    let decode_models = want_models || want_static;
    let mut model_ids: HashMap<u32, usize> = HashMap::new();
    let mut decoded: Vec<T6Model> = Vec::new();
    let mut materials: HashMap<u32, T6Material> = HashMap::new();
    let mut images: HashMap<u32, (String, u32)> = HashMap::new();
    let mut out = T6Models::default();
    let mut offs: Option<Offsets> = None;
    let mut hook = |v: &View, ty: &str, hdr: u32, id: u32| {
        if !matches!(ty, "XModel" | "Material" | "GfxImage") && !(want_world && matches!(ty, "GfxWorld" | "ComWorld")) {
            return;
        }
        if ty == "ComWorld" {
            super::t6_world::read_com_lights(v, hdr, &mut out.lights);
            return;
        }
        if ty == "GfxImage" {
            if let Some(off) = v.member("GfxImage", "name") {
                if let Some(n) = v.ptr(hdr + off).and_then(|p| v.cstr(p)) {
                    let hash = v.member("GfxImage", "hash").and_then(|o| v.u32(hdr + o)).unwrap_or(0);
                    images.insert(id, (n, hash));
                }
            }
            return;
        }
        if offs.is_none() {
            offs = Offsets::new(v);
        }
        let Some(o) = offs.as_ref() else { return };
        if ty == "XModel" && !decode_models {
            return;
        }
        if ty == "Material" {
            if let Some(m) = read_material(v, o, hdr, &images) {
                materials.insert(id, m);
            }
        } else if ty == "GfxWorld" {
            if out.world.is_none() {
                out.world = super::t6_world::read_world(v, hdr, &materials, want_static);
                super::t6_world::read_hero_lights(v, hdr, &mut out.lights);
            }
        } else if decode_models {
            if let Some(m) = read_model(v, o, hdr, &materials) {
                model_ids.insert(id, decoded.len());
                decoded.push(m);
            }
        }
    };
    let report = walk_with(Game::T6, zone, Some(&mut hook));
    out.materials = materials.len();
    if let Some(w) = out.world.as_mut() {
        w.resolve_static_models(&model_ids, &decoded);
    }
    if want_models {
        out.models = decoded;
    }
    (out, report)
}

/// Opens a Black Ops II fastfile and reads its models.
pub fn read_models_file(path: &std::path::Path) -> Result<(T6Models, WalkReport)> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let zone = ZoneImage::open(Game::T6, &name, &bytes).with_context(|| format!("opening {}", path.display()))?;
    Ok(read_models(&zone))
}

fn read_material(v: &View, o: &Offsets, hdr: u32, images: &HashMap<u32, (String, u32)>) -> Option<T6Material> {
    // MaterialInfo starts with the name
    let name = v.cstr(v.ptr(hdr + o.mat[0])?)?;
    let count = v.u8(hdr + o.mat[1])? as u32;
    let table = v.ptr(hdr + o.mat[2])?;
    let mut m = T6Material { name, ..Default::default() };
    if table == 0 {
        return Some(m);
    }
    // MaterialTextureDef: nameHash, nameStart, nameEnd, samplerState (2), semantic, isMatureContent, pad, image
    let stride = v.struct_size("MaterialTextureDef").unwrap_or(16);
    let (n_start, n_end, sem, img) = (v.member("MaterialTextureDef", "nameStart")?, v.member("MaterialTextureDef", "nameEnd")?, v.member("MaterialTextureDef", "semantic")?, v.member("MaterialTextureDef", "image")?);
    let mut defs: Vec<(u8, u8, u8, String)> = Vec::new();
    for i in 0..count {
        let d = table + i * stride;
        let Some(id) = v.asset_id_at_slot(d + img) else { continue };
        let Some((image, hash)) = images.get(&id) else { continue };
        m.image_hashes.insert(image.clone(), *hash);
        defs.push((v.u8(d + sem)?, v.u8(d + n_start)?.to_ascii_lowercase(), v.u8(d + n_end)?.to_ascii_lowercase(), image.clone()));
    }
    // the same choice OpenAssetTools makes between several candidates
    let pick = |want: &dyn Fn(u8) -> bool, prefer: &[(u8, u8)]| -> Option<String> {
        let c: Vec<&(u8, u8, u8, String)> = defs.iter().filter(|d| want(d.0)).collect();
        if c.len() <= 1 {
            return c.first().map(|d| d.3.clone());
        }
        for &(a, b) in prefer {
            if let Some(d) = c.iter().find(|d| d.1 == a && d.2 == b) {
                return Some(d.3.clone());
            }
        }
        Some(c[0].3.clone())
    };
    m.color = pick(&|s| s == 2 || (0xC..=0x1B).contains(&s), &[(b'c', b'p'), (b'r', b'k'), (b'd', b'p')]);
    m.normal = pick(&|s| s == 5, &[(b'n', b'p')]);
    m.spec = pick(&|s| s == 8, &[(b's', b'p')]);
    m.textures = defs.into_iter().map(|d| (d.0, d.3)).collect();
    Some(m)
}

fn read_model(v: &View, o: &Offsets, hdr: u32, materials: &HashMap<u32, T6Material>) -> Option<T6Model> {
    let x = |i: usize| hdr + o.xmodel[i];
    let name = v.cstr(v.ptr(x(0))?)?;
    let (num_bones, num_roots, num_surfs) = (v.u8(x(1))? as usize, v.u8(x(2))? as usize, v.u8(x(3))? as usize);
    let strings = v.script_strings();
    let mut model = T6Model { name, num_root_bones: num_roots as u8, ..Default::default() };
    model.radius = v.f32(x(13))?;
    model.mins = [v.f32(x(14))?, v.f32(x(14) + 4)?, v.f32(x(14) + 8)?];
    model.maxs = [v.f32(x(15))?, v.f32(x(15) + 4)?, v.f32(x(15) + 8)?];
    model.flags = v.u32(x(16))?;

    // skeleton
    let (names, parents, quats, trans, base) = (v.ptr(x(4))?, v.ptr(x(5))?, v.ptr(x(6))?, v.ptr(x(7))?, v.ptr(x(8))?);
    if num_bones > 0 && names != 0 && base != 0 {
        for b in 0..num_bones {
            let sidx = v.u16(names + 2 * b as u32)? as usize;
            let bone_name = strings.get(sidx).cloned().unwrap_or_else(|| "INVALID_BONE_NAME".to_string());
            let root = b < num_roots;
            let parent = if root || parents == 0 {
                -1
            } else {
                let p = b as i32 - v.u8(parents + (b - num_roots) as u32)? as i32;
                if p >= 0 && (p as usize) < num_bones { p } else { -1 }
            };
            let m = base + 32 * b as u32;
            let global = ([v.f32(m + 16)?, v.f32(m + 20)?, v.f32(m + 24)?], [v.f32(m)?, v.f32(m + 4)?, v.f32(m + 8)?, v.f32(m + 12)?]);
            let local = if root || quats == 0 || trans == 0 {
                ([0.0; 3], [0.0, 0.0, 0.0, 1.0])
            } else {
                let k = (b - num_roots) as u32;
                let q = |c: u32| v.u16(quats + 8 * k + 2 * c).map(|w| w as i16 as f32 / 32767.0);
                ([v.f32(trans + 12 * k)?, v.f32(trans + 12 * k + 4)?, v.f32(trans + 12 * k + 8)?], [q(0)?, q(1)?, q(2)?, q(3)?])
            };
            model.bones.push(T6Bone { name: bone_name, parent, local, global });
        }
    }

    // LODs: four records of dist, numsurfs, surfIndex, partBits[5]
    let num_lods = (v.u16(x(12))? as usize).min(4);
    let lod_base = x(11);
    for l in 0..num_lods {
        let r = lod_base + 28 * l as u32;
        model.lods.push(T6Lod { dist: v.f32(r)?, num_surfs: v.u16(r + 4)?, surf_index: v.u16(r + 6)? });
    }

    // surfaces and the materials they use
    let surfs = v.ptr(x(9))?;
    let handles = v.ptr(x(10))?;
    let ssz = v.struct_size("XSurface")?;
    for i in 0..num_surfs as u32 {
        model.materials.push(if handles == 0 { None } else { v.asset_id_at_slot(handles + 4 * i).and_then(|id| materials.get(&id)).cloned() });
        if surfs == 0 {
            continue;
        }
        model.surfaces.push(read_surface(v, o, surfs + i * ssz)?);
    }
    Some(model)
}

fn read_surface(v: &View, o: &Offsets, s: u32) -> Option<T6Surface> {
    let f = |i: usize| s + o.surf[i];
    let mut out = T6Surface { tile_mode: v.u8(f(0))? as i8, flags: v.u16(f(2))?, base_vert_index: v.u16(f(5))?, ..Default::default() };
    let (rigid_count, vert_count, tri_count) = (v.u8(f(1))? as usize, v.u16(f(3))? as usize, v.u16(f(4))? as usize);
    // XSurfaceVertexInfo: vertCount[4] (i16), vertsBlend, tensionData
    let info = f(7);
    for k in 0..4 {
        out.blend_counts[k] = v.u16(info + 2 * k as u32)?;
    }
    let blend_words: usize = (0..4).map(|k| out.blend_counts[k] as usize * (2 * k + 1)).sum();
    let blend_ptr = v.ptr(info + v.member("XSurfaceVertexInfo", "vertsBlend")?)?;
    if blend_words > 0 && blend_ptr != 0 {
        out.blend = v.bytes(blend_ptr, blend_words * 2)?.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect();
    }
    let verts = v.ptr(f(8))?;
    if vert_count > 0 && verts != 0 {
        let stride = v.struct_size("GfxPackedVertex")? as usize;
        let bytes = v.bytes(verts, vert_count * stride)?;
        for b in bytes.chunks_exact(stride) {
            let fl = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            let tc = u(20);
            out.vertices.push(T6Vertex {
                pos: [fl(0), fl(4), fl(8)],
                binormal_sign: fl(12),
                color: b[16..20].try_into().unwrap(),
                uv: [half_to_f32((tc & 0xFFFF) as u16), half_to_f32((tc >> 16) as u16)],
                normal: normalized(unpack_unit_vec(u(24))),
                tangent: normalized(unpack_unit_vec(u(28))),
            });
        }
    }
    let tris = v.ptr(f(6))?;
    if tri_count > 0 && tris != 0 {
        out.triangles = v.bytes(tris, tri_count * 6)?.chunks_exact(6).map(|t| [u16::from_le_bytes([t[0], t[1]]), u16::from_le_bytes([t[2], t[3]]), u16::from_le_bytes([t[4], t[5]])]).collect();
    }
    let list = v.ptr(f(9))?;
    if rigid_count > 0 && list != 0 {
        let stride = v.struct_size("XRigidVertList")? as usize;
        out.rigid = v
            .bytes(list, rigid_count * stride)?
            .chunks_exact(stride)
            .map(|r| T6Rigid { bone_offset: u16::from_le_bytes([r[0], r[1]]), vert_count: u16::from_le_bytes([r[2], r[3]]), tri_offset: u16::from_le_bytes([r[4], r[5]]), tri_count: u16::from_le_bytes([r[6], r[7]]) })
            .collect();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_match_ieee_for_ordinary_numbers() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(half_to_f32(0x3800), 0.5);
        assert_eq!(half_to_f32(0), 0.0);
    }

    #[test]
    fn packed_unit_vectors_decode_to_unit_length() {
        // the packer's inverse: (v - -24624.0939334638) * 0.0001218318939208984 as a float, low ten bits
        let pack = |v: [f32; 3]| {
            let p = |c: f32| (((c + 24624.0939334638) * 0.000_121_831_89f32).to_bits()) & 0x3FF;
            p(v[0]) | p(v[1]) << 10 | p(v[2]) << 20
        };
        for v in [[0.0f32, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.577, 0.577, -0.577]] {
            let d = unpack_unit_vec(pack(v));
            for k in 0..3 {
                assert!((d[k] - v[k]).abs() < 0.01, "{v:?} -> {d:?}");
            }
        }
    }
}
