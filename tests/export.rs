//! Export checks against real files. `CODOL_TEST_SEMODELS` = a folder of SEModel files (the viewmodel dumps).

use atlas::export::semodel::SeModel;
use std::path::PathBuf;

#[test]
fn every_reference_semodel_reads_and_writes_back_identically() {
    let Some(dir) = std::env::var_os("CODOL_TEST_SEMODELS").map(PathBuf::from) else { return };
    let (mut n, mut diff) = (0, Vec::new());
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "semodel") {
            continue;
        }
        let bytes = std::fs::read(&p).unwrap();
        let m = SeModel::read(&bytes).unwrap_or_else(|err| panic!("{}: {err:#}", p.display()));
        if m.write() != bytes && diff.len() < 5 {
            diff.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
        n += 1;
    }
    assert!(n > 100, "only {n} files");
    assert!(diff.is_empty(), "round trip differs for {diff:?}");
}

/// Our parse of every viewmodel agrees with the reference SEModel of the same model: same meshes, vertices (the
/// reference is in centimetres, ours in inches), UVs, normals, triangles and skin weights.
#[test]
fn streamed_models_match_the_reference_semodels() {
    use atlas::export::ExportModel;
    use atlas::model::Model;
    use atlas::modelindex::ModelIndex;
    let (Some(dir), Some(game), Some(index)) = (
        std::env::var_os("CODOL_TEST_SEMODELS").map(PathBuf::from),
        std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from),
        std::env::var_os("CODOL_TEST_INDEX").map(PathBuf::from),
    ) else {
        return;
    };
    let index = ModelIndex::load(&index).unwrap();
    let (mut compared, mut bad, mut total_bad) = (0usize, Vec::<String>::new(), 0usize);
    let close = |a: f32, b: f32, tol: f32| (a - b).abs() <= tol * (1.0 + b.abs());
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "semodel") {
            continue;
        }
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let Some(m) = index.models.iter().find(|m| m.name == name) else { continue };
        let Some(file) = m.lods.first().and_then(|l| l.file.clone()) else { continue };
        let file = file.trim_start_matches(',').to_string();
        let Ok(bytes) = std::fs::read(game.join("main").join("models").join(&file)) else { continue };
        let ours = ExportModel::from_streamed(&Model::parse(&bytes).unwrap(), Some(&index), &file);
        let theirs = SeModel::read(&std::fs::read(&p).unwrap()).unwrap();
        let mut problem = None;
        if ours.parts.len() != theirs.meshes.len() {
            problem = Some(format!("{} parts vs {} meshes", ours.parts.len(), theirs.meshes.len()));
        } else {
            for (i, (a, b)) in ours.parts.iter().zip(&theirs.meshes).enumerate() {
                let n = a.positions.len();
                if n != b.positions.len() || a.faces != b.faces {
                    problem = Some(format!("mesh {i}: geometry size/faces differ"));
                    break;
                }
                let bad_pos = (0..n).any(|v| (0..3).any(|k| !close(a.positions[v][k] * 2.54, b.positions[v][k], 1e-4)));
                let bad_uv = (0..n).any(|v| (0..2).any(|k| !close(a.uvs[v][k], b.uvs[v][k], 1e-4)));
                let bad_n = (0..n).any(|v| (0..3).any(|k| !close(a.normals[v][k], b.normals[v][k], 1e-3)));
                let skin = b.max_skin as usize;
                let bad_w = (0..n).any(|v| {
                    let mut ours_w = a.weights[v].clone();
                    ours_w.sort_by(|x, y| x.0.cmp(&y.0));
                    let mut theirs_w: Vec<(u32, f32)> = b.weights[v * skin..(v + 1) * skin].iter().copied().filter(|w| w.1 > 0.0).collect();
                    theirs_w.sort_by(|x, y| x.0.cmp(&y.0));
                    ours_w.retain(|w| w.1 > 0.0);
                    ours_w.len() != theirs_w.len() || ours_w.iter().zip(&theirs_w).any(|(x, y)| x.0 != y.0 || !close(x.1, y.1, 2e-3))
                });
                if bad_pos && std::env::var_os("EXPORT_DEBUG").is_some() && total_bad < 3 {
                    println!("{name} mesh {i}: ours bones {:?}", ours.bones.iter().map(|b| (&b.name, b.parent, b.global)).collect::<Vec<_>>());
                    println!("  theirs bones {:?}", theirs.bones.iter().map(|b| (&b.name, b.parent, b.global, b.local)).collect::<Vec<_>>());
                    for v in [0usize, n / 2, n - 1] {
                        println!("  v{v}: ours {:?} w {:?} theirs {:?} w {:?}", a.positions[v], a.weights[v], b.positions[v], &b.weights[v * b.max_skin as usize..(v + 1) * b.max_skin as usize]);
                    }
                }
                if bad_pos || bad_uv || bad_n || bad_w {
                    problem = Some(format!("mesh {i}: positions {bad_pos} uv {bad_uv} normals {bad_n} weights {bad_w}"));
                    break;
                }
            }
        }
        compared += 1;
        if let Some(p) = problem {
            total_bad += 1;
            if bad.len() < 12 {
                bad.push(format!("{name}: {p}"));
            }
        }
    }
    assert!(compared > 500, "only {compared} models compared");
    // The reference dumps come from another CoDOL build than the QA zones, and same-named weapon models differ
    // between builds (different vertex counts, UVs, whole meshes), so most, not all, must agree exactly.
    println!("{compared} models compared, {total_bad} differ");
    assert!(total_bad * 100 <= compared * 20, "{total_bad} of {compared} differ, first: {bad:#?}");
}

