//! SEModel (version 1) reader and writer.
//!
//! SEModel is the small binary model format of the SE-Tools family (Blender/Maya/3ds Max importers exist for it) and
//! is what the CoD community's viewmodel dumps in `weapon_viewmodel_codol` use. Layout, as found in those files:
//!
//! ```text
//! "SEModel", u16 version = 1, u16 header size (counted from the end of the version field)
//! u8 data flags (1 bones, 2 meshes, 4 materials), u8 bone flags (1 global, 2 local, 4 scale), u8 mesh flags
//! (1 normals, 2 uv sets, 4 colours, 8 weights; stored in the order positions, uv sets, normals, colours, weights), u32 bone / mesh / material counts, 3 reserved bytes
//! bone names (NUL-terminated), then per bone: u8 0, i32 parent, [global pos f32x3, rot f32x4 xyzw]
//!   [local pos, rot] [scale f32x3]
//! per mesh: u8 0, u8 uv set count, u8 max skin influences, u32 vertices, u32 faces, positions,
//!   [uv per vertex per set], [normals], [colours u8x4], [per vertex per influence: bone index, f32 weight], faces
//!   (indices sized by the count they index into: u8 below 256 entries, u16 below 65536, else u32), one i32 material index per uv set
//! per material: name, u8 simple flag, diffuse / normal / specular map names (NUL-terminated)
//! ```
//! Bone indices are sized by the bone count in the same way.

