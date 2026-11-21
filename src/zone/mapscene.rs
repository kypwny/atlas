//! The editable parts of a map as drawable layers: collision brushes (as the solids their planes enclose), the brushes of brush
//! models, a marker per static model (an oriented box) and a marker per point entity, grouped by kind. The world surfaces come from
//! `zone::world`; these are what an editor adds on top. Everything is flat shaded with vertex colours.

use super::mapsrc::{Brush, Entity, MapSource, StaticModel};
use crate::model::{Surface, Vertex};

/// One layer: a name for the UI and the surface to draw.
pub struct Layer {
    pub name: &'static str,
    pub surface: Surface,
}

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l == 0.0 { a } else { [a[0] / l, a[1] / l, a[2] / l] }
}

#[derive(Default)]
struct Mesh {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
}

impl Mesh {
    /// A flat shaded convex polygon (counter-clockwise seen from where `normal` points), fanned into triangles.
    fn polygon(&mut self, points: &[V3], normal: V3, color: [u8; 4]) {
        if points.len() < 3 {
            return;
        }
        let base = self.vertices.len() as u32;
        let n = [normal[0] as f32, normal[1] as f32, normal[2] as f32];
        for p in points {
            self.vertices.push(Vertex { pos: [p[0] as f32, p[1] as f32, p[2] as f32], normal: n, tangent: [1.0, 0.0, 0.0], binormal_sign: 1.0, uv: [0.0, 0.0], color });
        }
        for k in 1..points.len() as u32 - 1 {
            self.indices.extend_from_slice(&[base, base + k, base + k + 1]);
        }
    }

    fn into_surface(self) -> Surface {
        Surface { vertices: self.vertices, indices: self.indices, rigid: Vec::new(), blend_counts: [0; 4], blend: Vec::new(), header: [0; 84] }
    }
}

/// The corners of the solid a brush's planes enclose, then one polygon per plane with at least three of them. Planes are
/// `normal . x <= dist`, normals pointing out. A brush whose planes enclose nothing gives no polygons.
pub fn brush_polygons(b: &Brush) -> Vec<(V3, Vec<V3>)> {
    let p = &b.sides;
    let mut corners: Vec<V3> = Vec::new();
    for i in 0..p.len() {
        for j in i + 1..p.len() {
            for k in j + 1..p.len() {
                let (a, bb, c) = (p[i].normal, p[j].normal, p[k].normal);
                let det = dot(a, cross(bb, c));
                if det.abs() < 1e-6 {
                    continue;
                }
                let (bc, ca, ab) = (cross(bb, c), cross(c, a), cross(a, bb));
                let v: V3 = std::array::from_fn(|n| (p[i].dist * bc[n] + p[j].dist * ca[n] + p[k].dist * ab[n]) / det);
                if p.iter().all(|s| dot(s.normal, v) <= s.dist + 0.05) && !corners.iter().any(|c| dot(sub(*c, v), sub(*c, v)) < 1e-4) {
                    corners.push(v);
                }
            }
        }
    }
    let mut out = Vec::new();
    for s in p {
        let on: Vec<V3> = corners.iter().copied().filter(|c| (dot(s.normal, *c) - s.dist).abs() < 0.05).collect();
        if on.len() < 3 {
            continue;
        }
        let n = unit(s.normal);
        let centre: V3 = std::array::from_fn(|k| on.iter().map(|c| c[k]).sum::<f64>() / on.len() as f64);
        let helper = if n[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
        let u = unit(cross(helper, n));
        let v = cross(n, u);
        let mut sorted = on;
        sorted.sort_by(|a, b| {
            let (da, db) = (sub(*a, centre), sub(*b, centre));
            dot(da, v).atan2(dot(da, u)).total_cmp(&dot(db, v).atan2(dot(db, u)))
        });
        out.push((n, sorted));
    }
    out
}

/// Longest side of the box a brush's corners span; maps carry boundary and sky brushes tens of thousands of units wide.
fn extent(polys: &[(V3, Vec<V3>)]) -> f64 {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in polys.iter().flat_map(|(_, v)| v) {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max)
}

/// Brushes wider than this (units) are the map's boundary shell, not something one edits: they go to a layer of their own.
pub const OVERSIZED: f64 = 8192.0;

/// The contents bit of solid collision; every other kind of brush (hint, clip volumes, water, triggers' physics ..) is a volume the
/// player cannot touch as a wall and is drawn in a layer of its own.
pub const CONTENTS_SOLID: u32 = 1;

/// Where `add_brushes` puts the brushes that are not plain solid geometry.
struct Others<'a> {
    /// Non-solid contents (hint, clip, water ..).
    volumes: &'a mut Mesh,
    /// Solid brushes wider than [`OVERSIZED`] (the map's boundary shell).
    shell: &'a mut Mesh,
}

