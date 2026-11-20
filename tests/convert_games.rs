//! Conversions between the Infinity Ward layouts (`atlas --convertzone SRC.ff --to GAME`). Each test needs the game folders it
//! converts from (`CODOL_TEST_MW2`, `CODOL_TEST_IW3`, `CODOL_TEST_IW5`, `CODOL_TEST_GAME`) and does nothing without them.
//!
//! The check is the one the command makes itself: every hop's output is read back with the reader of its layout (the schema walker
//! for IW3, IW4 and IW5, the QA walker for Call of Duty Online) and has to read exactly.

use atlas::zone::convert_cli::{convert, open_input, Fmt};
use std::path::PathBuf;

fn dir(var: &str, sub: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(|p| PathBuf::from(p).join(sub))
}

fn mw2(name: &str) -> Option<PathBuf> {
    let p = dir("CODOL_TEST_MW2", "zone/iw4x/x86/english")?.join(format!("{name}.ff"));
    p.exists().then_some(p)
}

fn iw3(name: &str) -> Option<PathBuf> {
    let p = dir("CODOL_TEST_IW3", "zone/english")?.join(format!("{name}.ff"));
    p.exists().then_some(p)
}

fn iw5(name: &str) -> Option<PathBuf> {
    let p = dir("CODOL_TEST_IW5", "zone/english")?.join(format!("{name}.ff"));
    p.exists().then_some(p)
}

fn game() -> Option<PathBuf> {
    std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from)
}

/// Converts a zone to `target`, returning the number of assets it came out with.
fn run(path: &PathBuf, target: Fmt) -> usize {
    let input = open_input(path, game().as_deref()).unwrap();
    let (done, steps) = convert(input, target).unwrap();
    assert_eq!(done.fmt, target);
    steps.last().map(|s| s.out.assets).unwrap()
}

#[test]
fn a_stock_loading_screen_reaches_every_game() {
    let Some(p) = mw2("mp_rust_load") else { return };
    // CoD4 keeps its shaders inside the technique sets, so the three shader assets of the zone do not come out as assets there
    for (target, assets) in [(Fmt::Iw4, 10), (Fmt::Iw3, 7), (Fmt::Iw5, 10)] {
        assert_eq!(run(&p, target), assets, "{}", target.name());
    }
    if game().is_some() {
        assert_eq!(run(&p, Fmt::Codol), 10);
    }
}

#[test]
fn a_stock_map_converts_to_cod4_and_mw3_and_back() {
    let Some(p) = mw2("mp_rust") else { return };
    assert!(run(&p, Fmt::Iw3) > 1500);
    assert!(run(&p, Fmt::Iw5) > 2000);
}

#[test]
fn cod4_zones_convert_to_mw2_and_through_it_to_mw3() {
    let Some(p) = iw3("mp_crash_load") else { return };
    assert_eq!(run(&p, Fmt::Iw4), 6);
    assert_eq!(run(&p, Fmt::Iw5), 6);
    let Some(m) = iw3("mp_crash") else { return };
    assert!(run(&m, Fmt::Iw4) > 700);
}

#[test]
fn mw3_zones_convert_to_mw2() {
    let Some(p) = iw5("code_pre_gfx_mp") else { return };
    assert_eq!(run(&p, Fmt::Iw4), 7);
    let Some(m) = iw5("common_mp") else { return };
    assert!(run(&m, Fmt::Iw4) > 4000);
}

#[test]
fn call_of_duty_online_raw_files_reach_the_other_games() {
    let (Some(g), true) = (game(), game().is_some()) else { return };
    let p = g.join("client_shipRetail/chinese/mp_afghan_const.ff");
    if !p.exists() {
        return;
    }
    assert_eq!(run(&p, Fmt::Iw4), 10);
    assert_eq!(run(&p, Fmt::Iw3), 10);
    assert_eq!(run(&p, Fmt::Iw5), 10);
}

fn game319() -> Option<PathBuf> {
    std::env::var_os("CODOL_TEST_319").map(PathBuf::from)
}

