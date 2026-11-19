//! Stock Modern Warfare 2 zone -> CoDOL QA zone conversion (`atlas --convertzone`). Needs CODOL_TEST_MW2 (the x86 MW2 folder with
//! `zone/iw4x/x86/english`); the sealed-file check also needs CODOL_TEST_GAME (the QA 3.3.3.3 game folder, for the archive key).
//!
//! The strongest check there is without a client: the QA walker (`Replay`, the layouts of the QA loader) walks the converted zone
//! asset by asset, every asset ends exactly at its extent, the memory blocks end where the zone header says and no reference
//! dangles. `convert_file` does that walk itself and fails otherwise.

use atlas::zone::{Zone, convert_zone, export};
use std::path::PathBuf;

fn mw2() -> Option<PathBuf> {
    std::env::var_os("CODOL_TEST_MW2").map(|p| PathBuf::from(p).join("zone/iw4x/x86/english"))
}

fn zone(name: &str) -> Option<PathBuf> {
    let p = mw2()?.join(format!("{name}.ff"));
    p.exists().then_some(p)
}

#[test]
fn loading_screen_zone_converts() {
    let Some(path) = zone("mp_rust_load") else { return };
    let c = convert_zone::convert_file(&path).unwrap();
    assert_eq!(c.assets, 10);
    assert!(c.warnings.is_empty(), "{:?}", c.warnings);
}

/// The target of the converter: a stock multiplayer map, every asset type in it converted.
#[test]
fn stock_map_converts_and_the_qa_walker_accepts_it() {
    let Some(path) = zone("mp_rust") else { return };
    let c = convert_zone::convert_file(&path).unwrap();
    // 2,582 stock assets: the game world has no loader in the QA build and the map entities move into the collision map
    assert_eq!(c.assets, 2580);
    assert_eq!(c.nulled, 0);
    assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    let left_out: usize = c.stats.values().map(|s| s.left_out.values().sum::<usize>()).sum();
    assert_eq!(left_out, 1, "only the game world is left out: {:?}", c.stats);
    for (ty, n) in [(0u32, 15usize), (1, 7), (4, 126), (5, 282), (9, 245), (10, 608), (15, 1), (21, 1), (30, 161), (31, 1)] {
        assert_eq!(c.stats[&ty].converted, n, "type {ty}");
    }
    // the converted world: every surface and static model names its material or model
    let zone = Zone::from_image(atlas::zone::fastfile::VERSION_QA, c.image).unwrap();
    let mut out = String::new();
    let r = export::export_zone(&zone, "mp_rust", &mut out);
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(r.models, 126);
}

/// The converted world is the stock world: the same materials, vertices and triangles come out of the QA reader as out of the stock one.
#[test]
fn the_converted_world_is_the_stock_world() {
    use atlas::zone::{fastfile, stock, world};
    let Some(path) = zone("mp_rust") else { return };
    let (_, image) = stock::open(&std::fs::read(&path).unwrap()).unwrap();
    let (smodel, snames, _) = stock::model_of_image(&image).expect("stock world");
    let c = convert_zone::convert_file(&path).unwrap();
    let zone = Zone::from_image(fastfile::VERSION_QA, c.image).unwrap();
    let (qmodel, qnames, _) = world::model_of_zone(&zone).expect("converted world");
    assert_eq!(snames, qnames);
    let count = |m: &atlas::model::Model| (m.surfaces.iter().map(|s| s.vertices.len()).sum::<usize>(), m.surfaces.iter().map(|s| s.indices.len()).sum::<usize>());
    assert_eq!(count(&smodel), count(&qmodel));
    for (a, b) in smodel.surfaces.iter().zip(&qmodel.surfaces) {
        assert_eq!(a.indices, b.indices);
        assert_eq!(a.vertices.len(), b.vertices.len());
    }
}

#[test]
fn map_zones_with_water_and_animations_convert() {
    for name in ["mp_brecourt", "mp_afghan", "mp_terminal"] {
        let Some(path) = zone(name) else { return };
        let c = convert_zone::convert_file(&path).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert!(c.warnings.is_empty(), "{name}: {:?}", c.warnings);
        for (ty, s) in &c.stats {
            // weapons are not converted (CoDOL has its own), the game world has no loader in the QA build
            assert!(*ty == 28 || s.left_out.keys().all(|why| why.contains("no loader")), "{name}: type {ty}: {:?}", s.left_out);
        }
    }
}

/// The command seals the converted image with a container of the QA game; the sealed file opens and replays.
#[test]
fn sealed_file_opens_with_the_qa_key() {
    let (Some(path), Some(game)) = (zone("mp_rust_load"), std::env::var_os("CODOL_TEST_GAME")) else { return };
    let out = std::env::temp_dir().join("atlas_convert_zone_test.ff");
    let args = vec![path.to_string_lossy().into_owned(), game.to_string_lossy().into_owned(), out.to_string_lossy().into_owned()];
    convert_zone::command(&args).unwrap();
    let key = export::find_key(&PathBuf::from(game)).unwrap();
    let zone = Zone::open(&out, &key).unwrap();
    assert_eq!(zone.assets.len(), 10);
    let mut sink = String::new();
    let r = export::export_zone(&zone, "mp_rust_load", &mut sink);
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    let _ = std::fs::remove_file(out);
}
