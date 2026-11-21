//! Writing an edited `.map` back into a CoDOL (QA / 3.19) map zone: what can be changed without moving anything else in the zone.
//!
//! * **Entities.** The entity text nested in the collision map is replaced (it may change length; `ZoneBuilder` relocates what
//!   lies behind it).
//! * **Static models.** Moving, turning and scaling a model rewrites its draw record (origin, axis, scale), its culling bounds
//!   and, when the model has collision, its collision record (origin, inverse scaled axis, absolute bounds). Adding or
//!   deleting static models changes counts and lists all over the world asset and is refused.
//! * **Brushes.** Added, deleted and changed collision brushes of the world and of existing brush models are rewritten by
//!   [`clipwrite`](super::clipwrite) (new brushes without edge data; the BSP and the terrain keep their shape). New brush
//!   entities (a brush model that does not exist yet) are refused.
//!
//! Everything compared with what [`mapsrc::read`] finds in the zone, with the tolerance of the `.map`'s three decimals.

use super::mapsrc::{self, Brush, Entity, MapSource, StaticModel};
use super::Zone;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;

/// The new bytes of the two assets an edit touches, and what changed.
#[derive(Debug, Default)]
pub struct Plan {
    pub world_asset: usize,
    pub clip_asset: usize,
    pub world_blob: Vec<u8>,
    pub clip_blob: Vec<u8>,
    pub models_changed: usize,
    pub collision_changed: usize,
    pub entities_changed: bool,
    /// Set when the world asset's bytes changed (static models, planes appended for new brushes).
    pub world_changed: bool,
    /// Set when the collision brushes were rewritten: how the old allocations of the collision asset became the new ones.
    pub rebuilt: Option<super::writer::Rebuilt>,
    pub notes: Vec<String>,
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn put(b: &mut [u8], o: usize, v: f32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn angle_close(a: f64, b: f64, tol: f64) -> bool {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d) <= tol
}

/// Whether two placements are the same within the precision of the `.map` text (three decimals).
fn same_angles(a: &StaticModel, b: &StaticModel) -> bool {
    (0..3).all(|k| angle_close(a.angles[k], b.angles[k], 0.002))
}

fn same_placement(a: &StaticModel, b: &StaticModel) -> bool {
    a.name == b.name && (0..3).all(|k| close(a.origin[k], b.origin[k], 0.002)) && same_angles(a, b) && close(a.scale, b.scale, 0.002)
}

/// The brushes that differ between the zone and the map: (world or brush model, index) of each.
fn brush_differences(orig: &MapSource, edited: &MapSource) -> Result<Vec<String>> {
    let mut diff = Vec::new();
    let same = |a: &Brush, b: &Brush| {
        a.sides.len() == b.sides.len()
            && a.sides.iter().zip(&b.sides).all(|(x, y)| (0..3).all(|k| close(x.normal[k], y.normal[k], 0.002)) && close(x.dist, y.dist, 0.06))
    };
    if orig.world.len() != edited.world.len() {
        bail!("the map has {} world brushes, the zone {}: adding or deleting brushes is not supported yet", edited.world.len(), orig.world.len());
    }
    for (i, (a, b)) in orig.world.iter().zip(&edited.world).enumerate() {
        if !same(a, b) {
            diff.push(format!("world brush {i}"));
        }
    }
    // brush models without brushes (triggers made of other things) do not appear in a .map: leave them out of the comparison
    let by_index = |m: &MapSource| -> std::collections::BTreeMap<u32, usize> { m.submodels.iter().enumerate().filter(|(_, (_, b))| !b.is_empty()).map(|(k, (i, _))| (*i, k)).collect() };
    let (oi, ei) = (by_index(orig), by_index(edited));
    if oi.keys().ne(ei.keys()) {
        bail!("the brush models of the map differ from the zone's (adding or removing brush entities is not supported yet)");
    }
    for (idx, &ok) in &oi {
        let (a, b) = (&orig.submodels[ok].1, &edited.submodels[ei[idx]].1);
        if a.len() != b.len() {
            bail!("brush model {idx} has {} brushes in the map and {} in the zone", b.len(), a.len());
        }
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            if !same(x, y) {
                diff.push(format!("brush model {idx}, brush {i}"));
            }
        }
    }
    Ok(diff)
}

fn entities_equal(a: &[Entity], b: &[Entity]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.keys == y.keys)
}

/// Entity text as the game reads it: one block per entity.
pub fn entity_text(entities: &[Entity]) -> String {
    let mut out = String::new();
    for e in entities {
        out.push_str("{\n");
        for (k, v) in &e.keys {
            out.push_str(&format!("\"{k}\" \"{v}\"\n"));
        }
        out.push_str("}\n");
    }
    out
}