/// Draws the solid brushes into `mesh`; the others go to `others` when given (and are skipped otherwise).
fn add_brushes(mesh: &mut Mesh, mut others: Option<Others>, brushes: &[Brush], color: [u8; 4]) {
    for b in brushes {
        let polys = brush_polygons(b);
        let target = if b.contents & CONTENTS_SOLID == 0 {
            match others.as_mut() {
                Some(o) => &mut *o.volumes,
                None => &mut *mesh,
            }
        } else if extent(&polys) > OVERSIZED {
            match others.as_mut() {
                Some(o) => &mut *o.shell,
                None => continue,
            }
        } else {
            &mut *mesh
        };
        for (n, poly) in polys {
            target.polygon(&poly, n, color);
        }
    }
}

/// A box of half size `h` whose axes are the rows of `axis`, centred on `at`.
fn add_box(mesh: &mut Mesh, at: V3, axis: &[V3; 3], h: V3, color: [u8; 4]) {
    let corner = |sx: f64, sy: f64, sz: f64| -> V3 { std::array::from_fn(|k| at[k] + axis[0][k] * h[0] * sx + axis[1][k] * h[1] * sy + axis[2][k] * h[2] * sz) };
    // (axis index, sign) of each face, corners counter-clockwise seen from outside
    for (a, sign) in [(0usize, 1.0f64), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0), (2, -1.0)] {
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        let at_corner = |sb: f64, sc: f64| {
            let mut s = [0.0; 3];
            s[a] = sign;
            s[b] = sb;
            s[c] = sc;
            corner(s[0], s[1], s[2])
        };
        let mut pts = [at_corner(-1.0, -1.0), at_corner(1.0, -1.0), at_corner(1.0, 1.0), at_corner(-1.0, 1.0)];
        let n: V3 = std::array::from_fn(|k| axis[a][k] * sign);
        // the winding follows from the axis order b x c = a; flip when the sign is negative
        if sign < 0.0 {
            pts.reverse();
        }
        mesh.polygon(&pts, n, color);
    }
}

/// A flat shaded octahedron of radius `r`.
fn add_marker(mesh: &mut Mesh, at: V3, r: f64, color: [u8; 4]) {
    let p = |x: f64, y: f64, z: f64| -> V3 { [at[0] + x * r, at[1] + y * r, at[2] + z * r] };
    let (px, nx, py, ny, pz, nz) = (p(1.0, 0.0, 0.0), p(-1.0, 0.0, 0.0), p(0.0, 1.0, 0.0), p(0.0, -1.0, 0.0), p(0.0, 0.0, 1.0), p(0.0, 0.0, -1.0));
    for (top, a, b) in [(pz, px, py), (pz, py, nx), (pz, nx, ny), (pz, ny, px), (nz, py, px), (nz, nx, py), (nz, ny, nx), (nz, px, ny)] {
        let n = unit(cross(sub(a, top), sub(b, top)));
        let n = if dot(n, sub(top, at)) < 0.0 { [-n[0], -n[1], -n[2]] } else { n };
        // counter-clockwise seen from outside
        let tri = if dot(cross(sub(a, top), sub(b, top)), n) > 0.0 { [top, a, b] } else { [top, b, a] };
        mesh.polygon(&tri, n, color);
    }
}

