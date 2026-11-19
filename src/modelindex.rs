//! The model index: which materials and textures each streamed model uses.
//!
//! The streamed geometry files carry no material information; that lives in
//! the `XModel` definitions inside the fastfile zones. `zone::export::build_index`
//! walks the zones and writes one TSV that
//! this module reads (records, tab-separated):
//!
//! ```text
//! zone      <file>  <version>
//! model     <name>  <bones> <roots> <surfaces>
//! lod       <model> <i> <56 bytes hex> <streamed surfaces file | ->
//! mat       <model> <slot> <material | ->
//! material  <name>  <texture count>
//! tex       <material> <j> <semantic> <sampler> <name hash hex> <image | ->
//! ```
//!
//! A LOD's streamed file is what `main/models/<name>` holds; its surface `k`
//! uses `materials[lod.surf_index + k]`.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// The images a surface samples: colour, normal and specular map names (None = the material has none).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SurfaceTextures {
    pub color: Option<String>,
    pub normal: Option<String>,
    pub spec: Option<String>,
}

/// Texture semantics seen in material definitions (the IW4 set).
pub const SEMANTIC_COLOR: u8 = 2;
pub const SEMANTIC_DETAIL: u8 = 3;
pub const SEMANTIC_NORMAL: u8 = 5;
pub const SEMANTIC_SPECULAR: u8 = 8;

#[derive(Clone, Debug)]
pub struct Texture {
    pub semantic: u8,
    pub sampler: u8,
    pub hash: u32,
    pub image: Option<String>,
}

/// Blend, alpha-test, cull and depth state of a material, decoded from its stored state bits.
///
/// Layout of the first state word (checked against every material class in the QA zones): bits 0-3 source and
/// 4-7 destination colour factor, 8-10 blend operation (0 = blending off), 11-13 alpha test (bit 11 set =
/// disabled, bit 12 = greater than 0, bits 12+13 = greater or equal 128, bit 13 = less than 128), 14-15 cull
/// (1 none, 2 back, 3 front). Second word: bit 0 depth write, bit 4 polygon offset. Blend factors use the
/// D3D9 numbering the engine keeps: 1 zero, 2 one, 3 src colour, 4 inv src colour, 5 src alpha, 6 inv src
/// alpha, 7 dest alpha, 8 inv dest alpha, 9 dest colour, 10 inv dest colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaterialState {
    pub sort_key: u8,
    pub src: u8,
    pub dst: u8,
    /// 0 disabled, 1 add, 2 subtract, 3 reverse subtract, 4 min, 5 max
    pub op: u8,
    /// 0 off, 1 alpha > 0, 2 alpha < 128, 3 alpha >= 128
    pub alpha_test: u8,
    /// 1 none, 2 back, 3 front
    pub cull: u8,
    pub depth_write: bool,
    pub polygon_offset: bool,
    /// Technique slot whose state was used.
    pub technique: u8,
}

impl MaterialState {
    /// Decodes the state the lit technique uses (slot 9), falling back to the unlit one (slot 4) and then to the
    /// first slot in use. `header` is the 104-byte material header, `table` the state-bits table.
    pub fn decode(header: &[u8], table: &[u8]) -> Option<MaterialState> {
        if header.len() < 104 {
            return None;
        }
        let entries = &header[32..80];
        let count = table.len() / 8;
        let slot = [9usize, 4].into_iter().chain(0..48).find(|&i| entries[i] != 255 && (entries[i] as usize) < count)?;
        let e = entries[slot] as usize;
        let b0 = u32::from_le_bytes(table[8 * e..8 * e + 4].try_into().ok()?);
        let b1 = u32::from_le_bytes(table[8 * e + 4..8 * e + 8].try_into().ok()?);
        Some(MaterialState {
            sort_key: header[28],
            src: (b0 & 15) as u8,
            dst: ((b0 >> 4) & 15) as u8,
            op: ((b0 >> 8) & 7) as u8,
            alpha_test: match (b0 >> 11) & 7 {
                2 => 1,
                4 => 2,
                6 => 3,
                _ => 0,
            },
            cull: ((b0 >> 14) & 3) as u8,
            depth_write: b1 & 1 != 0,
            polygon_offset: b1 & 0x10 != 0,
            technique: slot as u8,
        })
    }

    pub fn blended(&self) -> bool {
        self.op != 0
    }