fn asset_containing(zone: &Zone, offset: usize) -> Result<usize> {
    zone.assets.iter().position(|a| offset >= a.start && offset < a.start + a.extent).context("an offset lies in no asset")
}

/// What has to change in the zone to turn it into `edited`. `orig` is [`mapsrc::read`] of the same zone.
pub fn plan(zone: &Zone, orig: &MapSource, edited: &MapSource) -> Result<Plan> {
    plan_with(zone, orig, edited, super::clipwrite::Options::default())
}

/// [`plan`] with options for the collision rewrite.
pub fn plan_with(zone: &Zone, orig: &MapSource, edited: &MapSource, opts: super::clipwrite::Options) -> Result<Plan> {
    let brush_edit = opts.rebuild_bsp || match brush_differences(orig, edited) {
        Ok(d) => !d.is_empty(),
        // a different number of brushes is an edit the rewrite handles; a changed set of brush models is not
        Err(e) if e.to_string().contains("brush models of the map differ") => return Err(e),
        Err(_) => true,
    };
    if orig.models.len() != edited.models.len() {
        bail!("the map has {} static models, the zone {}: adding or deleting static models is not supported yet", edited.models.len(), orig.models.len());
    }
    let lay = &orig.layout;
    let mut plan = Plan::default();
    let changed: Vec<usize> = (0..orig.models.len()).filter(|&i| !same_placement(&orig.models[i], &edited.models[i])).collect();
    let entities_changed = !entities_equal(&orig.entities, &edited.entities);

    let world_at = lay.draw.first().map(|d| d.0);
    let clip_at = if lay.entity_text.2 > 0 { Some(lay.entity_text.0) } else if lay.clip_statics.1 > 0 { Some(lay.clip_statics.0) } else { None };
    if (!changed.is_empty() && world_at.is_none()) || (entities_changed && lay.entity_text.2 == 0) || (brush_edit && lay.clip_statics.1 == 0 && lay.entity_text.2 == 0) {
        bail!("this zone has no static models or entity text to write to");
    }
    if let Some(w) = world_at {
        plan.world_asset = asset_containing(zone, w)?;
        let a = &zone.assets[plan.world_asset];
        plan.world_blob = zone.data[a.start..a.start + a.extent].to_vec();
    }
    if let Some(c) = clip_at {
        plan.clip_asset = asset_containing(zone, c)?;
        let a = &zone.assets[plan.clip_asset];
        plan.clip_blob = zone.data[a.start..a.start + a.extent].to_vec();
    }
    let (world_start, clip_start) = (zone.assets.get(plan.world_asset).map_or(0, |a| a.start), zone.assets.get(plan.clip_asset).map_or(0, |a| a.start));

    let mut used_clip: HashSet<usize> = HashSet::new();
    for &i in &changed {
        let (old, new) = (&orig.models[i], &edited.models[i]);
        let (draw_at, list_index) = lay.draw[i];
        let d = draw_at - world_start;
        // an axis the user did not turn stays as the zone has it (it may not be a plain rotation, which angles cannot say)
        let keep_axis = same_angles(old, new);
        let new_axis: [[f32; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| if keep_axis { old.axis[r][c] } else { new.axis[r][c] } as f32));
        let new_origin: [f32; 3] = std::array::from_fn(|k| new.origin[k] as f32);
        let new_scale = if new.scale > 0.0 { new.scale as f32 } else { 1.0 };
        let (old_scale, old_origin) = (f32_at(&plan.world_blob, d + 48), [f32_at(&plan.world_blob, d), f32_at(&plan.world_blob, d + 4), f32_at(&plan.world_blob, d + 8)]);
        let old_axis: [[f32; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| f32_at(&plan.world_blob, d + 12 + 12 * r + 4 * c)));
        let pure_move = (0..3).all(|r| (0..3).all(|c| (old_axis[r][c] - new_axis[r][c]).abs() < 1e-3)) && (old_scale - new_scale).abs() < 1e-3;
        for k in 0..3 {
            put(&mut plan.world_blob, d + 4 * k, new_origin[k]);
        }
        for r in 0..3 {
            for c in 0..3 {
                put(&mut plan.world_blob, d + 12 + 12 * r + 4 * c, new_axis[r][c]);
            }
        }
        put(&mut plan.world_blob, d + 48, new_scale);
        let delta: [f32; 3] = std::array::from_fn(|k| new_origin[k] - old_origin[k]);
        let ratio = new_scale / old_scale.max(1e-6);

        // culling bounds of the model (mins, size, a copy of the mins used for lighting)
        let b = lay.insts - world_start + 36 * list_index;
        let (mins, size): ([f32; 3], [f32; 3]) = (std::array::from_fn(|k| f32_at(&plan.world_blob, b + 4 * k)), std::array::from_fn(|k| f32_at(&plan.world_blob, b + 12 + 4 * k)));
        if pure_move {
            for k in 0..3 {
                put(&mut plan.world_blob, b + 4 * k, mins[k] + delta[k]);
                let ground = f32_at(&plan.world_blob, b + 24 + 4 * k);
                put(&mut plan.world_blob, b + 24 + 4 * k, ground + delta[k]);
            }
        } else {
            // a turned or scaled model: a box around the sphere that holds the old box, scaled
            let centre: [f32; 3] = std::array::from_fn(|k| mins[k] + size[k] * 0.5 - old_origin[k]);
            let radius = ((centre.iter().map(|x| x * x).sum::<f32>()).sqrt() + (size.iter().map(|x| x * x).sum::<f32>()).sqrt() * 0.5) * ratio;
            for k in 0..3 {
                put(&mut plan.world_blob, b + 4 * k, new_origin[k] - radius);
                put(&mut plan.world_blob, b + 12 + 4 * k, radius * 2.0);
                put(&mut plan.world_blob, b + 24 + 4 * k, new_origin[k] - radius);
            }
        }

        // the collision record of the same model, found by origin and orientation
        let (c0, cn) = lay.clip_statics;
        for ci in 0..cn {
            let at = c0 + 76 * ci - clip_start;
            if used_clip.contains(&ci) || plan.clip_blob.len() < at + 76 {
                continue;
            }
            let o = [f32_at(&plan.clip_blob, at + 4), f32_at(&plan.clip_blob, at + 8), f32_at(&plan.clip_blob, at + 12)];
            if o != old_origin {
                continue;
            }
            // inverse scaled axis = transposed axis / scale (as the zone stores it)
            let matches_old = (0..3).all(|r| (0..3).all(|c| (f32_at(&plan.clip_blob, at + 16 + 12 * r + 4 * c) - old_axis[c][r] / old_scale).abs() < 2e-3));
            if !matches_old {
                continue;
            }
            used_clip.insert(ci);
            for k in 0..3 {
                put(&mut plan.clip_blob, at + 4 + 4 * k, new_origin[k]);
            }
            for r in 0..3 {
                for c in 0..3 {
                    put(&mut plan.clip_blob, at + 16 + 12 * r + 4 * c, new_axis[c][r] / new_scale);
                }
            }
            let (mid, half): ([f32; 3], [f32; 3]) = (std::array::from_fn(|k| f32_at(&plan.clip_blob, at + 52 + 4 * k)), std::array::from_fn(|k| f32_at(&plan.clip_blob, at + 64 + 4 * k)));
            if pure_move {
                for k in 0..3 {
                    put(&mut plan.clip_blob, at + 52 + 4 * k, mid[k] + delta[k]);
                }
            } else {
                let offset: [f32; 3] = std::array::from_fn(|k| mid[k] - old_origin[k]);
                let radius = ((offset.iter().map(|x| x * x).sum::<f32>()).sqrt() + (half.iter().map(|x| x * x).sum::<f32>()).sqrt()) * ratio;
                for k in 0..3 {
                    put(&mut plan.clip_blob, at + 52 + 4 * k, new_origin[k]);
                    put(&mut plan.clip_blob, at + 64 + 4 * k, radius);
                }
            }
            plan.collision_changed += 1;
            break;
        }
        plan.models_changed += 1;
    }

    if entities_changed {
        let (hdr, at, n) = lay.entity_text;
        plan.clip_blob = crate::zonefiles::with_nested_text(&plan.clip_blob, (hdr - clip_start, at - clip_start, n), &entity_text(&edited.entities));
        plan.entities_changed = true;
    }
    if brush_edit && opts.render_new_brushes {
        // the faces of the new brushes as render surfaces; this edits the world asset before the planes are appended to it
        // (the planes lie in front of everything the surface writer touches)
        let mut faces: Vec<super::worldwrite::NewFace> = Vec::new();
        for b in super::clipwrite::added_world_brushes(orig, edited) {
            for (si, n, corners) in super::mapscene::brush_faces(&b) {
                faces.push(super::worldwrite::NewFace { corners, normal: n, material: b.sides[si].material.clone() });
            }
        }
        if !faces.is_empty() {
            if plan.world_blob.is_empty() {
                plan.world_asset = asset_containing(zone, lay.world_header)?;
                let a = &zone.assets[plan.world_asset];
                plan.world_blob = zone.data[a.start..a.start + a.extent].to_vec();
            }
            let added = super::worldwrite::add_surfaces(zone, &mut plan.world_blob, plan.world_asset, &faces)?;
            plan.world_changed = true;
            plan.notes.push(format!("render world: {} surfaces ({} vertices, {} triangles) added", added.surfaces, added.vertices, added.triangles));
            for s in added.skipped.iter().collect::<std::collections::BTreeSet<_>>() {
                plan.notes.push(format!("render world: skipped a face: {s}"));
            }
        }
    }
    if brush_edit {
        match super::clipwrite::rebuild(zone, &plan.clip_blob, orig, edited, opts)? {
            Some(rb) => {
                plan.clip_blob = rb.blob;
                plan.rebuilt = Some(rb.rebuilt);
                plan.notes.extend(rb.notes);
                // the new brushes' planes join the world's plane array (the collision map's sides point into it): they go behind the
                // last plane, and the count in the world header follows
                if !rb.new_planes.is_empty() {
                    let (wh, wp) = (lay.world_header, lay.world_planes);
                    if wp.1 == 0 && wp.0 == 0 {
                        bail!("this zone's world has no plane array to extend");
                    }
                    if plan.world_blob.is_empty() {
                        plan.world_asset = asset_containing(zone, wp.0)?;
                        let a = &zone.assets[plan.world_asset];
                        plan.world_blob = zone.data[a.start..a.start + a.extent].to_vec();
                    }
                    let ws = zone.assets[plan.world_asset].start;
                    let at = wp.0 + 20 * wp.1 - ws;
                    let mut bytes = Vec::with_capacity(20 * rb.new_planes.len());
                    for p in &rb.new_planes {
                        for k in 0..3 {
                            bytes.extend_from_slice(&p[k].to_le_bytes());
                        }
                        bytes.extend_from_slice(&p[3].to_le_bytes());
                        let axis_type = (0..3).find(|&a| p[a].abs() > 0.9999).map_or(3u8, |a| a as u8);
                        let signbits = (0..3).fold(0u8, |b, a| b | (((p[a] < 0.0) as u8) << a));
                        bytes.extend_from_slice(&[axis_type, signbits, 0, 0]);
                    }
                    plan.world_blob.splice(at..at, bytes);
                    let count_at = wh + 8 - ws;
                    let n = u32::from_le_bytes(plan.world_blob[count_at..count_at + 4].try_into().unwrap()) + rb.new_planes.len() as u32;
                    plan.world_blob[count_at..count_at + 4].copy_from_slice(&n.to_le_bytes());
                    plan.world_changed = true;
                }
            }
            None => {}
        }
    }
    if plan.models_changed == 0 && !plan.entities_changed && plan.rebuilt.is_none() {
        plan.notes.push("the map matches the zone: nothing to write".to_string());
    }
    Ok(plan)
}

