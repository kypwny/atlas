//! Model export: the shared in-memory model and the writers (SEModel, OBJ, glTF, XMODEL_EXPORT).

pub mod cast;
pub mod gltf;
pub mod obj;
pub mod seanim;
pub mod t6;
pub mod semodel;
pub mod xmodel_export;

use crate::model::Model;
use crate::modelindex::{MaterialState, ModelIndex, SurfaceTextures};
use std::collections::HashMap;

/// One drawable piece (a surface of the streamed model).
#[derive(Clone, Debug, Default)]
pub struct Part {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub colors: Vec<[u8; 4]>,
    /// (bone, weight) per vertex; weights sum to 1.
    pub weights: Vec<Vec<(u32, f32)>>,
    /// Triangles exactly as stored (the game's winding).
    pub faces: Vec<[u32; 3]>,
    /// Index into `ExportModel::materials`.
    pub material: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ExportMaterial {
    pub name: String,
    pub textures: SurfaceTextures,
    pub state: Option<MaterialState>,
}

#[derive(Clone, Debug, Default)]
pub struct ExportBone {
    pub name: String,
    pub parent: i32,
    /// Position and rotation (x, y, z, w) relative to the parent, in the game's units.
    pub local: ([f32; 3], [f32; 4]),
    /// The same in model space.
    pub global: ([f32; 3], [f32; 4]),
}

#[derive(Clone, Debug, Default)]
pub struct ExportModel {
    pub name: String,
    pub parts: Vec<Part>,
    pub materials: Vec<ExportMaterial>,
    /// Empty when the skeleton is not known; writers then use a single root bone.
    pub bones: Vec<ExportBone>,
}

impl ExportModel {
    /// Builds from a streamed model. `file` is the streamed file's name, used with `index` to find each
    /// surface's material and textures.
    pub fn from_streamed(model: &Model, index: Option<&ModelIndex>, file: &str) -> ExportModel {
        let weights = model.skin_weights();
        // name the export after the model the zones call it, when the index knows
        let name = index.and_then(|i| i.find_file(file)).map(|(m, _)| m.name.clone()).unwrap_or_else(|| file.to_string());
        let mut out = ExportModel { name, ..Default::default() };
        if let Some(sk) = index.and_then(|i| i.find_file(file)).and_then(|(m, _)| m.skeleton.as_ref()) {
            for (i, name) in sk.names.iter().enumerate() {
                out.bones.push(ExportBone {
                    name: name.clone(),
                    parent: sk.parents[i],
                    local: (sk.local[i].1, sk.local[i].0),
                    global: (sk.global[i].1, sk.global[i].0),
                });
            }
        }
        for (k, s) in model.surfaces.iter().enumerate() {
            let material = index.and_then(|i| i.surface_material(file, k));
            let name = material.map(|m| m.name.trim_start_matches(',').to_string()).unwrap_or_else(|| format!("material_{k}"));
            let slot = match out.materials.iter().position(|m| m.name == name) {
                Some(i) => i,
                None => {
                    out.materials.push(ExportMaterial {
                        name,
                        textures: index.map(|i| i.surface_textures(file, model.surfaces.len())[k].clone()).unwrap_or_default(),
                        state: material.and_then(|m| m.render),
                    });
                    out.materials.len() - 1
                }
            };
            out.parts.push(Part {
                name: format!("{}_{k}", out.name),
                positions: s.vertices.iter().map(|v| v.pos).collect(),
                normals: s.vertices.iter().map(|v| v.normal).collect(),
                uvs: s.vertices.iter().map(|v| v.uv).collect(),
                colors: s.vertices.iter().map(|v| v.color).collect(),
                weights: weights[k].iter().map(|w| if w.is_empty() { vec![(0, 1.0)] } else { w.clone() }).collect(),
                faces: s.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect(),
                material: slot,
            });
        }
        out
    }
}

/// Game-axis conversion used by the OBJ and glTF writers, the same as OpenAssetTools: (x, y, z) -> (x, z, -y).
pub fn to_y_up(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[2], -p[1]]
}

impl ExportModel {
    /// Every image the materials refer to (colour, normal, specular), each once.
    pub fn image_names(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for m in &self.materials {
            for n in [&m.textures.color, &m.textures.normal, &m.textures.spec].into_iter().flatten() {
                let n = n.trim_start_matches(',').to_string();
                if !v.contains(&n) {
                    v.push(n);
                }
            }
        }
        v
    }

    /// Bones to write: the recorded skeleton, or a single root at the origin that every vertex follows.
    pub fn bones_or_default(&self) -> Vec<ExportBone> {
        if !self.bones.is_empty() {
            return self.bones.clone();
        }
        vec![ExportBone { name: "tag_origin".into(), parent: -1, local: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]), global: ([0.0; 3], [0.0, 0.0, 0.0, 1.0]) }]
    }

    /// File name for an image as the writers refer to it (`<name>.png`, without a leading comma).
    pub fn image_file(name: &str) -> String {
        format!("{}.png", name.trim_start_matches(','))
    }
}

/// Replays the game folder's zones and decodes the animations whose names match (exact names, or a prefix ending in `*`);
/// the first definition of a name wins. Takes a while (every zone is read).
pub fn find_animations(root: &std::path::Path, names: &[String]) -> anyhow::Result<(Vec<crate::anim::Animation>, Vec<String>)> {
    let key = crate::zone::export::find_key(root)?;
    let (mut found, mut problems) = (Vec::<crate::anim::Animation>::new(), Vec::new());
    for path in crate::zone::export::zone_files(root) {
        let Ok(z) = crate::zone::Zone::open(&path, &key) else { continue };
        for raw in crate::zone::export::zone_anims(&z) {
            let wanted = names.iter().any(|n| match n.strip_suffix('*') {
                Some(prefix) => raw.name.starts_with(prefix),
                None => raw.name == *n,
            });
            if wanted && !found.iter().any(|a| a.name == raw.name) {
                match raw.decode() {
                    Ok(a) => found.push(a),
                    Err(e) => problems.push(format!("{}: {e:#}", raw.name)),
                }
            }
        }
    }
    Ok((found, problems))
}

