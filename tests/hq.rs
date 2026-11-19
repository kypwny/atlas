//! Call of Duty HQ (`cod26`) containers. The tests that read files need the folder `Call of Duty HQ` (the one that has `cod26`
//! and `oo2core_8_win64.dll`) in `CODOL_TEST_HQ` and are skipped without it. `CODOL_TEST_HQ_ALL=1` opens every `.ff` there.

use atlas::kapi::{self, Kapi, Kind};
use atlas::oodle::Oodle;
use atlas::zone::hq::{self, Refused};
use std::path::PathBuf;

fn root() -> Option<PathBuf> {
    std::env::var_os("CODOL_TEST_HQ").map(PathBuf::from)
}

fn oodle(root: &std::path::Path) -> Oodle {
    let dll = Oodle::find(&root.join("cod26")).expect("oo2core_*_win64.dll next to the game");
    Oodle::load(&dll).unwrap()
}

#[test]
fn other_flavours_are_refused() {
    let mut dlc = b"IWff0100".to_vec();
    dlc.extend_from_slice(&[0u8; 64]);
    let e = hq::open_bytes(&dlc, None).err().unwrap();
    assert!(e.downcast_ref::<Refused>().is_some(), "{e:#}");
    let mut patch = b"IWffd100".to_vec();
    patch.extend_from_slice(&[0u8; 64]);
    let e = hq::open_bytes(&patch, None).err().unwrap();
    assert!(e.downcast_ref::<Refused>().is_some(), "{e:#}");
    // a version the reader does not know
    let mut v = b"IWffa100".to_vec();
    v.extend_from_slice(&26u32.to_le_bytes());
    v.extend_from_slice(&[0u8; 64]);
    assert!(hq::open_bytes(&v, None).err().unwrap().downcast_ref::<Refused>().is_some());
}

#[test]
fn high_entropy_blocks_are_refused_not_failed() {
    // a header and signature section as in the real files, then data that does not parse as blocks and looks random
    let mut b = vec![0u8; 0x100];
    b[..8].copy_from_slice(b"IWffa100");
    b[8..12].copy_from_slice(&25u32.to_le_bytes());
    b.extend_from_slice(&[0x02, 0x49, 0x57, 0x43]);
    b.extend_from_slice(b"IWffs100");
    b.resize(0x100 + 4 + 0x8000, 0);
    b.extend_from_slice(&0x1000u32.to_le_bytes());
    b.extend_from_slice(&0x0601_0000u32.to_le_bytes());
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..8192 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        b.push((x >> 24) as u8);
    }
    let e = hq::open_bytes(&b, None).err().unwrap();
    assert!(e.downcast_ref::<Refused>().is_some(), "{e:#}");
}

#[test]
fn small_zone_has_its_asset_list() {
    let Some(root) = root() else { return };
    let o = oodle(&root);
    let z = hq::open(&root.join("cod26/code_pre_gfx.ff"), Some(&o)).unwrap();
    assert_eq!((z.blocks, z.signature_gaps, z.image.len(), z.trailer), (22, 0, 1_382_674, 0));
    assert_eq!(z.signature_name.as_deref(), Some("code_pre_gfx"));
    let l = z.asset_list().unwrap();
    assert_eq!(l.count, 110);
    assert_eq!(l.histogram().len(), 12);
    assert!(l.records.iter().all(|(_, p)| *p == u64::MAX));
    let s = hq::find_strings(&z.image, 4);
    assert!(s.zlib_streams >= 60 && s.zlib_text >= 40, "{s:?}");
}

#[test]
fn zone_with_signature_gap_opens() {
    let Some(root) = root() else { return };
    let o = oodle(&root);
    let p = root.join("cod26/mp_rew_sierra_poi_vulcan_e2b739af_0_tr.ff");
    if !p.exists() {
        return;
    }
    let z = hq::open(&p, Some(&o)).unwrap();
    assert_eq!(z.image.len(), 0x2091684);
    assert_eq!((z.blocks, z.signature_gaps), (522, 1));
    assert!(z.notes.is_empty(), "{:?}", z.notes);
}

#[test]
fn zones_open() {
    let Some(root) = root() else { return };
    let o = oodle(&root);
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("cod26")).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "ff")).collect();
    files.sort();
    assert!(files.len() > 1000, "{} zones", files.len());
    if std::env::var_os("CODOL_TEST_HQ_ALL").is_none() {
        files = files.into_iter().step_by(40).collect();
    }
    for f in files {
        let z = hq::open(&f, Some(&o)).unwrap_or_else(|e| panic!("{}: {e:#}", f.display()));
        let l = z.asset_list().unwrap_or_else(|e| panic!("{}: {e:#}", f.display()));
        assert!(l.count > 0, "{}", f.display());
        assert!(z.notes.is_empty(), "{}: {:?}", f.display(), z.notes);
    }
}

#[test]
fn small_xsub_decodes_completely() {
    let Some(root) = root() else { return };
    let o = oodle(&root);
    let k = Kapi::open(&root.join("cod26/boot-00000.xsub")).unwrap();
    assert_eq!((k.kind, k.entries.len()), (Kind::Sub, 38));
    let mut stored = 0;
    for e in &k.entries {
        let v = k.read_entry(e, Some(&o)).unwrap_or_else(|err| panic!("{:#018x}: {err:#}", e.key));
        assert_eq!(v.len(), e.ex as usize);
        stored += !e.has_blocks() as usize;
    }
    assert!(stored > 0 && stored < 38);
    // an entry of one block: the header says so and the decoded size matches the table
    let e = k.entries.iter().find(|e| e.has_blocks() && e.ex == 0x2800).unwrap();
    let raw = k.read_raw(e).unwrap();
    let info = kapi::payload_info(&raw, e).unwrap();
    assert_eq!((info.segments, info.blocks, info.codecs[6]), (1, 1, 1));
}

#[test]
fn xpak_agrees_with_its_xsub() {
    let Some(root) = root() else { return };
    let o = oodle(&root);
    let k = Kapi::open(&root.join("cod26/codhq.xpak")).unwrap();
    assert_eq!((k.kind, k.entries.len(), k.subs.len()), (Kind::Pak, 40954, 10));
    // entries of xsub 4 (100 MB): the xpak and the xsub name the same payload; decode a few
    let sub = Kapi::open(&root.join("cod26/codhq-00004.xsub")).unwrap();
    let by_key: std::collections::HashMap<u64, kapi::Entry> = sub.entries.iter().map(|e| (e.key, *e)).collect();
    let mut checked = 0;
    for e in k.entries.iter().filter(|e| e.sub() == 4) {
        let s = by_key.get(&e.key).expect("xpak entry in the xsub");
        assert_eq!((s.offset(), s.size(), s.ex), (e.offset(), e.size(), e.ex));
        if checked < 20 && e.size() < (8 << 20) {
            assert_eq!(k.read_entry(e, Some(&o)).unwrap().len(), e.ex as usize);
            checked += 1;
        }
    }
    assert_eq!(checked, 20);
}