/// The origin of an entity (`"origin" "x y z"`), when it has one.
pub fn origin_of(e: &Entity) -> Option<V3> {
    let mut it = e.get("origin")?.split_whitespace().map(|t| t.parse::<f64>());
    Some([it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?])
}

/// Which marker layer an entity belongs to.
pub fn entity_group(e: &Entity) -> &'static str {
    let class = e.get("classname").unwrap_or("");
    if class.contains("spawn") || class.starts_with("info_player") || class == "mp_global_intermission" {
        "entities: spawns"
    } else if class.starts_with("node_") {
        "entities: path nodes"
    } else if class.starts_with("script_") {
        "entities: script"
    } else if class.starts_with("light") || class.starts_with("reflection") {
        "entities: lights"
    } else {
        "entities: other"
    }
}

fn group_color(name: &str) -> [u8; 4] {
    match name {
        "entities: spawns" => [60, 220, 230, 255],
        "entities: path nodes" => [120, 140, 200, 255],
        "entities: script" => [240, 220, 70, 255],
        "entities: lights" => [255, 255, 255, 255],
        _ => [230, 90, 200, 255],
    }
}

fn model_box(m: &StaticModel) -> (V3, [V3; 3], V3) {
    let s = if m.scale > 0.0 { m.scale } else { 1.0 };
    (m.origin, m.axis, [10.0 * s, 10.0 * s, 10.0 * s])
}

