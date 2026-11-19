//! Map geometry: the world asset of stock MW2 and CoDOL zones becomes a model with a surface per material.

use atlas::zone::{Zone, export, stock, world};
use std::path::PathBuf;

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn check(model: &atlas::model::Model, names: &[String]) -> (usize, usize) {
    assert_eq!(model.surfaces.len(), names.len());
    let (mut vertices, mut triangles) = (0, 0);
    for s in &model.surfaces {
        assert!(s.indices.iter().all(|&i| (i as usize) < s.vertices.len()));
        vertices += s.vertices.len();
        triangles += s.indices.len() / 3;
    }
    // a map has far more than a few thousand triangles and its bounds are a plausible size (game units, inches)
    let (lo, hi) = model.bounds().expect("bounds");
    let size = (0..3).map(|a| hi[a] - lo[a]).fold(0.0f32, f32::max);
    assert!(size > 500.0 && size < 1_000_000.0, "map extent {size}");
    (vertices, triangles)
}

#[test]
fn a_stock_map_becomes_a_model() {
    let Some(root) = env("CODOL_TEST_MW2") else { return };
    let path = root.join("zone/iw4x/x86/english/mp_rust.ff");
    if !path.exists() {
        return;
    }
    let (_, image) = stock::open(&std::fs::read(path).unwrap()).unwrap();
    let (model, names, textures) = stock::model_of_image(&image).expect("a world");
    assert_eq!(textures.len(), names.len());
    assert!(textures.iter().filter(|t| t.color.is_some()).count() * 2 > names.len(), "most materials name a colour image");
    let (v, t) = check(&model, &names);
    println!("mp_rust: {} materials, {v} vertices, {t} triangles", names.len());
    assert!(t > 100_000, "{t} triangles");
}

#[test]
fn a_codol_map_becomes_a_model() {
    for var in ["CODOL_TEST_GAME", "CODOL_TEST_319"] {
        let Some(root) = env(var) else { continue };
        let key = export::find_key(&root).unwrap();
        let mut found = 0;
        for path in export::zone_files(&root) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.starts_with("mp_") || name.ends_with("_load.ff") {
                continue;
            }
            let Ok(zone) = Zone::open(&path, &key) else { continue };
            if !zone.assets.iter().any(|a| a.asset_type == 21) {
                continue;
            }
            let Some((model, names, textures)) = world::model_of_zone(&zone) else { continue };
            assert_eq!(textures.len(), names.len());
            assert!(textures.iter().filter(|t| t.color.is_some()).count() * 2 > names.len(), "most materials name a colour image");
            let (v, t) = check(&model, &names);
            println!("{var} {name}: {} materials, {v} vertices, {t} triangles", names.len());
            found += 1;
            if found >= 3 {
                break;
            }
        }
        assert!(found > 0, "{var}: no map with a world");
    }
}

#[test]
fn the_codol_world_insert_decodes_on_every_map_that_has_one() {
    let Some(root) = env("CODOL_TEST_GAME") else { return };
    let key = export::find_key(&root).unwrap();
    let mut checked = 0;
    for path in export::zone_files(&root) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("mp_") || name.contains("_load") || name.contains("_const") {
            continue;
        }
        let Ok(zone) = Zone::open(&path, &key) else { continue };
        let Some(a) = zone.assets.iter().find(|a| a.asset_type == 21) else { continue };
        let ins = world::world_insert(&zone.data[a.start..a.start + 1608]).unwrap_or_else(|| panic!("{name}: insert"));
        assert_eq!(ins.records.len(), ins.count as usize);
        // every record's seventh word is a distance-like value and its colour ends in 1.0
        assert!(ins.records.iter().all(|r| r[6].is_finite() && r[6].abs() < 100_000.0), "{name}: record distances {:?}", ins.records.iter().map(|r| r[6]).collect::<Vec<_>>());
        assert_eq!(ins.colour[3], 1.0, "{name}");
        checked += 1;
    }
    assert!(checked > 5, "{checked} maps");
}

/// A static model record: origin, a 3x3 rotation (rows are unit vectors) and a scale in front of the model pointer.
fn placement_ok(image: &[u8], at: usize) -> bool {
    let f = |i: usize| f32::from_le_bytes(image[at + 4 * i..at + 4 * i + 4].try_into().unwrap());
    let rows_unit = (0..3).all(|r| ((f(3 + 3 * r).powi(2) + f(4 + 3 * r).powi(2) + f(5 + 3 * r).powi(2)).sqrt() - 1.0).abs() < 0.02);
    rows_unit && f(12) > 0.0 && f(12) < 100.0 && (0..3).all(|i| f(i).is_finite() && f(i).abs() < 100_000.0)
}

#[test]
fn static_model_records_hold_placements() {
    let Some(root) = env("CODOL_TEST_MW2") else { return };
    let path = root.join("zone/iw4x/x86/english/mp_rust.ff");
    if !path.exists() {
        return;
    }
    let (_, image) = stock::open(&std::fs::read(path).unwrap()).unwrap();
    let p = stock::probe(&image, 334).unwrap();
    let w = p.world.unwrap();
    assert!(w.static_models.len() > 1000, "{}", w.static_models.len());
    assert!(w.static_models.iter().all(|(at, name)| name.is_some() && placement_ok(&image, *at)));
}

#[test]
fn codol_static_models_have_names_and_placements() {
    let Some(root) = env("CODOL_TEST_GAME") else { return };
    let key = export::find_key(&root).unwrap();
    let path = export::zone_files(&root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == "mp_afghan.ff").unwrap();
    let zone = Zone::open(&path, &key).unwrap();
    let mut r = atlas::zone::replay::Replay::new(&zone).unwrap();
    for i in 0..zone.assets.len() {
        let _ = r.replay_asset(&zone, i);
    }
    let w = r.rec.world.take().unwrap();
    assert!(w.static_models.len() > 100, "{}", w.static_models.len());
    let named = w.static_models.iter().filter(|m| m.1.is_some()).count();
    assert!(named * 10 >= w.static_models.len() * 9, "{named} of {} named", w.static_models.len());
    assert!(w.static_models.iter().all(|(at, _)| placement_ok(&zone.data, *at)));
}
