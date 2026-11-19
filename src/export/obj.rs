//! Wavefront OBJ + MTL writer, following OpenAssetTools' conventions: Y-up axes, reversed triangle order, V flipped,
//! one `o` per surface, textures referenced as `../images/<name>.png`.

use super::{ExportModel, to_y_up};
use std::fmt::Write;

/// Returns (obj text, mtl text). `scale` multiplies positions (1.0 keeps the game's inches).
pub fn write(model: &ExportModel, mtl_name: &str, scale: f32) -> (String, String) {
    let mut o = String::new();
    let _ = writeln!(o, "# atlas OBJ export of {}", model.name);
    let _ = writeln!(o, "mtllib {mtl_name}");
    let mut base = 1usize;
    for part in &model.parts {
        let _ = writeln!(o, "o {}", part.name);
        for p in &part.positions {
            let p = to_y_up(*p);
            let _ = writeln!(o, "v {:.6} {:.6} {:.6}", p[0] * scale, p[1] * scale, p[2] * scale);
        }
        for n in &part.normals {
            let n = to_y_up(*n);
            let _ = writeln!(o, "vn {:.6} {:.6} {:.6}", n[0], n[1], n[2]);
        }
        for uv in &part.uvs {
            let _ = writeln!(o, "vt {:.6} {:.6}", uv[0], 1.0 - uv[1]);
        }
        if let Some(m) = model.materials.get(part.material) {
            let _ = writeln!(o, "usemtl {}", m.name);
        }
        for f in &part.faces {
            // the game's triangles are wound the other way round
            let i = |k: usize| base + f[k] as usize;
            let _ = writeln!(o, "f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}", a = i(2), b = i(1), c = i(0));
        }
        base += part.positions.len();
    }
    let mut m = String::new();
    let _ = writeln!(m, "# atlas MTL export of {}", model.name);
    for mat in &model.materials {
        let _ = writeln!(m, "\nnewmtl {}", mat.name);
        if let Some(n) = &mat.textures.color {
            let _ = writeln!(m, "map_Kd ../images/{}", ExportModel::image_file(n));
        }
        if let Some(n) = &mat.textures.normal {
            let _ = writeln!(m, "map_bump ../images/{}", ExportModel::image_file(n));
        }
        if let Some(n) = &mat.textures.spec {
            let _ = writeln!(m, "map_Ks ../images/{}", ExportModel::image_file(n));
        }
    }
    (o, m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{ExportMaterial, Part};

    #[test]
    fn writes_reversed_y_up_triangles() {
        let m = ExportModel {
            name: "t".into(),
            parts: vec![Part {
                name: "t_0".into(),
                positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 2.0]],
                normals: vec![[0.0, 0.0, 1.0]; 3],
                uvs: vec![[0.0, 0.25]; 3],
                faces: vec![[0, 1, 2]],
                ..Default::default()
            }],
            materials: vec![ExportMaterial { name: "m".into(), ..Default::default() }],
            bones: vec![],
        };
        let (obj, mtl) = write(&m, "t.mtl", 1.0);
        assert!(obj.contains("v 0.000000 2.000000 -1.000000"));
        assert!(obj.contains("vn 0.000000 1.000000 -0.000000"));
        assert!(obj.contains("vt 0.000000 0.750000"));
        assert!(obj.contains("f 3/3/3 2/2/2 1/1/1"));
        assert!(mtl.contains("newmtl m"));
    }
}