/// Every layer of a map: collision brushes of the world, brushes of brush models, static models and the entity groups.
pub fn layers(m: &MapSource) -> Vec<Layer> {
    let mut out = Vec::new();

    let (mut world, mut shell, mut volumes) = (Mesh::default(), Mesh::default(), Mesh::default());
    add_brushes(&mut world, Some(Others { volumes: &mut volumes, shell: &mut shell }), &m.world, [110, 150, 190, 255]);
    out.push(Layer { name: "collision: world brushes", surface: world.into_surface() });

    let mut movers = Mesh::default();
    for (_, brushes) in &m.submodels {
        add_brushes(&mut movers, None, brushes, [235, 150, 60, 255]);
    }
    out.push(Layer { name: "collision: brush models", surface: movers.into_surface() });
    out.push(Layer { name: "collision: boundary shell", surface: shell.into_surface() });
    out.push(Layer { name: "collision: volumes (hint, clip ..)", surface: volumes.into_surface() });

    let mut statics = Mesh::default();
    for sm in &m.models {
        let (at, axis, h) = model_box(sm);
        add_box(&mut statics, at, &axis, h, [90, 190, 110, 255]);
    }
    out.push(Layer { name: "static models", surface: statics.into_surface() });

    let groups = ["entities: spawns", "entities: path nodes", "entities: script", "entities: lights", "entities: other"];
    let mut meshes: Vec<Mesh> = groups.iter().map(|_| Mesh::default()).collect();
    for e in &m.entities {
        if e.get("classname") == Some("worldspawn") || e.get("model").is_some_and(|v| v.starts_with('*')) {
            continue;
        }
        let Some(at) = origin_of(e) else { continue };
        let g = entity_group(e);
        let i = groups.iter().position(|x| *x == g).unwrap_or(groups.len() - 1);
        add_marker(&mut meshes[i], at, 14.0, group_color(g));
    }
    for (name, mesh) in groups.iter().zip(meshes) {
        out.push(Layer { name, surface: mesh.into_surface() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zone::mapsrc::Side;

    fn cube() -> Brush {
        let side = |n: V3, d: f64| Side { normal: n, dist: d, material: "x".into() };
        Brush {
            sides: vec![side([-1.0, 0.0, 0.0], 0.0), side([1.0, 0.0, 0.0], 64.0), side([0.0, -1.0, 0.0], 0.0), side([0.0, 1.0, 0.0], 32.0), side([0.0, 0.0, -1.0], 0.0), side([0.0, 0.0, 1.0], 16.0)],
            contents: CONTENTS_SOLID,
        }
    }

    #[test]
    fn a_box_brush_gives_six_quads_facing_out() {
        let polys = brush_polygons(&cube());
        assert_eq!(polys.len(), 6);
        for (n, p) in &polys {
            assert_eq!(p.len(), 4);
            // counter-clockwise seen from outside: the polygon's own normal points the way the plane's does
            let own = cross(sub(p[1], p[0]), sub(p[2], p[0]));
            assert!(dot(own, *n) > 0.0, "{n:?} {p:?}");
        }
        let mut mesh = Mesh::default();
        add_brushes(&mut mesh, None, &[cube()], [1, 2, 3, 255]);
        assert_eq!((mesh.vertices.len(), mesh.indices.len()), (24, 36));
    }

    #[test]
    fn a_wedge_and_an_open_brush() {
        let mut b = cube();
        // cut one corner off with a slanted plane
        b.sides.push(Side { normal: unit([1.0, 1.0, 0.0]), dist: unit([1.0, 1.0, 0.0])[0] * 90.0, material: "x".into() });
        assert_eq!(brush_polygons(&b).len(), 7);
        // two parallel planes enclose nothing finite
        let open = Brush { sides: cube().sides[..2].to_vec(), contents: 0 };
        assert!(brush_polygons(&open).is_empty());
    }

    #[test]
    fn brushes_are_sorted_into_layers_by_contents_and_size() {
        let (mut solid, mut volumes, mut shell) = (Mesh::default(), Mesh::default(), Mesh::default());
        let mut hint = cube();
        hint.contents = 0x1000_0000;
        let mut huge = cube();
        for s in huge.sides.iter_mut().filter(|s| s.dist > 0.0) {
            s.dist += 20000.0;
        }
        add_brushes(&mut solid, Some(Others { volumes: &mut volumes, shell: &mut shell }), &[cube(), hint, huge], [0; 4]);
        assert_eq!((solid.vertices.len(), volumes.vertices.len(), shell.vertices.len()), (24, 24, 24));
        // without the extra layers only the plain solid brush is drawn
        let mut only = Mesh::default();
        let mut hint = cube();
        hint.contents = 0x1000_0000;
        add_brushes(&mut only, None, &[cube(), hint], [0; 4]);
        assert_eq!(only.vertices.len(), 48);
    }

    #[test]
    fn markers_and_boxes_have_the_expected_size() {
        let mut m = Mesh::default();
        add_marker(&mut m, [1.0, 2.0, 3.0], 5.0, [0; 4]);
        assert_eq!((m.vertices.len(), m.indices.len()), (24, 24));
        let mut m = Mesh::default();
        add_box(&mut m, [0.0; 3], &[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], [1.0; 3], [0; 4]);
        assert_eq!((m.vertices.len(), m.indices.len()), (24, 36));
        // every box face points away from the centre
        for t in m.indices.chunks(3) {
            let (a, b, c) = (m.vertices[t[0] as usize].pos, m.vertices[t[1] as usize].pos, m.vertices[t[2] as usize].pos);
            let f = |p: [f32; 3]| [p[0] as f64, p[1] as f64, p[2] as f64];
            let n = cross(sub(f(b), f(a)), sub(f(c), f(a)));
            let centre: V3 = std::array::from_fn(|k| (f(a)[k] + f(b)[k] + f(c)[k]) / 3.0);
            assert!(dot(n, centre) > 0.0);
        }
    }

    #[test]
    fn entities_are_grouped_by_class() {
        let e = |class: &str| Entity { keys: vec![("classname".into(), class.into())] };
        assert_eq!(entity_group(&e("mp_tdm_spawn_axis_start")), "entities: spawns");
        assert_eq!(entity_group(&e("node_pathnode")), "entities: path nodes");
        assert_eq!(entity_group(&e("script_origin")), "entities: script");
        assert_eq!(entity_group(&e("trigger_multiple")), "entities: other");
        let with = Entity { keys: vec![("origin".into(), "1 -2.5 3".into())] };
        assert_eq!(origin_of(&with), Some([1.0, -2.5, 3.0]));
    }
}