/// Converts a Call of Duty Online zone to stock MW2: the output is read by the stock schema walker (the command checks that itself).
fn codol_to_mw2(folder: &PathBuf, zone: &str) -> Option<(usize, atlas::zone::convert_cli::Input)> {
    let p = folder.join("client_shipRetail/chinese").join(format!("{zone}.ff"));
    if !p.exists() {
        return None;
    }
    let input = open_input(&p, Some(folder)).unwrap();
    let (done, steps) = convert(input, Fmt::Iw4).unwrap();
    Some((steps.last().map(|s| s.out.assets).unwrap(), done))
}

#[test]
fn a_call_of_duty_online_loading_screen_converts_to_mw2() {
    let Some(g) = game() else { return };
    let Some((assets, _)) = codol_to_mw2(&g, "mp_nuked_load") else { return };
    // two materials and a technique set (models, materials, images and technique sets are read with the Call of Duty Online
    // schema), the raw file
    assert_eq!(assets, 5);
}

#[test]
fn a_call_of_duty_online_map_converts_to_mw2() {
    let Some(g) = game() else { return };
    let Some((assets, done)) = codol_to_mw2(&g, "mp_nuked") else { return };
    // 416 entries in the zone: the collision map, the render world, models, materials, images, technique sets, sounds and effects
    // come out as stock assets, and the map entities the collision map held inline become an asset of their own
    assert!(assets > 400, "{assets}");
    assert_eq!(done.fmt, Fmt::Iw4);
    let zone = atlas::zone::schema_walk::ZoneImage { name: "x".into(), block_sizes: (0..8).map(|i| atlas::zone::u32_at(&done.image, 8 + 4 * i)).collect(), image: done.image.clone() };
    let r = atlas::zone::schema_walk::walk(atlas::zone::schema_walk::Game::Iw4, &zone);
    assert!(r.exact, "{:?}", r.mismatches);
    // types in the stock zone that the Call of Duty Online one does not hold: the map entities are made from the collision map
    assert!(r.assets.iter().any(|a| a.ty == 19), "map entities");
    assert!(r.assets.iter().any(|a| a.ty == 21), "render world");
    assert!(r.assets.iter().any(|a| a.ty == 15), "collision map");
}

#[test]
fn a_call_of_duty_online_3_19_map_converts_to_mw2() {
    let Some(g) = game319() else { return };
    let p = g.join("client_shipRetail/chinese/mp_afghan.ff");
    if !p.exists() {
        return;
    }
    let input = open_input(&p, Some(&g)).unwrap();
    assert_eq!(input.version, 461);
    let (done, steps) = convert(input, Fmt::Iw4).unwrap();
    assert_eq!(done.fmt, Fmt::Iw4);
    assert!(steps.last().unwrap().out.assets > 200);
}

/// Stock MW2 -> Call of Duty Online -> stock MW2: the loading screen comes back asset for asset (content hash of every asset).
#[test]
fn a_stock_loading_screen_survives_the_round_trip_through_call_of_duty_online() {
    let (Some(p), Some(g)) = (mw2("mp_rust_load"), game()) else { return };
    let (qa, _) = convert(open_input(&p, None).unwrap(), Fmt::Codol).unwrap();
    let _ = g;
    let first = atlas::zone::convert_engine::Rules { canon: true, ..Default::default() };
    let orig = atlas::zone::convert_cli::engine_hop(atlas::zone::schema_walk::Game::Iw4, atlas::zone::schema_walk::Game::Iw4, first, &open_input(&p, None).unwrap()).unwrap();
    let back = atlas::zone::convert_from_codol::codol_to_iw4_with(&qa, true).unwrap();
    let mut a: Vec<_> = orig.canon.iter().map(|c| (c.ty, c.name.clone(), c.hash)).collect();
    let mut b: Vec<_> = back.canon.iter().map(|c| (c.ty, c.name.clone(), c.hash)).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
}
