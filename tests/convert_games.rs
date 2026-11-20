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
