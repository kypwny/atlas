//! Black Ops II export: `atlas --t6export ZONE.ff|FOLDER OUT_DIR [--format cast|obj|sa] [--lod N] [--name FILTER] [--anims [FILTER]]
//! [--images] [--ipak DIR]`.
//!
//! Reads every `XModel` of the zone with the schema walker (`zone::t6_models`) and writes the chosen LOD (0 by default) through
//! the shared writers: `xmodel/<name>.cast` (a skeleton of `Bone`s, a `Mesh` per surface with skin weights, `Material`s whose
//! images are `File` nodes pointing at `../images/<image>.png`) or `xmodel/<name>.obj` + `.mtl`. `--anims` also writes the zone's
//! animations (`zone::t6_anims`) as `xanim/<name>.cast` (an `Animation` of `Curve`s, imported onto the armature of a model
//! file) and, with the `sa` format, `.seanim`. `--images` writes the colour, normal and specular image of every material
//! as PNG under `images/`, looked up by the image's hash in the IPAK archives next to the zone (`--ipak DIR` names another
//! folder). No window and no game folder is needed.

use super::{ExportBone, ExportMaterial, ExportModel, Formats, Options, Part};
use crate::ipak::{Ipak, IpakEntry};
use crate::modelindex::SurfaceTextures;
use crate::zone::t6_anims::read_anims_file;
use crate::zone::t6_models::{T6Model, read_models_file};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A file name for an image: the characters a file name cannot have become `_`.
pub fn image_name(name: &str) -> String {
    name.trim_start_matches(',').chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~') { c } else { '_' }).collect()
}

/// Builds the shared export model from one LOD of a model. Image names are made safe as file names.
pub fn to_export_model(m: &T6Model, lod: usize) -> ExportModel {
    to_export_model_with_images(m, lod).0
}

fn to_export_model_with_images(m: &T6Model, lod: usize) -> (ExportModel, Vec<(String, String, u32)>) {
    let mut out = ExportModel { name: m.name.clone(), ..Default::default() };
    // (safe file name, image name, hash) of every image the LOD's materials use
    let mut images: Vec<(String, String, u32)> = Vec::new();
    for b in &m.bones {
        out.bones.push(ExportBone { name: b.name.clone(), parent: b.parent, local: (b.local.0, b.local.1), global: (b.global.0, b.global.1) });
    }
    let Some(l) = m.lods.get(lod) else { return (out, images) };
    for (k, s) in m.lod_surfaces(lod).iter().enumerate() {
        if s.vertices.is_empty() || s.triangles.is_empty() {
            continue;
        }
        let abs = l.surf_index as usize + k;
        let mat = m.materials.get(abs).and_then(|x| x.as_ref());
        let name = mat.map(|x| x.name.trim_start_matches(',').to_string()).unwrap_or_else(|| format!("material_{abs}"));
        let slot = match out.materials.iter().position(|x| x.name == name) {
            Some(i) => i,
            None => {
                let safe = |n: &Option<String>| n.as_ref().map(|n| image_name(n));
                if let Some(x) = mat {
                    for n in [&x.color, &x.normal, &x.spec].into_iter().flatten() {
                        if let Some(&h) = x.image_hashes.get(n) {
                            images.push((image_name(n), n.clone(), h));
                        }
                    }
                }
                out.materials.push(ExportMaterial {
                    name,
                    textures: mat.map(|x| SurfaceTextures { color: safe(&x.color), normal: safe(&x.normal), spec: safe(&x.spec) }).unwrap_or_default(),
                    state: None,
                });
                out.materials.len() - 1
            }
        };
        let weights = s.skin_weights();
        let nverts = s.vertices.len() as u32;
        out.parts.push(Part {
            name: format!("{}_{k}", out.name),
            positions: s.vertices.iter().map(|v| v.pos).collect(),
            normals: s.vertices.iter().map(|v| v.normal).collect(),
            uvs: s.vertices.iter().map(|v| v.uv).collect(),
            colors: s.vertices.iter().map(|v| v.color).collect(),
            weights: weights.into_iter().map(|w| if w.is_empty() { vec![(0, 1.0)] } else { w }).collect(),
            faces: s.triangles.iter().map(|t| [t[0] as u32, t[1] as u32, t[2] as u32]).filter(|t| t.iter().all(|&i| i < nverts)).collect(),
            material: slot,
        });
    }
    (out, images)
}

/// The images of the IPAK archives in some folders, found by the image's hash (the key the archives are sorted by).
pub struct ImageStore {
    ipaks: Vec<Ipak>,
    index: HashMap<u32, Vec<(usize, IpakEntry)>>,
}

