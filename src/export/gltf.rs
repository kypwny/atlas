//! glTF 2.0 binary (`.glb`) writer, with OpenAssetTools' conventions: (x, y, z) -> (x, z, -y), triangles reversed,
//! UVs as stored (glTF's V already runs downwards like the game's), the model's units kept unless scaled. Bones become
//! nodes with a skin (inverse bind matrices from the bind pose); surfaces become skinned primitives. Images are
//! embedded as PNG.

use super::{ExportModel, to_y_up};
use glam::{Mat4, Quat, Vec3, Vec4};
use serde_json::{Value, json};
use std::collections::HashMap;

/// Reconstructs a standard tangent-space normal map (RGB, +Z out) from the game's packed one: X in alpha and Y in
/// green, both stored as slopes scaled by about 4 around one half (the lit shaders use `N + x*T + y*B`), no Z.
pub fn normal_map_to_standard(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        let x = p[3] as f32 / 255.0 * 4.08 - 2.08;
        let y = p[1] as f32 / 255.0 * 4.06452 - 2.06452;
        let l = (x * x + y * y + 1.0).sqrt();
        let c = |v: f32| ((v / l * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
        out.extend([c(x), c(y), c(1.0), 255]);
    }
    out
}

struct Buf {
    data: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Buf {
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while self.data.len() % 4 != 0 {
            self.data.push(0);
        }
        let mut v = json!({"buffer": 0, "byteOffset": self.data.len(), "byteLength": bytes.len()});
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.data.extend_from_slice(bytes);
        self.views.push(v);
        self.views.len() - 1
    }

    fn accessor(&mut self, bytes: &[u8], target: Option<u32>, component: u32, kind: &str, count: usize, bounds: Option<([f32; 3], [f32; 3])>) -> usize {
        let view = self.view(bytes, target);
        let mut a = json!({"bufferView": view, "componentType": component, "count": count, "type": kind});
        if let Some((lo, hi)) = bounds {
            a["min"] = json!(lo);
            a["max"] = json!(hi);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }
}

fn floats(v: impl IntoIterator<Item = f32>) -> Vec<u8> {
    v.into_iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Builds the file. `images` maps an image file name (`ExportModel::image_file`) to PNG bytes; materials whose images
/// are missing are written without that texture. `scale` multiplies positions (0.0254 gives metres).
pub fn write_glb(model: &ExportModel, scale: f32, images: &HashMap<String, Vec<u8>>, anims: &[crate::anim::Animation]) -> Vec<u8> {
    let mut b = Buf { data: Vec::new(), views: Vec::new(), accessors: Vec::new() };
    let skinned = !model.bones.is_empty();
    let to_gltf = Mat4::from_cols(Vec4::new(1.0, 0.0, 0.0, 0.0), Vec4::new(0.0, 0.0, -1.0, 0.0), Vec4::new(0.0, 1.0, 0.0, 0.0), Vec4::W);
    let from_gltf = to_gltf.inverse();

    // ----- images, textures, materials
    let mut texture_of: HashMap<String, usize> = HashMap::new();
    let (mut tex_json, mut img_json) = (Vec::<Value>::new(), Vec::<Value>::new());
    let mut texture = |name: &Option<String>, b: &mut Buf| -> Option<usize> {
        let file = ExportModel::image_file(name.as_deref()?);
        if let Some(&t) = texture_of.get(&file) {
            return Some(t);
        }
        let png = images.get(&file)?;
        let view = b.view(png, None);
        img_json.push(json!({"bufferView": view, "mimeType": "image/png", "name": file}));
        tex_json.push(json!({"source": img_json.len() - 1, "sampler": 0}));
        texture_of.insert(file, tex_json.len() - 1);
        Some(tex_json.len() - 1)
    };
    let mut materials = Vec::new();
    for m in &model.materials {
        let mut pbr = json!({"metallicFactor": 0.0, "roughnessFactor": 1.0});
        if let Some(t) = texture(&m.textures.color, &mut b) {
            pbr["baseColorTexture"] = json!({"index": t});
        }
        let mut mat = json!({"name": m.name, "pbrMetallicRoughness": pbr});
        if let Some(t) = texture(&m.textures.normal, &mut b) {
            mat["normalTexture"] = json!({"index": t});
        }
        if let Some(s) = &m.state {
            if s.blended() {
                mat["alphaMode"] = json!("BLEND");
            } else if s.alpha_test != 0 {
                mat["alphaMode"] = json!("MASK");
                mat["alphaCutoff"] = json!(0.5);
            }
            if s.cull == 1 {
                mat["doubleSided"] = json!(true);
            }
        }
        materials.push(mat);
    }

    // ----- skeleton
    let bones = model.bones.clone();
    let global: Vec<Mat4> = bones
        .iter()
        .map(|bone| {
            let q = bone.global.1;
            let m = Mat4::from_rotation_translation(Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize(), Vec3::from(bone.global.0) * scale);
            to_gltf * m * from_gltf
        })
        .collect();
    let mut nodes: Vec<Value> = Vec::new();
    let mut root_nodes: Vec<usize> = Vec::new();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); bones.len()];
    for (i, bone) in bones.iter().enumerate() {
        if bone.parent >= 0 && (bone.parent as usize) < bones.len() {
            children[bone.parent as usize].push(i);
        } else {
            root_nodes.push(i);
        }
    }
    for (i, bone) in bones.iter().enumerate() {
        let local = if bone.parent >= 0 && (bone.parent as usize) < bones.len() { global[bone.parent as usize].inverse() * global[i] } else { global[i] };
        let (_, r, t) = local.to_scale_rotation_translation();
        let mut n = json!({"name": bone.name, "translation": [t.x, t.y, t.z], "rotation": [r.x, r.y, r.z, r.w]});
        if !children[i].is_empty() {
            n["children"] = json!(children[i]);
        }
        nodes.push(n);
    }
    let skin = if skinned {
        let ibm: Vec<f32> = global.iter().flat_map(|m| m.inverse().to_cols_array()).collect();
        let acc = b.accessor(&floats(ibm), None, 5126, "MAT4", bones.len(), None);
        Some(json!({"joints": (0..bones.len()).collect::<Vec<_>>(), "inverseBindMatrices": acc, "skeleton": root_nodes.first().copied().unwrap_or(0)}))
    } else {
        None
    };

    // ----- meshes
    let mut meshes = Vec::new();
    let mut mesh_nodes = Vec::new();
    for part in &model.parts {
        let n = part.positions.len();
        if n == 0 || part.faces.is_empty() {
            continue;
        }
        let pos: Vec<[f32; 3]> = part.positions.iter().map(|&p| to_y_up(p).map(|c| c * scale)).collect();
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &pos {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let mut attrs = serde_json::Map::new();
        attrs.insert("POSITION".into(), json!(b.accessor(&floats(pos.iter().flatten().copied()), Some(34962), 5126, "VEC3", n, Some((lo, hi)))));
        attrs.insert("NORMAL".into(), json!(b.accessor(&floats(part.normals.iter().flat_map(|&v| to_y_up(v))), Some(34962), 5126, "VEC3", n, None)));
        attrs.insert("TEXCOORD_0".into(), json!(b.accessor(&floats(part.uvs.iter().flatten().copied()), Some(34962), 5126, "VEC2", n, None)));
        if part.colors.iter().any(|c| *c != [255; 4]) {
            attrs.insert("COLOR_0".into(), json!(b.accessor(&floats(part.colors.iter().flat_map(|c| c.map(|x| x as f32 / 255.0))), Some(34962), 5126, "VEC4", n, None)));
        }
        if skinned {
            let mut joints = Vec::with_capacity(n * 8);
            let mut weights = Vec::with_capacity(n * 4);
            for w in &part.weights {
                let mut list: Vec<(u32, f32)> = w.clone();
                list.sort_by(|a, b| b.1.total_cmp(&a.1));
                list.truncate(4);
                let sum: f32 = list.iter().map(|x| x.1).sum::<f32>().max(1e-6);
                for k in 0..4 {
                    let (j, x) = list.get(k).copied().unwrap_or((0, 0.0));
                    joints.extend((j as u16).to_le_bytes());
                    weights.push(x / sum);
                }
            }
            attrs.insert("JOINTS_0".into(), json!(b.accessor(&joints, Some(34962), 5123, "VEC4", n, None)));
            attrs.insert("WEIGHTS_0".into(), json!(b.accessor(&floats(weights), Some(34962), 5126, "VEC4", n, None)));
        }
        let idx: Vec<u8> = part.faces.iter().flat_map(|f| [f[2], f[1], f[0]]).flat_map(|i| i.to_le_bytes()).collect();
        let indices = b.accessor(&idx, Some(34963), 5125, "SCALAR", part.faces.len() * 3, None);
        meshes.push(json!({"name": part.name, "primitives": [{"attributes": attrs, "indices": indices, "material": part.material, "mode": 4}]}));
        let mut node = json!({"name": part.name, "mesh": meshes.len() - 1});
        if skinned {
            node["skin"] = json!(0);
        }
        mesh_nodes.push(node);
    }
    let first_mesh_node = nodes.len();
    nodes.extend(mesh_nodes.iter().cloned());
    let mut scene_nodes: Vec<usize> = root_nodes.clone();
    scene_nodes.extend(first_mesh_node..nodes.len());

    // ----- animations: every driven bone sampled at every frame (local transforms, converted to the glTF axes)
    let mut animations = Vec::new();
    if skinned {
        let axis = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        for anim in anims {
            let frames = anim.num_frames as usize + 1;
            let times: Vec<f32> = (0..frames).map(|f| f as f32 / anim.frame_rate.max(1.0)).collect();
            let time_acc = b.accessor(&floats(times.iter().copied()), None, 5126, "SCALAR", frames, Some(([times[0], 0.0, 0.0], [*times.last().unwrap(), 0.0, 0.0])));
            let (mut samplers, mut channels) = (Vec::new(), Vec::new());
            let all_tracks = anim.tracks_with_delta();
            for (node, bone) in bones.iter().enumerate() {
                let Some(track) = all_tracks.iter().find(|t| t.name == bone.name) else { continue };
                if track.rot != crate::anim::Rotation::None {
                    let q: Vec<f32> = (0..frames).flat_map(|f| {
                        let q = track.rotation_at(f as f32).unwrap_or([0.0, 0.0, 0.0, 1.0]);
                        let r = axis * Quat::from_xyzw(q[0], q[1], q[2], q[3]) * axis.inverse();
                        [r.x, r.y, r.z, r.w]
                    }).collect();
                    let acc = b.accessor(&floats(q), None, 5126, "VEC4", frames, None);
                    samplers.push(json!({"input": time_acc, "output": acc, "interpolation": "LINEAR"}));
                    channels.push(json!({"sampler": samplers.len() - 1, "target": {"node": node, "path": "rotation"}}));
                }
                if track.trans != crate::anim::Translation::None {
                    let v: Vec<f32> = (0..frames).flat_map(|f| to_y_up(track.translation_at(f as f32).unwrap_or([0.0; 3])).map(|c| c * scale)).collect();
                    let acc = b.accessor(&floats(v), None, 5126, "VEC3", frames, None);
                    samplers.push(json!({"input": time_acc, "output": acc, "interpolation": "LINEAR"}));
                    channels.push(json!({"sampler": samplers.len() - 1, "target": {"node": node, "path": "translation"}}));
                }
            }
            animations.push(json!({"name": anim.name, "samplers": samplers, "channels": channels}));
        }
    }

    let mut root = json!({
        "asset": {"version": "2.0", "generator": "atlas"},
        "scene": 0,
        "scenes": [{"nodes": scene_nodes}],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "buffers": [{"byteLength": b.data.len()}],
        "bufferViews": b.views,
        "accessors": b.accessors,
    });
    if !tex_json.is_empty() {
        root["textures"] = json!(tex_json);
        root["images"] = json!(img_json);
        root["samplers"] = json!([{"magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497}]);
    }
    if let Some(s) = skin {
        root["skins"] = json!([s]);
    }
    if !animations.is_empty() {
        root["animations"] = json!(animations);
    }
    let mut json_bytes = serde_json::to_vec(&root).unwrap();
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }
    let mut bin = b.data;
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((json_bytes.len() as u32).to_le_bytes());
    out.extend(b"JSON");
    out.extend(&json_bytes);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend([b'B', b'I', b'N', 0]);
    out.extend(&bin);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{ExportBone, ExportMaterial, Part};

    fn model(skinned: bool) -> ExportModel {
        ExportModel {
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
            materials: vec![ExportMaterial { name: "m".into(), ..Default::default() }],
            bones: if skinned { vec![ExportBone { name: "root".into(), parent: -1, local: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]), global: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]) }] } else { vec![] },
        }
    }

    fn json_of(glb: &[u8]) -> Value {
        assert_eq!(&glb[..4], b"glTF");
        let len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        assert_eq!(&glb[16..20], b"JSON");
        assert_eq!(u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize, glb.len());
        serde_json::from_slice(&glb[20..20 + len]).unwrap()
    }

    #[test]
    fn static_model_has_one_mesh_and_no_skin() {
        let j = json_of(&write_glb(&model(false), 1.0, &HashMap::new(), &[]));
        assert_eq!(j["meshes"].as_array().unwrap().len(), 1);
        assert!(j.get("skins").is_none());
        // y-up: the game's y of 1 becomes z = -1
        assert_eq!(j["accessors"][0]["min"][2], -1.0);
        assert_eq!(j["accessors"][0]["max"][0], 1.0);
    }

    #[test]
    fn skinned_model_gets_joints_weights_and_inverse_bind_matrices() {
        let j = json_of(&write_glb(&model(true), 0.0254, &HashMap::new(), &[]));
        let prim = &j["meshes"][0]["primitives"][0]["attributes"];
        assert!(prim.get("JOINTS_0").is_some() && prim.get("WEIGHTS_0").is_some());
        assert_eq!(j["skins"][0]["joints"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn normal_maps_are_reconstructed_as_unit_normals() {
        // alpha 0.5 and green 0.5 are the "flat" slope of about zero in the game's encoding
        let out = normal_map_to_standard(&[0, 128, 0, 128]);
        assert!((out[0] as i32 - 128).abs() <= 8 && (out[1] as i32 - 128).abs() <= 8 && out[2] >= 250);
    }
}
