//! Diagnostic: the alias words of a zone whose value lies in no block-3 allocation (what the writer calls "lost").
use atlas::zone::{Zone, export};
use std::path::PathBuf;

#[test]
fn alias_words_outside_every_allocation() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let name = std::env::var("CODOL_GAP_ZONE").unwrap_or_else(|_| "sdp_addon_mp_cross_fire.ff".to_string());
    let path = export::zone_files(&root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == name).unwrap();
    let z = Zone::open(&path, &key).unwrap();
    let mut r = atlas::zone::replay::Replay::new(&z).unwrap();
    r.enable_tracing();
    for i in 0..z.assets.len() {
        r.replay_asset(&z, i).unwrap();
    }
    let allocs: Vec<(usize, u32, usize)> = r.block3_allocations().to_vec();
    let first = allocs.first().map(|a| a.1).unwrap_or(u32::MAX);
    println!("{name}: {} assets, first allocation {first:#x}, directory {:#x}", z.assets.len(), r.directory_reference);
    for w in r.alias_words() {
        if w.value < first {
            continue;
        }
        let inside = allocs.iter().any(|a| w.value >= a.1 && (w.value - a.1) as usize <= a.2);
        if !inside {
            let a = &z.assets[w.asset];
            let before = allocs.iter().filter(|x| x.1 <= w.value).last();
            println!("  asset {} type {} +{:#x}: {:08x}; nearest allocation before: {:x?}", w.asset, a.asset_type, w.at - a.start, w.value, before);
        }
    }
}
