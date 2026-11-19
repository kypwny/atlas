//! Black Ops II zone editing (`zone::t6_rebuild`): replace the contents of raw files, localize entries and string tables, remove
//! assets, and check by walking the result again. Needs `CODOL_TEST_T6=<game folder>` (the one that has `zone\all`); skipped
//! without it. `CODOL_TEST_T6_ALL=1` also runs the identity rebuild and the string table check on every zone.
//!
//! These checks are structural (the walker accepts the new zone and the block sizes agree with the header); the game was not run.

use atlas::zone::t6;
use atlas::zone::t6_rebuild::*;
use std::path::{Path, PathBuf};

fn root() -> Option<PathBuf> {
    let d = std::env::var_os("CODOL_TEST_T6").map(PathBuf::from)?.join("zone");
    d.is_dir().then_some(d)
}

fn all() -> bool {
    std::env::var_os("CODOL_TEST_T6_ALL").is_some()
}

fn zone_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for sub in ["all", "english", "japanese"] {
        let Ok(rd) = std::fs::read_dir(root.join(sub)) else { continue };
        files.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "ff")));
    }
    files.sort();
    files
}

fn load(p: &Path) -> T6Image {
    let raw = std::fs::read(p).unwrap();
    let z = t6::open(&raw).unwrap();
    T6Image::from_zone(&z).unwrap_or_else(|e| panic!("{}: {e:#}", p.display()))
}

/// Zones of moderate size that hold what the edit tests need.
fn candidates(root: &Path, max_len: u64) -> Vec<PathBuf> {
    let mut v: Vec<(u64, PathBuf)> = zone_files(root).into_iter().map(|p| (p.metadata().unwrap().len(), p)).filter(|x| x.0 <= max_len).collect();
    v.sort();
    v.into_iter().map(|x| x.1).collect()
}

fn type_count(z: &T6Image, ty: u32) -> usize {
    (0..z.asset_count()).filter(|&k| z.asset_type(k) == ty && matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT)).count()
}

fn first_of(z: &T6Image, ty: u32) -> Option<usize> {
    (0..z.asset_count()).find(|&k| z.asset_type(k) == ty && matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT))
}

/// An untouched rebuild and "replace every Nth asset by itself" give back the same image.
fn identity(z: &T6Image, step: usize) {
    let r = rebuild(z, vec![]).unwrap_or_else(|e| panic!("{}: {e:#}", z.name()));
    assert!(r.zone.image() == z.image(), "{}: an empty rebuild changed the image", z.name());
    let edits: Vec<Edit> = (0..z.asset_count()).step_by(step).filter(|&k| matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT)).map(|k| Edit::Replace(k, z.unchanged(k))).collect();
    if edits.is_empty() {
        return;
    }
    let r = rebuild(z, edits).unwrap_or_else(|e| panic!("{}: {e:#}", z.name()));
    assert!(r.zone.image() == z.image(), "{}: replacing assets by themselves changed the image", z.name());
}

#[test]
fn rebuilding_without_changes_is_the_identity() {
    let Some(root) = root() else { return };
    let zones = if all() { zone_files(&root) } else { candidates(&root, 3_000_000).into_iter().take(12).collect() };
    for p in zones {
        // the largest zones are not walked twice for every asset
        let big = p.metadata().unwrap().len() > 20_000_000;
        let z = load(&p);
        identity(&z, if big { z.asset_count().max(1) } else { 1 });
    }
}

