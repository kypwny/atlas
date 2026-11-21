//! The collision map's leaf-brush tree: the spatial index the engine walks to find the brushes near a trace or a point.
//! Read from a zone, walked the way the engine walks it (`CM_TestInLeafBrushNode_r`), built again from brush boxes, and
//! checked: every brush must be found by any query box that touches it. A compiler that writes brushes needs the builder; the
//! checker is how both the zone's own trees and a built tree are shown to be sound.
//!
//! Node layout (20 bytes): `axis u8, pad u8, leafBrushCount i16, contents u32`, then either the address of `leafBrushCount`
//! 16-bit brush numbers (a leaf, count > 0) or `dist f32, range f32, childOffset u16[2]` (an interior node). A node whose count
//! is negative is interior *and* has the node behind it as a leaf of brushes that straddle its plane. `childOffset[0]` leads to
//! the side with coordinates above `dist`, `childOffset[1]` to the side below; both are offsets from the node itself.

use super::mapsrc::Resolver;
use super::replay::Replay;
use super::Zone;
use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub lo: [f32; 3],
    pub hi: [f32; 3],
}

impl Aabb {
    pub fn touches(&self, o: &Aabb) -> bool {
        (0..3).all(|k| self.lo[k] <= o.hi[k] && o.lo[k] <= self.hi[k])
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub axis: u8,
    /// > 0: a leaf of `brushes`; < 0: interior with a straddler leaf at the next node; 0: interior.
    pub count: i16,
    pub contents: u32,
    pub dist: f32,
    pub range: f32,
    pub child: [u16; 2],
    pub brushes: Vec<u16>,
}

#[derive(Debug, Default, Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
}

impl Tree {
    /// The brushes the engine would test for a query box, starting at node `root`, for a trace whose contents match everything.
    pub fn query(&self, root: usize, q: &Aabb) -> Vec<u16> {
        let mut out = Vec::new();
        self.visit(root, q, &mut out, 0);
        out
    }

    fn visit(&self, mut i: usize, q: &Aabb, out: &mut Vec<u16>, depth: usize) {
        if depth > 200 {
            return;
        }
        while let Some(n) = self.nodes.get(i) {
            if n.contents == 0 {
                return;
            }
            if n.count > 0 {
                out.extend_from_slice(&n.brushes);
                return;
            }
            if n.count < 0 {
                self.visit(i + 1, q, out, depth + 1);
            }
            let a = n.axis as usize % 3;
            if n.dist >= q.lo[a] {
                if n.dist <= q.hi[a] {
                    self.visit(i + n.child[0] as usize, q, out, depth + 1);
                }
                i += n.child[1] as usize;
            } else {
                i += n.child[0] as usize;
            }
        }
    }

    /// Every brush must be found by queries that touch it: its own box, each of its corners' neighbourhood and the box grown by
    /// `grow` units. Returns the brushes that were missed by some query (with the query kind), at most `limit` of them.
    pub fn check(&self, root: usize, boxes: &[Aabb], members: &[u16], grow: f32, limit: usize) -> Vec<(u16, &'static str)> {
        let mut missed = Vec::new();
        for &b in members {
            let bx = boxes[b as usize];
            let centre: [f32; 3] = std::array::from_fn(|k| (bx.lo[k] + bx.hi[k]) * 0.5);
            let queries = [
                ("own box", bx),
                ("centre point", Aabb { lo: centre, hi: centre }),
                ("low corner", Aabb { lo: bx.lo, hi: bx.lo }),
                ("high corner", Aabb { lo: bx.hi, hi: bx.hi }),
                ("grown box", Aabb { lo: std::array::from_fn(|k| bx.lo[k] - grow), hi: std::array::from_fn(|k| bx.hi[k] + grow) }),
            ];
            for (kind, q) in queries {
                if !self.query(root, &q).contains(&b) {
                    missed.push((b, kind));
                    break;
                }
            }
            if missed.len() >= limit {
                break;
            }
        }
        missed
    }

    /// Builds a tree over `boxes`: a median split on the axis along which the box centres spread most; brushes that lie on
    /// both sides of the plane go to the straddler leaf behind the node. `range` is left at 0, which makes a swept trace
    /// test both sides near the plane (always correct, a little slower than the compiler's own tighter value). Returns the
    /// tree and the index of its root (always 0). `members` lists the brushes of this tree.
    pub fn build(boxes: &[Aabb], contents: &[u32], members: &[u16], leaf_size: usize) -> Tree {
        let mut t = Tree::default();
        t.emit(boxes, contents, members.to_vec(), leaf_size.max(1), 0);
        t
    }

