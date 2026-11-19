//! Image reading for the other games' installs. Each test needs its folder in an environment variable and is
//! skipped without it: `CODOL_TEST_COD4` (Call of Duty 4), `CODOL_TEST_MW2` (Modern Warfare 2 / iw4x).

use atlas::iwi;
use atlas::source::{self, Progress};
use std::path::PathBuf;

/// Scans a game folder, then reads and decodes the first and last mip of every image. Every file must be of
/// `version`, parse, have exactly the length its header implies, and decode.
fn check(var: &str, version: u8, min_images: usize) {
    let Some(root) = std::env::var_os(var).map(PathBuf::from) else { return };
    let cat = source::scan(source::discover(&root), &Progress::default());
    let mut n = 0;
    for e in &cat.entries {
        for v in &e.variants {
            let h = v.header.as_ref().unwrap_or_else(|| panic!("{}: {:?}", e.name, v.error));
            assert_eq!(h.version, version, "{}", e.name);
            let data = cat.providers[v.provider].read(&v.key).unwrap();
            if let Some(want) = iwi::expected_size(h) {
                assert_eq!(want, data.len(), "{} ({:?})", e.name, h.format());
            }
            iwi::decode_level(&data, h, 0).unwrap_or_else(|err| panic!("{}: {err:#}", e.name));
            iwi::decode_level(&data, h, h.mip_count() - 1).unwrap();
            n += 1;
        }
    }
    assert!(n >= min_images, "only {n} images");
}

#[test]
fn call_of_duty_4_images_are_version_6_and_decode() {
    check("CODOL_TEST_COD4", 6, 6000);
}

#[test]
fn modern_warfare_2_images_decode() {
    // the iw4x folder also holds a few version 9 files copied from Call of Duty Online, which are skipped by
    // pointing the test at the stock `main` archives only
    let Some(root) = std::env::var_os("CODOL_TEST_MW2").map(PathBuf::from) else { return };
    let cat = source::scan(source::discover(&root), &Progress::default());
    let mut n = 0;
    for e in &cat.entries {
        for v in &e.variants {
            let h = v.header.as_ref().unwrap_or_else(|| panic!("{}: {:?}", e.name, v.error));
            if h.version != 8 {
                continue;
            }
            let data = cat.providers[v.provider].read(&v.key).unwrap();
            if let Some(want) = iwi::expected_size(h) {
                assert_eq!(want, data.len(), "{}", e.name);
            }
            iwi::decode_level(&data, h, 0).unwrap_or_else(|err| panic!("{}: {err:#}", e.name));
            n += 1;
        }
    }
    assert!(n >= 10000, "only {n} version 8 images");
}
