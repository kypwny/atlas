//! Streamed model files (`main/models/<name>`): the geometry container that
//! CoDOL keeps outside its fastfiles.
//!
//! Layout (from the project's own loader notes, `codol_stream_model.c`, and
//! checked against the QA build's files): `u32 1`, `u32 0xE9C9C447`, then four
//! 20-byte descriptors `{size, offset, fixup_start, fixup_count, _}`. Blocks
//! 0-2 hold data (0: headers, 1 and 2: bulk arrays); block 3 is a table of
//! fix-ups, each `location << 3 | target_block`, naming a `u32` in the current
//! block that holds an offset into the target block. Block 0 starts with a
//! 48-byte `XModelSurfs` header: name reference, surfaces reference, `u16`
//! surface count at +8. Each `XSurface` is 84 bytes; vertices are 32-byte
//! packed vertices, triangles three `u16` indices.

use anyhow::{Result, bail};

const MAGIC: u32 = 0xE9C9_C447;
const SURFACE_BYTES: usize = 84;
const VERTEX_BYTES: usize = 32;

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 3],
    pub binormal_sign: f32,
    pub uv: [f32; 2],
    /// RGBA as stored (usually white).
    pub color: [u8; 4],
}

/// A run of vertices that all follow one bone (`XRigidVertList`).
#[derive(Clone, Copy, Debug)]
pub struct Rigid {
    pub bone_offset: u16,
    pub vert_count: u16,
    pub tri_offset: u16,
    pub tri_count: u16,
}

#[derive(Clone, Debug)]
pub struct Surface {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub rigid: Vec<Rigid>,
    /// Vertices with 0, 1, 2 and 3 extra blend bones (`XSurfaceVertexInfo`).
    pub blend_counts: [u16; 4],
    /// Raw blend words, kept for skinning later.
    pub blend: Vec<u16>,
    pub header: [u8; 84],
}

impl Surface {
    /// Bone influences of each vertex as (bone, weight), weights summing to 1; empty for a vertex nothing binds.
    ///
    /// Vertices come in this order (as OpenAssetTools reads the same layout): first the rigid runs, each bound
    /// entirely to one bone, then the vertices with 0, 1, 2 and 3 *extra* blend bones (`blend_counts`). A surface
    /// has one kind or the other. A blend vertex stores its main bone, then (bone, weight) pairs for the extra
    /// ones, and the main bone takes whatever weight is left. Bones are stored as byte offsets into the
    /// 64-byte-per-bone matrix array, so the index is `offset / 64`; weights are `u16 / 65535`.
    pub fn skin_weights(&self) -> Vec<Vec<(u32, f32)>> {
        let mut out: Vec<Vec<(u32, f32)>> = Vec::with_capacity(self.vertices.len());
        for r in &self.rigid {
            for _ in 0..r.vert_count {
                out.push(vec![((r.bone_offset / 64) as u32, 1.0)]);
            }
        }
        let mut w = 0usize;
        for extra in 0..4usize {
            for _ in 0..self.blend_counts[extra] {
                let word = |k: usize| self.blend.get(w + k).copied().unwrap_or(0);
                let main = (word(0) / 64) as u32;
                let mut list = Vec::with_capacity(extra + 1);
                let mut left = 1.0f32;
                for k in 0..extra {
                    let (bone, weight) = ((word(1 + 2 * k) / 64) as u32, word(2 + 2 * k) as f32 / 65535.0);
                    list.push((bone, weight));
                    left -= weight;
                }
                list.insert(0, (main, left.max(0.0)));
                out.push(list);
                w += 2 * extra + 1;
            }
        }
        out.resize(self.vertices.len(), Vec::new());
        out
    }
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    pub surfaces: Vec<Surface>,
}

/// Half-precision float to f32.
fn half(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 31) as i32;
    let frac = (h & 0x3FF) as f32;
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 => if frac == 0.0 { sign * f32::INFINITY } else { f32::NAN },
        e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Unpacks the 4-byte unit vector: three biased bytes and a scale byte.
pub(crate) fn unpack_unit(v: u32) -> [f32; 3] {
    let b = v.to_le_bytes();
    let scale = (b[3] as f32 + 192.0) / 32385.0;
    [(b[0] as f32 - 127.0) * scale, (b[1] as f32 - 127.0) * scale, (b[2] as f32 - 127.0) * scale]
}