#[test]
fn string_tables_have_the_hashes_and_index_the_rules_give() {
    let Some(root) = root() else { return };
    let zones = if all() { zone_files(&root) } else { candidates(&root, 60_000_000) };
    let (mut checked, mut unordered) = (0, Vec::new());
    for p in zones {
        if !all() && checked >= 40 {
            break;
        }
        let z = load(&p);
        for k in 0..z.asset_count() {
            if z.asset_type(k) == STRINGTABLE && matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT) {
                let t = z.stringtable(k).unwrap();
                // the cell hashes are exactly the rules' hashes; the cell index follows the order rule except for a few tables
                // whose entries the game's tool placed by an order that cannot be told from the stored data
                let texts = t.raw();
                assert!(texts.iter().zip(&t.hashes).all(|(s, &h)| hash_bytes(s) == h), "{}: string table {} (asset {k}): stored hashes differ from the rules", z.name(), t.name);
                if !t.consistent() {
                    unordered.push(format!("{}/{}", z.name(), t.name));
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
    eprintln!("{checked} string tables, {} with an index that is not in the stored order's chain: {unordered:?}", unordered.len());
    assert!(unordered.len() * 20 <= checked, "too many string tables with an unexplained cell index");
}

#[test]
fn replacing_a_raw_file_a_string_table_and_a_localize_entry_keeps_the_zone_walking() {
    let Some(root) = root() else { return };
    // a zone with all three kinds of asset and some data after them
    let p = candidates(&root, 60_000_000)
        .into_iter()
        .find(|p| {
            let z = load(p);
            type_count(&z, RAWFILE) > 0 && type_count(&z, STRINGTABLE) > 0 && type_count(&z, LOCALIZE) > 0 && z.asset_count() > 20
        })
        .expect("a zone with raw files, string tables and localize entries");
    let z = load(&p);
    eprintln!("{}: {} assets", z.name(), z.asset_count());

    let raw = first_of(&z, RAWFILE).unwrap();
    let table = first_of(&z, STRINGTABLE).unwrap();
    let loc = first_of(&z, LOCALIZE).unwrap();
    let old_raw = z.rawfile(raw).unwrap();
    let (loc_name, old_loc) = z.localize(loc).unwrap();
    let mut t = z.stringtable(table).unwrap();
    eprintln!("raw {:?} ({} bytes), localize {loc_name} = {old_loc:?}, table {} ({}x{})", z.asset_name(raw), old_raw.len(), t.name, t.rows, t.columns);

    // each edit alone, then all together
    let mut new_raw = old_raw.clone();
    new_raw.extend_from_slice(b"\n// edited by the Atlas test, a longer file than before\n");
    let new_loc = format!("{old_loc} (edited, and longer than before)");
    let mut cells = t.raw();
    if let Some(c) = cells.get_mut(t.columns + 1) {
        *c = b"atlas_edit_cell_longer_than_before".to_vec();
    } else if let Some(c) = cells.first_mut() {
        *c = b"atlas_edit_cell".to_vec();
    }
    let single = |e: Edit| {
        let r = rebuild(&z, vec![e]).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(r.stats.new_len != r.stats.old_len || r.stats.replaced == 1);
        r
    };
    let r = single(Edit::Replace(raw, z.set_rawfile(raw, &new_raw).unwrap()));
    assert_eq!(r.zone.rawfile(raw).unwrap(), new_raw);
    assert_eq!(r.zone.asset_count(), z.asset_count());
    let r = single(Edit::Replace(loc, z.set_localize(loc, &new_loc).unwrap()));
    assert_eq!(r.zone.localize(loc).unwrap().1, new_loc);
    assert_eq!(r.zone.localize(loc).unwrap().0, loc_name);
    let r = single(Edit::Replace(table, z.set_stringtable(table, t.columns, t.rows, &cells).unwrap()));
    let t2 = r.zone.stringtable(table).unwrap();
    assert_eq!(t2.raw(), cells);
    assert!(t2.consistent());
    t.cells.clear();

    // all three at once; the rest of the zone is untouched
    let r = rebuild(
        &z,
        vec![
            Edit::Replace(raw, z.set_rawfile(raw, &new_raw).unwrap()),
            Edit::Replace(table, z.set_stringtable(table, t.columns, t.rows, &cells).unwrap()),
            Edit::Replace(loc, z.set_localize(loc, &new_loc).unwrap()),
        ],
    )
    .unwrap();
    eprintln!("{:?}", r.stats);
    assert!(r.zone.report.exact);
    for k in 0..z.asset_count() {
        if k != raw && k != table && k != loc {
            assert_eq!(r.zone.asset_type(k), z.asset_type(k));
            assert_eq!(r.zone.asset_name(k), z.asset_name(k), "asset {k}");
            if !matches!(z.asset_type(k), STRINGTABLE | RAWFILE | LOCALIZE) {
                assert!(r.zone.asset_bytes(k).len() == z.asset_bytes(k).len(), "asset {k} changed size");
            }
        }
    }
    // the data tables the walker collects show the new content
    assert!(r.zone.report.data.rawfiles.iter().any(|(n, b)| n == z.asset_name(raw) && *b == new_raw));
    assert!(r.zone.report.data.localize.iter().any(|(n, v)| *n == loc_name && *v == new_loc));
}

#[test]
fn removing_assets_relocates_the_references_behind_them() {
    let Some(root) = root() else { return };
    let p = candidates(&root, 60_000_000).into_iter().find(|p| type_count(&load(p), RAWFILE) > 1).expect("a zone with raw files");
    let z = load(&p);
    let n = z.asset_count();
    // remove every raw file, string table and localize entry that nothing refers to
    let mut removed = 0;
    let mut edits = Vec::new();
    for k in 0..n {
        if matches!(z.asset_type(k), RAWFILE | STRINGTABLE | LOCALIZE) && rebuild(&z, vec![Edit::Remove(k)]).is_ok() {
            edits.push(Edit::Remove(k));
            removed += 1;
        }
    }
    assert!(removed > 0);
    let r = rebuild(&z, edits).unwrap();
    assert_eq!(r.zone.asset_count(), n - removed);
    assert!(r.zone.report.exact);
    assert!(r.zone.image().len() < z.image().len());
    eprintln!("{}: removed {removed} of {n} assets, {:?}", z.name(), r.stats);
    // the first asset goes too (the asset list array shifts under every reference to later assets)
    let r = rebuild(&z, vec![Edit::Remove(0)]);
    match r {
        Ok(r) => assert_eq!(r.zone.asset_count(), n - 1),
        Err(e) => eprintln!("asset 0 is referenced: {e:#}"),
    }
}

/// Removing an asset either gives a zone that walks exactly, or is refused because something points into the asset (the error
/// says so); any other outcome (a walk that fails, operations that differ) is a bug in the relocation.
fn remove_spread(z: &T6Image, tries: usize) -> (usize, usize) {
    let n = z.asset_count();
    let (mut ok, mut refused) = (0, 0);
    for t in 0..tries.min(n) {
        let k = if tries >= n { t } else { t * n / tries + (t * 7) % (n / tries).max(1) };
        match rebuild(z, vec![Edit::Remove(k)]) {
            Ok(r) => {
                assert_eq!(r.zone.asset_count(), n - 1);
                assert!(r.zone.report.exact);
                ok += 1;
            }
            Err(e) => {
                let m = format!("{e:#}");
                assert!(m.contains("which was replaced or removed"), "{}: removing asset {k} ({} {:?}) fails for another reason: {m}", z.name(), z.asset_type(k), z.asset_name(k));
                refused += 1;
            }
        }
    }
    (ok, refused)
}

#[test]
fn removing_assets_anywhere_is_exact_or_refused() {
    let Some(root) = root() else { return };
    let mut total = (0, 0);
    for p in candidates(&root, 12_000_000).into_iter().rev().take(5) {
        let z = load(&p);
        let (ok, refused) = remove_spread(&z, 40);
        eprintln!("{}: {} assets, removals {ok} exact, {refused} refused (referenced)", z.name(), z.asset_count());
        total = (total.0 + ok, total.1 + refused);
    }
    assert!(total.0 > 0);
    if all() {
        // a map-sized zone
        let p = zone_files(&root).into_iter().find(|p| p.ends_with("mp_hijacked.ff") || p.ends_with("mp_nuketown_2020.ff"));
        if let Some(p) = p {
            let z = load(&p);
            let (ok, refused) = remove_spread(&z, 6);
            eprintln!("{}: {} assets, removals {ok} exact, {refused} refused", z.name(), z.asset_count());
        }
    }
}

#[test]
fn replacing_in_the_middle_and_at_the_end_relocates_what_follows() {
    let Some(root) = root() else { return };
    for p in candidates(&root, 12_000_000).into_iter().rev().take(4) {
        let z = load(&p);
        let n = z.asset_count();
        let raws: Vec<usize> = (0..n).filter(|&k| z.asset_type(k) == RAWFILE && matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT)).collect();
        let locs: Vec<usize> = (0..n).filter(|&k| z.asset_type(k) == LOCALIZE && matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT)).collect();
        for list in [&raws, &locs] {
            for &k in [list.first(), list.get(list.len() / 2), list.last()].into_iter().flatten() {
                let na = if z.asset_type(k) == RAWFILE {
                    let mut c = z.rawfile(k).unwrap();
                    c.extend_from_slice(&[b'x'; 77]);
                    z.set_rawfile(k, &c).unwrap()
                } else {
                    let (_, v) = z.localize(k).unwrap();
                    z.set_localize(k, &format!("{v}{}", "y".repeat(33))).unwrap()
                };
                let r = rebuild(&z, vec![Edit::Replace(k, na)]).unwrap_or_else(|e| panic!("{} asset {k}: {e:#}", z.name()));
                assert!(r.zone.report.exact);
                assert_eq!(r.stats.new_len as i64 - r.stats.old_len as i64, if z.asset_type(k) == RAWFILE { 77 } else { 33 });
            }
        }
    }
}

#[test]
fn a_script_parse_tree_takes_a_longer_compiled_buffer() {
    let Some(root) = root() else { return };
    let p = candidates(&root, 60_000_000).into_iter().find(|p| type_count(&load(p), SCRIPT_PARSE_TREE) > 1).expect("a zone with script parse trees");
    let z = load(&p);
    let scripts: Vec<usize> = (0..z.asset_count()).filter(|&k| z.asset_type(k) == SCRIPT_PARSE_TREE && matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT)).collect();
    for k in [scripts[0], scripts[scripts.len() / 2], *scripts.last().unwrap()] {
        let mut c = z.rawfile(k).unwrap();
        c.extend_from_slice(&[0xABu8; 130]);
        let r = rebuild(&z, vec![Edit::Replace(k, z.set_rawfile(k, &c).unwrap())]).unwrap_or_else(|e| panic!("{} script {k}: {e:#}", z.name()));
        assert_eq!(r.zone.rawfile(k).unwrap(), c);
        assert_eq!(r.stats.new_len, r.stats.old_len + 130);
        assert!(r.zone.report.exact);
    }
    eprintln!("{}: {} script parse trees edited at the start, middle and end", z.name(), scripts.len());
}
