//! Black Ops II zone writing (`zone::t6_write`): the container round trip. Needs `CODOL_TEST_T6=<game folder>` (the one that has
//! `zone\all`); skipped without it. `CODOL_TEST_T6_ALL=1` also runs every zone of `zone\all` (a few minutes).
//!
//! Acceptance is what the loader (`t6::open_named`) reads back and what the schema walker accepts; the game was not run.

use atlas::zone::schema_walk::{Game, ZoneImage, walk};
use atlas::zone::t6::{self, T6Zone};
use atlas::zone::t6_write::{WriteOptions, write_fastfile, write_zone};
use std::path::{Path, PathBuf};

fn zone_dir() -> Option<PathBuf> {
    let d = std::env::var_os("CODOL_TEST_T6").map(PathBuf::from)?.join("zone").join("all");
    d.is_dir().then_some(d)
}

fn all() -> bool {
    std::env::var_os("CODOL_TEST_T6_ALL").is_some()
}

fn small_zones(dir: &Path, n: usize) -> Vec<PathBuf> {
    let mut v: Vec<(u64, PathBuf)> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "ff")).map(|p| (p.metadata().unwrap().len(), p)).collect();
    v.sort();
    v.into_iter().map(|x| x.1).take(n).collect()
}

fn stem(p: &Path) -> String {
    p.file_stem().unwrap().to_string_lossy().into_owned()
}

fn same_zone(a: &T6Zone, b: &T6Zone, what: &str) {
    assert!(a.image == b.image, "{what}: image differs");
    assert_eq!((a.size, a.external_size, a.block_sizes), (b.size, b.external_size, b.block_sizes), "{what}: header");
}

/// One zone through every kind of container; whether the signed re-encoding equals the original file byte for byte.
fn round_trip(path: &Path) -> bool {
    let name = stem(path);
    let raw = std::fs::read(path).unwrap();
    let z = t6::open(&raw).unwrap();
    assert_eq!(z.name, name);

    // 1. same chunking, signature copied: the signed container; the image must match, and the file may match byte for byte
    let signed = write_zone(&z, true).unwrap();
    let back = t6::open(&signed).unwrap();
    same_zone(&z, &back, &format!("{name} signed"));
    assert_eq!(back.auth_header, z.auth_header, "{name}: signature");
    assert_eq!(back.chunk_sizes, z.chunk_sizes, "{name}: chunking");

    // 2. unsigned, the way OpenAssetTools cuts it: the name comes from the file name
    let unsigned = write_fastfile(&name, &z.image, &WriteOptions::default()).unwrap();
    assert_eq!(&unsigned[..8], b"TAffu100");
    assert!(t6::open(&unsigned).is_err(), "an unsigned zone has no name of its own");
    let back = t6::open_named(&unsigned, Some(&name)).unwrap();
    same_zone(&z, &back, &format!("{name} unsigned"));
    assert!(back.chunk_sizes.iter().all(|&c| c as usize <= atlas::zone::t6_write::CHUNK_IMAGE_SIZE));
    assert_eq!(unsigned.len() % 0x40, 0);
    // a wrong name decrypts to garbage
    assert!(t6::open_named(&unsigned, Some("wrong_name")).map(|w| w.image != z.image).unwrap_or(true));

    // 3. the walker takes the unsigned file as it takes the original
    let img = ZoneImage::open(Game::T6, &name, &unsigned).unwrap();
    assert!(img.image == z.image);

    // how close the bytes come to the original (informational: the deflate encoder is not zlib)
    signed == raw
}

#[test]
fn container_round_trip_on_small_zones() {
    let Some(dir) = zone_dir() else { return };
    for p in small_zones(&dir, 6) {
        round_trip(&p);
    }
}

#[test]
fn unsigned_zone_walks_exactly() {
    let Some(dir) = zone_dir() else { return };
    let p = small_zones(&dir, 3).pop().unwrap();
    let name = stem(&p);
    let raw = std::fs::read(&p).unwrap();
    let z = t6::open(&raw).unwrap();
    let unsigned = write_fastfile(&name, &z.image, &WriteOptions::default()).unwrap();
    let img = ZoneImage::open(Game::T6, &name, &unsigned).unwrap();
    let r = walk(Game::T6, &img);
    assert!(r.exact, "{name}: {:?} {:?}", r.failure, r.mismatches);
}

#[test]
fn container_round_trip_on_every_zone() {
    let Some(dir) = zone_dir() else { return };
    if !all() {
        return;
    }
    let root = dir.parent().unwrap().to_path_buf();
    let mut files: Vec<PathBuf> = Vec::new();
    for sub in ["all", "english", "japanese"] {
        let Ok(rd) = std::fs::read_dir(root.join(sub)) else { continue };
        files.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "ff")));
    }
    files.sort();
    let (mut n, mut same) = (0, 0);
    for p in &files {
        same += round_trip(p) as usize;
        n += 1;
    }
    eprintln!("{n} zones round-tripped with an identical image; {same} of them also byte for byte identical to the original file");
    assert!(n >= 200);
}