    fn emit(&mut self, boxes: &[Aabb], contents: &[u32], set: Vec<u16>, leaf: usize, depth: usize) -> usize {
        let at = self.nodes.len();
        let all = set.iter().fold(0u32, |c, &b| c | contents[b as usize]);
        if set.is_empty() {
            self.nodes.push(Node::default());
            return at;
        }
        let split = if set.len() > leaf && depth < 60 { Self::choose(boxes, &set) } else { None };
        let Some((axis, dist)) = split else {
            self.nodes.push(Node { count: set.len() as i16, contents: all, brushes: set, ..Default::default() });
            return at;
        };
        let (mut above, mut below, mut across) = (Vec::new(), Vec::new(), Vec::new());
        for &b in &set {
            let bx = boxes[b as usize];
            if bx.lo[axis] >= dist {
                above.push(b);
            } else if bx.hi[axis] <= dist {
                below.push(b);
            } else {
                across.push(b);
            }
        }
        // a split that separates nothing would recurse forever: keep everything in one leaf
        if above.is_empty() && below.is_empty() {
            self.nodes.push(Node { count: set.len() as i16, contents: all, brushes: set, ..Default::default() });
            return at;
        }
        self.nodes.push(Node { axis: axis as u8, count: if across.is_empty() { 0 } else { -1 }, contents: all, dist, range: 0.0, ..Default::default() });
        if !across.is_empty() {
            let c = across.iter().fold(0u32, |c, &b| c | contents[b as usize]);
            self.nodes.push(Node { count: across.len() as i16, contents: c, brushes: across, ..Default::default() });
        }
        let a = self.emit(boxes, contents, above, leaf, depth + 1);
        let b = self.emit(boxes, contents, below, leaf, depth + 1);
        self.nodes[at].child = [(a - at) as u16, (b - at) as u16];
        at
    }

    /// The axis and position of the split: the median of the box centres along the axis they spread most on, moved to a box
    /// edge when it would cut nothing off.
    fn choose(boxes: &[Aabb], set: &[u16]) -> Option<(usize, f32)> {
        let centre = |b: u16, k: usize| (boxes[b as usize].lo[k] + boxes[b as usize].hi[k]) * 0.5;
        let axis = (0..3)
            .max_by(|&x, &y| {
                let spread = |k: usize| {
                    let (lo, hi) = set.iter().fold((f32::MAX, f32::MIN), |(l, h), &b| (l.min(centre(b, k)), h.max(centre(b, k))));
                    hi - lo
                };
                spread(x).total_cmp(&spread(y))
            })
            .unwrap_or(0);
        let mut c: Vec<f32> = set.iter().map(|&b| centre(b, axis)).collect();
        c.sort_by(|a, b| a.total_cmp(b));
        let dist = c[c.len() / 2];
        (c[0] < c[c.len() - 1]).then_some((axis, dist))
    }
}

/// What a zone's collision map holds for the tree: boxes and contents of every brush, the tree, and the roots (model 0, the
/// world, then each brush model) with the brushes each root is meant to reach.
pub struct ZoneTree {
    pub boxes: Vec<Aabb>,
    pub contents: Vec<u32>,
    pub tree: Tree,
    /// (node index of the root, brushes the model owns). Model 0 is the world, which has no root of its own: its brushes are
    /// reached through the BSP leaves (`leaf_roots`).
    pub roots: Vec<(usize, Vec<u16>)>,
    /// The distinct roots of the leaves' trees (the leaf-brush node each BSP leaf points at; 0 means none).
    pub leaf_roots: Vec<usize>,
}

impl ZoneTree {
    /// Every brush listed anywhere under `root` (straddlers included).
    pub fn tree_brushes(&self, root: usize, out: &mut Vec<u16>) {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            let Some(n) = self.tree.nodes.get(i) else { continue };
            if !seen.insert(i) {
                continue;
            }
            if n.count > 0 {
                out.extend_from_slice(&n.brushes);
            } else {
                if n.count < 0 {
                    stack.push(i + 1);
                }
                stack.push(i + n.child[0] as usize);
                stack.push(i + n.child[1] as usize);
            }
        }
    }
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

