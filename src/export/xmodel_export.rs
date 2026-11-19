//! XMODEL_EXPORT version 6: the text model format of the Call of Duty 4 / World at War / Black Ops modding tools
//! (Maya plug-ins, Radiant asset converters). Layout and conventions follow OpenAssetTools' writer: the game's own
//! axes and units, bones with a model-space offset and rotation matrix, positions merged per (position, weights),
//! per-corner normal / colour / UV, and triangles as stored.

use super::ExportModel;
use std::collections::HashMap;
use std::fmt::Write;

/// Rotation matrix columns (x axis, y axis, z axis) of a quaternion (x, y, z, w).
pub fn quat_axes(q: [f32; 4]) -> [[f32; 3]; 3] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt().max(1e-12);
    let (x, y, z, w) = (q[0] / n, q[1] / n, q[2] / n, q[3] / n);
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w)],
        [2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w)],
        [2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

pub fn write(model: &ExportModel, scale: f32) -> String {
    let bones = model.bones_or_default();
    let mut o = String::new();
    let _ = writeln!(o, "// atlas XMODEL_EXPORT file\n// Model: {}\nMODEL\nVERSION 6\n", model.name);
    let _ = writeln!(o, "NUMBONES {}", bones.len());
    for (i, b) in bones.iter().enumerate() {
        let _ = writeln!(o, "BONE {i} {} \"{}\"", b.parent, b.name);
    }
    let _ = writeln!(o);
    for (i, b) in bones.iter().enumerate() {
        let a = quat_axes(b.global.1);
        let p = b.global.0;
        let _ = writeln!(o, "BONE {i}");
        let _ = writeln!(o, "OFFSET {:.6}, {:.6}, {:.6}", p[0] * scale, p[1] * scale, p[2] * scale);
        let _ = writeln!(o, "SCALE 1.000000, 1.000000, 1.000000");
        for (name, v) in ["X", "Y", "Z"].iter().zip(a) {
            let _ = writeln!(o, "{name} {:.6}, {:.6}, {:.6}", v[0], v[1], v[2]);
        }
        let _ = writeln!(o);
    }

    // distinct (position, weights) records, in first-use order; every vertex maps to one
    type Key = ([u32; 3], Vec<(u32, u32)>);
    let mut distinct: Vec<(&[f32; 3], &Vec<(u32, f32)>)> = Vec::new();
    let mut lookup: HashMap<Key, usize> = HashMap::new();
    let mut map: Vec<Vec<usize>> = Vec::new();
    for part in &model.parts {
        let mut m = Vec::with_capacity(part.positions.len());
        for (v, p) in part.positions.iter().enumerate() {
            let w = &part.weights[v];
            let key: Key = ([p[0].to_bits(), p[1].to_bits(), p[2].to_bits()], w.iter().map(|&(b, x)| (b, x.to_bits())).collect());
            let idx = *lookup.entry(key).or_insert_with(|| {
                distinct.push((p, w));
                distinct.len() - 1
            });
            m.push(idx);
        }
        map.push(m);
    }
    let _ = writeln!(o, "NUMVERTS {}", distinct.len());
    for (i, (p, w)) in distinct.iter().enumerate() {
        let _ = writeln!(o, "VERT {i}\nOFFSET {:.6}, {:.6}, {:.6}\nBONES {}", p[0] * scale, p[1] * scale, p[2] * scale, w.len());
        for (b, x) in w.iter() {
            let _ = writeln!(o, "BONE {b} {x:.6}");
        }
        let _ = writeln!(o);
    }
    let faces: usize = model.parts.iter().map(|p| p.faces.len()).sum();
    let _ = writeln!(o, "NUMFACES {faces}");
    for (pi, part) in model.parts.iter().enumerate() {
        for f in &part.faces {
            let _ = writeln!(o, "TRI {pi} {} 0 0", part.material);
            for &k in f {
                let k = k as usize;
                let c = part.colors.get(k).copied().unwrap_or([255; 4]);
                let _ = writeln!(o, "VERT {}", map[pi][k]);
                let _ = writeln!(o, "NORMAL {:.6} {:.6} {:.6}", part.normals[k][0], part.normals[k][1], part.normals[k][2]);
                let _ = writeln!(o, "COLOR {:.6} {:.6} {:.6} {:.6}", c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0);
                let _ = writeln!(o, "UV 1 {:.6} {:.6}", part.uvs[k][0], part.uvs[k][1]);
            }
            let _ = writeln!(o);
        }
    }
    let _ = writeln!(o, "NUMOBJECTS {}", model.parts.len());
    for (i, p) in model.parts.iter().enumerate() {
        let _ = writeln!(o, "OBJECT {i} \"{}\"", p.name);
    }
    let _ = writeln!(o, "\nNUMMATERIALS {}", model.materials.len());
    for (i, m) in model.materials.iter().enumerate() {
        let image = m.textures.color.as_deref().map(ExportModel::image_file).unwrap_or_default();
        let _ = writeln!(o, "MATERIAL {i} \"{}\" \"Lambert\" \"../images/{image}\"", m.name);
        let _ = writeln!(o, "COLOR 0.000000 0.000000 0.000000 1.000000");
        let _ = writeln!(o, "TRANSPARENCY 0.000000 0.000000 0.000000 1.000000");
        let _ = writeln!(o, "AMBIENTCOLOR 0.000000 0.000000 0.000000 1.000000");
        let _ = writeln!(o, "INCANDESCENCE 0.000000 0.000000 0.000000 1.000000");
        let _ = writeln!(o, "COEFFS 0.800000 0.000000");
        let _ = writeln!(o, "GLOW 0.000000 0");
        let _ = writeln!(o, "REFRACTIVE 6 1.000000");
        let _ = writeln!(o, "SPECULARCOLOR -1.000000 -1.000000 -1.000000 1.000000");
        let _ = writeln!(o, "REFLECTIVECOLOR -1.000000 -1.000000 -1.000000 1.000000");
        let _ = writeln!(o, "REFLECTIVE 6 -1.000000");
        let _ = writeln!(o, "BLINN -1.000000 -1.000000");
        let _ = writeln!(o, "PHONG -1.000000\n");
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{ExportMaterial, Part};

    #[test]
    fn identity_rotation_gives_unit_axes() {
        assert_eq!(quat_axes([0.0, 0.0, 0.0, 1.0]), [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        // 90 degrees about z turns x into y
        let a = quat_axes([0.0, 0.0, 0.70710677, 0.70710677]);
        assert!((a[0][1] - 1.0).abs() < 1e-6 && a[0][0].abs() < 1e-6);
    }

    #[test]
    fn writes_the_version_6_sections_in_order() {
        let m = ExportModel {
            name: "t".into(),
            parts: vec![Part {
                name: "t_0".into(),
                positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                normals: vec![[0.0, 0.0, 1.0]; 3],
                uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                colors: vec![],
                weights: vec![vec![(0, 1.0)]; 3],
                faces: vec![[0, 1, 2]],
                material: 0,
            }],
            materials: vec![ExportMaterial { name: "mat".into(), ..Default::default() }],
            bones: vec![],
        };
        let t = write(&m, 1.0);
        let order = ["MODEL", "VERSION 6", "NUMBONES 1", "BONE 0 -1 \"tag_origin\"", "NUMVERTS 3", "NUMFACES 1", "TRI 0 0 0 0", "NUMOBJECTS 1", "NUMMATERIALS 1"];
        let mut at = 0;
        for s in order {
            at += t[at..].find(s).unwrap_or_else(|| panic!("missing or out of order: {s}"));
        }
    }
}