impl ImageStore {
    pub fn open(dirs: &[PathBuf]) -> ImageStore {
        let mut s = ImageStore { ipaks: Vec::new(), index: HashMap::new() };
        let mut seen = std::collections::HashSet::new();
        for d in dirs {
            let mut files: Vec<PathBuf> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ipak"))).collect();
            files.sort();
            for p in files {
                if !seen.insert(p.clone()) {
                    continue;
                }
                if let Ok(ip) = Ipak::open(&p) {
                    let i = s.ipaks.len();
                    for e in &ip.entries {
                        s.index.entry(e.name_hash).or_default().push((i, *e));
                    }
                    s.ipaks.push(ip);
                }
            }
        }
        s
    }

    pub fn archives(&self) -> usize {
        self.ipaks.len()
    }

    /// The largest version of the image with this hash as RGBA8 (width, height, pixels) and whether it is a BC5/DXN image.
    pub fn rgba(&self, hash: u32) -> Result<(u32, u32, Vec<u8>, bool)> {
        use crate::iwi;
        let versions = self.index.get(&hash).context("no archive holds this image")?;
        let mut best: Option<(u64, usize, IpakEntry, iwi::Header)> = None;
        for &(i, e) in versions {
            let Ok(head) = self.ipaks[i].read_prefix(&e, 128) else { continue };
            let Ok(h) = iwi::Header::parse(&head) else { continue };
            let area = h.width as u64 * h.height as u64;
            if best.as_ref().is_none_or(|b| area > b.0) {
                best = Some((area, i, e, h));
            }
        }
        let (_, i, e, h) = best.context("no readable version")?;
        let data = self.ipaks[i].read(&e)?;
        let level = iwi::decode_level(&data, &h, 0)?;
        let dxn = h.format() == Some(iwi::Format::Dxn);
        Ok((level.width, level.height, level.faces.into_iter().next().context("no face")?, dxn))
    }
}

fn png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image::write_buffer_with_format(&mut std::io::Cursor::new(&mut out), rgba, w, h, image::ColorType::Rgba8, image::ImageFormat::Png)?;
    Ok(out)
}

/// A BC5 normal map decodes to its X and Y in red and green; Z is rebuilt so it is a standard tangent-space map.
fn rebuild_z(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let (x, y) = (p[0] as f32 / 255.0 * 2.0 - 1.0, p[1] as f32 / 255.0 * 2.0 - 1.0);
        p[2] = (((1.0 - x * x - y * y).max(0.0).sqrt() * 0.5 + 0.5) * 255.0).round() as u8;
        p[3] = 255;
    }
}

fn zone_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut e: Vec<_> = std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path()).collect();
        e.sort();
        for p in e {
            zone_files(&p, out);
        }
    } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("ff")) {
        out.push(path.to_path_buf());
    }
}

/// What to export.
#[derive(Clone, Debug)]
pub struct T6Options {
    pub formats: Formats,
    pub lod: usize,
    /// Only models whose name contains this.
    pub filter: Option<String>,
    /// Export animations: `Some(filter)` (an empty filter takes all; a trailing `*` makes it a prefix, otherwise it is a substring).
    pub anims: Option<String>,
    pub images: bool,
    pub ipak_dir: Option<PathBuf>,
    /// Skip models (so only animations are written).
    pub no_models: bool,
    /// The unit and the extra factor of the written positions, and whether animations get a key on every frame (see `Options`).
    pub unit: super::Unit,
    pub scale: f32,
    pub bake: bool,
}

impl Default for T6Options {
    fn default() -> Self {
        T6Options { formats: Formats { glb: false, obj: false, xmodel_export: false, semodel: false, cast: true, seanim: false }, lod: 0, filter: None, anims: None, images: false, ipak_dir: None, no_models: false, unit: super::Unit::Default, scale: 1.0, bake: false }
    }
}

/// What an export run did.
#[derive(Debug, Default)]
pub struct Summary {
    pub zones: usize,
    pub models: usize,
    pub written: Vec<PathBuf>,
    pub skipped_without_geometry: usize,
    pub triangles: usize,
    pub problems: Vec<String>,
    pub anims: usize,
    pub anims_failed: usize,
    pub images: usize,
    pub images_missing: Vec<String>,
}

/// The hash of every image any zone in the folders defines, by name (a stub image in one zone, `,name`, only names an image
/// that another zone holds).
fn image_hashes_of_folders(dirs: &[PathBuf]) -> HashMap<String, u32> {
    use crate::zone::schema_walk::{Game, View, ZoneImage, walk_with};
    let mut out = HashMap::new();
    for d in dirs {
        let mut zones: Vec<PathBuf> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ff"))).collect();
        zones.sort();
        for z in zones {
            let Ok(bytes) = std::fs::read(&z) else { continue };
            let Ok(img) = ZoneImage::open(Game::T6, "", &bytes) else { continue };
            let mut hook = |v: &View, ty: &str, hdr: u32, _id: u32| {
                if ty != "GfxImage" {
                    return;
                }
                let (Some(n), Some(h)) = (v.member("GfxImage", "name"), v.member("GfxImage", "hash")) else { return };
                if let (Some(name), Some(hash)) = (v.ptr(hdr + n).and_then(|p| v.cstr(p)), v.u32(hdr + h)) {
                    if hash != 0 && !name.starts_with(',') {
                        out.insert(name, hash);
                    }
                }
            };
            walk_with(Game::T6, &img, Some(&mut hook));
        }
    }
    out
}