/// Reads the leaf-brush tree and brush boxes of a QA / 3.19 map zone.
pub fn read(zone: &Zone) -> Result<ZoneTree> {
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("the walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    r.record_addresses(true);
    for i in 0..zone.assets.len() {
        let _ = r.replay_asset(zone, i);
    }
    let Some(c) = r.rec.clips.iter().find(|c| c.tag == 0) else { bail!("this zone has no collision map in the QA / 3.19 layout") };
    let resolver = Resolver::new(std::mem::take(&mut r.rec.addrs));
    let d = &zone.data;
    let (b0, nb) = c.brushes;
    if c.bounds == 0 || c.contents == 0 {
        bail!("the collision map has no brush bounds");
    }
    let boxes: Vec<Aabb> = (0..nb)
        .map(|i| {
            let o = c.bounds + 24 * i;
            let (mid, half): ([f32; 3], [f32; 3]) = (std::array::from_fn(|k| f32_at(d, o + 4 * k)), std::array::from_fn(|k| f32_at(d, o + 12 + 4 * k)));
            Aabb { lo: std::array::from_fn(|k| mid[k] - half[k]), hi: std::array::from_fn(|k| mid[k] + half[k]) }
        })
        .collect();
    let contents: Vec<u32> = (0..nb).map(|i| u32_at(d, c.contents + 4 * i)).collect();
    let _ = b0;
    let (n0, nn) = c.lb_nodes;
    let mut tree = Tree::default();
    for i in 0..nn {
        let o = n0 + 20 * i;
        let count = u16_at(d, o + 2) as i16;
        let mut node = Node { axis: d[o], count, contents: u32_at(d, o + 4), ..Default::default() };
        if count > 0 {
            let ptr = u32_at(d, o + 8);
            let at = if ptr == u32::MAX { c.lb_inline.get(i).copied().filter(|&a| a != 0) } else { resolver.offset(ptr) };
            if let Some(at) = at {
                node.brushes = (0..count as usize).filter(|k| at + 2 * k + 2 <= d.len()).map(|k| u16_at(d, at + 2 * k)).collect();
            }
        } else {
            node.dist = f32_at(d, o + 8);
            node.range = f32_at(d, o + 12);
            node.child = [u16_at(d, o + 16), u16_at(d, o + 18)];
        }
        tree.nodes.push(node);
    }
    let (m0, nm) = r.rec.cmodels;
    let mut roots = Vec::new();
    for i in 0..nm {
        let root = u32_at(d, m0 + 72 * i + 68) as usize;
        roots.push((root, Vec::new()));
    }
    // the brushes a model owns: what its tree reaches (the world owns the rest)
    let mut owned = std::collections::HashSet::new();
    for (root, set) in roots.iter_mut().skip(1) {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![*root];
        while let Some(i) = stack.pop() {
            let Some(n) = tree.nodes.get(i) else { continue };
            if !seen.insert(i) {
                continue;
            }
            if n.count > 0 {
                set.extend(n.brushes.iter().copied());
            } else {
                if n.count < 0 {
                    stack.push(i + 1);
                }
                stack.push(i + n.child[0] as usize);
                stack.push(i + n.child[1] as usize);
            }
        }
        set.sort_unstable();
        set.dedup();
        owned.extend(set.iter().copied());
    }
    if let Some(w) = roots.first_mut() {
        w.1 = (0..nb as u16).filter(|b| !owned.contains(b)).collect();
    }
    let (l0, nl) = r.rec.clip_leafs;
    let mut leaf_roots: Vec<usize> = (0..nl).map(|i| u32_at(d, l0 + 40 * i + 36) as usize).filter(|&n| n != 0).collect();
    leaf_roots.sort_unstable();
    leaf_roots.dedup();
    Ok(ZoneTree { boxes, contents, tree, roots, leaf_roots })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxes(n: usize) -> Vec<Aabb> {
        // a grid of unit-ish boxes, some wide ones that straddle every split
        (0..n)
            .map(|i| {
                let (x, y, z) = ((i % 17) as f32 * 40.0, ((i / 17) % 13) as f32 * 40.0, (i / 221) as f32 * 40.0);
                let wide = if i % 29 == 0 { 300.0 } else { 20.0 };
                Aabb { lo: [x, y, z], hi: [x + wide, y + 20.0, z + 20.0] }
            })
            .collect()
    }

    #[test]
    fn a_built_tree_finds_every_brush() {
        let b = boxes(900);
        let contents = vec![1u32; b.len()];
        let all: Vec<u16> = (0..b.len() as u16).collect();
        for leaf in [1, 4, 8] {
            let t = Tree::build(&b, &contents, &all, leaf);
            assert!(t.check(0, &b, &all, 16.0, 5).is_empty(), "leaf size {leaf}");
            assert!(t.nodes.len() > b.len() / (leaf * 4), "{} nodes", t.nodes.len());
        }
    }

    #[test]
    fn a_far_query_finds_few_brushes() {
        let b = boxes(900);
        let all: Vec<u16> = (0..b.len() as u16).collect();
        let t = Tree::build(&b, &vec![1; b.len()], &all, 4);
        let found = t.query(0, &Aabb { lo: [0.0; 3], hi: [10.0; 3] });
        // a tenth of the brushes at most share the neighbourhood, wide ones that straddle the planes excepted
        assert!(found.len() < b.len() / 4, "{} of {} brushes for a small box", found.len(), b.len());
        assert!(t.query(0, &Aabb { lo: [1e6; 3], hi: [1e6 + 1.0; 3] }).len() < b.len() / 4);
    }

    #[test]
    fn empty_and_single_brush_trees() {
        let b = boxes(1);
        let t = Tree::build(&b, &[1], &[0], 4);
        assert_eq!(t.nodes.len(), 1);
        assert_eq!(t.query(0, &b[0]), vec![0]);
        let empty = Tree::build(&b, &[1], &[], 4);
        assert!(empty.query(0, &b[0]).is_empty());
    }

    #[test]
    fn child_offsets_stay_inside_the_tree() {
        let b = boxes(900);
        let all: Vec<u16> = (0..b.len() as u16).collect();
        let t = Tree::build(&b, &vec![1; b.len()], &all, 2);
        for (i, n) in t.nodes.iter().enumerate() {
            if n.count <= 0 && n.contents != 0 {
                assert!(i + (n.child[0] as usize) < t.nodes.len() && i + (n.child[1] as usize) < t.nodes.len());
            }
        }
    }
}
