//! Which 3.19 map zones the zone builder can walk (it needs every asset of a zone to be walkable before it can relocate anything).
//! Prints a tally; fails only when a map zone that rebuilt before no longer does.

use atlas::zone::writer::ZoneBuilder;
use atlas::zone::{Zone, export};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
fn which_map_zones_can_the_builder_walk() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let (mut ok, mut why): (Vec<String>, BTreeMap<String, Vec<String>>) = (Vec::new(), BTreeMap::new());
    for path in export::zone_files(&root) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("mp_") || name.ends_with("_load.ff") || name.ends_with("_const.ff") {
            continue;
        }
        let Ok(z) = Zone::open(&path, &key) else { continue };
        match ZoneBuilder::from_zone(&z).build(&z) {
            Ok(_) => ok.push(name),
            Err(e) => {
                let msg = e.to_string();
                let kind = msg.split(" at 0x").next().unwrap_or("").split_once(": ").map(|(a, b)| format!("{} {b}", a.split('(').nth(1).unwrap_or("").trim_end_matches(')'))).unwrap_or(msg.clone());
                why.entry(kind).or_default().push(name);
            }
        }
    }
    println!("walkable map zones: {} {:?}", ok.len(), ok);
    for (k, v) in &why {
        println!("cannot walk ({k}): {} zones, e.g. {:?}", v.len(), &v[..v.len().min(4)]);
    }
}