fn matches_filter(name: &str, f: &str) -> bool {
    match f.strip_suffix('*') {
        Some(p) => name.starts_with(p),
        None => name.contains(f),
    }
}

/// Exports the models of one or more zones (see [`export_zones_with`]).
pub fn export_zones(input: &Path, out_dir: &Path, formats: Formats, lod: usize, filter: Option<&str>) -> Result<Summary> {
    export_zones_with(input, out_dir, &T6Options { formats, lod, filter: filter.map(str::to_string), ..Default::default() })
}

/// Exports the models (and optionally animations and images) of one or more zones.
pub fn export_zones_with(input: &Path, out_dir: &Path, o: &T6Options) -> Result<Summary> {
    let mut files = Vec::new();
    zone_files(input, &mut files);
    if files.is_empty() {
        bail!("no .ff files at {}", input.display());
    }
    let opts = Options { formats: o.formats, images: false, scale: o.scale, unit: o.unit, bake: o.bake };
    let mut sum = Summary::default();
    let mut seen = std::collections::HashSet::new();
    let mut seen_images = std::collections::HashSet::new();
    let mut stub_hashes: Option<HashMap<String, u32>> = None;
    let store = o.images.then(|| {
        let mut dirs: Vec<PathBuf> = o.ipak_dir.iter().cloned().collect();
        if dirs.is_empty() {
            for f in &files {
                if let Some(d) = f.parent() {
                    if !dirs.contains(&d.to_path_buf()) {
                        dirs.push(d.to_path_buf());
                    }
                }
            }
        }
        ImageStore::open(&dirs)
    });
    if let Some(s) = &store {
        if s.archives() == 0 {
            sum.problems.push("--images: no .ipak files found next to the zones (use --ipak DIR)".into());
        }
    }
    for f in files.clone() {
        sum.zones += 1;
        if !o.no_models {
            let (models, report) = match read_models_file(&f) {
                Ok(r) => r,
                Err(e) => {
                    sum.problems.push(format!("{}: {e:#}", f.display()));
                    continue;
                }
            };
            if let Some(fail) = &report.failure {
                sum.problems.push(format!("{}: the walk stopped at asset {} ({})", report.zone, fail.asset_index, fail.reason));
            }
            for m in &models.models {
                if o.filter.as_deref().is_some_and(|p| !m.name.contains(p)) || !seen.insert(m.name.clone()) {
                    continue;
                }
                sum.models += 1;
                let (em, images) = to_export_model_with_images(m, o.lod);
                if em.parts.is_empty() {
                    sum.skipped_without_geometry += 1;
                    continue;
                }
                sum.triangles += em.parts.iter().map(|p| p.faces.len()).sum::<usize>();
                if let Some(store) = &store {
                    let dir = out_dir.join("images");
                    for (safe, original, mut hash) in images {
                        if hash == 0 {
                            // a stub: the image is defined in another zone of the folder
                            let table = stub_hashes.get_or_insert_with(|| {
                                let mut dirs: Vec<PathBuf> = o.ipak_dir.iter().cloned().collect();
                                dirs.extend(files.iter().filter_map(|f| f.parent().map(Path::to_path_buf)));
                                dirs.sort();
                                dirs.dedup();
                                image_hashes_of_folders(&dirs)
                            });
                            hash = table.get(original.trim_start_matches(',')).copied().unwrap_or(0);
                        }
                        if !seen_images.insert(safe.clone()) {
                            continue;
                        }
                        let is_normal = m.materials.iter().flatten().any(|x| x.normal.as_deref() == Some(original.as_str()));
                        match store.rgba(hash).and_then(|(w, h, mut px, dxn)| {
                            if is_normal && dxn {
                                rebuild_z(&mut px);
                            }
                            png(&px, w, h)
                        }) {
                            Ok(bytes) => {
                                std::fs::create_dir_all(&dir)?;
                                let p = dir.join(format!("{safe}.png"));
                                std::fs::write(&p, bytes).with_context(|| p.display().to_string())?;
                                sum.images += 1;
                                sum.written.push(p);
                            }
                            Err(e) => sum.images_missing.push(format!("{original} ({hash:08x}): {e:#}")),
                        }
                    }
                }
                let (written, _) = super::export(&em, out_dir, None, &opts, &[]).with_context(|| m.name.clone())?;
                sum.written.extend(written);
            }
        }
        if let Some(pattern) = &o.anims {
            let (anims, report) = match read_anims_file(&f) {
                Ok(r) => r,
                Err(e) => {
                    sum.problems.push(format!("{}: {e:#}", f.display()));
                    continue;
                }
            };
            if let Some(fail) = &report.failure {
                sum.problems.push(format!("{}: the walk stopped at asset {} ({})", report.zone, fail.asset_index, fail.reason));
            }
            let dir = out_dir.join("xanim");
            for a in &anims {
                // (a leading comma marks a stub that only names an animation another zone holds)
                if a.name.starts_with(',') || !pattern.is_empty() && !matches_filter(&a.name, pattern) || !seen.insert(format!("anim:{}", a.name)) {
                    continue;
                }
                let anim = match &a.anim {
                    Ok(x) => x,
                    Err(e) => {
                        sum.anims_failed += 1;
                        sum.problems.push(format!("animation {}: {e}", a.name));
                        continue;
                    }
                };
                std::fs::create_dir_all(&dir)?;
                let mut se = super::seanim::from_animation(anim, None, 2.54 * opts.scale_centimeters());
                if opts.bake {
                    super::seanim::bake(&mut se);
                }
                let base = super::safe(&a.name);
                if o.formats.cast {
                    // rotations are local to the parent bone (absolute), translations are offsets from the bind pose (checked by
                    // posing a character: absolute translations sink it 96 cm, offsets stand it on the ground); type 6 (the
                    // look-around poses) is additive
                    se.anim_type = if anim.asset_type == 6 { 1 } else { 4 };
                    let p = dir.join(format!("{base}.cast"));
                    std::fs::write(&p, super::cast::write_animation(&se))?;
                    sum.written.push(p);
                }
                if o.formats.seanim {
                    // SEAnim has no per-channel mode, and the translations here are offsets from the model's bind pose
                    se.anim_type = 2;
                    let p = dir.join(format!("{base}.seanim"));
                    std::fs::write(&p, se.write())?;
                    sum.written.push(p);
                }
                sum.anims += 1;
            }
        }
    }
    Ok(sum)
}

