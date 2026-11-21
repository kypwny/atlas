//! Terrain collision of a map zone: the triangle mesh players and bullets collide with where the world is not made of brushes.
//!
//! The collision map keeps it in five arrays: vertices (12 bytes), triangles (three 16-bit vertex numbers), borders (28 bytes: edge
//! planes for capsule traces), partitions (12 bytes: a run of triangles with its borders) and an aabb tree (32 bytes: a box, a
//! material, and either children or - a node with no children - a partition). Every BSP leaf lists the first nodes of the trees
//! that lie in it (`firstCollAabbIndex`, `collAabbCount` of its `cLeaf_t`).

use super::replay::Replay;
use super::Zone;
use anyhow::{bail, Result};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy)]
pub struct Partition {
    pub tri_count: u8,
    pub border_count: u8,
    /// Vertex numbers of the triangles are relative to `first_vert_segment * 1024`.
    pub first_vert_segment: u8,
    pub first_tri: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct AabbNode {
    pub origin: [f32; 3],
    pub half: [f32; 3],
    pub material: u16,
    pub child_count: u16,
    /// With children: the number of the first child (they are consecutive). Without: the partition.
    pub link: u32,
}

#[derive(Debug, Default)]
pub struct Terrain {
    pub verts: Vec<[f32; 3]>,
    pub tris: Vec<[u16; 3]>,
    pub partitions: Vec<Partition>,
    pub nodes: Vec<AabbNode>,
    /// (first node, count) of each BSP leaf.
    pub leaf_ranges: Vec<(u16, u16)>,
    /// Names of the collision map's materials, by number.
    pub materials: Vec<Option<String>>,
}

fn f32_at(d: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}

/// A terrain triangle in world space with its material number.
#[derive(Debug, Clone, Copy)]
pub struct Tri {
    pub p: [[f32; 3]; 3],
    pub material: u16,
    pub partition: u32,
}

pub fn read(zone: &Zone) -> Result<Terrain> {
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("the walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    for i in 0..zone.assets.len() {
        let _ = r.replay_asset(zone, i);
    }
    let Some(c) = r.rec.clips.iter().find(|c| c.tag == 0) else { bail!("this zone has no collision map in the QA / 3.19 layout") };
    let t = r.rec.clip_terrain;
    let d = &zone.data;
    let mut out = Terrain { materials: c.material_names.clone(), ..Default::default() };
    out.verts = (0..t.verts.1).map(|i| [f32_at(d, t.verts.0 + 12 * i), f32_at(d, t.verts.0 + 12 * i + 4), f32_at(d, t.verts.0 + 12 * i + 8)]).collect();
    out.tris = (0..t.tris.1).map(|i| std::array::from_fn(|k| u16_at(d, t.tris.0 + 6 * i + 2 * k))).collect();
    out.partitions = (0..t.partitions.1)
        .map(|i| {
            let o = t.partitions.0 + 12 * i;
            Partition { tri_count: d[o], border_count: d[o + 1], first_vert_segment: d[o + 2], first_tri: u32_at(d, o + 4) }
        })
        .collect();
    out.nodes = (0..t.aabbs.1)
        .map(|i| {
            let o = t.aabbs.0 + 32 * i;
            AabbNode {
                origin: [f32_at(d, o), f32_at(d, o + 4), f32_at(d, o + 8)],
                material: u16_at(d, o + 12),
                child_count: u16_at(d, o + 14),
                half: [f32_at(d, o + 16), f32_at(d, o + 20), f32_at(d, o + 24)],
                link: u32_at(d, o + 28),
            }
        })
        .collect();
    let (l0, nl) = r.rec.clip_leafs;
    out.leaf_ranges = (0..nl).map(|i| (u16_at(d, l0 + 40 * i), u16_at(d, l0 + 40 * i + 2))).collect();
    Ok(out)
}

impl Terrain {
    /// The partitions reached from the BSP leaves' trees, each once, with the node that holds it.
    pub fn reachable_partitions(&self) -> Vec<(u32, usize)> {
        let mut seen_nodes: HashSet<usize> = HashSet::new();
        let mut seen_parts: HashSet<u32> = HashSet::new();
        let mut out = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for &(first, count) in &self.leaf_ranges {
            stack.extend((first as usize..first as usize + count as usize).filter(|&n| n < self.nodes.len()));
        }
        while let Some(n) = stack.pop() {
            if !seen_nodes.insert(n) {
                continue;
            }
            let node = &self.nodes[n];
            if node.child_count > 0 {
                let first = node.link as usize;
                stack.extend((first..first + node.child_count as usize).filter(|&c| c < self.nodes.len()));
            } else if (node.link as usize) < self.partitions.len() && seen_parts.insert(node.link) {
                out.push((node.link, n));
            }
        }
        out.sort_unstable();
        out
    }

    /// The triangles of one partition, in world space.
    pub fn partition_triangles(&self, p: u32) -> Option<Vec<[[f32; 3]; 3]>> {
        let part = self.partitions.get(p as usize)?;
        let base = part.first_vert_segment as usize * 1024;
        (0..part.tri_count as usize)
            .map(|k| {
                let t = self.tris.get(part.first_tri as usize + k)?;
                let v = |i: u16| self.verts.get(base + i as usize).copied();
                Some([v(t[0])?, v(t[1])?, v(t[2])?])
            })
            .collect()
    }

    /// Every terrain triangle reachable from the BSP, with its material.
    pub fn triangles(&self) -> Vec<Tri> {
        let mut out = Vec::new();
        for (p, node) in self.reachable_partitions() {
            if let Some(tris) = self.partition_triangles(p) {
                out.extend(tris.into_iter().map(|p3| Tri { p: p3, material: self.nodes[node].material, partition: p }));
            }
        }
        out
    }

    /// Wavefront OBJ text of the terrain (one group per material).
    pub fn to_obj(&self) -> String {
        use std::fmt::Write;
        let mut tris = self.triangles();
        tris.sort_by_key(|t| t.material);
        let mut s = String::from("# terrain collision\n");
        let mut n = 0usize;
        let mut group = u32::MAX;
        for t in &tris {
            if group != t.material as u32 {
                group = t.material as u32;
                let name = self.materials.get(t.material as usize).cloned().flatten().unwrap_or_else(|| format!("material{}", t.material));
                let _ = writeln!(s, "g {name}");
            }
            for v in t.p {
                let _ = writeln!(s, "v {} {} {}", v[0], v[1], v[2]);
            }
            let _ = writeln!(s, "f {} {} {}", n + 1, n + 2, n + 3);
            n += 3;
        }
        s
    }
}