/// Packed 32-byte vertices (`GfxPackedVertex`), as streamed files and zones store them.
pub fn decode_vertices(vbytes: &[u8]) -> Vec<Vertex> {
    vbytes
        .chunks_exact(VERTEX_BYTES)
        .map(|v| {
            let f = |o: usize| f32::from_le_bytes(v[o..o + 4].try_into().unwrap());
            let u = |o: usize| u32::from_le_bytes(v[o..o + 4].try_into().unwrap());
            let tc = u(20);
            Vertex {
                pos: [f(0), f(4), f(8)],
                binormal_sign: f(12),
                color: u(16).to_le_bytes(),
                uv: [half((tc >> 16) as u16), half((tc & 0xFFFF) as u16)], // high half = U, low half = V (checked against an SEModel export)
                normal: unpack_unit(u(24)),
                tangent: unpack_unit(u(28)),
            }
        })
        .collect()
}

/// A rigid surface from a zone's own arrays: 32-byte vertices and `u16` triangles (no skinning data).
pub fn surface_from_zone(header: &[u8], vbytes: &[u8], ibytes: &[u8]) -> Option<Surface> {
    let vertices = decode_vertices(vbytes);
    let indices: Vec<u32> = ibytes.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]]) as u32).collect();
    if indices.iter().any(|&i| i as usize >= vertices.len()) {
        return None;
    }
    let mut h = [0u8; 84];
    h.copy_from_slice(header.get(..84)?);
    Some(Surface { vertices, indices, rigid: Vec::new(), blend_counts: [0; 4], blend: Vec::new(), header: h })
}

struct Container {
    blocks: [Vec<u8>; 3],
}

