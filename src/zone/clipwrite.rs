//! Rewriting the brushes of a map zone's collision map: brushes added, deleted or changed in a `.map` go back into the zone.
//!
//! The collision asset is one stream: the header, the material list, then the brush arrays of the first `ClipInfo` (sides and the
//! planes they carry inline, edges, the leaf-brush tree nodes with their brush lists, the brush records, bounds and contents),
//! then everything else (a second `ClipInfo` header, static models, the BSP, terrain, models, entities). The rewrite touches only
//! that run of brush arrays and keeps everything else byte for byte:
//!
//! * Old brushes stay where they are, with their sides, planes and edges. A deleted brush gets contents 0 and is left out of
//!   every tree; a changed brush is a deleted one plus a new one.
//! * New brushes are appended: sides with their own inline planes, bounds, contents; no edge data (box traces against their
//!   slanted edges are a little less exact than the compiler's).
//! * Every spatial tree is built again (one per BSP leaf, one per brush model) from the brushes that touch the leaf or belong to the
//!   model, with [`colltree::Tree`]. The BSP and the terrain keep their shape.
//!
//! The result is a new asset blob and the [`Rebuilt`] description the zone builder needs: which old allocation became which new one,
//! and which pointer words of new brush records point at the new sides array.

use super::colltree::{Aabb, Tree};
use super::mapscene::brush_polygons;
use super::mapsrc::{Brush, MapSource, Side, NO_ID};
use super::replay::Replay;
use super::writer::Rebuilt;
use super::Zone;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;