/// Skeletons recorded from the zones agree with the reference SEModel's bones: names, parents and bind pose.
/// A weapon model name can be defined in several zones with different data (the index keeps the first), so a
/// reference counts as matched when the skeleton of the same name in *any* zone equals it.
#[test]
fn recorded_skeletons_match_the_reference_bones() {
    use atlas::modelindex::{ModelIndex, Skeleton};
    let (Some(dir), Some(index)) = (std::env::var_os("CODOL_TEST_SEMODELS").map(PathBuf::from), std::env::var_os("CODOL_TEST_INDEX").map(PathBuf::from)) else {
        return;
    };
    let text = String::from_utf8_lossy(&std::fs::read(index).unwrap()).into_owned();
    // one index per zone, so every zone's own definition of a name is visible
    let mut per_zone: Vec<ModelIndex> = Vec::new();
    let mut chunk = String::new();
    for line in text.lines() {
        if line.starts_with("zone	") && !chunk.is_empty() {
            per_zone.push(ModelIndex::parse(&chunk));
            chunk.clear();
        }
        chunk.push_str(line);
        chunk.push('\n');
    }
    per_zone.push(ModelIndex::parse(&chunk));
    let close = |a: f32, b: f32| (a - b).abs() <= 2e-3 * (1.0 + b.abs());
    let agrees = |sk: &Skeleton, theirs: &SeModel| {
        sk.names.len() == theirs.bones.len()
            && (0..sk.names.len()).all(|i| {
                let b = &theirs.bones[i];
                sk.names[i] == b.name
                    && sk.parents[i] == b.parent
                    && (0..3).all(|k| close(sk.global[i].1[k] * 2.54, b.global.0[k]) && close(sk.local[i].1[k] * 2.54, b.local.0[k]))
                    && (0..4).all(|k| close(sk.global[i].0[k], b.global.1[k]) && close(sk.local[i].0[k], b.local.1[k]))
            })
    };
    let (mut compared, mut matched, mut multi, mut unmatched) = (0usize, 0usize, 0usize, Vec::<String>::new());
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "semodel") {
            continue;
        }
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let skeletons: Vec<&Skeleton> = per_zone.iter().filter_map(|z| z.models.iter().find(|m| m.name == name)).filter_map(|m| m.skeleton.as_ref()).collect();
        if skeletons.is_empty() {
            continue;
        }
        let theirs = SeModel::read(&std::fs::read(&p).unwrap()).unwrap();
        compared += 1;
        multi += (theirs.bones.len() > 1) as usize;
        if skeletons.iter().any(|sk| agrees(sk, &theirs)) {
            matched += 1;
        } else if unmatched.len() < 5 {
            unmatched.push(format!("{name} ({} bones in the reference, {} candidate skeleton(s))", theirs.bones.len(), skeletons.len()));
        }
    }
    println!("{compared} reference models have a recorded skeleton ({multi} with several bones); {matched} agree exactly");
    assert!(compared > 100, "only {compared} compared");
    // The reference dumps come from another CoDOL build than the QA zones; same-named models differ between builds
    // (bone axes, counts), so about three quarters agree exactly. That is what is required here.
    assert!(matched * 100 >= compared * 70, "only {matched} of {compared} agree, for example: {unmatched:#?}");
}

/// The exported triangle order (reversed, Y-up) is counter-clockwise seen from the side the vertex normals point
/// to, as glTF and OBJ require.
#[test]
fn exported_winding_agrees_with_the_vertex_normals() {
    use atlas::export::{ExportModel, to_y_up};
    use atlas::model::Model;
    let Some(game) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else { return };
    let dir = game.join("main").join("models");
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains("_vmgun") || n.contains("_lod010")).take(300).collect();
    names.sort();
    let (mut agree, mut total) = (0usize, 0usize);
    for n in &names {
        let Ok(model) = Model::parse(&std::fs::read(dir.join(n)).unwrap()) else { continue };
        let em = ExportModel::from_streamed(&model, None, n);
        for part in &em.parts {
            for f in &part.faces {
                let p = |i: u32| to_y_up(part.positions[i as usize]);
                // reversed order, as the writers emit it
                let (a, b, c) = (p(f[2]), p(f[1]), p(f[0]));
                let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let g = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                let nrm = to_y_up(part.normals[f[0] as usize]);
                let dot = g[0] * nrm[0] + g[1] * nrm[1] + g[2] * nrm[2];
                if dot.abs() > 1e-12 {
                    total += 1;
                    agree += (dot > 0.0) as usize;
                }
            }
        }
    }
    assert!(total > 10_000, "only {total} triangles");
    assert!(agree * 100 >= total * 90, "{agree} of {total} triangles face the way their normals do");
}

#[test]
fn a_real_model_exports_as_cast() {
    use atlas::export::{ExportModel, cast};
    use atlas::model::Model;
    let Some(game) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else { return };
    let dir = game.join("main").join("models");
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains("_vmgun")).take(20).collect();
    names.sort();
    let mut done = 0;
    for n in &names {
        let Ok(model) = Model::parse(&std::fs::read(dir.join(n)).unwrap()) else { continue };
        let em = ExportModel::from_streamed(&model, None, n);
        if em.parts.is_empty() {
            continue;
        }
        let bytes = cast::write(&em, &[], 2.54);
        assert_eq!(&bytes[..4], b"cast");
        let vertices: usize = em.parts.iter().map(|p| p.positions.len()).sum();
        assert!(bytes.len() > vertices * 12, "{n}: {} bytes for {vertices} vertices", bytes.len());
        done += 1;
    }
    assert!(done > 3, "only {done} models exported");
}
