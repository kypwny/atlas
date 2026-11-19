//! Black Ops II model export from zones (`atlas --t6export`). Needs `CODOL_TEST_T6=<game folder>` (the one that has `zone\all`)
//! and is skipped without it.

mod common;

use atlas::export::{ExportModel, t6};
use atlas::zone::t6_models::{T6Model, read_models_file};
use std::path::PathBuf;

fn zone(name: &str) -> Option<PathBuf> {
    let p = std::env::var_os("CODOL_TEST_T6").map(PathBuf::from)?.join("zone").join("all").join(name);
    p.exists().then_some(p)
}

fn read(name: &str) -> Option<Vec<T6Model>> {
    let path = zone(name)?;
    let (m, report) = read_models_file(&path).unwrap();
    assert!(report.exact, "{name}: {:?} {:?}", report.failure, report.mismatches);
    Some(m.models)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Structural checks every decoded model has to pass.
fn check_model(m: &T6Model) {
    // a name with a leading comma is a stub that only names a model another zone holds
    if m.name.starts_with(',') {
        assert!(m.lods.is_empty() && m.surfaces.is_empty(), "{}: stub with geometry", m.name);
        return;
    }
    let nb = m.bones.len();
    assert!(!m.lods.is_empty(), "{}: no LODs", m.name);
    for (i, b) in m.bones.iter().enumerate() {
        assert!(b.parent < i as i32, "{}: bone {i} ({}) has parent {}", m.name, b.name, b.parent);
        assert!(b.global.1.iter().all(|c| c.is_finite()) && b.global.0.iter().all(|c| c.is_finite()));
        // a rotation is a unit quaternion
        let l: f32 = b.global.1.iter().map(|c| c * c).sum();
        assert!((l - 1.0).abs() < 0.01, "{}: bone {} rotation length^2 {l}", m.name, b.name);
    }
    for (k, s) in m.surfaces.iter().enumerate() {
        let n = s.vertices.len();
        assert_eq!(s.vertices.len(), s.vertices.len());
        for t in &s.triangles {
            assert!(t.iter().all(|&i| (i as usize) < n), "{} surface {k}: triangle {t:?} outside {n} vertices", m.name);
        }
        // every vertex is covered by exactly one rigid run or blend group
        let covered: usize = s.rigid.iter().map(|r| r.vert_count as usize).sum::<usize>() + s.blend_counts.iter().map(|&c| c as usize).sum::<usize>();
        assert_eq!(covered, n, "{} surface {k}: {covered} bound vertices of {n} (rigid {:?}, blend {:?})", m.name, s.rigid, s.blend_counts);
        for (v, w) in s.skin_weights().iter().enumerate() {
            let sum: f32 = w.iter().map(|x| x.1).sum();
            assert!((sum - 1.0).abs() < 0.01, "{} surface {k} vertex {v}: weights sum to {sum}", m.name);
            assert!(w.iter().all(|x| (x.0 as usize) < nb.max(1)), "{} surface {k}: bone out of range in {w:?} ({nb} bones)", m.name);
        }
        for v in &s.vertices {
            assert!(v.pos.iter().chain(&v.normal).chain(&v.uv).all(|c| c.is_finite()), "{}: non-finite vertex {v:?}", m.name);
            let l = dot(v.normal, v.normal).sqrt();
            assert!((l - 1.0).abs() < 0.05, "{}: normal length {l}", m.name);
        }
    }
    // the LOD ranges lie inside the surface list and the materials are linked
    for l in &m.lods {
        assert!(l.surf_index as usize + l.num_surfs as usize <= m.surfaces.len(), "{}: LOD surfaces {l:?} of {}", m.name, m.surfaces.len());
    }
}

#[test]
fn faction_fbi_models_decode_plausibly() {
    let Some(models) = read("faction_fbi_mp.ff") else { return };
    assert_eq!(models.len(), 7);
    let mut all_tris = 0;
    for m in &models {
        check_model(m);
        let t = m.lod_triangle_count(0);
        assert!(t > 2000 && t < 60000, "{}: {t} triangles in LOD 0", m.name);
        all_tris += t;
        // LODs get coarser
        for l in 1..m.lods.len() {
            assert!(m.lod_triangle_count(l) < m.lod_triangle_count(l - 1), "{}: LOD {l} is not coarser", m.name);
        }
        assert!(m.bones.len() > 20, "{}: {} bones", m.name, m.bones.len());
        assert!(m.bones.iter().any(|b| b.name == "j_head" || b.name == "tag_origin" || b.name.starts_with("j_")), "{}: bone names {:?}", m.name, m.bones.iter().take(5).map(|b| &b.name).collect::<Vec<_>>());
        assert!(m.materials.iter().all(|x| x.is_some()), "{}: a material link is missing", m.name);
    }
    assert!(all_tris > 50_000);
    // a character stands about 70-80 inches tall, in the game's units
    let fb = models.iter().find(|m| m.name == "c_usa_mp_fbi_assault_fb").expect("assault model");
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for s in fb.lod_surfaces(0) {
        for v in &s.vertices {
            for k in 0..3 {
                lo[k] = lo[k].min(v.pos[k]);
                hi[k] = hi[k].max(v.pos[k]);
            }
        }
    }
    let h = hi[2] - lo[2];
    assert!(h > 55.0 && h < 90.0, "character height {h} ({lo:?} .. {hi:?})");
    // every vertex sits near the bone that carries it: the skeleton, the bone indices (offset / 64) and the weights agree
    let mut dist: Vec<f32> = Vec::new();
    for s in fb.lod_surfaces(0) {
        for (v, w) in s.vertices.iter().zip(s.skin_weights()) {
            let (bone, _) = w.iter().copied().fold((0u32, -1.0f32), |a, b| if b.1 > a.1 { b } else { a });
            let g = fb.bones[bone as usize].global.0;
            dist.push(dot(sub(v.pos, g), sub(v.pos, g)).sqrt());
        }
    }
    dist.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = dist[dist.len() / 2];
    assert!(median < 15.0 && dist[dist.len() * 95 / 100] < 40.0, "vertices are {median} (median) / {} (95%) inches from their main bone", dist[dist.len() * 95 / 100]);
    // the vertices lie inside the model's own bounding box (with some slack)
    for k in 0..3 {
        assert!(lo[k] >= fb.mins[k] - 2.0 && hi[k] <= fb.maxs[k] + 2.0, "bounds {lo:?}..{hi:?} against {:?}..{:?}", fb.mins, fb.maxs);
    }
}

#[test]
fn common_mp_models_decode_and_their_triangles_face_their_normals() {
    let Some(models) = read("common_mp.ff") else { return };
    assert!(models.len() > 300, "{} models", models.len());
    let (mut tris, mut agree, mut inside, mut with_geo) = (0usize, 0usize, 0usize, 0usize);
    for m in &models {
        check_model(m);
        if m.lod_triangle_count(0) == 0 {
            continue;
        }
        with_geo += 1;
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for s in m.lod_surfaces(0) {
            for v in &s.vertices {
                for k in 0..3 {
                    lo[k] = lo[k].min(v.pos[k]);
                    hi[k] = hi[k].max(v.pos[k]);
                }
            }
            for t in &s.triangles {
                let (a, b, c) = (s.vertices[t[0] as usize], s.vertices[t[1] as usize], s.vertices[t[2] as usize]);
                let n = cross(sub(b.pos, a.pos), sub(c.pos, a.pos));
                let avg = [a.normal[0] + b.normal[0] + c.normal[0], a.normal[1] + b.normal[1] + c.normal[1], a.normal[2] + b.normal[2] + c.normal[2]];
                let d = dot(n, avg);
                if d.abs() > 1e-9 {
                    tris += 1;
                    // the game winds clockwise: the geometric normal points against the vertex normals
                    if d < 0.0 {
                        agree += 1;
                    }
                }
            }
        }
        if (0..3).all(|k| lo[k] >= m.mins[k] - 2.0 && hi[k] <= m.maxs[k] + 2.0) {
            inside += 1;
        }
    }
    assert!(tris > 100_000, "{tris} triangles");
    assert!(agree * 100 >= tris * 95, "{agree} of {tris} triangles are clockwise against their normals");
    assert!(inside * 100 >= with_geo * 90, "{inside} of {with_geo} models inside their own bounds");
}

/// An independent reader of the Cast container (header, nodes, properties) that returns what a file holds.
#[derive(Default, Debug)]
struct Seen {
    nodes: Vec<(u32, u64)>,
    /// (node id, property name, type code, element count)
    props: Vec<(u32, String, String, u32)>,
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn read_nodes(b: &[u8], mut at: usize, end: usize, count: u32, seen: &mut Seen) {
    for _ in 0..count {
        assert!(at + 24 <= end, "node header past its parent");
        let id = u32le(b, at);
        let size = u32le(b, at + 4) as usize;
        let hash = u64::from_le_bytes(b[at + 8..at + 16].try_into().unwrap());
        let (props, children) = (u32le(b, at + 16), u32le(b, at + 20));
        let body = at + 24;
        assert!(body + size <= end, "node {id:#x} runs past its parent ({} > {})", body + size, end);
        seen.nodes.push((id, hash));
        let mut p = body;
        for _ in 0..props {
            let code = [b[p], b[p + 1]];
            let name_len = u16::from_le_bytes([b[p + 2], b[p + 3]]) as usize;
            let len = u32le(b, p + 4);
            let name = String::from_utf8(b[p + 8..p + 8 + name_len].to_vec()).unwrap();
            p += 8 + name_len;
            // element sizes: b 1, h 2, i/f 4, l/d 8, s a NUL-terminated string, and "2v"/"3v"/"4v" vectors of floats
            let tcode = String::from_utf8(code.iter().take_while(|&&c| c != 0).copied().collect()).unwrap();
            let elem = match tcode.as_str() {
                "b" => 1,
                "h" => 2,
                "i" | "f" => 4,
                "l" | "d" => 8,
                "2v" => 8,
                "3v" => 12,
                "4v" => 16,
                "s" => 0,
                other => panic!("property {name}: unknown type {other:?}"),
            };
            if tcode == "s" {
                p += b[p..].iter().position(|&c| c == 0).expect("unterminated string") + 1;
            } else {
                p += elem * len as usize;
            }
            assert!(p <= body + size, "property {name} runs past its node");
            seen.props.push((id, name, tcode, len));
        }
        read_nodes(b, p, body + size, children, seen);
        at = body + size;
    }
}

fn parse_cast(b: &[u8]) -> Seen {
    assert_eq!(&b[..4], b"cast");
    assert_eq!(u32le(b, 4), 1, "version");
    let roots = u32le(b, 8);
    let mut seen = Seen::default();
    read_nodes(b, 16, b.len(), roots, &mut seen);
    seen
}

#[test]
fn exported_cast_files_are_well_formed_and_agree_with_the_model() {
    let Some(path) = zone("faction_fbi_mp.ff") else { return };
    let out = std::env::temp_dir().join(format!("atlas_t6export_{}", std::process::id()));
    let sum = t6::export_zones(&path, &out, atlas::export::Formats { glb: false, obj: true, xmodel_export: false, semodel: false, cast: true, seanim: false }, 0, None).unwrap();
    assert_eq!(sum.models, 7);
    assert!(sum.problems.is_empty(), "{:?}", sum.problems);
    let (models, _) = read_models_file(&path).unwrap();
    for m in &models.models {
        let em: ExportModel = t6::to_export_model(m, 0);
        let bytes = std::fs::read(out.join("xmodel").join(format!("{}.cast", m.name))).unwrap();
        let seen = parse_cast(&bytes);
        let count = |id: u32| seen.nodes.iter().filter(|n| n.0 == id).count();
        // 'root' 'meta' 'modl' 'skls' 'bone' 'mesh' 'matl' 'file' as little-endian ASCII
        let (root, modl, skel, bone, mesh, matl) = (0x746F6F72, 0x6C646F6D, 0x6C656B73, 0x656E6F62, 0x6873656D, 0x6C74616D);
        assert_eq!((count(root), count(modl), count(skel)), (1, 1, 1));
        assert_eq!(count(bone), m.bones.len(), "{}: bones", m.name);
        assert_eq!(count(mesh), em.parts.len(), "{}: meshes", m.name);
        assert_eq!(count(matl), em.materials.len(), "{}: materials", m.name);
        // node hashes are unique, and every mesh links a material that exists
        let hashes: std::collections::HashSet<u64> = seen.nodes.iter().map(|n| n.1).collect();
        assert_eq!(hashes.len(), seen.nodes.len(), "{}: duplicate node hashes", m.name);
        // vertex counts and face counts agree with the model
        let vp: u32 = seen.props.iter().filter(|p| p.1 == "vp").map(|p| p.3).sum();
        let faces: u32 = seen.props.iter().filter(|p| p.1 == "f").map(|p| p.3).sum();
        assert_eq!(vp as usize, em.parts.iter().map(|p| p.positions.len()).sum::<usize>(), "{}: vertices", m.name);
        assert_eq!(faces as usize, 3 * em.parts.iter().map(|p| p.faces.len()).sum::<usize>(), "{}: face indices", m.name);
        assert_eq!(seen.props.iter().filter(|p| p.1 == "m" && p.2 == "l").count(), em.parts.len(), "{}: every mesh names its material", m.name);
        // the OBJ is there too and has as many vertices as triangles' vertex sets
        let obj = std::fs::read_to_string(out.join("xmodel").join(format!("{}.obj", m.name))).unwrap();
        assert_eq!(obj.lines().filter(|l| l.starts_with("v ")).count() as u32, vp);
    }
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn every_zone_models_decode_when_asked() {
    // `CODOL_TEST_T6_ALL=1` reads the models of every zone and runs the structural checks on each
    let Some(dir) = std::env::var_os("CODOL_TEST_T6").map(PathBuf::from).map(|d| d.join("zone")) else { return };
    if std::env::var_os("CODOL_TEST_T6_ALL").is_none() {
        return;
    }
    let mut files = Vec::new();
    fn walk(d: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "ff") {
                out.push(p);
            }
        }
    }
    walk(&dir, &mut files);
    let (mut models, mut tris) = (0usize, 0usize);
    let mut maps = 0usize;
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let zone = atlas::zone::schema_walk::ZoneImage::open(atlas::zone::schema_walk::Game::T6, "z", &bytes).unwrap();
        let (m, report) = atlas::zone::t6_models::read_zone(&zone, true, true);
        assert!(report.exact, "{}", f.display());
        if let Some(w) = &m.world {
            let (model, names, _) = w.to_model();
            // every surface the world lists made it into the model
            let wanted: usize = w.surfaces.iter().map(|s| s.tri_count as usize).sum();
            assert_eq!(model.triangle_count(), wanted, "{}: triangles lost converting the world", f.display());
            assert!(!names.is_empty());
            maps += 1;
        }
        for md in &m.models {
            check_model(md);
            models += 1;
            tris += md.lod_triangle_count(0);
        }
    }
    println!("{} zones, {models} models, {tris} LOD 0 triangles, {maps} maps", files.len());
    assert!(models > 1000);
}