    /// Lit transparent materials output premultiplied colour (one / inverse source alpha).
    pub fn premultiplied(&self) -> bool {
        self.op == 1 && self.src == 2 && self.dst == 6
    }

    pub fn describe(&self) -> String {
        let blend = if !self.blended() {
            "opaque".to_string()
        } else if self.src == 2 && self.dst == 2 {
            "additive".to_string()
        } else if self.src == 5 && self.dst == 6 {
            "alpha blend".to_string()
        } else if self.premultiplied() {
            "premultiplied blend".to_string()
        } else {
            format!("blend {}/{} op {}", self.src, self.dst, self.op)
        };
        let test = match self.alpha_test {
            1 => ", alpha > 0",
            2 => ", alpha < 128",
            3 => ", alpha >= 128",
            _ => "",
        };
        let cull = match self.cull {
            1 => ", two-sided",
            3 => ", front culled",
            _ => "",
        };
        format!("{blend}{test}{cull}")
    }
}

#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    pub textures: Vec<Texture>,
    pub techset: Option<String>,
    pub render: Option<MaterialState>,
    /// Material constants by name (for example `envMapParms`), four floats each.
    pub constants: Vec<(String, [f32; 4])>,
}

impl Material {
    pub fn constant(&self, name: &str) -> Option<[f32; 4]> {
        self.constants.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v)
    }
}

impl Material {
    /// The first resolved image with the given semantic.
    pub fn image_for(&self, semantic: u8) -> Option<&str> {
        self.textures.iter().find(|t| t.semantic == semantic && t.image.is_some()).and_then(|t| t.image.as_deref())
    }

