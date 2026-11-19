//! Hash index (`.wni`) and the hashed names of the Call of Duty HQ (`cod26`) containers.
//!
//! The synthetic tests need no game files. The tests that read the install need the folder `Call of Duty HQ` in `CODOL_TEST_HQ`
//! and are skipped without it. No real WNI file is used anywhere: the index is built inside the test.

use atlas::hashindex::{self, HashIndex, MASK60, MASK63};
use atlas::kapi::{Entry, Kapi};
use atlas::oodle::Oodle;
use atlas::zone::hq;
use std::collections::HashSet;
use std::path::PathBuf;

fn root() -> Option<PathBuf> {
    std::env::var_os("CODOL_TEST_HQ").map(PathBuf::from)
}

#[test]
fn entries_resolve_through_a_synthetic_index() {
    let names = ["wpc/rex/priority_psos_frontend.csv", "codhq26_shared", "cs_water.hlsl/cs_water_tile_generate_underwater_mesh_9x9"];
    let pairs: Vec<(u64, &str)> = names.iter().map(|n| (hashindex::hash_iw_resource(n), *n)).collect();
    let ix = HashIndex::from_bytes(&hashindex::build_wni(&pairs)).unwrap();
    // entry keys: the 63-bit hash, the same with the high bit set (as 238,384 of the install's keys have it), and an unknown one
    let entries: Vec<Entry> = [pairs[0].0, pairs[1].0 | 1 << 63, pairs[2].0, 0x1234_5678_9abc_def0 & MASK63].iter().map(|&key| Entry { key, packed: 0, ex: 0 }).collect();
    let resolved: Vec<Option<&str>> = entries.iter().map(|e| e.name(&ix)).collect();
    assert_eq!(resolved, vec![Some(names[0]), Some(names[1]), Some(names[2]), None]);
}

#[test]
fn index_file_round_trip_on_disk() {
    let dir = std::env::temp_dir().join(format!("atlas_wni_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("synthetic.wni");
    std::fs::write(&f, hashindex::build_wni(&[(hashindex::hash_iw_resource("a/b"), "a/b")])).unwrap();
    let ix = HashIndex::open(&f).unwrap();
    assert_eq!(ix.lookup(hashindex::hash_iw_resource("a/b") & MASK60), Some("a/b"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn zone_record_types_get_names() {
    let Some(root) = root() else { return };
    let dll = Oodle::find(&root.join("cod26")).expect("oo2core next to the game");
    let o = Oodle::load(&dll).unwrap();
    let z = hq::open(&root.join("cod26/code_pre_gfx.ff"), Some(&o)).unwrap();
    let l = z.asset_list().unwrap();
    let hist = l.histogram();
    let named: Vec<(u64, &str)> = hist.keys().filter_map(|&t| hashindex::asset_type_name(t as u32).map(|n| (t, n))).collect();
    // the type of the raw files that the zone is full of is the 32-bit FNV-1a of "rawfile"
    assert!(named.contains(&(0x3d57_8c37, "rawfile")), "{named:?} of {} types", hist.len());
}

/// Empirical check of the idea behind the name lookup on the real keys: plain strings of the zones, hashed as asset names, against
/// every entry key of the install's xpak packages. The result is printed (`--nocapture`); it is not asserted, because it records a
/// finding about the files (at the time of writing no form matched), not a property of the code.
#[test]
fn plain_zone_strings_against_package_keys() {
    let Some(root) = root() else { return };
    let dll = Oodle::find(&root.join("cod26")).expect("oo2core next to the game");
    let o = Oodle::load(&dll).unwrap();
    let mut keys: HashSet<u64> = HashSet::new();
    for e in std::fs::read_dir(root.join("cod26")).unwrap().flatten() {
        if e.path().extension().is_some_and(|x| x == "xpak") {
            if let Ok(k) = Kapi::open(&e.path()) {
                keys.extend(k.entries.iter().map(|e| e.key & MASK63));
            }
        }
    }
    assert!(keys.len() > 1_000_000, "{} keys", keys.len());
    let mut strings: HashSet<String> = HashSet::new();
    for zone in ["code_pre_gfx", "boot", "frontend_ui_boot", "global_shared_mp"] {
        let Ok(z) = hq::open(&root.join(format!("cod26/{zone}.ff")), Some(&o)) else { continue };
        strings.insert(zone.to_string());
        for run in z.image.split(|&c| !(0x20..0x7f).contains(&c)) {
            if run.len() >= 4 {
                strings.insert(String::from_utf8_lossy(run).into_owned());
            }
        }
    }
    let mut tried = 0usize;
    let mut hits = Vec::new();
    for s in &strings {
        for (label, h) in hashindex::forms(s) {
            tried += 1;
            if keys.contains(&(h & MASK63)) {
                hits.push((label, s.clone()));
            }
        }
    }
    eprintln!("{} strings, {tried} hashes, {} package keys: {} matches {:?}", strings.len(), keys.len(), hits.len(), &hits[..hits.len().min(5)]);
    assert!(tried > 100_000);
}
