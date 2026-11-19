//! Map lights: the Com world's primary lights and the GfxWorld's hero lights recorded while walking a map zone.

use atlas::model::Model;
use atlas::zone::world::{self, LightKind, MapLights};
use atlas::zone::{Zone, export, stock};
use std::path::PathBuf;

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// Plausibility of a map's lights against its geometry: counts, finite values, colours in range, and most origins inside
/// (a margin around) the map's bounds.
fn check(label: &str, model: &Model, l: &MapLights) {
    let used: Vec<_> = l.all().collect();
    let kinds = |k: LightKind| used.iter().filter(|x| x.kind == k).count();
    println!(
        "{label}: {} primary ({} used), {} hero, sun index {:?}; {} sun, {} spot, {} omni",
        l.primary.len(),
        l.primary.iter().filter(|x| x.kind != LightKind::Other).count(),
        l.hero.len(),
        l.sun_index,
        kinds(LightKind::Sun),
        kinds(LightKind::Spot),
        kinds(LightKind::Omni)
    );
    assert!(!l.primary.is_empty(), "{label}: no primary lights");
    assert!(used.len() >= 5 && used.len() < 4096, "{label}: {} lights", used.len());
    for x in &used {
        assert!(x.color.iter().all(|c| c.is_finite() && *c >= 0.0 && *c < 1000.0), "{label}: colour {:?}", x.color);
        assert!(x.origin.iter().all(|c| c.is_finite() && c.abs() < 1.0e6), "{label}: origin {:?}", x.origin);
        assert!(x.radius.is_finite() && x.radius >= 0.0 && x.radius < 1.0e5, "{label}: radius {}", x.radius);
        let d = x.direction;
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        assert!(len.is_finite() && len < 1.5, "{label}: direction {d:?}");
    }
    // lights that stand somewhere (spots and omnis) lie inside the map, within a margin
    let (lo, hi) = model.bounds().expect("bounds");
    let positional: Vec<_> = used.iter().filter(|x| x.kind == LightKind::Spot || x.kind == LightKind::Omni).collect();
    assert!(!positional.is_empty(), "{label}: no spot or omni lights");
    let inside = positional.iter().filter(|x| (0..3).all(|a| x.origin[a] >= lo[a] - 512.0 && x.origin[a] <= hi[a] + 512.0)).count();
    println!("{label}: {inside} of {} positional lights inside the map bounds", positional.len());
    assert!(inside * 10 >= positional.len() * 8, "{label}: {inside} of {} inside", positional.len());
    // the sun is a directional light with a unit-ish direction
    let sun = l.sun().unwrap_or_else(|| panic!("{label}: no sun"));
    let d = sun.direction;
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    assert!((len - 1.0).abs() < 0.05, "{label}: sun direction {d:?}");
    println!("{label}: sun dir {:?} colour {:?}", sun.direction, sun.color);
}

#[test]
fn stock_map_lights() {
    let Some(root) = env("CODOL_TEST_MW2") else { return };
    for name in ["mp_rust.ff", "mp_highrise.ff"] {
        let path = root.join("zone/iw4x/x86/english").join(name);
        if !path.exists() {
            continue;
        }
        let (_, image) = stock::open(&std::fs::read(path).unwrap()).unwrap();
        let (model, _, _, lights) = stock::map_of_image(&image).expect("a world");
        check(&format!("stock {name}"), &model, &lights);
    }
}

#[test]
fn codol_map_lights() {
    for (var, maps) in [("CODOL_TEST_GAME", &["mp_afghan.ff", "mp_rust.ff"][..]), ("CODOL_TEST_319", &["mp_afghan.ff", "mp_rust.ff"][..])] {
        let Some(root) = env(var) else { continue };
        let key = export::find_key(&root).unwrap();
        let mut found = 0;
        for path in export::zone_files(&root) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !maps.contains(&name.as_str()) {
                continue;
            }
            let Ok(zone) = Zone::open(&path, &key) else { continue };
            let Some((model, _, _, lights)) = world::map_of_zone(&zone) else { continue };
            check(&format!("{var} {name}"), &model, &lights);
            found += 1;
        }
        assert!(found > 0, "{var}: no map");
    }
}

#[test]
fn t6_map_lights() {
    let Some(dir) = env("CODOL_TEST_T6") else { return };
    for name in ["mp_nuketown_2020.ff", "mp_carrier.ff", "mp_la.ff", "mp_magma.ff"] {
        let path = dir.join("zone").join("all").join(name);
        if !path.exists() {
            continue;
        }
        let (map, _) = atlas::zone::t6_world::map_of_t6_file(&path, false).unwrap();
        let (model, _, _, lights) = map.expect("a GfxWorld");
        check(&format!("t6 {name}"), &model, &lights);
    }
}

/// `CODOL_TEST_LIGHTS_ALL=1` reads the lights of every map of every configured game and prints a line per map.
#[test]
fn every_map_lights() {
    if std::env::var_os("CODOL_TEST_LIGHTS_ALL").is_none() {
        return;
    }
    let line = |label: &str, l: &MapLights| {
        println!("{label}: {} primary, {} hero, sun {:?}, {:?}", l.primary.len(), l.hero.len(), l.sun_index, l.sun().map(|s| s.direction));
        assert!(l.sun().is_some() || l.primary.is_empty(), "{label}: no sun");
    };
    if let Some(root) = env("CODOL_TEST_MW2") {
        let dir = root.join("zone/iw4x/x86/english");
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let n = path.file_name().unwrap().to_string_lossy().into_owned();
            if !n.starts_with("mp_") || n.contains("_load") || !n.ends_with(".ff") {
                continue;
            }
            let Ok((_, image)) = stock::open(&std::fs::read(&path).unwrap()) else { continue };
            match stock::map_of_image(&image) {
                Some((_, _, _, l)) => line(&format!("stock {n}"), &l),
                None => println!("stock {n}: no world"),
            }
        }
    }
    for var in ["CODOL_TEST_GAME", "CODOL_TEST_319"] {
        let Some(root) = env(var) else { continue };
        let key = export::find_key(&root).unwrap();
        for path in export::zone_files(&root) {
            let n = path.file_name().unwrap().to_string_lossy().into_owned();
            if !n.starts_with("mp_") || n.contains("_load") || n.contains("_const") {
                continue;
            }
            let Ok(zone) = Zone::open(&path, &key) else { continue };
            if !zone.assets.iter().any(|a| a.asset_type == 21) {
                continue;
            }
            match world::map_of_zone(&zone) {
                Some((_, _, _, l)) => line(&format!("{var} {n}"), &l),
                None => println!("{var} {n}: no world"),
            }
        }
    }
}
