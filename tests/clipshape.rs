//! Diagnostic: the allocations of a map's collision asset in stream order, grouped by the section they belong to.
use atlas::zone::{Zone, export};
use std::path::PathBuf;

#[test]
fn clip_allocations() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let name = std::env::var("CODOL_GAP_ZONE").unwrap_or_else(|_| "mp_dr_cistern.ff".to_string());
    let path = export::zone_files(&root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == name).unwrap();
    let z = Zone::open(&path, &key).unwrap();
    let mut r = atlas::zone::replay::Replay::new(&z).unwrap();
    r.enable_tracing();
    for i in 0..z.assets.len() {
        r.replay_asset(&z, i).unwrap();
    }
    let c = r.rec.clips.iter().find(|c| c.tag == 0).unwrap();
    let ai = z.assets.iter().position(|a| c.header >= a.start && c.header < a.start + a.extent).unwrap();
    let a = &z.assets[ai];
    let allocs: Vec<((usize, u32, usize), (usize, bool))> = r.block3_allocations().iter().copied().zip(r.block3_allocation_info().iter().copied()).filter(|(x, _)| x.0 == ai).collect();
    println!("clip asset {ai}: {} bytes, {} block-3 allocations", a.extent, allocs.len());
    let sections = [
        ("sides", c.sides.0, c.sides.0 + 8 * c.sides.1),
        ("edges", c.edges.0, c.edges.0 + c.edges.1),
        ("lb nodes", c.lb_nodes.0, c.lb_nodes.0 + 20 * c.lb_nodes.1),
        ("brushes", c.brushes.0, c.brushes.0 + 36 * c.brushes.1),
        ("bounds", c.bounds, c.bounds + 24 * c.brushes.1),
        ("contents", c.contents, c.contents + 4 * c.brushes.1),
    ];
    let mut last = String::new();
    let mut run = 0usize;
    for (k, ((_, addr, len), (at, read))) in allocs.iter().enumerate() {
        let sec = sections
            .iter()
            .find(|s| *at >= s.1 && *at < s.2 && s.1 != s.2)
            .map(|s| s.0)
            .unwrap_or(if *at < c.sides.0 { "header..materials" } else if *at < c.contents { "inside region (other)" } else { "after region" });
        if sec != last {
            if run > 0 {
                println!("   ... {run} allocations");
            }
            println!("[{k}] {sec}: first alloc {addr:#x} len {len} at {} read {read}", at - a.start);
            last = sec.to_string();
            run = 0;
        }
        run += 1;
    }
    println!("   ... {run} allocations");
    let w = |o: usize| u32::from_le_bytes(z.data[c.header + o..c.header + o + 4].try_into().unwrap());
    println!("header words +8..+112: {:x?}", (2..28).map(|k| w(4 * k)).collect::<Vec<_>>());
    println!("pinfo at {} (stream); region sides {} edges {:?} nodes {:?} brushes {:?} bounds {} contents {} end {}", r.rec.clip_pinfo, c.sides.0, c.edges, c.lb_nodes, c.brushes, c.bounds, c.contents, c.contents + 4 * c.brushes.1);
    if r.rec.clip_pinfo != 0 {
        let p = r.rec.clip_pinfo;
        let words: Vec<String> = (0..16).map(|k| format!("{:x}", u32::from_le_bytes(z.data[p + 4 * k..p + 4 * k + 4].try_into().unwrap()))).collect();
        println!("pinfo words: {words:?}");
    }
    let (l0, nl) = r.rec.clip_leafs;
    for i in 0..3.min(nl) {
        let o = l0 + 40 * i;
        let words: Vec<String> = (0..10).map(|k| format!("{:x}", u32::from_le_bytes(z.data[o + 4 * k..o + 4 * k + 4].try_into().unwrap()))).collect();
        println!("leaf {i}: {words:?}");
    }
    let (n0, nn) = r.rec.clip_nodes;
    for i in 0..3.min(nn) {
        let o = n0 + 8 * i;
        let words: Vec<String> = (0..2).map(|k| format!("{:x}", u32::from_le_bytes(z.data[o + 4 * k..o + 4 * k + 4].try_into().unwrap()))).collect();
        println!("node {i}: {words:?}");
    }
}