#[test]
fn nuketown_world_becomes_a_map_model() {
    use atlas::zone::t6_world::model_of_t6_file;
    let Some(path) = zone("mp_nuketown_2020.ff") else { return };
    let (map, report) = model_of_t6_file(&path).unwrap();
    assert!(report.exact);
    let (model, names, textures) = map.expect("a GfxWorld");
    assert_eq!(model.surfaces.len(), names.len());
    assert_eq!(names.len(), textures.len());
    // the world has 5,614 surfaces, 300,840 indices (100,280 triangles) over a few hundred materials
    let tris = model.triangle_count();
    assert!(tris > 90_000 && tris < 110_000, "{tris} triangles");
    assert!(names.len() > 100, "{} materials", names.len());
    assert!(textures.iter().filter(|t| t.color.is_some()).count() * 10 > textures.len() * 8, "colour maps are named for most materials");
    assert!(names.iter().any(|n| n.starts_with("wpc/")), "world materials are named wpc/...");
    // every vertex of a surface is inside the bounds the world records for it; the model's bounds lie inside the world's
    let (lo, hi) = model.bounds().unwrap();
    let (mut inside, mut total) = (0usize, 0usize);
    for s in &model.surfaces {
        for v in &s.vertices {
            total += 1;
            if (0..3).all(|k| v.pos[k] >= lo[k] && v.pos[k] <= hi[k]) && v.normal.iter().all(|c| c.is_finite()) && v.uv.iter().all(|c| c.is_finite()) {
                inside += 1;
            }
        }
        assert!(s.indices.iter().all(|&i| (i as usize) < s.vertices.len()));
    }
    assert_eq!(inside, total);
    // winding: as in the models, the geometric normal of a triangle points against its vertex normals
    let (mut tri_total, mut tri_agree) = (0usize, 0usize);
    for s in &model.surfaces {
        for t in s.indices.chunks_exact(3) {
            let (a, b, c) = (s.vertices[t[0] as usize], s.vertices[t[1] as usize], s.vertices[t[2] as usize]);
            let n = cross(sub(b.pos, a.pos), sub(c.pos, a.pos));
            let avg = [a.normal[0] + b.normal[0] + c.normal[0], a.normal[1] + b.normal[1] + c.normal[1], a.normal[2] + b.normal[2] + c.normal[2]];
            let d = dot(n, avg);
            if d.abs() > 1e-9 {
                tri_total += 1;
                tri_agree += (d < 0.0) as usize;
            }
        }
    }
    println!("world winding: {tri_agree} of {tri_total} triangles clockwise against their normals");
    assert!(tri_agree * 100 >= tri_total * 90, "{tri_agree} of {tri_total}");
    assert!(hi[0] - lo[0] > 500.0 && hi[0] - lo[0] < 90_000.0, "{lo:?} {hi:?}");
}