impl Container {
    /// Applies the fix-ups; afterwards every reference word is
    /// `(block << 28) + offset + 1` with block 3, 6 or 7 for source blocks 0, 1, 2.
    fn read(b: &[u8]) -> Result<Container> {
        if b.len() < 88 || u32_at(b, 0) != Some(1) || u32_at(b, 4) != Some(MAGIC) {
            bail!("not a streamed model file");
        }
        let mut size = [0usize; 4];
        let mut offset = [0usize; 4];
        let mut start = [0usize; 4];
        let mut count = [0usize; 4];
        for i in 0..4 {
            let d = 8 + 20 * i;
            size[i] = u32_at(b, d).unwrap() as usize;
            offset[i] = u32_at(b, d + 4).unwrap() as usize;
            start[i] = u32_at(b, d + 8).unwrap() as usize;
            count[i] = u32_at(b, d + 12).unwrap() as usize;
            if size[i] > 0x0FFF_FFFF || offset[i] > b.len() || size[i] > b.len() - offset[i] || (size[i] != 0 && offset[i] < 88) {
                bail!("block {i} lies outside the file");
            }
        }
        if size[0] < 48 || size[3] % 4 != 0 || size[3] >= 0x4000 {
            bail!("bad block sizes");
        }
        let table = &b[offset[3]..offset[3] + size[3]];
        let mut blocks: [Vec<u8>; 3] = [0, 1, 2].map(|i| b[offset[i]..offset[i] + size[i]].to_vec());
        for i in 0..3 {
            if start[i] > size[3] / 4 || count[i] > size[3] / 4 - start[i] {
                bail!("fix-up range of block {i} is out of bounds");
            }
            for j in 0..count[i] {
                let fix = u32_at(table, 4 * (start[i] + j)).unwrap();
                let (loc, target) = ((fix >> 3) as usize, (fix & 7) as usize);
                if target > 2 || blocks[i].len() < 4 || loc > blocks[i].len() - 4 || loc % 4 != 0 {
                    bail!("bad fix-up in block {i}");
                }
                let value = u32_at(&blocks[i], loc).unwrap() as usize;
                if value >= size[target] {
                    bail!("fix-up in block {i} points outside block {target}");
                }
                let base = [3u32, 6, 7][target];
                let word = (base << 28).wrapping_add(value as u32).wrapping_add(1);
                blocks[i][loc..loc + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        Ok(Container { blocks })
    }

    /// Follows a relocated reference to `len` bytes.
    fn deref(&self, reference: u32, len: usize) -> Result<&[u8]> {
        if reference == 0 {
            bail!("null reference");
        }
        let r = reference - 1;
        let block = match r >> 28 {
            3 => 0,
            6 => 1,
            7 => 2,
            other => bail!("reference into unknown block {other}"),
        };
        let off = (r & 0x0FFF_FFFF) as usize;
        let data = &self.blocks[block];
        match data.get(off..off.checked_add(len).unwrap_or(usize::MAX)) {
            Some(s) => Ok(s),
            None => bail!("reference {off}+{len} runs past block {block} ({} bytes)", data.len()),
        }
    }
}

impl Model {
    pub fn parse(bytes: &[u8]) -> Result<Model> {
        let c = Container::read(bytes)?;
        let root = &c.blocks[0];
        let name_ref = u32_at(root, 0).unwrap_or(0);
        let surfaces_ref = u32_at(root, 4).unwrap_or(0);
        let count = u16_at(root, 8).unwrap_or(0) as usize;
        // The name is a NUL-terminated string at the referenced offset.
        let name = {
            let rest = c.deref(name_ref, 1)?;
            let off = ((name_ref - 1) & 0x0FFF_FFFF) as usize;
            let _ = rest;
            String::from_utf8_lossy(c.blocks[0][off..].split(|&x| x == 0).next().unwrap_or(&[])).into_owned()
        };
        let headers = if count > 0 { c.deref(surfaces_ref, count * SURFACE_BYTES)? } else { &[][..] };
        let mut surfaces = Vec::with_capacity(count);
        for i in 0..count {
            let h = &headers[i * SURFACE_BYTES..(i + 1) * SURFACE_BYTES];
            let vert_count = u16_at(h, 4).unwrap() as usize;
            let tri_count = u16_at(h, 6).unwrap() as usize;
            let mut blend_counts = [0u16; 4];
            let mut blend_len = 0usize;
            for j in 0..4 {
                blend_counts[j] = u16_at(h, 20 + 2 * j).unwrap();
                blend_len += blend_counts[j] as usize * (2 * j + 1);
            }
            let blend = if blend_len > 0 {
                c.deref(u32_at(h, 28).unwrap(), blend_len * 2)?.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect()
            } else {
                Vec::new()
            };
            let vbytes = if vert_count > 0 { c.deref(u32_at(h, 32).unwrap(), vert_count * VERTEX_BYTES)? } else { &[][..] };
            let ibytes = if tri_count > 0 { c.deref(u32_at(h, 16).unwrap(), tri_count * 6)? } else { &[][..] };
            let vertices = decode_vertices(vbytes);
            let indices: Vec<u32> = ibytes.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]]) as u32).collect();
            if let Some(&bad) = indices.iter().find(|&&x| x as usize >= vert_count) {
                bail!("surface {i}: index {bad} is outside its {vert_count} vertices");
            }
            let rigid_count = u32_at(h, 40).unwrap() as usize;
            let rigid = if rigid_count > 0 && rigid_count < 4096 {
                c.deref(u32_at(h, 44).unwrap(), rigid_count * 12)?
                    .chunks_exact(12)
                    .map(|r| Rigid {
                        bone_offset: u16::from_le_bytes([r[0], r[1]]),
                        vert_count: u16::from_le_bytes([r[2], r[3]]),
                        tri_offset: u16::from_le_bytes([r[4], r[5]]),
                        tri_count: u16::from_le_bytes([r[6], r[7]]),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut header = [0u8; 84];
            header.copy_from_slice(h);
            surfaces.push(Surface { vertices, indices, rigid, blend_counts, blend, header });
        }
        Ok(Model { name, surfaces })
    }

    pub fn vertex_count(&self) -> usize {
        self.surfaces.iter().map(|s| s.vertices.len()).sum()
    }

    /// Skin weights of every vertex of every surface (see [`Surface::skin_weights`]).
    pub fn skin_weights(&self) -> Vec<Vec<Vec<(u32, f32)>>> {
        self.surfaces.iter().map(Surface::skin_weights).collect()
    }

    pub fn triangle_count(&self) -> usize {
        self.surfaces.iter().map(|s| s.indices.len() / 3).sum()
    }

    /// Axis-aligned bounds of all vertices as stored; None when empty.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.surfaces.iter().flat_map(|s| s.vertices.iter());
        let first = it.next()?;
        let (mut lo, mut hi) = (first.pos, first.pos);
        for v in it {
            for k in 0..3 {
                lo[k] = lo[k].min(v.pos[k]);
                hi[k] = hi[k].max(v.pos[k]);
            }
        }
        Some((lo, hi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(half(0x3C00), 1.0);
        assert_eq!(half(0xC000), -2.0);
        assert_eq!(half(0x0000), 0.0);
        assert!((half(0x31A4) - 0.1760).abs() < 1e-3);
    }

    #[test]
    fn unit_vectors_are_roughly_unit_length() {
        let n = unpack_unit(0x3f7f_0399);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((len - 1.0).abs() < 0.05, "{len}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(Model::parse(&[0u8; 200]).is_err());
        assert!(Model::parse(b"short").is_err());
    }

    /// Builds the smallest valid container: one surface with one triangle.
    fn tiny() -> Vec<u8> {
        // Block 0 (root): header(48) + name "t\0" padded to 4 + surface header(84) + rigid(12)
        let mut b0 = vec![0u8; 48];
        let name_off = b0.len();
        b0.extend(b"t\0\0\0");
        let surf_off = b0.len();
        b0.extend(vec![0u8; 84]);
        let rigid_off = b0.len();
        b0.extend(vec![0u8; 12]);
        // Block 1: three vertices; block 2: one triangle.
        let mut b1 = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in p {
                b1.extend(c.to_le_bytes());
            }
            b1.extend(1.0f32.to_le_bytes());
            b1.extend(0xFFFF_FFFFu32.to_le_bytes());
            b1.extend(0u32.to_le_bytes());
            b1.extend(0x3f7f_7f7fu32.to_le_bytes());
            b1.extend(0x3f7f_7f7fu32.to_le_bytes());
        }
        let mut b2 = Vec::new();
        for i in [0u16, 1, 2] {
            b2.extend(i.to_le_bytes());
        }
        b2.extend([0u8; 2]);
        let put = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut b0, 0, name_off as u32);
        put(&mut b0, 4, surf_off as u32);
        b0[8..10].copy_from_slice(&1u16.to_le_bytes());
        b0[surf_off + 4..surf_off + 6].copy_from_slice(&3u16.to_le_bytes());
        b0[surf_off + 6..surf_off + 8].copy_from_slice(&1u16.to_le_bytes());
        put(&mut b0, surf_off + 16, 0); // triangle indices -> block 2 offset 0
        put(&mut b0, surf_off + 32, 0); // vertices -> block 1 offset 0
        put(&mut b0, surf_off + 40, 1);
        put(&mut b0, surf_off + 44, rigid_off as u32);
        b0[rigid_off + 2..rigid_off + 4].copy_from_slice(&3u16.to_le_bytes());
        b0[rigid_off + 6..rigid_off + 8].copy_from_slice(&1u16.to_le_bytes());
        // fix-ups: (location << 3 | target)
        let fixes = [
            (0usize, 0u32),
            (4, 0),
            (surf_off + 16, 2),
            (surf_off + 32, 1),
            (surf_off + 44, 0),
        ];
        let table: Vec<u8> = fixes.iter().flat_map(|&(l, t)| (((l as u32) << 3) | t).to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend(1u32.to_le_bytes());
        out.extend(MAGIC.to_le_bytes());
        let mut offset = 88usize;
        let blocks = [&b0, &b1, &b2];
        let mut descriptors = Vec::new();
        for (i, blk) in blocks.iter().enumerate() {
            descriptors.push((blk.len() as u32, offset as u32, if i == 0 { 0u32 } else { 5u32 }, if i == 0 { 5u32 } else { 0u32 }));
            offset += blk.len();
        }
        descriptors.push((table.len() as u32, offset as u32, 0, 0));
        for d in &descriptors {
            out.extend(d.0.to_le_bytes());
            out.extend(d.1.to_le_bytes());
            out.extend(d.2.to_le_bytes());
            out.extend(d.3.to_le_bytes());
            out.extend(0u32.to_le_bytes());
        }
        for blk in blocks {
            out.extend(blk.iter());
        }
        out.extend(table);
        out
    }

    #[test]
    fn parses_a_minimal_container() {
        let m = Model::parse(&tiny()).unwrap();
        assert_eq!(m.name, "t");
        assert_eq!(m.surfaces.len(), 1);
        assert_eq!(m.vertex_count(), 3);
        assert_eq!(m.triangle_count(), 1);
        assert_eq!(m.surfaces[0].rigid.len(), 1);
        let (lo, hi) = m.bounds().unwrap();
        assert_eq!((lo, hi), ([0.0, 0.0, 0.0], [1.0, 1.0, 0.0]));
    }
}