/// Reads `map_text`, compares it with the zone at `zone_path` and writes the changed zone to `out` (unsigned).
pub fn apply(zone_path: &std::path::Path, key: &super::fastfile::ArchiveKey, map_text: &str, out: &std::path::Path) -> Result<Plan> {
    apply_with(zone_path, key, map_text, out, super::clipwrite::Options::default())
}

/// [`apply`] with options for the collision rewrite.
pub fn apply_with(zone_path: &std::path::Path, key: &super::fastfile::ArchiveKey, map_text: &str, out: &std::path::Path, opts: super::clipwrite::Options) -> Result<Plan> {
    let opened = super::fastfile::open(zone_path, key)?;
    let zone = Zone::from_image(opened.version, opened.image.clone())?;
    let name = zone_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let orig = mapsrc::read(&zone, &name)?;
    let edited = mapsrc::parse_map(map_text)?;
    let plan = plan_with(&zone, &orig, &edited, opts)?;
    let mut builder = super::writer::ZoneBuilder::from_zone(&zone);
    if plan.models_changed > 0 || plan.world_changed {
        builder.replace_same_layout(plan.world_asset, plan.world_blob.clone());
    }
    if let Some(rb) = &plan.rebuilt {
        builder.replace_rebuilt(plan.clip_asset, plan.clip_blob.clone(), rb.clone());
    } else if plan.models_changed > 0 || plan.entities_changed {
        builder.replace_same_layout(plan.clip_asset, plan.clip_blob.clone());
    }
    let built = builder.build(&zone)?;
    let mut container = opened.container.clone();
    container.signed = false;
    std::fs::write(out, super::fastfile::seal(&built.image, opened.version, &container)?)?;
    Ok(plan)
}
