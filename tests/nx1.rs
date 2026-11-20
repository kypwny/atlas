//! Future Warfare (NX1) Xbox 360 development build: the fastfile container and the zone walker. Needs `CODOL_TEST_NX1` (the folder of
//! the build's `.ff` files) and does nothing without it.

use atlas::zone::nx1;
use atlas::zone::nx1_walk::{walk_file, Game};
use std::path::PathBuf;

fn folder() -> Option<PathBuf> {
    std::env::var_os("CODOL_TEST_NX1").map(PathBuf::from)
}

fn zone(name: &str) -> Option<PathBuf> {
    let p = folder()?.join(format!("{name}.ff"));
    p.exists().then_some(p)
}

#[test]
fn the_container_opens_and_its_sizes_agree() {
    let Some(p) = zone("mp_nx_shipyard") else { return };
    let raw = std::fs::read(p).unwrap();
    let f = nx1::open(&raw).unwrap();
    assert!(f.signed);
    assert_eq!(f.version, 357);
    let (size, _, blocks) = f.xfile();
    assert_eq!(size as usize + 32, f.image.len());
    let list = nx1::asset_list(&f.image).unwrap();
    assert_eq!(list.entries.len(), 601);
    assert_eq!(list.entries.iter().filter(|e| e.0 == 5).count(), 410, "materials");
    assert_eq!(blocks[3], 18_076_285);
}

#[test]
fn a_small_zone_walks_exactly() {
    let Some(p) = zone("code_pre_gfx") else { return };
    let rep = walk_file(Game::Nx1, &p).unwrap();
    assert!(rep.exact, "{:?} {:?}", rep.failure, rep.mismatches);
    assert_eq!(rep.assets_walked, 5);
}

#[test]
fn a_map_walks_exactly() {
    let Some(p) = zone("mp_nx_asylum_2") else { return };
    let rep = walk_file(Game::Nx1, &p).unwrap();
    assert!(rep.exact, "{:?} {:?}", rep.failure, rep.mismatches);
    assert_eq!(rep.assets_walked, 846);
}

/// Every zone of the folder (slow: the maps are hundreds of megabytes).
#[test]
fn every_zone_walks_exactly() {
    if std::env::var_os("CODOL_TEST_NX1_ALL").is_none() {
        return;
    }
    let Some(dir) = folder() else { return };
    let mut bad = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "ff") {
            let rep = walk_file(Game::Nx1, &p).unwrap();
            if !rep.exact {
                bad.push(format!("{}: {:?} {:?}", p.display(), rep.failure, rep.mismatches));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// NX1 as the source of a conversion to stock Modern Warfare 2: the output is read back by the IW4 walker (`convert` checks it).
#[test]
fn a_zone_converts_to_stock_mw2_and_reads_back() {
    use atlas::zone::convert_cli::{convert, open_input, Fmt};
    for name in ["code_pre_gfx", "mp_nx_shipyard"] {
        let Some(p) = zone(name) else { continue };
        let input = open_input(&p, None).unwrap();
        assert_eq!(input.fmt, Fmt::Nx1);
        let (done, steps) = convert(input, Fmt::Iw4).unwrap();
        assert_eq!(done.fmt, Fmt::Iw4);
        assert!(steps.last().unwrap().out.assets > 0);
    }
}