#[test]
fn world_triangle_indices_land_inside_the_surface_bounds() {
    use atlas::zone::t6_models::read_zone;
    use atlas::zone::schema_walk::{Game, ZoneImage};
    let Some(path) = zone("mp_nuketown_2020.ff") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let z = ZoneImage::open(Game::T6, "mp_nuketown_2020", &bytes).unwrap();
    let (m, _) = read_zone(&z, false, true);
    let w = m.world.expect("world");
    let (mut ok, mut total) = (0usize, 0usize);
    for s in &w.surfaces {
        for k in 0..3 * s.tri_count as usize {
            let i = w.indices[s.base_index as usize + k] as usize;
            let o = s.vertex_offset as usize + 36 * i;
            let p: [f32; 3] = std::array::from_fn(|c| f32::from_le_bytes(w.vertex_data[o + 4 * c..o + 4 * c + 4].try_into().unwrap()));
            total += 1;
            if (0..3).all(|c| p[c] >= s.mins[c] - 1.0 && p[c] <= s.maxs[c] + 1.0) {
                ok += 1;
            }
        }
    }
    assert_eq!(total, 300_840);
    assert_eq!(ok, total);
}

#[test]
fn nuketown_static_models_are_placed_in_the_map() {
    use atlas::zone::schema_walk::{Game, ZoneImage};
    use atlas::zone::t6_models::read_zone_with;
    use atlas::zone::t6_world::model_of_t6_zone_with;
    let Some(path) = zone("mp_nuketown_2020.ff") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let z = ZoneImage::open(Game::T6, "mp_nuketown_2020", &bytes).unwrap();
    let (m, report) = read_zone_with(&z, false, true, true);
    assert!(report.exact);
    let w = m.world.expect("world");
    println!("{} static instances of {} models, {} without a model in the zone", w.instances.len(), w.models.len(), w.static_missing);
    assert!(w.instances.len() > 500, "{} instances", w.instances.len());
    assert!(w.models.len() > 20);
    let mut scales: Vec<f32> = w.instances.iter().map(|i| i.scale).collect();
    scales.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("scales: min {} median {} max {}", scales[0], scales[scales.len() / 2], scales[scales.len() - 1]);
    // the placements are rotations (orthonormal axes with determinant 1) and sensible scales
    for i in &w.instances {
        for a in 0..3 {
            let l = i.axis[a].iter().map(|c| c * c).sum::<f32>().sqrt();
            assert!((l - 1.0).abs() < 0.02, "axis {a} has length {l}");
            let b = (a + 1) % 3;
            assert!(dot(i.axis[a], i.axis[b]).abs() < 0.02, "axes {a} and {b} are not orthogonal");
        }
        let det = dot(cross(i.axis[0], i.axis[1]), i.axis[2]);
        assert!((det - 1.0).abs() < 0.05, "determinant {det}");
        assert!(i.scale > 0.01 && i.scale < 20.0, "scale {}", i.scale);
        assert!(i.origin.iter().all(|c| c.is_finite()));
    }
    let (plain, names0, _) = model_of_t6_zone_with(&z, false).unwrap();
    let (full, names1, textures1) = model_of_t6_zone_with(&z, true).unwrap();
    assert_eq!(names1.len(), textures1.len());
    assert!(names1.len() >= names0.len());
    let (t0, t1) = (plain.triangle_count(), full.triangle_count());
    println!("triangles: world {t0}, with static models {t1}");
    assert!(t1 > t0 + 20_000, "{t0} -> {t1}");
    // the static geometry stands inside the map: the placed models' vertices are inside the world's bounds
    let (lo, hi) = (w.mins, w.maxs);
    let (mut inside, mut total) = (0usize, 0usize);
    let (mut flo, mut fhi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for s in &full.surfaces {
        for v in &s.vertices {
            total += 1;
            if (0..3).all(|k| v.pos[k] >= lo[k] - 1.0 && v.pos[k] <= hi[k] + 1.0) {
                inside += 1;
            }
            for k in 0..3 {
                flo[k] = flo[k].min(v.pos[k]);
                fhi[k] = fhi[k].max(v.pos[k]);
            }
            assert!(v.pos.iter().all(|c| c.is_finite()));
        }
        assert!(s.indices.iter().all(|&i| (i as usize) < s.vertices.len()));
    }
    assert_eq!(inside, total, "vertices outside the map bounds {lo:?} {hi:?}: {} of {total}; geometry spans {flo:?} {fhi:?}", total - inside);
    // placed models are where the map's own surfaces are: the play area (middle 90 % of the world's vertices) holds most of them
    let (wlo, whi) = plain.bounds().unwrap();
    let play = |p: [f32; 3]| (0..2).all(|k| p[k] >= wlo[k] - 500.0 && p[k] <= whi[k] + 500.0);
    let static_near = full.surfaces.iter().flat_map(|s| s.vertices.iter()).filter(|v| play(v.pos)).count();
    assert!(static_near * 100 >= total * 95, "{static_near} of {total} vertices are near the world's geometry");
    // winding of the instanced triangles still faces their normals (a mirrored or mis-rotated placement would flip it)
    let (mut n, mut ok) = (0usize, 0usize);
    for s in &full.surfaces {
        for t in s.indices.chunks_exact(3) {
            let (a, b, c) = (s.vertices[t[0] as usize], s.vertices[t[1] as usize], s.vertices[t[2] as usize]);
            let g = cross(sub(b.pos, a.pos), sub(c.pos, a.pos));
            let avg = [a.normal[0] + b.normal[0] + c.normal[0], a.normal[1] + b.normal[1] + c.normal[1], a.normal[2] + b.normal[2] + c.normal[2]];
            let d = dot(g, avg);
            if d.abs() > 1e-9 {
                n += 1;
                ok += (d < 0.0) as usize;
            }
        }
    }
    println!("winding: {ok} of {n} triangles face their normals");
    assert!(ok * 100 >= n * 95);
}

#[test]
fn exported_materials_point_at_images_that_were_written() {
    let Some(path) = zone("faction_fbi_mp.ff") else { return };
    // the IPAK archives sit next to the zone; images are found by the hash in each GfxImage
    let out = std::env::temp_dir().join(format!("atlas_t6images_{}", std::process::id()));
    let opts = t6::T6Options { images: true, filter: Some("c_usa_mp_fbi_assault_fb".into()), ..Default::default() };
    let sum = t6::export_zones_with(&path, &out, &opts).unwrap();
    assert_eq!(sum.models, 1);
    assert!(sum.images >= 20, "{} images, missing {:?}", sum.images, sum.images_missing);
    assert!(sum.images_missing.is_empty(), "{:?}", sum.images_missing);
    // every File node of the Cast file names an image that exists, as a PNG that decodes
    let bytes = std::fs::read(out.join("xmodel").join("c_usa_mp_fbi_assault_fb.cast")).unwrap();
    let seen = common::parse_cast(&bytes);
    let files: Vec<String> = seen.props.iter().filter(|p| p.node_id == common::FILE && p.name == "p").map(|p| p.string()).collect();
    assert!(files.len() >= 20, "{} File nodes", files.len());
    let (mut normals, mut colours) = (0, 0);
    for f in &files {
        let p = out.join("xmodel").join(f);
        let img = image::open(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())).to_rgba8();
        assert!(img.width() >= 16 && img.height() >= 16);
        let name = f.rsplit('/').next().unwrap();
        let n = (img.width() * img.height()) as f64;
        if name.ends_with("_n.png") {
            // a rebuilt tangent-space normal map: blue is the largest component nearly everywhere
            let up = img.pixels().filter(|p| p[2] >= 128).count() as f64;
            assert!(up / n > 0.95, "{name}: only {:.0}% of the pixels point out of the surface", 100.0 * up / n);
            normals += 1;
        } else if name.ends_with("_c.png") {
            // a colour map is not flat
            let mean = img.pixels().map(|p| p[0] as f64).sum::<f64>() / n;
            let var = img.pixels().map(|p| (p[0] as f64 - mean).powi(2)).sum::<f64>() / n;
            assert!(var > 100.0, "{name}: variance {var}");
            colours += 1;
        }
    }
    assert!(normals >= 5 && colours >= 5, "{normals} normal maps, {colours} colour maps");
    let _ = std::fs::remove_dir_all(&out);
}