    /// The colour map's image name (falling back to the first texture with an image).
    pub fn color_image(&self) -> Option<&str> {
        self.textures
            .iter()
            .find(|t| t.semantic == SEMANTIC_COLOR && t.image.is_some())
            .or_else(|| self.textures.iter().find(|t| t.image.is_some()))
            .and_then(|t| t.image.as_deref())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Lod {
    pub distance: f32,
    pub num_surfaces: u16,
    pub surface_index: u16,
    /// Name of the streamed geometry file for this LOD.
    pub file: Option<String>,
}

/// A model's skeleton as recorded from its zone (see `zone::replay::SkelRec`).
#[derive(Clone, Debug, Default)]
pub struct Skeleton {
    pub names: Vec<String>,
    /// Parent bone index per bone, -1 for the roots.
    pub parents: Vec<i32>,
    /// Local rotation (x, y, z, w) and position of every bone relative to its parent; roots are the identity.
    pub local: Vec<([f32; 4], [f32; 3])>,
    /// Model-space rotation (x, y, z, w) and position of every bone (the bind pose).
    pub global: Vec<([f32; 4], [f32; 3])>,
}

impl Skeleton {
    fn parse(names: &str, parents: &str, quats: &str, trans: &str, base: &str, roots: usize) -> Option<Skeleton> {
        let names: Vec<String> = names.split(',').map(str::to_string).collect();
        let n = names.len();
        let (parents, quats, trans, base) = (hex_bytes(parents), hex_bytes(quats), hex_bytes(trans), hex_bytes(base));
        if n == 0 || roots > n || parents.len() != n - roots || quats.len() != 8 * (n - roots) || trans.len() != 12 * (n - roots) || base.len() != 32 * n {
            return None;
        }
        let f = |b: &[u8], o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let global: Vec<_> = (0..n).map(|i| ([f(&base, 32 * i), f(&base, 32 * i + 4), f(&base, 32 * i + 8), f(&base, 32 * i + 12)], [f(&base, 32 * i + 16), f(&base, 32 * i + 20), f(&base, 32 * i + 24)])).collect();
        let mut local = Vec::with_capacity(n);
        let mut parent = Vec::with_capacity(n);
        for i in 0..n {
            if i < roots {
                parent.push(-1);
                // root bones carry no local pose (OpenAssetTools and the reference dumps both use the identity)
                local.push(([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0]));
            } else {
                let k = i - roots;
                parent.push(i as i32 - parents[k] as i32);
                let q = |c: usize| i16::from_le_bytes([quats[8 * k + 2 * c], quats[8 * k + 2 * c + 1]]) as f32 / 32768.0;
                local.push(([q(0), q(1), q(2), q(3)], [f(&trans, 12 * k), f(&trans, 12 * k + 4), f(&trans, 12 * k + 8)]));
            }
        }
        Some(Skeleton { names, parents: parent, local, global })
    }
}

#[derive(Clone, Debug)]
pub struct IndexModel {
    pub skeleton: Option<Skeleton>,
    pub name: String,
    pub bones: u8,
    pub roots: u8,
    pub surfaces: u8,
    pub lods: Vec<Lod>,
    /// Material name per slot.
    pub materials: Vec<Option<String>>,
}

#[derive(Default)]
pub struct ModelIndex {
    pub models: Vec<IndexModel>,
    materials: HashMap<String, Material>,
    by_file: HashMap<String, (usize, usize)>,
    pub zones: usize,
}

fn dash(s: &str) -> Option<String> {
    (!s.is_empty() && s != "-").then(|| s.to_string())
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

impl ModelIndex {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        // Names are single-byte text; decode lossily so odd bytes never fail the load.
        Ok(Self::parse(&String::from_utf8_lossy(&bytes)))
    }

    pub fn parse(text: &str) -> Self {
        let mut idx = ModelIndex::default();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        // The same model appears in several zones; the first definition wins.
        let mut skipping_model = false;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.first().copied() {
                Some("zone") => idx.zones += 1,
                Some("model") if f.len() >= 5 => {
                    let key = f[1].to_ascii_lowercase();
                    skipping_model = by_name.contains_key(&key);
                    if !skipping_model {
                        by_name.insert(key, idx.models.len());
                        idx.models.push(IndexModel {
                            skeleton: None,
                            name: f[1].to_string(),
                            bones: f[2].parse().unwrap_or(0),
                            roots: f[3].parse().unwrap_or(0),
                            surfaces: f[4].parse().unwrap_or(0),
                            lods: Vec::new(),
                            materials: Vec::new(),
                        });
                    }
                }
                Some("skel") if f.len() >= 7 && !skipping_model => {
                    let m = idx.models.last_mut().unwrap();
                    if m.name == f[1] {
                        m.skeleton = Skeleton::parse(f[2], f[3], f[4], f[5], f[6], m.roots as usize);
                    }
                }
                Some("lod") if f.len() >= 5 && !skipping_model => {
                    let raw = hex_bytes(f[3]);
                    let m = idx.models.last_mut().unwrap();
                    if raw.len() >= 8 && m.name == f[1] {
                        m.lods.push(Lod {
                            distance: f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
                            num_surfaces: u16::from_le_bytes([raw[4], raw[5]]),
                            surface_index: u16::from_le_bytes([raw[6], raw[7]]),
                            file: dash(f[4]),
                        });
                    }
                }
                Some("mat") if f.len() >= 4 && !skipping_model => {
                    let m = idx.models.last_mut().unwrap();
                    if m.name == f[1] {
                        m.materials.push(dash(f[3]));
                    }
                }
                Some("material") if f.len() >= 3 => {
                    idx.materials.entry(f[1].to_ascii_lowercase()).or_insert_with(|| Material { name: f[1].to_string(), textures: Vec::new(), techset: None, render: None, constants: Vec::new() });
                }
                Some("tex") if f.len() >= 7 => {
                    if let Some(mat) = idx.materials.get_mut(&f[1].to_ascii_lowercase()) {
                        let j: usize = f[2].parse().unwrap_or(usize::MAX);
                        // A material repeated in another zone must not gain duplicate textures.
                        if j == mat.textures.len() {
                            mat.textures.push(Texture {
                                semantic: f[3].parse().unwrap_or(0),
                                sampler: f[4].parse().unwrap_or(0),
                                hash: u32::from_str_radix(f[5], 16).unwrap_or(0),
                                image: dash(f[6]),
                            });
                        }
                    }
                }
                Some("state") if f.len() >= 5 => {
                    if let Some(mat) = idx.materials.get_mut(&f[1].to_ascii_lowercase()) {
                        if mat.render.is_none() && mat.techset.is_none() {
                            mat.techset = dash(f[2]);
                            mat.render = MaterialState::decode(&hex_bytes(f[3]), &hex_bytes(f[4]));
                        }
                    }
                }
                Some("const") if f.len() >= 7 => {
                    if let Some(mat) = idx.materials.get_mut(&f[1].to_ascii_lowercase()) {
                        // Only the first definition of a material counts (it can repeat in other zones).
                        let v: Vec<f32> = f[3..7].iter().map(|x| x.parse().unwrap_or(0.0)).collect();
                        if !mat.constants.iter().any(|(n, _)| n == f[2]) && mat.constants.len() < 64 && mat.render.is_some() {
                            mat.constants.push((f[2].to_string(), [v[0], v[1], v[2], v[3]]));
                        }
                    }
                }
                _ => {}
            }
        }
        for (mi, m) in idx.models.iter().enumerate() {
            for (li, lod) in m.lods.iter().enumerate() {
                if let Some(file) = &lod.file {
                    idx.by_file.entry(file.to_ascii_lowercase()).or_insert((mi, li));
                }
            }
        }
        idx
    }

    pub fn material(&self, name: &str) -> Option<&Material> {
        self.materials.get(&name.to_ascii_lowercase())
    }

    pub fn material_count(&self) -> usize {
        self.materials.len()
    }

    /// The model and LOD whose streamed geometry file has this name.
    pub fn find_file(&self, file_name: &str) -> Option<(&IndexModel, &Lod)> {
        let &(mi, li) = self.by_file.get(&file_name.to_ascii_lowercase())?;
        Some((&self.models[mi], &self.models[mi].lods[li]))
    }

    /// Colour-map image name of every surface of a streamed file (None where the index has no answer).
    pub fn surface_images(&self, file_name: &str, surfaces: usize) -> Vec<Option<String>> {
        (0..surfaces).map(|k| self.surface_material(file_name, k).and_then(|m| m.color_image()).map(str::to_string)).collect()
    }

    /// Colour, normal and specular image names of every surface of a streamed file.
    pub fn surface_textures(&self, file_name: &str, surfaces: usize) -> Vec<SurfaceTextures> {
        (0..surfaces)
            .map(|k| match self.surface_material(file_name, k) {
                Some(m) => SurfaceTextures {
                    color: m.color_image().map(str::to_string),
                    normal: m.image_for(SEMANTIC_NORMAL).map(str::to_string),
                    spec: m.image_for(SEMANTIC_SPECULAR).map(str::to_string),
                },
                None => SurfaceTextures::default(),
            })
            .collect()
    }

    /// The `envMapParms` constant (Fresnel minimum, maximum and exponent) of every surface; None when absent.
    pub fn surface_env(&self, file_name: &str, surfaces: usize) -> Vec<Option<[f32; 4]>> {
        (0..surfaces).map(|k| self.surface_material(file_name, k).and_then(|m| m.constant("envMapParms"))).collect()
    }

    /// Render state of every surface of a streamed file (None where the index has no state).
    pub fn surface_states(&self, file_name: &str, surfaces: usize) -> Vec<Option<MaterialState>> {
        (0..surfaces).map(|k| self.surface_material(file_name, k).and_then(|m| m.render)).collect()
    }

    /// Material of surface `k` of a streamed file, or None when the index does not cover it.
    pub fn surface_material(&self, file_name: &str, k: usize) -> Option<&Material> {
        let (model, lod) = self.find_file(file_name)?;
        if k >= lod.num_surfaces as usize {
            return None;
        }
        let slot = model.materials.get(lod.surface_index as usize + k)?.as_deref()?;
        self.material(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "#codol-zone-export 1\nzone\ta.ff\t423\n\
model\tgun_a\t22\t1\t2\n\
lod\tgun_a\t0\t0000964302000000feffffffcdc01330000000000000000000000000000000000000000000000000000000000000000000000000\tgun_a_vmgun10\n\
lod\tgun_a\t1\t0000fa4301000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\t-\n\
mat\tgun_a\t0\tmc/mtl_a\n\
mat\tgun_a\t1\tmc/mtl_b\n\
mat\tgun_a\t2\t-\n\
material\tmc/mtl_a\t2\n\
tex\tmc/mtl_a\t0\t5\t11\t59d30d0f\ta_nml\n\
tex\tmc/mtl_a\t1\t2\t11\ta0ab1041\ta_col\n\
material\tmc/mtl_b\t1\n\
tex\tmc/mtl_b\t0\t8\t11\t34ecccb3\t-\n\
zone\tb.ff\t423\n\
model\tGUN_A\t1\t1\t1\n\
lod\tGUN_A\t0\t0000964301000000feffffff00000000\tother10\n\
material\tmc/mtl_a\t2\n\
tex\tmc/mtl_a\t0\t5\t11\t59d30d0f\tWRONG\n";

    #[test]
    fn parses_models_lods_and_materials() {
        let idx = ModelIndex::parse(SAMPLE);
        assert_eq!(idx.zones, 2);
        assert_eq!(idx.models.len(), 1, "a repeated model must keep its first definition");
        let m = &idx.models[0];
        assert_eq!((m.bones, m.surfaces), (22, 2));
        assert_eq!(m.lods[0].distance, 300.0);
        assert_eq!((m.lods[0].num_surfaces, m.lods[0].surface_index), (2, 0));
        assert_eq!(m.lods[1].surface_index, 2);
        assert_eq!(m.materials, vec![Some("mc/mtl_a".into()), Some("mc/mtl_b".into()), None]);
    }

    #[test]
    fn decodes_state_bits_of_the_material_classes() {
        // Values taken from QA materials: additive (ONE/ONE), alpha blend (SRCALPHA/INVSRCALPHA), opaque unlit.
        let table = |b0: u32, b1: u32| [b0.to_le_bytes(), b1.to_le_bytes()].concat();
        let mut header = vec![0u8; 104];
        header[28] = 29;
        for b in &mut header[32..80] {
            *b = 255;
        }
        header[32 + 4] = 0;
        let add = MaterialState::decode(&header, &table(0x5921_4922, 0xc)).unwrap();
        assert_eq!((add.src, add.dst, add.op, add.alpha_test, add.cull, add.depth_write), (2, 2, 1, 0, 1, false));
        assert_eq!(add.describe(), "additive, two-sided");
        assert_eq!(add.sort_key, 29);
        let alpha = MaterialState::decode(&header, &table(0x5928_9165, 0xc)).unwrap();
        assert_eq!((alpha.src, alpha.dst, alpha.alpha_test, alpha.cull), (5, 6, 1, 2));
        assert_eq!(alpha.describe(), "alpha blend, alpha > 0");
        // opaque: ONE/ZERO, blend off, alpha test greater-equal 128, cull back, depth write on
        let b0 = 2 | (1 << 4) | (6 << 11) | (2 << 14);
        let opaque = MaterialState::decode(&header, &table(b0, 0xd)).unwrap();
        assert_eq!((opaque.op, opaque.alpha_test, opaque.cull, opaque.depth_write), (0, 3, 2, true));
        assert_eq!(opaque.describe(), "opaque, alpha >= 128");
        // a table without the referenced entry has no state
        assert!(MaterialState::decode(&header, &[]).is_none());
    }

    #[test]
    fn reads_material_constants_and_texture_roles() {
        let text = "model\tm\t1\t1\t1\n\
lod\tm\t0\t0000964301000000feffffff00000000\tm10\n\
mat\tm\t0\tmc/x\n\
material\tmc/x\t3\n\
tex\tmc/x\t0\t8\t11\t00000001\tx_spc\n\
tex\tmc/x\t1\t5\t11\t00000002\tx_nml\n\
tex\tmc/x\t2\t2\t11\t00000003\tx_col\n\
state\tmc/x\t-\t0000000000000000000000000000000000000000000000000000000000000000ffffffff00000000000000000000000000000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\t\n\
const\tmc/x\tenvMapParms\t0.33\t1.3\t1.1\t1.23\n";
        let idx = ModelIndex::parse(text);
        let t = &idx.surface_textures("m10", 1)[0];
        assert_eq!((t.color.as_deref(), t.normal.as_deref(), t.spec.as_deref()), (Some("x_col"), Some("x_nml"), Some("x_spc")));
        // constants are only kept for materials that have a decoded state; this one has none, so none are stored
        assert!(idx.surface_env("m10", 1)[0].is_none());
    }

    #[test]
    fn finds_surface_materials_by_file_name() {
        let idx = ModelIndex::parse(SAMPLE);
        let a = idx.surface_material("GUN_A_VMGUN10", 0).unwrap();
        assert_eq!(a.name, "mc/mtl_a");
        assert_eq!(a.color_image(), Some("a_col"));
        let b = idx.surface_material("gun_a_vmgun10", 1).unwrap();
        assert_eq!(b.color_image(), None, "unresolved image names stay unresolved");
        assert!(idx.surface_material("gun_a_vmgun10", 2).is_none(), "outside the LOD's surfaces");
        assert!(idx.surface_material("nothing", 0).is_none());
        assert_eq!(idx.material("mc/mtl_a").unwrap().textures.len(), 2, "repeated materials must not duplicate textures");
    }
}
