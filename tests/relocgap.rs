//! Diagnostic for the zone writer: words that hold the exact start address of a block-3 allocation but that the walker does not
//! record as relocatable. Random data rarely equals an allocation's start, so what this prints are fields the relocation misses.
//! `CODOL_GAP_ZONE` names the zone (default mp_bridge.ff), `CODOL_GAP_FROM_TEXT=1` limits the scan to what follows the entity text.

use atlas::zone::{Zone, export};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

#[test]
fn unrecorded_words_that_hold_allocation_starts() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let name = std::env::var("CODOL_GAP_ZONE").unwrap_or_else(|_| "mp_bridge.ff".to_string());
    let path = export::zone_files(&root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == name).unwrap();
    let z = Zone::open(&path, &key).unwrap();
    let mut r = atlas::zone::replay::Replay::new(&z).unwrap();
    r.enable_tracing();
    for i in 0..z.assets.len() {
        r.replay_asset(&z, i).unwrap();
    }
    let recorded: HashSet<usize> = r.alias_words().iter().map(|w| w.at).collect();
    // allocation start -> (asset, length, stream offset, read)
    let mut starts: HashMap<u32, (usize, usize, usize, bool)> = HashMap::new();
    for (k, &(asset, address, length)) in r.block3_allocations().iter().enumerate() {
        let (at, read) = r.block3_allocation_info()[k];
        if length >= 4 {
            starts.entry(address).or_insert((asset, length, at, read));
        }
    }
    let from = if std::env::var("CODOL_GAP_FROM_TEXT").is_ok() { r.rec.map_ents.iter().map(|e| e.1).max().unwrap_or(0) } else { z.assets[0].start };
    let d = &z.data;
    let mut by_kind: BTreeMap<(u32, u32), Vec<(usize, usize, u32)>> = BTreeMap::new();
    let mut total = 0usize;
    for (i, a) in z.assets.iter().enumerate() {
        let (s, e) = (a.start.max(from), a.start + a.extent);
        if e < s + 4 {
            continue;
        }
        for o in s..e - 3 {
            let v = u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
            if v >> 28 != 3 || recorded.contains(&o) {
                continue;
            }
            if let Some(&(target_asset, _, _, _)) = starts.get(&v) {
                total += 1;
                let target_type = z.assets.get(target_asset).map(|t| t.asset_type).unwrap_or(999);
                by_kind.entry((a.asset_type, target_type)).or_default().push((i, o - a.start, v));
            }
        }
    }
    println!("{name}: {} recorded alias words, {} allocations; {total} unrecorded words hold an allocation start", recorded.len(), starts.len());
    for ((ty, target), list) in &by_kind {
        let same_asset = list.iter().filter(|(i, _, v)| starts[v].0 == *i).count();
        println!("  in type {ty} -> allocation of a type {target} asset: {} words ({} into the same asset); first: {:?}", list.len(), same_asset, &list[..list.len().min(4)].iter().map(|(i, o, v)| format!("asset {i} +{o:#x} = {v:08x} (len {})", starts[v].1)).collect::<Vec<_>>());
    }
}
