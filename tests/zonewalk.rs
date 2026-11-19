//! Walking zones of other games with the schema interpreter. Each game needs its folder in an environment variable and is skipped
//! without it:
//! - `CODOL_TEST_T6`: Black Ops II (the folder that has `zone\all`)
//! - `CODOL_TEST_IW3`: Call of Duty 4 (the folder that has `zone\english`)
//! - `CODOL_TEST_IW4`: a folder of stock Modern Warfare 2 zones (`*.ff`)
//! - `CODOL_TEST_IW5`: Modern Warfare 3 (the folder that has `zone\english`; 64-bit zones, only partly supported)
//!
//! A handful of zones are walked by default; `CODOL_TEST_<GAME>_ALL=1` walks every zone (a few minutes in a debug build).

use atlas::zone::schema_walk::{Game, WalkReport, walk_file};
use std::path::{Path, PathBuf};

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn walk_exact(game: Game, path: &Path) -> WalkReport {
    let r = walk_file(game, path).unwrap();
    assert!(r.exact, "{}: {:?} {:?}", path.display(), r.failure, r.mismatches);
    assert_eq!(r.assets_walked, r.assets_total);
    assert_eq!(r.consumed, r.image_len);
    r
}

fn all_zones(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            all_zones(&p, out);
        } else if p.extension().is_some_and(|x| x == "ff") {
            out.push(p);
        }
    }
}

fn walk_all_exact(game: Game, dir: &Path) {
    let mut files = Vec::new();
    all_zones(dir, &mut files);
    assert!(!files.is_empty());
    for f in files {
        walk_exact(game, &f);
    }
}

#[test]
fn t6_sample_zones_walk_exactly() {
    let Some(dir) = env_dir("CODOL_TEST_T6").map(|d| d.join("zone")) else { return };
    for f in ["all/dlc0_load_mp.ff", "all/patch_ui_zm.ff", "all/faction_fbi_mp.ff", "all/code_post_gfx_mp.ff", "all/mp_nuketown_2020.ff", "all/common_zm.ff", "english/en_ui_mp.ff"] {
        let r = walk_exact(Game::T6, &dir.join(f));
        assert!(r.assets_total > 0);
    }
}

#[test]
fn t6_every_zone_walks_exactly() {
    let Some(dir) = env_dir("CODOL_TEST_T6").map(|d| d.join("zone")) else { return };
    if std::env::var_os("CODOL_TEST_T6_ALL").is_some() {
        walk_all_exact(Game::T6, &dir);
    }
}

#[test]
fn t6_content_of_localize_entries_raw_files_and_string_tables_is_recorded() {
    let Some(dir) = env_dir("CODOL_TEST_T6").map(|d| d.join("zone")) else { return };
    let r = walk_exact(Game::T6, &dir.join("english/en_ui_mp.ff"));
    let perk = r.data.localize.iter().find(|(n, _)| n == "CUSTOM_PERK_ROF").expect("CUSTOM_PERK_ROF");
    assert_eq!(perk.1, "RATE OF FIRE");
    let r = walk_exact(Game::T6, &dir.join("all/patch_ui_zm.ff"));
    let lobby = r.data.rawfiles.iter().find(|(n, _)| n == "ui/t6/lobby.lua").expect("lobby.lua");
    assert_eq!(&lobby.1[..4], b"\x1bLua");
    let r = walk_exact(Game::T6, &dir.join("all/common_mp.ff"));
    let t = r.data.stringtables.iter().find(|t| t.name == "mp/gridpointstable.csv").expect("gridpointstable");
    assert_eq!(t.cells.len(), t.rows * t.columns);
    assert_eq!(t.cells[0], "a0");
    // every asset of a walked zone has a name, except references and the few unnamed ones
    let named = r.assets.iter().filter(|a| !a.name.is_empty()).count();
    assert!(named * 10 > r.assets.len() * 9);
}

#[test]
fn iw3_zones_walk_exactly() {
    let Some(dir) = env_dir("CODOL_TEST_IW3").map(|d| d.join("zone").join("english")) else { return };
    for f in ["common_mp.ff", "mp_crash.ff", "ui_mp.ff", "code_post_gfx_mp.ff", "mp_crash_load.ff"] {
        let r = walk_exact(Game::Iw3, &dir.join(f));
        assert!(r.assets_total > 0);
    }
    // the string tables of the stat tables are read (cells are plain string pointers in this game)
    let r = walk_exact(Game::Iw3, &dir.join("code_post_gfx_mp.ff"));
    assert!(r.data.stringtables.iter().any(|t| t.cells.len() == t.rows * t.columns && t.cells.len() > 10));
    if std::env::var_os("CODOL_TEST_IW3_ALL").is_some() {
        walk_all_exact(Game::Iw3, &env_dir("CODOL_TEST_IW3").unwrap());
    }
}

#[test]
fn iw4_stock_zones_walk_exactly() {
    let Some(dir) = env_dir("CODOL_TEST_IW4") else { return };
    // patch_mp and code_post_gfx_mp are the ones a hand-written walker could not finish
    for f in ["patch_mp.ff", "code_post_gfx_mp.ff", "mp_rust.ff"] {
        let p = dir.join(f);
        if p.exists() {
            walk_exact(Game::Iw4, &p);
        }
    }
    if std::env::var_os("CODOL_TEST_IW4_ALL").is_some() {
        walk_all_exact(Game::Iw4, &dir);
    }
}

#[test]
fn iw5_zones_that_the_definitions_cover_walk_exactly() {
    // The 64-bit Modern Warfare 3 zones differ from the 32-bit structure definitions in places; the fixes read from the engine
    // make 157 of 159 zones exact (the other two stop at their last asset). This pins what works.
    let Some(dir) = env_dir("CODOL_TEST_IW5").map(|d| d.join("zone").join("english")) else { return };
    for f in ["mp_alpha_load.ff", "code_pre_gfx.ff", "mp_dome_load.ff", "common_mp.ff", "mp_alpha.ff", "patch_sp_intro.ff"] {
        let p = dir.join(f);
        if p.exists() {
            let r = walk_exact(Game::Iw5, &p);
            assert!(r.assets_total > 0);
        }
    }
    if std::env::var_os("CODOL_TEST_IW5_ALL").is_some() {
        let mut files = Vec::new();
        all_zones(&env_dir("CODOL_TEST_IW5").unwrap(), &mut files);
        let exact = files.iter().filter(|f| walk_file(Game::Iw5, f).map(|r| r.exact).unwrap_or(false)).count();
        assert!(exact >= 155, "{exact} of {} MW3 zones walk exactly", files.len());
    }
}