pub struct Rebuild {
    pub blob: Vec<u8>,
    pub rebuilt: Rebuilt,
    /// Planes (normal, distance) to append to the world's plane array: the collision map's sides point into that shared array.
    pub new_planes: Vec<[f32; 4]>,
    pub notes: Vec<String>,
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rdf(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// The same brush, as far as a `.map`'s three decimals can tell.
pub fn same_brush(a: &Brush, b: &Brush) -> bool {
    a.sides.len() == b.sides.len()
        && a.sides.iter().zip(&b.sides).all(|(x, y)| (0..3).all(|k| (x.normal[k] - y.normal[k]).abs() <= 0.002) && (x.dist - y.dist).abs() <= 0.06)
}

/// Which brushes of `old` are kept (matched to a brush of `new`), and which brushes of `new` are not in `old`.
fn align(old: &[Brush], new: &[Brush]) -> (Vec<bool>, Vec<usize>) {
    let (mut kept, mut added) = (vec![false; old.len()], Vec::new());
    let (mut i, mut j) = (0usize, 0usize);
    while i < old.len() && j < new.len() {
        if same_brush(&old[i], &new[j]) {
            kept[i] = true;
            i += 1;
            j += 1;
            continue;
        }
        // an insertion, a deletion, or a brush that was changed (both)
        let ahead_new = (1..=64).find(|d| j + d < new.len() && same_brush(&old[i], &new[j + d]));
        let ahead_old = (1..=64).find(|d| i + d < old.len() && same_brush(&old[i + d], &new[j]));
        match (ahead_new, ahead_old) {
            (Some(d), _) if ahead_old.map_or(true, |o| d <= o) => {
                added.extend(j..j + d);
                j += d;
            }
            (_, Some(d)) => i += d,
            _ => {
                added.push(j);
                i += 1;
                j += 1;
            }
        }
    }
    added.extend(j..new.len());
    (kept, added)
}

struct NewBrush {
    sides: Vec<Side>,
    aabb: Aabb,
    contents: u32,
}

fn hull_box(b: &Brush) -> Option<Aabb> {
    let polys = brush_polygons(b);
    if polys.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in polys.iter().flat_map(|(_, v)| v) {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    Some(Aabb { lo: std::array::from_fn(|k| lo[k] as f32), hi: std::array::from_fn(|k| hi[k] as f32) })
}

/// Rewrites the collision brushes of `blob` (the collision asset of `zone`, possibly with static model and entity edits applied)
/// so that they match `edited`. `None` when the brushes did not change.
pub fn rebuild(zone: &Zone, blob: &[u8], orig: &MapSource, edited: &MapSource) -> Result<Option<Rebuild>> {
    // 1. which brushes stay, go and come
    let mut groups: Vec<(Option<u32>, &[Brush], &[Brush])> = vec![(None, &orig.world, &edited.world)];
    let empty: Vec<Brush> = Vec::new();
    for (idx, old) in &orig.submodels {
        let new = edited.submodels.iter().find(|(i, _)| i == idx).map(|(_, b)| b).unwrap_or(&empty);
        groups.push((Some(*idx), old, new));
    }
    if let Some((idx, _)) = edited.submodels.iter().find(|(i, b)| !b.is_empty() && !orig.submodels.iter().any(|(j, _)| j == i)) {
        bail!("brush model {idx} has brushes in the map but none in the zone: adding brushes to a model that has none is not supported yet");
    }
    let mut deleted: Vec<u32> = Vec::new();
    let mut added: Vec<(Option<u32>, Brush)> = Vec::new();
    for (model, old, new) in &groups {
        let (kept, add) = align(old, new);
        deleted.extend(old.iter().zip(&kept).filter(|(_, k)| !**k).map(|(b, _)| b.id));
        added.extend(add.into_iter().map(|j| (*model, new[j].clone())));
    }
    if deleted.is_empty() && added.is_empty() {
        return Ok(None);
    }

    // 2. the old collision map as the walker sees it
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("the walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    r.enable_tracing();
    for i in 0..zone.assets.len() {
        r.replay_asset(zone, i).map_err(|f| anyhow::anyhow!("asset {i}: {} at {:#x}", f.what, f.at))?;
    }
    let c = r.rec.clips.iter().find(|c| c.tag == 0).context("no collision map")?.clone();
    let ai = zone.assets.iter().position(|a| c.header >= a.start && c.header < a.start + a.extent).context("the collision map lies in no asset")?;
    let base = zone.assets[ai].start;
    let rel = |o: usize| o - base;
    let (ns, nb, nn) = (c.sides.1, c.brushes.1, c.lb_nodes.1);
    if c.bounds == 0 || c.contents == 0 || c.sides.0 == 0 || c.lb_nodes.0 == 0 || c.brushes.0 == 0 {
        bail!("the collision map's brush arrays are not inline; cannot rewrite them");
    }

    // the run of brush arrays, in the order the loader reads them
    let sides_end = c.sides.0 + 8 * ns;
    let edges_start = if c.edges.1 > 0 { c.edges.0 } else { c.lb_nodes.0 };
    let edges_end = if c.edges.1 > 0 { c.edges.0 + c.edges.1 } else { edges_start };
    let nodes_end = c.lb_nodes.0 + 20 * nn;
    let brushes_end = c.brushes.0 + 36 * nb;
    let (bounds_end, contents_end) = (c.bounds + 24 * nb, c.contents + 4 * nb);
    if !(sides_end <= edges_start && edges_end == c.lb_nodes.0 && nodes_end <= c.brushes.0 && brushes_end <= c.bounds && bounds_end == c.contents) {
        bail!("the brush arrays of this collision map are not laid out as expected");
    }
    let (r0, r1) = (c.sides.0, contents_end);
    let (leaf0, leaf_n) = r.rec.clip_leafs;
    let (cm0, cm_n) = r.rec.cmodels;
    if leaf0 < r1 || cm0 < r1 || (r.rec.clip_pinfo != 0 && r.rec.clip_pinfo < r1) {
        bail!("the leaves or models of this collision map lie inside the brush arrays");
    }

    // allocations of this asset, in order: (offset, length)
    let allocs: Vec<(usize, usize)> = r.block3_allocations().iter().zip(r.block3_allocation_info()).filter(|(x, _)| x.0 == ai).map(|(x, i)| (i.0, x.2)).collect();
    let k0 = allocs.iter().filter(|a| a.0 < r0).count();

    // 3. old brush boxes and contents (from the blob), world and model membership
    let old_box = |id: usize| -> Aabb {
        let o = rel(c.bounds) + 24 * id;
        let (m, h): ([f32; 3], [f32; 3]) = (std::array::from_fn(|k| rdf(blob, o + 4 * k)), std::array::from_fn(|k| rdf(blob, o + 12 + 4 * k)));
        Aabb { lo: std::array::from_fn(|k| m[k] - h[k]), hi: std::array::from_fn(|k| m[k] + h[k]) }
    };
    let old_contents = |id: usize| rd32(blob, rel(c.contents) + 4 * id);
    let mut world_members: Vec<usize> = orig.world.iter().map(|b| b.id as usize).filter(|id| !deleted.contains(&(*id as u32))).collect();
    let mut model_members: Vec<(u32, Vec<usize>)> =
        orig.submodels.iter().map(|(i, bs)| (*i, bs.iter().map(|b| b.id as usize).filter(|id| !deleted.contains(&(*id as u32))).collect())).collect();

    // 4. the new brushes: boxes, sides, ids
    let mut new_brushes: Vec<NewBrush> = Vec::new();
    for (model, b) in &added {
        let aabb = hull_box(b).with_context(|| "a new brush's planes enclose no volume".to_string())?;
        let id = nb + new_brushes.len();
        match model {
            None => world_members.push(id),
            Some(m) => model_members.iter_mut().find(|(i, _)| i == m).unwrap().1.push(id),
        }
        new_brushes.push(NewBrush { sides: b.sides.clone(), aabb, contents: if b.contents != 0 && b.contents != 1 { b.contents } else { 1 } });
    }
    let total = nb + new_brushes.len();
    if total >= 65535 {
        bail!("{total} brushes do not fit the 16-bit brush numbers of the leaf-brush trees");
    }
    let mut boxes: Vec<Aabb> = (0..nb).map(old_box).collect();
    boxes.extend(new_brushes.iter().map(|n| n.aabb));
    let mut contents: Vec<u32> = (0..nb).map(old_contents).collect();
    for id in &deleted {
        contents[*id as usize] = 0;
    }
    contents.extend(new_brushes.iter().map(|n| n.contents));

    // 5. the trees: one per BSP leaf with brushes in it, one per brush model; node 0 is an empty node ("no tree")
    let mut nodes: Vec<super::colltree::Node> = vec![Default::default()];
    let mut cache: HashMap<Vec<u16>, usize> = HashMap::new();
    let mut tree_for = |members: Vec<u16>, nodes: &mut Vec<super::colltree::Node>| -> (usize, u32) {
        if members.is_empty() {
            return (0, 0);
        }
        let all = members.iter().fold(0u32, |c, &b| c | contents[b as usize]);
        if let Some(&root) = cache.get(&members) {
            return (root, all);
        }
        let t = Tree::build(&boxes, &contents, &members, 4);
        let root = nodes.len();
        nodes.extend(t.nodes);
        cache.insert(members, root);
        (root, all)
    };
    let world_ids: Vec<u16> = world_members.iter().map(|&i| i as u16).collect();
    let mut leaf_edits: Vec<(usize, usize, u32)> = Vec::new(); // (leaf, root, contents)
    for i in 0..leaf_n {
        let o = rel(leaf0) + 40 * i;
        let (mid, half): ([f32; 3], [f32; 3]) = (std::array::from_fn(|k| rdf(blob, o + 12 + 4 * k)), std::array::from_fn(|k| rdf(blob, o + 24 + 4 * k)));
        let leaf = Aabb { lo: std::array::from_fn(|k| mid[k] - half[k] - 0.01), hi: std::array::from_fn(|k| mid[k] + half[k] + 0.01) };
        let members: Vec<u16> = world_ids.iter().copied().filter(|&b| boxes[b as usize].touches(&leaf)).collect();
        let (root, all) = tree_for(members, &mut nodes);
        leaf_edits.push((i, root, all));
    }
    let mut model_edits: Vec<(u32, usize, u32)> = Vec::new();
    for (m, members) in &model_members {
        let (root, all) = tree_for(members.iter().map(|&i| i as u16).collect(), &mut nodes);
        model_edits.push((*m, root, all));
    }
    if nodes.len() > i32::MAX as usize {
        bail!("too many tree nodes");
    }

    // 6. the new run of arrays
    let old_blob_sides = &blob[rel(c.sides.0)..rel(sides_end)];
    let old_planes = &blob[rel(sides_end)..rel(edges_start)];
    let old_edges = &blob[rel(edges_start)..rel(edges_end)];
    let old_brush_records = &blob[rel(c.brushes.0)..rel(brushes_end)];
    let old_brush_inline = &blob[rel(brushes_end)..rel(c.bounds)];
    let old_bounds = &blob[rel(c.bounds)..rel(bounds_end)];

    let materials = &c.material_names;
    let material_of = |name: &str| -> i16 {
        materials.iter().position(|m| m.as_deref().map_or(false, |m| m.eq_ignore_ascii_case(name))).or_else(|| materials.iter().position(|m| m.as_deref() == Some("caulk"))).unwrap_or(0) as i16
    };
    let (mut new_sides, mut new_records, mut new_bounds, mut new_contents) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    // the shared plane array: its address (the collision map's plane pointer) and how many planes it holds now
    let plane_base = rd32(blob, rel(c.header) + 12);
    let plane_count0 = rd32(blob, rel(c.header) + 8) as usize;
    let mut added_planes: Vec<[f32; 4]> = Vec::new();
    let mut fixed: Vec<(usize, u32)> = Vec::new();
    let mut side_count = ns;
    let mut link_sites: Vec<(usize, usize)> = Vec::new(); // (record offset inside the new records, first side index)
    for (k, nbk) in new_brushes.iter().enumerate() {
        let (lo, hi) = (nbk.aabb.lo, nbk.aabb.hi);
        // the six implicit axial sides come from the bounds; a plane that coincides with one is not stored again
        let mut axial = [[material_of("caulk"); 3]; 2];
        let mut stored: Vec<&Side> = Vec::new();
        for s in &nbk.sides {
            let mut hit = None;
            for a in 0..3 {
                let mut n = [0.0f64; 3];
                n[a] = 1.0;
                if (s.normal[a] - 1.0).abs() < 1e-3 && (s.dist - hi[a] as f64).abs() < 0.05 {
                    hit = Some((1usize, a));
                }
                if (s.normal[a] + 1.0).abs() < 1e-3 && (s.dist + lo[a] as f64).abs() < 0.05 {
                    hit = Some((0usize, a));
                }
                let _ = n;
            }
            match hit {
                Some((side, a)) => axial[side][a] = material_of(&s.material),
                None => stored.push(s),
            }
        }
        let first = side_count;
        for (si, s) in stored.iter().enumerate() {
            let plane = [s.normal[0] as f32, s.normal[1] as f32, s.normal[2] as f32, s.dist as f32];
            let at = added_planes.iter().position(|p| *p == plane).unwrap_or_else(|| {
                added_planes.push(plane);
                added_planes.len() - 1
            });
            // the word's value is final: the plane array starts where it always did
            fixed.push((rel(r0) + 8 * (first + si), plane_base + 20 * (plane_count0 + at) as u32));
            new_sides.extend_from_slice(&plane_base.to_le_bytes());
            new_sides.extend_from_slice(&(material_of(&s.material) as u16).to_le_bytes());
            new_sides.extend_from_slice(&[0, 0]);
        }
        side_count += stored.len();
        // brush record: numsides, glass piece, sides (link), base adjacent side, axial materials, first offsets, edge counts
        let at = new_records.len();
        new_records.extend_from_slice(&(stored.len() as u16).to_le_bytes());
        new_records.extend_from_slice(&0u16.to_le_bytes());
        new_records.extend_from_slice(&0x3000_0001u32.to_le_bytes());
        new_records.extend_from_slice(&0u32.to_le_bytes());
        for side in 0..2 {
            for a in 0..3 {
                new_records.extend_from_slice(&axial[side][a].to_le_bytes());
            }
        }
        new_records.extend_from_slice(&[0u8; 12]);
        debug_assert_eq!(new_records.len() - at, 36);
        link_sites.push((at, first));
        let mid: [f32; 3] = std::array::from_fn(|a| (lo[a] + hi[a]) * 0.5);
        let half: [f32; 3] = std::array::from_fn(|a| (hi[a] - lo[a]) * 0.5);
        for v in mid.iter().chain(half.iter()) {
            new_bounds.extend_from_slice(&v.to_le_bytes());
        }
        new_contents.extend_from_slice(&nbk.contents.to_le_bytes());
        let _ = k;
    }
    let new_plane_allocs = 0usize;

    // nodes and their inline brush lists
    let (mut node_bytes, mut list_bytes, mut lists) = (Vec::with_capacity(nodes.len() * 20), Vec::new(), 0usize);
    for n in &nodes {
        node_bytes.push(n.axis);
        node_bytes.push(0);
        node_bytes.extend_from_slice(&n.count.to_le_bytes());
        node_bytes.extend_from_slice(&n.contents.to_le_bytes());
        if n.count > 0 {
            node_bytes.extend_from_slice(&u32::MAX.to_le_bytes());
            node_bytes.extend_from_slice(&[0u8; 8]);
            for b in &n.brushes {
                list_bytes.extend_from_slice(&b.to_le_bytes());
            }
            lists += 1;
        } else {
            node_bytes.extend_from_slice(&n.dist.to_le_bytes());
            node_bytes.extend_from_slice(&n.range.to_le_bytes());
            node_bytes.extend_from_slice(&n.child[0].to_le_bytes());
            node_bytes.extend_from_slice(&n.child[1].to_le_bytes());
        }
    }
    // the lists must follow the node array in node order, each as its own allocation: they are, `lists` of them
    let keep_trees = std::env::var_os("ATLAS_CLIP_KEEPTREES").is_some();
    if keep_trees {
        node_bytes = blob[rel(c.lb_nodes.0)..rel(nodes_end)].to_vec();
        list_bytes = blob[rel(nodes_end)..rel(c.brushes.0)].to_vec();
        lists = allocs.iter().filter(|a| a.0 >= nodes_end && a.0 < c.brushes.0).count();
    }

    // contents of the old brushes: deleted ones become 0
    let mut old_contents_bytes = blob[rel(c.contents)..rel(contents_end)].to_vec();
    for id in &deleted {
        put32(&mut old_contents_bytes, 4 * *id as usize, 0);
    }

    let mut region: Vec<u8> = Vec::new();
    let mut region_alloc_starts: Vec<(&'static str, usize)> = Vec::new(); // (kind, offset in the region)
    region_alloc_starts.push(("sides", region.len()));
    region.extend_from_slice(old_blob_sides);
    region.extend_from_slice(&new_sides);
    let planes_at = region.len();
    region.extend_from_slice(old_planes);
    let edges_at = region.len();
    region.extend_from_slice(old_edges);
    let nodes_at = region.len();
    region.extend_from_slice(&node_bytes);
    region.extend_from_slice(&list_bytes);
    let brushes_at = region.len();
    region.extend_from_slice(old_brush_records);
    let new_records_at = region.len();
    region.extend_from_slice(&new_records);
    region.extend_from_slice(old_brush_inline);
    let bounds_at = region.len();
    region.extend_from_slice(old_bounds);
    region.extend_from_slice(&new_bounds);
    region.extend_from_slice(&old_contents_bytes);
    region.extend_from_slice(&new_contents);
    let _ = (planes_at, edges_at, nodes_at, brushes_at, bounds_at);

    // 7. the new blob: head, region, tail
    let (head, tail) = (&blob[..rel(r0)], &blob[rel(r1)..]);
    let mut out = Vec::with_capacity(head.len() + region.len() + tail.len());
    out.extend_from_slice(head);
    out.extend_from_slice(&region);
    out.extend_from_slice(tail);
    let delta = region.len() as i64 - (r1 - r0) as i64;
    let shifted = |o: usize| -> usize { (rel(o) as i64 + delta) as usize };

    // the clipMap header's ClipInfo, and the second one's header (a mirror of it)
    let plane_total = (plane_count0 + added_planes.len()) as u32;
    let fix_info = |out: &mut Vec<u8>, at: usize| {
        put32(out, at, plane_total);
        put32(out, at + 16, side_count as u32);
        if !keep_trees {
            put32(out, at + 32, nodes.len() as u32);
            put32(out, at + 40, 0);
            put32(out, at + 44, 0);
        }
        out[at + 48..at + 50].copy_from_slice(&(total as u16).to_le_bytes());
    };
    fix_info(&mut out, rel(c.header) + 8);
    if r.rec.clip_pinfo != 0 {
        fix_info(&mut out, shifted(r.rec.clip_pinfo));
    }
    // the BSP leaves: tree root and contents
    for (i, root, all) in leaf_edits.iter().filter(|_| !keep_trees) {
        let o = shifted(leaf0) + 40 * i;
        put32(&mut out, o + 4, *all);
        put32(&mut out, o + 36, *root as u32);
    }
    // brush models: model 0 is the world and has no tree of its own
    for (m, root, all) in model_edits.iter().filter(|_| !keep_trees) {
        if (*m as usize) < cm_n && *m > 0 {
            let o = shifted(cm0) + 72 * *m as usize;
            put32(&mut out, o + 36, *all);
            put32(&mut out, o + 68, *root as u32);
        }
    }

    // 8. which old allocation became which new one
    let classify = |off: usize| -> &'static str {
        if off == c.sides.0 {
            "sides"
        } else if off < edges_start {
            "plane"
        } else if off < c.lb_nodes.0 {
            "edges"
        } else if off == c.lb_nodes.0 {
            "nodes"
        } else if off < c.brushes.0 {
            "list"
        } else if off == c.brushes.0 {
            "brushes"
        } else if off < c.bounds {
            "inline"
        } else if off == c.bounds {
            "bounds"
        } else {
            "contents"
        }
    };
    let old_region: Vec<(usize, &'static str)> = allocs.iter().enumerate().filter(|(_, a)| a.0 >= r0 && a.0 < r1).map(|(i, a)| (i, classify(a.0))).collect();
    let old_region_n = old_region.len();
    let count_of = |kind: &str| old_region.iter().filter(|(_, k)| *k == kind).count();
    let (old_planes_n, old_inline_n) = (count_of("plane"), count_of("inline"));
    // new allocation indices: sides, planes (old then new), edges, nodes, lists, brushes, inline, bounds, contents
    let mut next = k0;
    let new_sides_idx = next;
    next += 1;
    let new_plane_base = next;
    next += old_planes_n + new_plane_allocs;
    let new_edges_idx = (count_of("edges") > 0).then(|| {
        let i = next;
        next += 1;
        i
    });
    let new_nodes_idx = next;
    next += 1;
    let new_lists_base = next;
    next += lists;
    let new_brushes_idx = next;
    next += 1;
    let new_inline_base = next;
    next += old_inline_n;
    let new_bounds_idx = next;
    next += 1;
    let new_contents_idx = next;
    next += 1;
    let new_region_n = next - k0;
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for i in 0..k0 {
        pairs.push((i, i));
    }
    let (mut pi, mut ii, mut li) = (0usize, 0usize, 0usize);
    for (i, kind) in &old_region {
        match *kind {
            "sides" => pairs.push((*i, new_sides_idx)),
            "plane" => {
                pairs.push((*i, new_plane_base + pi));
                pi += 1;
            }
            "edges" => pairs.push((*i, new_edges_idx.unwrap())),
            "nodes" => pairs.push((*i, new_nodes_idx)),
            "list" if keep_trees => {
                pairs.push((*i, new_lists_base + li));
                li += 1;
            }
            "list" => {}
            "brushes" => pairs.push((*i, new_brushes_idx)),
            "inline" => {
                pairs.push((*i, new_inline_base + ii));
                ii += 1;
            }
            "bounds" => pairs.push((*i, new_bounds_idx)),
            _ => pairs.push((*i, new_contents_idx)),
        }
    }
    for (i, a) in allocs.iter().enumerate() {
        if a.0 >= r1 {
            pairs.push((i, i + new_region_n - old_region_n));
        }
    }
    // pointer words of the new brush records: their sides
    let links: Vec<(usize, usize, usize)> =
        link_sites.iter().map(|(at, first)| (rel(r0) + new_records_at + at + 4, new_sides_idx, 8 * first)).collect();
    let notes = vec![format!(
        "collision brushes: {} deleted, {} added; {} tree nodes in {} trees; {} brushes, {} sides",
        deleted.len(),
        new_brushes.len(),
        nodes.len(),
        cache.len(),
        total,
        side_count
    )];
    let _ = NO_ID;
    Ok(Some(Rebuild { blob: out, rebuilt: Rebuilt { pairs, links, fixed }, new_planes: added_planes, notes }))
}