use anyhow::{Result, bail};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bone {
    pub name: String,
    pub parent: i32,
    /// World-space position and rotation (x, y, z, w).
    pub global: ([f32; 3], [f32; 4]),
    /// Position and rotation relative to the parent.
    pub local: ([f32; 3], [f32; 4]),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub uv_sets: u8,
    pub max_skin: u8,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// `uvs[vertex * uv_sets + set]`.
    pub uvs: Vec<[f32; 2]>,
    pub colors: Vec<[u8; 4]>,
    /// `weights[vertex * max_skin + k]` as (bone, weight).
    pub weights: Vec<(u32, f32)>,
    pub faces: Vec<[u32; 3]>,
    /// One material index per uv set.
    pub materials: Vec<i32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Material {
    pub name: String,
    pub diffuse: String,
    pub normal: String,
    pub specular: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeModel {
    pub bones: Vec<Bone>,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
}

fn index_size(count: usize) -> usize {
    if count < 0x100 {
        1
    } else if count < 0x1_0000 {
        2
    } else {
        4
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let Some(s) = self.b.get(self.p..self.p.checked_add(n).unwrap_or(usize::MAX)) else { bail!("SEModel is cut off at byte {}", self.p) };
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec<const N: usize>(&mut self) -> Result<[f32; N]> {
        let mut v = [0.0; N];
        for x in v.iter_mut() {
            *x = self.f32()?;
        }
        Ok(v)
    }
    fn index(&mut self, size: usize) -> Result<u32> {
        Ok(match size {
            1 => self.u8()? as u32,
            2 => u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as u32,
            _ => self.u32()?,
        })
    }
    fn cstr(&mut self) -> Result<String> {
        let rest = &self.b[self.p.min(self.b.len())..];
        let Some(n) = rest.iter().position(|&c| c == 0) else { bail!("unterminated string in SEModel") };
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.p += n + 1;
        Ok(s)
    }
}

impl SeModel {
    pub fn read(bytes: &[u8]) -> Result<SeModel> {
        if bytes.len() < 11 || &bytes[..7] != b"SEModel" {
            bail!("not an SEModel file");
        }
        let version = u16::from_le_bytes([bytes[7], bytes[8]]);
        if version != 1 {
            bail!("SEModel version {version} is not supported");
        }
        let header = u16::from_le_bytes([bytes[9], bytes[10]]) as usize;
        let mut r = Rd { b: bytes, p: 11 };
        let (data, bone_flags, mesh_flags) = (r.u8()?, r.u8()?, r.u8()?);
        let (nb, nm, nmat) = (r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
        if nb > 100_000 || nm > 100_000 || nmat > 100_000 {
            bail!("implausible SEModel counts");
        }
        r.p = 9 + header;
        let mut model = SeModel::default();
        if data & 1 != 0 {
            for _ in 0..nb {
                model.bones.push(Bone { name: r.cstr()?, ..Default::default() });
            }
            for b in model.bones.iter_mut() {
                r.u8()?;
                b.parent = r.i32()?;
                if bone_flags & 1 != 0 {
                    b.global = (r.vec::<3>()?, r.vec::<4>()?);
                }
                if bone_flags & 2 != 0 {
                    b.local = (r.vec::<3>()?, r.vec::<4>()?);
                }
                if bone_flags & 4 != 0 {
                    r.vec::<3>()?;
                }
            }
        }
        if data & 2 != 0 {
            for _ in 0..nm {
                r.u8()?;
                let (sets, skin) = (r.u8()?, r.u8()?);
                let (vc, fc) = (r.u32()? as usize, r.u32()? as usize);
                if vc > 10_000_000 || fc > 10_000_000 {
                    bail!("implausible SEModel mesh size");
                }
                let mut m = Mesh { uv_sets: sets, max_skin: skin, ..Default::default() };
                for _ in 0..vc {
                    m.positions.push(r.vec::<3>()?);
                }
                if mesh_flags & 2 != 0 {
                    for _ in 0..vc * sets as usize {
                        m.uvs.push(r.vec::<2>()?);
                    }
                }
                if mesh_flags & 1 != 0 {
                    for _ in 0..vc {
                        m.normals.push(r.vec::<3>()?);
                    }
                }
                if mesh_flags & 4 != 0 {
                    for _ in 0..vc {
                        m.colors.push(r.take(4)?.try_into().unwrap());
                    }
                }
                if mesh_flags & 8 != 0 {
                    let bsize = index_size(nb);
                    for _ in 0..vc * skin as usize {
                        let bone = r.index(bsize)?;
                        m.weights.push((bone, r.f32()?));
                    }
                }
                let isize = index_size(vc);
                for _ in 0..fc {
                    m.faces.push([r.index(isize)?, r.index(isize)?, r.index(isize)?]);
                }
                for _ in 0..sets {
                    m.materials.push(r.i32()?);
                }
                model.meshes.push(m);
            }
        }
        if data & 4 != 0 {
            for _ in 0..nmat {
                let name = r.cstr()?;
                let simple = r.u8()?;
                let (diffuse, normal, specular) = if simple != 0 { (r.cstr()?, r.cstr()?, r.cstr()?) } else { Default::default() };
                model.materials.push(Material { name, diffuse, normal, specular });
            }
        }
        Ok(model)
    }

    pub fn write(&self) -> Vec<u8> {
        let mut o: Vec<u8> = b"SEModel".to_vec();
        o.extend(1u16.to_le_bytes());
        o.extend(20u16.to_le_bytes());
        let has_bones = !self.bones.is_empty();
        let data = (has_bones as u8) | 2 | ((!self.materials.is_empty()) as u8) << 2;
        let mesh_flags = 1 | 2 | 8 * (self.meshes.iter().any(|m| !m.weights.is_empty()) as u8) | 4 * (self.meshes.iter().any(|m| !m.colors.is_empty()) as u8);
        o.extend([data, 3, mesh_flags]);
        for n in [self.bones.len(), self.meshes.len(), self.materials.len()] {
            o.extend((n as u32).to_le_bytes());
        }
        o.extend([0, 0, 0]);
        let f = |o: &mut Vec<u8>, v: &[f32]| v.iter().for_each(|x| o.extend(x.to_le_bytes()));
        if has_bones {
            for b in &self.bones {
                o.extend(b.name.as_bytes());
                o.push(0);
            }
            for b in &self.bones {
                o.push(0);
                o.extend(b.parent.to_le_bytes());
                f(&mut o, &b.global.0);
                f(&mut o, &b.global.1);
                f(&mut o, &b.local.0);
                f(&mut o, &b.local.1);
            }
        }
        let put_index = |o: &mut Vec<u8>, v: u32, size: usize| match size {
            1 => o.push(v as u8),
            2 => o.extend((v as u16).to_le_bytes()),
            _ => o.extend(v.to_le_bytes()),
        };
        for m in &self.meshes {
            let vc = m.positions.len();
            o.extend([0, m.uv_sets, m.max_skin]);
            o.extend((vc as u32).to_le_bytes());
            o.extend((m.faces.len() as u32).to_le_bytes());
            for p in &m.positions {
                f(&mut o, p);
            }
            for uv in &m.uvs {
                f(&mut o, uv);
            }
            for n in &m.normals {
                f(&mut o, n);
            }
            if mesh_flags & 4 != 0 {
                for i in 0..vc {
                    o.extend(m.colors.get(i).copied().unwrap_or([255; 4]));
                }
            }
            if mesh_flags & 8 != 0 {
                let bsize = index_size(self.bones.len());
                for k in 0..vc * m.max_skin as usize {
                    let (b, w) = m.weights.get(k).copied().unwrap_or((0, 0.0));
                    put_index(&mut o, b, bsize);
                    o.extend(w.to_le_bytes());
                }
            }
            let isize = index_size(vc);
            for t in &m.faces {
                for &i in t {
                    put_index(&mut o, i, isize);
                }
            }
            for mat in &m.materials {
                o.extend(mat.to_le_bytes());
            }
        }
        for m in &self.materials {
            o.extend(m.name.as_bytes());
            o.push(0);
            o.push(1);
            for s in [&m.diffuse, &m.normal, &m.specular] {
                o.extend(s.as_bytes());
                o.push(0);
            }
        }
        o
    }
}

/// Converts an export model to SEModel. SE-Tools expect centimetres, so positions are multiplied by `scale`
/// (2.54 turns the game's inches into centimetres, which is what the reference dumps use); axes are unchanged.
pub fn from_export(model: &super::ExportModel, scale: f32) -> SeModel {
    let bones = model.bones_or_default();
    let sc = |p: [f32; 3]| [p[0] * scale, p[1] * scale, p[2] * scale];
    let mut out = SeModel {
        bones: bones.iter().map(|b| Bone { name: b.name.clone(), parent: b.parent, global: (sc(b.global.0), b.global.1), local: (sc(b.local.0), b.local.1) }).collect(),
        ..Default::default()
    };
    for part in &model.parts {
        let skin = part.weights.iter().map(|w| w.len()).max().unwrap_or(1).clamp(1, 255);
        let mut m = Mesh { uv_sets: 1, max_skin: skin as u8, positions: part.positions.iter().map(|&p| sc(p)).collect(), normals: part.normals.clone(), uvs: part.uvs.clone(), faces: part.faces.clone(), materials: vec![part.material as i32], ..Default::default() };
        for w in &part.weights {
            for k in 0..skin {
                m.weights.push(w.get(k).map(|&(b, x)| (b, x)).unwrap_or((0, 0.0)));
            }
        }
        out.meshes.push(m);
    }
    let image = |n: &Option<String>| n.as_deref().map(|n| format!("_images\\{}", super::ExportModel::image_file(n))).unwrap_or_default();
    for m in &model.materials {
        out.materials.push(Material { name: m.name.clone(), diffuse: image(&m.textures.color), normal: image(&m.textures.normal), specular: image(&m.textures.spec) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SeModel {
        SeModel {
            bones: vec![
                Bone { name: "tag_origin".into(), parent: -1, global: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]), local: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]) },
                Bone { name: "tag_a".into(), parent: 0, global: ([1.0, 2.0, 3.0], [0.0, 0.0, 0.0, 1.0]), local: ([1.0, 2.0, 3.0], [0.0, 0.0, 0.0, 1.0]) },
            ],
            meshes: vec![Mesh {
                uv_sets: 1,
                max_skin: 1,
                positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                normals: vec![[0.0, 0.0, 1.0]; 3],
                uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                colors: vec![],
                weights: vec![(0, 1.0), (1, 1.0), (1, 1.0)],
                faces: vec![[0, 1, 2]],
                materials: vec![0],
            }],
            materials: vec![Material { name: "m".into(), diffuse: "d.png".into(), normal: String::new(), specular: String::new() }],
        }
    }

    #[test]
    fn write_then_read_gives_the_same_model() {
        let m = sample();
        assert_eq!(SeModel::read(&m.write()).unwrap(), m);
    }

    #[test]
    fn rejects_other_data() {
        assert!(SeModel::read(b"nope").is_err());
        let mut b = sample().write();
        b.truncate(b.len() - 8);
        assert!(SeModel::read(&b).is_err());
    }
}