/// Which files to write.
#[derive(Clone, Copy, Debug)]
pub struct Formats {
    pub glb: bool,
    pub obj: bool,
    pub xmodel_export: bool,
    pub semodel: bool,
    /// A Cast file (`xmodel/<name>.cast`) with the model and every animation given, for Blender and Maya.
    pub cast: bool,
    /// Write each animation given to `export` as `xanim/<name>.seanim`.
    pub seanim: bool,
}

impl Default for Formats {
    fn default() -> Self {
        Formats { glb: true, obj: false, xmodel_export: false, semodel: false, cast: false, seanim: true }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub formats: Formats,
    /// Also write the textures the model's materials use (as PNG) and embed them in the glTF.
    pub images: bool,
    /// Multiplies positions; 1.0 keeps the game's inches. SEModel is always written in centimetres on top of this.
    pub scale: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options { formats: Formats::default(), images: true, scale: 1.0 }
    }
}

/// Characters that cannot be in a file name.
fn safe(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).collect()
}

fn png_bytes(rgba: &[u8], w: u32, h: u32) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    image::write_buffer_with_format(&mut std::io::Cursor::new(&mut out), rgba, w, h, image::ColorType::Rgba8, image::ImageFormat::Png)?;
    Ok(out)
}

/// Writes the model under `dir`: `xmodel/<name>.<ext>` for each format and `images/<image>.png` for its textures
/// (the paths the OBJ/MTL, XMODEL_EXPORT and SEModel files refer to). Returns the files written. Images that cannot be
/// loaded are skipped and listed in the second result.
pub fn export(model: &ExportModel, dir: &std::path::Path, catalog: Option<&crate::source::Catalog>, opts: &Options, anims: &[crate::anim::Animation]) -> anyhow::Result<(Vec<std::path::PathBuf>, Vec<String>)> {
    use anyhow::Context;
    let mut written = Vec::new();
    let mut missing = Vec::new();
    let base = safe(&model.name);
    let xdir = dir.join("xmodel");
    std::fs::create_dir_all(&xdir).with_context(|| xdir.display().to_string())?;

    // images: the raw decoded picture for the loose files, and the same (normal maps converted) for the glTF
    let mut raw: HashMap<String, Vec<u8>> = HashMap::new();
    let mut embedded: HashMap<String, Vec<u8>> = HashMap::new();
    if opts.images {
        if let Some(cat) = catalog {
            let normals: Vec<String> = model.materials.iter().filter_map(|m| m.textures.normal.clone()).map(|n| n.trim_start_matches(',').to_string()).collect();
            for name in model.image_names() {
                match crate::modeltex::load_image(cat, &name) {
                    Ok(img) => {
                        let m = &img.mips[0];
                        let file = ExportModel::image_file(&name);
                        raw.insert(file.clone(), png_bytes(&m.rgba, m.width, m.height)?);
                        let e = if normals.contains(&name) { png_bytes(&gltf::normal_map_to_standard(&m.rgba), m.width, m.height)? } else { raw[&file].clone() };
                        embedded.insert(file, e);
                    }
                    Err(_) => missing.push(name),
                }
            }
        } else {
            missing.extend(model.image_names());
        }
        if !raw.is_empty() {
            let idir = dir.join("images");
            std::fs::create_dir_all(&idir)?;
            for (file, bytes) in &raw {
                let p = idir.join(file);
                std::fs::write(&p, bytes)?;
                written.push(p);
            }
        }
    }
    let mut put = |name: String, bytes: Vec<u8>| -> anyhow::Result<()> {
        let p = xdir.join(name);
        std::fs::write(&p, bytes).with_context(|| p.display().to_string())?;
        written.push(p);
        Ok(())
    };
    if opts.formats.glb {
        put(format!("{base}.glb"), gltf::write_glb(model, opts.scale, &embedded, anims))?;
    }
    if opts.formats.obj {
        let (obj, mtl) = obj::write(model, &format!("{base}.mtl"), opts.scale);
        put(format!("{base}.obj"), obj.into_bytes())?;
        put(format!("{base}.mtl"), mtl.into_bytes())?;
    }
    if opts.formats.xmodel_export {
        put(format!("{base}.XMODEL_EXPORT"), xmodel_export::write(model, opts.scale).into_bytes())?;
    }
    if opts.formats.semodel {
        put(format!("{base}.semodel"), semodel::from_export(model, 2.54 * opts.scale).write())?;
    }
    if opts.formats.cast {
        let converted: Vec<seanim::SeAnim> = anims.iter().map(|a| seanim::from_animation(a, Some(model), 2.54 * opts.scale)).collect();
        put(format!("{base}.cast"), cast::write(model, &converted, 2.54 * opts.scale))?;
    }
    drop(put);
    if opts.formats.seanim && !anims.is_empty() {
        let adir = dir.join("xanim");
        std::fs::create_dir_all(&adir)?;
        for a in anims {
            let p = adir.join(format!("{}.seanim", safe(&a.name)));
            std::fs::write(&p, seanim::from_animation(a, Some(model), 2.54 * opts.scale).write())?;
            written.push(p);
        }
    }
    Ok((written, missing))
}
