//! Diagnostic: how a map's GfxWorld ties surfaces to cells and to the draw order (set CODOL_TEST_319; CODOL_GAP_ZONE picks the map).
use atlas::zone::{Zone, export};
use std::path::PathBuf;

fn u16_at(d: &[u8], o: usize) -> usize {
    u16::from_le_bytes([d[o], d[o + 1]]) as usize
}
fn u32_at(d: &[u8], o: usize) -> usize {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize
}

#[test]
fn world_shape() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let name = std::env::var("CODOL_GAP_ZONE").unwrap_or_else(|_| "mp_dr_cistern.ff".to_string());
    let path = export::zone_files(&root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == name).unwrap();
    let z = Zone::open(&path, &key).unwrap();
    let mut r = atlas::zone::replay::Replay::new(&z).unwrap();
    for i in 0..z.assets.len() {
        let _ = r.replay_asset(&z, i);
    }
    let w = r.rec.world.take().unwrap();
    let d = &z.data;
    let h = w.header;
    let surfaces = u32_at(d, h + 16);
    println!("surfaces {surfaces} (records {}), cells {}, vertices {}, indices {}", w.surfaces.len(), u32_at(d, h + 52), w.vertices.1, w.indices.1);
    // the static dpvs block: models, staticSurfaceCount, noDecal, then six words, then the visibility counts
    let p = h + 980 + 456;
    let words: Vec<usize> = (0..14).map(|i| u32_at(d, p + 4 * i)).collect();
    println!("dpvs words from +456 (late-adjusted): {words:?}");
    println!("sorted array {:?}", w.sorted);
    // the leaf ranges of every cell tree
    let mut cover = vec![0u32; surfaces];
    let (mut nodes, mut leaves, mut max_end) = (0usize, 0usize, 0usize);
    for &(at, n) in &w.cell_nodes {
        for j in 0..n {
            let o = at + 44 * j;
            nodes += 1;
            let (child_count, count, start) = (u16_at(d, o + 24), u16_at(d, o + 26), u16_at(d, o + 28));
            if child_count == 0 {
                leaves += 1;
            }
            if count > 0 && start + count <= surfaces {
                for s in start..start + count {
                    cover[s] += 1;
                }
                max_end = max_end.max(start + count);
            }
        }
    }
    let unc = cover.iter().filter(|&&c| c == 0).count();
    let multi = cover.iter().filter(|&&c| c > 1).count();
    println!("tree nodes {nodes}, leaves {leaves}, highest range end {max_end}, surfaces in no range {unc}, in several {multi}");
    // parent/child: children at index j + childrenOffset / 44 ... + childCount; their runs tile the parent's run (both lists)
    let (mut parents, mut tile_ok, mut nd_ok, mut bound_ok) = (0, 0, 0, 0);
    for &(at, n) in &w.cell_nodes {
        for j in 0..n {
            let o = at + 44 * j;
            let (cc, cnt, st, cnd, snd) = (u16_at(d, o + 24), u16_at(d, o + 26), u16_at(d, o + 28), u16_at(d, o + 30), u16_at(d, o + 32));
            if cc == 0 {
                continue;
            }
            parents += 1;
            let first = j + u32_at(d, o + 40) / 44;
            let kids: Vec<(usize, usize, usize, usize)> = (first..first + cc)
                .filter(|&k| k < n)
                .map(|k| {
                    let q = at + 44 * k;
                    (u16_at(d, q + 26), u16_at(d, q + 28), u16_at(d, q + 30), u16_at(d, q + 32))
                })
                .collect();
            let (sc, lo, hi) = (kids.iter().map(|k| k.0).sum::<usize>(), kids.iter().map(|k| k.1).min().unwrap_or(0), kids.iter().map(|k| k.1 + k.0).max().unwrap_or(0));
            if kids.len() == cc && sc == cnt && lo == st && hi == st + cnt {
                tile_ok += 1;
            }
            let (sc2, lo2, hi2) = (kids.iter().map(|k| k.2).sum::<usize>(), kids.iter().map(|k| k.3).min().unwrap_or(0), kids.iter().map(|k| k.3 + k.2).max().unwrap_or(0));
            if sc2 == cnd && lo2 == snd && hi2 == snd + cnd {
                nd_ok += 1;
            }
            let f = |q: usize, k: usize| f32::from_le_bytes(d[q + 4 * k..q + 4 * k + 4].try_into().unwrap());
            if (first..first + cc).filter(|&k| k < n).all(|k| (0..3).all(|a| (f(at + 44 * k, a) - f(o, a)).abs() + f(at + 44 * k, 3 + a) <= f(o, 3 + a) + 0.01)) {
                bound_ok += 1;
            }
        }
    }
    println!("parents {parents}: surface runs tile {tile_ok}, no-decal runs tile {nd_ok}, child bounds inside {bound_ok}");
    // usage count vs surface count
    println!("usageCount {} for {surfaces} surfaces", u32_at(d, p + 4 + 48));
    // lists that hold surface numbers, and the brush models
    for kind in 0u8..4 {
        let ls: Vec<&(u8, usize, usize)> = w.lists.iter().filter(|l| l.0 == kind).collect();
        let (mut total, mut max, mut ge_static) = (0usize, 0usize, 0usize);
        for &&(k, at, n) in &ls {
            for i in 0..n {
                let v = if k == 0 { u32_at(d, at + 4 * i) } else { u16_at(d, at + 2 * i) };
                total += 1;
                max = max.max(v);
                if v >= words[2] {
                    ge_static += 1;
                }
            }
        }
        println!("list kind {kind}: {} lists, {total} values, max {max}, >= staticSurfaceCount {ge_static}; smodels {} static {} total {surfaces}", ls.len(), words[1], words[2]);
    }
    let (bm, bn) = w.brush_models;
    for i in 0..bn.min(8) {
        let o = bm + 60 * i;
        println!("brush model {i}: surfaces {} start {} noDecal {}", u16_at(d, o + 52), u16_at(d, o + 54), u16_at(d, o + 56));
    }
    println!("brush models {bn}");
    // first nodes of the first cell, raw
    if let Some(&(at, n)) = w.cell_nodes.first() {
        for j in 0..n.min(6) {
            let o = at + 44 * j;
            let f: Vec<f32> = (0..6).map(|k| f32::from_le_bytes(d[o + 4 * k..o + 4 * k + 4].try_into().unwrap())).collect();
            let u: Vec<usize> = (0..8).map(|k| u16_at(d, o + 24 + 2 * k)).collect();
            println!("node {j}: bounds {f:?} u16 {u:?} ptr {:#x} childrenOffset {}", u32_at(d, o + 36), u32_at(d, o + 40));
        }
    }
    // sorted array: monotone in what? print the first ones, and which surface indices it holds
    let (sat, sn) = w.sorted;
    let first: Vec<usize> = (0..sn.min(24)).map(|i| u16_at(d, sat + 2 * i)).collect();
    println!("sorted head {first:?}");
    let half = words[1].min(sn);
    let mut seen = vec![0u32; surfaces];
    for i in 0..half {
        seen[u16_at(d, sat + 2 * i)] += 1;
    }
    println!("first {half} sorted entries: {} distinct of {surfaces}", seen.iter().filter(|&&c| c > 0).count());
    // surface record fields
    for (rec, mat) in w.surfaces.iter().take(5) {
        let v: Vec<usize> = (0..6).map(|k| u32_at(d, rec + 4 * k)).collect();
        println!("surface {v:?} {mat:?}");
    }
}

/// The invariants of the static world hold on every shipped map (the writer's own check is the same list).
#[test]
fn shipped_worlds_pass_the_check() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    for path in export::zone_files(&root) {
        let n = path.file_name().unwrap().to_string_lossy().to_string();
        if !n.starts_with("mp_") {
            continue;
        }
        let Ok(z) = Zone::open(&path, &key) else { continue };
        let Ok(bad) = atlas::zone::worldwrite::check(&z) else { continue };
        println!("{n}: {} violations {:?}", bad.len(), bad.iter().take(3).collect::<Vec<_>>());
    }
}