/// `atlas --t6export ZONE.ff|FOLDER OUT_DIR [--format cast|obj|sa] [--lod N] [--name FILTER] [--anims [FILTER]] [--images] [--ipak DIR] [--no-models]`
pub fn command(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: atlas --t6export ZONE.ff|FOLDER OUT_DIR [--format cast|obj|sa] [--lod N] [--name FILTER] [--anims [FILTER]] [--images] [--ipak DIR] [--no-models] [--unit inches|cm|m] [--scale S] [--bake]";
    let input = PathBuf::from(args.first().context(USAGE)?);
    let out = PathBuf::from(args.get(1).context("output folder missing")?);
    let mut o = T6Options::default();
    let mut it = args[2..].iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--format" => {
                o.formats = Formats { glb: false, obj: false, xmodel_export: false, semodel: false, cast: false, seanim: false };
                for f in it.next().context("--format needs a list")?.split(',') {
                    match f.trim() {
                        "cast" => o.formats.cast = true,
                        "obj" => o.formats.obj = true,
                        "sa" | "seanim" => o.formats.seanim = true,
                        other => bail!("unknown format {other} (cast, obj, sa)"),
                    }
                }
            }
            "--lod" => o.lod = it.next().context("--lod needs a number")?.parse()?,
            "--name" => o.filter = it.next().cloned(),
            "--anims" => o.anims = Some(it.next_if(|n| !n.starts_with("--")).cloned().unwrap_or_default()),
            "--images" => o.images = true,
            "--ipak" => o.ipak_dir = it.next().map(PathBuf::from),
            "--no-models" => o.no_models = true,
            "--scale" => o.scale = it.next().context("--scale needs a number")?.parse()?,
            "--unit" => o.unit = super::Unit::from_code(it.next().context("--unit needs inches, cm or m")?).context("unknown unit (inches, cm, m)")?,
            "--bake" => o.bake = true,
            other => bail!("unknown option {other}"),
        }
    }
    let t = std::time::Instant::now();
    let s = export_zones_with(&input, &out, &o)?;
    for p in s.problems.iter().take(20) {
        eprintln!("{p}");
    }
    for p in s.images_missing.iter().take(10) {
        eprintln!("image not exported: {p}");
    }
    println!(
        "{} zones, {} models ({} without geometry), {} triangles, {} animations ({} failed), {} images ({} missing), {} files -> {} [{:.1}s]",
        s.zones,
        s.models,
        s.skipped_without_geometry,
        s.triangles,
        s.anims,
        s.anims_failed,
        s.images,
        s.images_missing.len(),
        s.written.len(),
        out.display(),
        t.elapsed().as_secs_f32()
    );
    Ok(())
}
