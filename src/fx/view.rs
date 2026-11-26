//! What the viewer draws for a running effect: its quads as viewer batches, a glow per light, and a floor to see them against.

use super::sim::DrawList;
use crate::model::{Model, Surface, Vertex};
use super::def::LoadedModel;
use crate::modelindex::{ModelIndex, SurfaceTextures};
use crate::modelsource::ModelCatalog;
use std::sync::Arc;
use crate::viewer::{FxBatch, FxVertex};
use glam::Vec3;

/// A checkered floor of 64-unit cells, 2048 units across, at z = 0.
pub fn ground_model() -> Model {
    let (n, cell) = (32i32, 64.0f32);
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for y in 0..n {
        for x in 0..n {
            let shade = if (x + y) % 2 == 0 { 58 } else { 44 };
            let (x0, y0) = ((x - n / 2) as f32 * cell, (y - n / 2) as f32 * cell);
            let base = vertices.len() as u32;
            for (dx, dy) in [(0.0, 0.0), (cell, 0.0), (cell, cell), (0.0, cell)] {
                vertices.push(Vertex { pos: [x0 + dx, y0 + dy, 0.0], normal: [0.0, 0.0, 1.0], tangent: [1.0, 0.0, 0.0], binormal_sign: 1.0, uv: [0.0, 0.0], color: [shade, shade, shade + 4, 255] });
            }
            // clockwise seen from above (the viewer's front faces)
            indices.extend([base, base + 3, base + 2, base, base + 2, base + 1]);
        }
    }
    Model { name: "ground".into(), surfaces: vec![Surface { vertices, indices, rigid: Vec::new(), blend_counts: [0; 4], blend: Vec::new(), header: [0; 84] }] }
}

/// The viewer's batches for what the player drew: the quads, and a soft additive glow for each light.
pub fn batches(list: &DrawList, right: Vec3, up: Vec3) -> Vec<FxBatch> {
    let mut out: Vec<FxBatch> = list
        .batches
        .iter()
        .map(|b| FxBatch { texture: b.textures.color.clone(), state: b.state, vertices: b.vertices.iter().map(|v| FxVertex { pos: v.pos, uv: v.uv, color: v.color }).collect() })
        .collect();
    if !list.lights.is_empty() {
        let mut vertices = Vec::new();
        for l in &list.lights {
            let r = (l.radius * 0.25).clamp(4.0, 64.0);
            let c = [(l.color[0] * 255.0) as u8, (l.color[1] * 255.0) as u8, (l.color[2] * 255.0) as u8, (l.color[3] * 140.0) as u8];
            let (a, b) = (right * r, up * r);
            let v = [l.pos - a + b, l.pos + a + b, l.pos + a - b, l.pos - a - b];
            let uv = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
            for k in [0usize, 1, 2, 0, 2, 3] {
                vertices.push(FxVertex { pos: v[k].to_array(), uv: uv[k], color: c });
            }
        }
        // additive (source alpha, one)
        let state = crate::modelindex::MaterialState { sort_key: 48, src: 5, dst: 2, op: 1, alpha_test: 0, cull: 1, depth_write: false, polygon_offset: false, technique: 0 };
        out.push(FxBatch { texture: None, state: Some(state), vertices });
    }
    out
}


/// The mesh of the xmodel `name`: from the zones the library read when they hold its geometry, else (see [`load_model`]) from the
/// streamed model files.
pub fn find_model(lib: &super::Library, catalog: Option<&ModelCatalog>, index: Option<&ModelIndex>, name: &str) -> Option<Arc<LoadedModel>> {
    if let Some(m) = lib.models.get(&name.to_ascii_lowercase()) {
        return Some(m.clone());
    }
    load_model(catalog?, index, name).map(Arc::new)
}

/// Reads the mesh of the xmodel `name`: the file of its first LOD per the model index, or a model file of that name. Its materials come
/// from the index (none without one).
pub fn load_model(catalog: &ModelCatalog, index: Option<&ModelIndex>, name: &str) -> Option<LoadedModel> {
    let file = index
        .and_then(|i| i.models.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
        .and_then(|m| m.lods.iter().find_map(|l| l.file.clone()))
        .unwrap_or_else(|| name.to_string());
    let entry = catalog.entries.iter().find(|e| e.name.eq_ignore_ascii_case(&file))?;
    let model = Model::parse(&catalog.providers[entry.provider].read(&entry.key).ok()?).ok()?;
    let n = model.surfaces.len();
    let (textures, states) = match index {
        Some(i) => (i.surface_textures(&entry.name, n), i.surface_states(&entry.name, n)),
        None => (vec![SurfaceTextures::default(); n], vec![None; n]),
    };
    Some(LoadedModel { model, textures, states })
}

/// The viewer's model placements for what the player drew, for the models already loaded.
pub fn models<'a>(list: &DrawList, loaded: &'a std::collections::HashMap<String, Option<Arc<LoadedModel>>>) -> Vec<crate::viewer::FxModel<'a>> {
    list.models
        .iter()
        .filter_map(|m| {
            let l = loaded.get(&m.name.to_ascii_lowercase())?.as_ref()?;
            Some(crate::viewer::FxModel {
                model: &l.model,
                textures: &l.textures,
                states: &l.states,
                origin: m.origin.to_array(),
                axis: [m.axis.col(0).to_array(), m.axis.col(1).to_array(), m.axis.col(2).to_array()],
                scale: m.scale,
            })
        })
        .collect()
}
