// SPDX-License-Identifier: GPL-3.0-only (ported by its copyright holder from his own server code, codol-qa-server)
//! Walkers for map zones: GfxWorld (type 21) and the QA collision map (type 15). Port of the corresponding parts
//! of the server project's `codol_load_blocks.c` for the QA 3.3.3.3 layout (version 423).

use super::replay::{INLINE, INSERT, Kind, Replay, Res, WorldRec, align4};

const LATE: usize = 980; // QA header grows by 980 bytes between the sun and the outdoor image

impl<'a> Replay<'a> {
    /// Bytes the CoDOL GfxWorld header has between the sun flare and the outdoor image that IW4's does not.
    fn late(&self) -> usize {
        if self.iw4_sizes() { 0 } else { LATE }
    }

    /// Runtime-only arrays live in zero-filled block 2 and consume no stream bytes.
    fn world_zero(&mut self, reference: u32, count: u64, stride: usize, alignment: usize) -> Res {
        self.push(2)?;
        if reference != 0 {
            let len = count.checked_mul(stride as u64).ok_or_else(|| self.fail("runtime array size"))?;
            self.reserve(2, len as usize, alignment, true)?;
        }
        self.pop()
    }

    fn world_visibility(&mut self) -> Res {
        let h = self.pos;
        // stock IW4: 628 bytes (CoDOL QA grows by LATE = 980 and by 12 in the draw block, see `late`)
        self.reserve(0, if self.iw4_sizes() { 628 } else if self.v319() { 1612 } else { 1608 }, 4, true)?;
        self.push(3)?;
        let name_at = self.pos;
        for i in [0usize, 4] {
            if self.ptr(h + i) == INLINE {
                self.string()?;
            }
        }
        if self.apply {
            let name = self.str_of(self.rd32(h), name_at);
            self.rec.world = Some(WorldRec { name, ..Default::default() });
        }
        let count = self.rd32(h + 20);
        if count > i32::MAX as u32 {
            return Err(self.fail("sky count"));
        }
        if self.rd32(h + 24) != 0 {
            let skies = self.bytes(count, 16, 4)?;
            for i in 0..count as usize {
                let sky = skies + i * 16;
                if self.rd32(sky + 4) != 0 {
                    let n = self.rd32(sky);
                    self.bytes(n, 4, 4)?;
                }
                self.asset_reference(self.rd32(sky + 8), Kind::Image)?;
            }
        }
        if self.ptr(h + 56) == INLINE {
            let n = self.rd32(h + 8);
            self.bytes(n, 20, 4)?;
        }
        if self.rd32(h + 60) != 0 {
            let n = self.rd32(h + 12);
            self.bytes(n, 2, 2)?;
        }
        let count = self.rd32(h + 52);
        if count > i32::MAX as u32 {
            return Err(self.fail("cell count"));
        }
        self.push(2)?;
        if self.rd32(h + 64) != 0 {
            self.reserve(2, count as usize * 2048, 4, true)?;
        }
        self.pop()?;
        let mut counts = None;
        if self.rd32(h + 68) != 0 {
            counts = Some(self.bytes(count, 4, 4)?);
        }
        if self.rd32(h + 72) != 0 {
            let trees = self.bytes(count, 4, 128)?;
            for i in 0..count as usize {
                if self.rd32(trees + 4 * i) != 0 {
                    let Some(counts) = counts else { return Err(self.fail("tree counts missing")) };
                    let n = self.rd32(counts + 4 * i);
                    if n > i32::MAX as u32 {
                        return Err(self.fail("tree node count"));
                    }
                    let nodes = self.bytes(n, 44, 4)?;
                    for j in 0..n as usize {
                        let node = nodes + j * 44;
                        if self.ptr(node + 36) == INLINE {
                            let m = self.rd16(node + 34);
                            self.bytes(m, 2, 2)?;
                        }
                    }
                }
            }
        }
        if self.rd32(h + 76) != 0 {
            let cell_size = if self.iw4_sizes() { 40 } else { 48 };
            let cells = self.bytes(count, cell_size, 4)?;
            for i in 0..count as usize {
                let cell = cells + i * cell_size;
                let n = self.rd32(cell + 24);
                if self.rd32(cell + 28) != 0 {
                    if n > i32::MAX as u32 {
                        return Err(self.fail("portal count"));
                    }
                    let portals = self.bytes(n, 60, 4)?;
                    for j in 0..n as usize {
                        let portal = portals + j * 60;
                        if self.rd32(portal + 28) != 0 {
                            let m = self.rd8(portal + 34);
                            self.bytes(m, 12, 4)?;
                        }
                    }
                }
                if self.rd32(cell + 36) != 0 {
                    let m = self.rd8(cell + 32);
                    self.bytes(m, 1, 1)?;
                }
                if !self.iw4_sizes() && self.rd32(cell + 44) != 0 {
                    let m = self.rd8(cell + 40);
                    self.bytes(m, 1, 1)?;
                }
            }
        }
        Ok(())
    }

    fn world_draw(&mut self, d: usize) -> Res {
        let count = self.rd32(d);
        if count > i32::MAX as u32 {
            return Err(self.fail("probe count"));
        }
        if self.rd32(d + 4) != 0 {
            let images = self.bytes(count, 4, 4)?;
            for i in 0..count as usize {
                self.asset_reference(self.rd32(images + 4 * i), Kind::Image)?;
            }
        }
        if self.rd32(d + 8) != 0 {
            // IW4's reflection probe is its origin (12 bytes); CoDOL's adds a list of visible items (20 bytes)
            let probe_size = if self.iw4_sizes() { 12 } else { 20 };
            let probes = self.bytes(count, probe_size, 4)?;
            for i in 0..count as usize {
                let probe = probes + i * probe_size;
                if self.iw4_sizes() || self.rd32(probe + 12) == 0 {
                    continue;
                }
                let n = self.rd32(probe + 16);
                if n > i32::MAX as u32 {
                    return Err(self.fail("probe list count"));
                }
                let lists = self.bytes(n, 8, 4)?;
                for j in 0..n as usize {
                    if self.rd32(lists + j * 8) != 0 {
                        let m = self.rd32(lists + j * 8 + 4);
                        self.bytes(m, 2, 2)?;
                    }
                }
            }
        }
        self.world_zero(self.rd32(d + 12), count as u64, 4, 4)?;
        // CoDOL adds two arrays (12-byte and 1-byte records, one count) before the lightmaps: 12 bytes of header
        let s = if self.iw4_sizes() { 12 } else { 0 };
        if !self.iw4_sizes() {
            if self.rd32(d + 20) != 0 {
                let n = self.rd32(d + 16);
                self.bytes(n, 12, 4)?;
            }
            if self.rd32(d + 24) != 0 {
                let n = self.rd32(d + 16);
                self.bytes(n, 1, 1)?;
            }
        }
        let count = self.rd32(d + 28 - s);
        if count > i32::MAX as u32 {
            return Err(self.fail("lightmap count"));
        }
        if self.rd32(d + 32 - s) != 0 {
            let images = self.bytes(count, 8, 4)?;
            for i in 0..count as usize {
                for j in 0..2 {
                    self.asset_reference(self.rd32(images + i * 8 + 4 * j), Kind::Image)?;
                }
            }
        }
        self.world_zero(self.rd32(d + 36 - s), count as u64, 4, 4)?;
        self.world_zero(self.rd32(d + 40 - s), count as u64, 4, 4)?;
        for i in [44usize, 48] {
            self.asset_reference(self.rd32(d + i - s), Kind::Image)?;
        }
        // in IW4 the world's vertices and indices belong to the vertex and index blocks (6 and 7)
        let (vertex_block, index_block) = (3, 3);
        if self.rd32(d + 56 - s) != 0 {
            let n = self.rd32(d + 52 - s);
            self.push(vertex_block)?;
            let at = self.bytes(n, 44, 4)?;
            if let (true, Some(w)) = (self.apply, self.rec.world.as_mut()) {
                w.vertices = (at, n as usize);
            }
            self.pop()?;
        }
        if self.rd32(d + 68 - s) != 0 {
            let n = self.rd32(d + 64 - s);
            self.push(vertex_block)?;
            self.bytes(n, 1, 1)?;
            self.pop()?;
        }
        if self.rd32(d + 80 - s) != 0 {
            let n = self.rd32(d + 76 - s);
            self.push(index_block)?;
            let at = self.bytes(n, 2, 2)?;
            if let (true, Some(w)) = (self.apply, self.rec.world.as_mut()) {
                w.indices = (at, n as usize);
            }
            self.pop()?;
        }
        Ok(())
    }

    fn world_light_grid(&mut self, g: usize) -> Res {
        if self.rd32(g + 28) != 0 {
            let axis = self.rd32(g + 20) as usize;
            if axis > 2 {
                return Err(self.fail("light grid axis"));
            }
            let (minimum, maximum) = (self.rd16(g + 8 + 2 * axis), self.rd16(g + 14 + 2 * axis));
            if maximum < minimum {
                return Err(self.fail("light grid bounds"));
            }
            self.bytes(maximum - minimum + 1, 2, 2)?;
        }
        if self.rd32(g + 36) != 0 {
            let n = self.rd32(g + 32);
            self.bytes(n, 1, 1)?;
        }
        if self.rd32(g + 44) != 0 {
            let n = self.rd32(g + 40);
            self.bytes(n, 4, 4)?;
        }
        if self.rd32(g + 52) != 0 {
            let n = self.rd32(g + 48);
            self.bytes(n, 168, 4)?;
        }
        Ok(())
    }

    fn world_scene(&mut self, h: usize) -> Res {
        const STRIDES: [u64; 8] = [4, 4, 6, 4, 32768, 4, 4, 1];
        let early = if self.iw4_sizes() { h } else { h + 12 };
        let cells = self.rd32(h + 52) as u64;
        let words = (cells + 31) / 32;
        let lights = self.rd32(h + 32);
        let sun = self.rd32(h + 28);
        // the sun flare holds four materials in CoDOL, two in IW4
        let suns = if self.iw4_sizes() { 2 } else { 4 };
        if self.rd32(early + 212) != 0 {
            let n = self.rd32(early + 208);
            self.bytes(n, 60, 4)?;
        }
        if self.rd32(early + 248) != 0 {
            let n = self.rd32(early + 244);
            if n > i32::MAX as u32 {
                return Err(self.fail("world material count"));
            }
            let slots = 0x3000_0001 + align4(self.positions()[3]) as u32;
            let records = self.bytes(n, 8, 4)?;
            for i in 0..n as usize {
                // later surfaces point at these slots to name their material
                let r = self.rd32(records + i * 8);
                let start = self.pos;
                self.asset_reference(r, Kind::Material)?;
                if r >= INSERT {
                    self.record_alias(5, slots + 8 * i as u32, start);
                }
            }
        }
        for i in 0..suns {
            self.asset_reference(self.rd32(early + 256 + 4 * i), Kind::Material)?;
        }
        let h = h + self.late();
        self.asset_reference(self.rd32(h + 412), Kind::Image)?;
        let mut non_sun = 0u64;
        if self.rd32(h + 432) != 0 || self.rd32(h + 436) != 0 || self.rd32(h + 440) != 0 {
            if lights <= sun {
                return Err(self.fail("light counts"));
            }
            non_sun = (lights - sun - 1) as u64;
        }
        let mut counts = [0u64; 8];
        counts[0] = words * cells;
        counts[1] = words;
        counts[2] = self.rd32(h + 572) as u64;
        counts[3] = self.rd32(h + 576) as u64;
        counts[4] = non_sun;
        counts[5] = non_sun * counts[2];
        counts[6] = non_sun * counts[3];
        counts[7] = counts[2];
        for i in 0..8 {
            self.push(2)?;
            if self.rd32(h + 416 + 4 * i) != 0 {
                let len = counts[i].checked_mul(STRIDES[i]).ok_or_else(|| self.fail("scene array size"))?;
                self.reserve(2, len as usize, if i == 7 { 1 } else { 4 }, true)?;
            }
            self.pop()?;
        }
        if lights > i32::MAX as u32 {
            return Err(self.fail("light count"));
        }
        if self.rd32(h + 448) != 0 {
            let records = self.bytes(lights, 12, 4)?;
            for i in 0..lights as usize {
                let r = records + i * 12;
                if self.rd32(r + 4) != 0 {
                    let n = self.rd16(r);
                    self.bytes(n, 2, 2)?;
                }
                if self.rd32(r + 8) != 0 {
                    let n = self.rd16(r + 2);
                    self.bytes(n, 2, 2)?;
                }
            }
        }
        if self.rd32(h + 452) != 0 {
            let records = self.bytes(lights, 8, 4)?;
            for i in 0..lights as usize {
                let r = records + i * 8;
                if self.rd32(r + 4) != 0 {
                    let n = self.rd32(r);
                    if n > i32::MAX as u32 {
                        return Err(self.fail("hull count"));
                    }
                    let hulls = self.bytes(n, 80, 4)?;
                    for j in 0..n as usize {
                        let hull = hulls + j * 80;
                        if self.rd32(hull + 76) != 0 {
                            let m = self.rd32(hull + 72);
                            self.bytes(m, 20, 4)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn world_dpvs(&mut self, h: usize) -> Res {
        let p = h + self.late() + 456;
        let d = h + self.late() + 564;
        let models = self.rd32(p);
        let surfaces = self.rd32(h + 16);
        let sorted = self.rd32(p + 4) as u64 + self.rd32(p + 8) as u64;
        if models > i32::MAX as u32 || surfaces > i32::MAX as u32 || sorted > u32::MAX as u64 {
            return Err(self.fail("dpvs counts"));
        }
        for i in 0..6usize {
            let count = self.rd32(p + if i < 3 { 0 } else { 4 }) as u64;
            self.world_zero(self.rd32(p + 52 + 4 * i), count, 1, 1)?;
        }
        if self.rd32(p + 76) != 0 {
            self.bytes(sorted as u32, 2, 2)?;
        }
        if self.rd32(p + 80) != 0 {
            let at = self.bytes(models, 36, 4)?;
            if self.apply {
                if let Some(w) = self.rec.world.as_mut() {
                    w.static_insts = at;
                }
            }
        }
        if self.rd32(p + 84) != 0 {
            let records = self.bytes(surfaces, 24, 4)?;
            for i in 0..surfaces as usize {
                let reference = self.rd32(records + i * 24 + 16);
                self.asset_reference(reference, Kind::Material)?;
                if self.apply {
                    // the material's name: the one just loaded, or the one an earlier reference loaded
                    let mut start = if reference >= INSERT { Some(self.rec.g_last_material_start as usize) } else { self.rec.aliases.get(&(5, reference)).copied() };
                    // stock zones: the loader's 4-byte slots for reusable pointers are not reserved here, so the addresses drift by a few
                    // bytes; materials sit hundreds of bytes apart, so the nearest one is the one meant
                    if start.is_none() && self.iw4_sizes() && reference < INSERT {
                        start = self.rec.aliases.iter().filter(|((t, _), _)| *t == 5).map(|((_, a), s)| ((*a as i64 - reference as i64).abs(), *s)).filter(|(d, _)| *d <= 64).min_by_key(|(d, _)| *d).map(|(_, s)| s);
                    }
                    let name = start.and_then(|s| self.rec.material_by_start.get(&s)).and_then(|&m| self.rec.materials[m].name.clone());
                    if let Some(w) = self.rec.world.as_mut() {
                        w.surfaces.push((records + i * 24, name));
                    }
                }
            }
        }
        if self.rd32(p + 88) != 0 {
            self.bytes(surfaces, 24, 4)?;
        }
        if self.rd32(p + 92) != 0 {
            let slots = 0x3000_0001 + align4(self.positions()[3]) as u32;
            let records = self.bytes(models, 76, 4)?;
            for i in 0..models as usize {
                let r = self.rd32(records + i * 76 + 52);
                let start = self.pos;
                self.asset_reference(r, Kind::Model)?;
                if r >= INSERT {
                    self.record_alias(4, slots + 76 * i as u32 + 52, start);
                }
                if self.apply {
                    let header = if r >= INSERT { Some(start) } else { self.rec.aliases.get(&(4, r)).copied() };
                    let name = header.and_then(|s| self.rec.model_names.get(&s)).cloned().flatten();
                    if let Some(w) = self.rec.world.as_mut() {
                        w.static_models.push((records + i * 76, name));
                    }
                }
            }
        }
        // a draw-surface key is 8 bytes in IW4 and 16 in CoDOL
        self.world_zero(self.rd32(p + 96), surfaces as u64, if self.iw4_sizes() { 8 } else { 16 }, 8)?;
        self.world_zero(self.rd32(p + 100), self.rd32(p + 48) as u64, 4, 128)?;
        for i in 0..2usize {
            let count = self.rd32(h + 52) as u64 * self.rd32(d + 4 * i) as u64;
            self.world_zero(self.rd32(d + 16 + 4 * i), count, 4, 4)?;
        }
        for i in 0..3usize {
            for j in 0..2usize {
                self.world_zero(self.rd32(d + 24 + 12 * j + 4 * i), self.rd32(d + 4 * j) as u64, 32, 16)?;
            }
        }
        Ok(())
    }

    pub(super) fn world(&mut self) -> Res {
        let h = self.pos;
        if self.v319() {
            self.world_header = Some((h, h + 1608));
        }
        let r = self.world_body(h);
        self.world_header = None;
        r
    }

    fn world_body(&mut self, h: usize) -> Res {
        self.world_visibility()?;
        self.world_draw(h + 80)?;
        self.world_light_grid(h + if self.iw4_sizes() { 152 } else { 164 })?;
        self.world_scene(h)?;
        self.world_dpvs(h)?;
        if self.apply {
            self.rec.lights.sun_index = Some(self.rd32(h + 28));
        }
        if self.rd32(h + self.late() + 620) != 0 {
            let n = self.rd32(h + self.late() + 616);
            let at = self.bytes(n, 56, 4)?;
            if self.apply {
                // GfxHeroLight: type, pad, color, dir, origin, radius, cosHalfFovOuter, cosHalfFovInner, exponent
                for i in 0..n as usize {
                    let l = at + i * 56;
                    let f = |s: &Self, o: usize| f32::from_bits(s.rd32(l + o));
                    let ty = self.rd8(l) as u8;
                    let light = super::world::MapLight {
                        kind: super::world::LightKind::of(ty),
                        ty,
                        color: [f(self, 4), f(self, 8), f(self, 12)],
                        direction: [f(self, 16), f(self, 20), f(self, 24)],
                        origin: [f(self, 28), f(self, 32), f(self, 36)],
                        radius: f(self, 40),
                        cos_outer: f(self, 44),
                        cos_inner: f(self, 48),
                        exponent: self.rd32(l + 52) as i32,
                        hero: true,
                        name: None,
                    };
                    self.rec.lights.hero.push(light);
                }
            }
        }
        self.pop()
    }

    // ---- collision (QA) ---------------------------------------------------------------------------------

    fn qa_plane(&mut self, pointer: u32) -> Res {
        if pointer != INLINE {
            self.note_pointer(pointer);
            return Ok(());
        }
        self.bytes(1, 20, 4)?;
        Ok(())
    }

    fn qa_clip_info(&mut self, c: usize) -> Res {
        if self.clip_on && self.apply {
            let (tag, header) = (self.clip_tag, self.clip_header);
            self.rec.clips.push(super::replay::ClipRec { tag, header, ..Default::default() });
        }
        if self.ptr(c + 4) == INLINE {
            let n = self.rd32(c);
            let at = self.bytes(n, 20, 4)?;
            self.clip_note(|k| k.planes = (at, n as usize));
        }
        if self.ptr(c + 12) == INLINE {
            let n = self.rd32(c + 8);
            let records = self.bytes(n, 12, 4)?;
            self.clip_note(|k| k.materials = (records, n as usize));
            for i in 0..n as usize {
                let named = self.ptr(records + 12 * i) == INLINE;
                let name = if named { Some(self.pos) } else { None };
                if named {
                    self.string()?;
                }
                if let Some(at) = name {
                    let text = self.cstr(at);
                    self.clip_note(|k| k.material_names.push(text));
                } else {
                    self.clip_note(|k| k.material_names.push(None));
                }
            }
        }
        if self.ptr(c + 20) == INLINE {
            let n = self.rd32(c + 16);
            let records = self.bytes(n, 8, 4)?;
            self.clip_note(|k| k.sides = (records, n as usize));
            for i in 0..n as usize {
                let p = self.rd32(records + 8 * i);
                if p != 0 {
                    self.qa_plane(p)?;
                }
            }
        }
        if self.ptr(c + 28) == INLINE {
            let n = self.rd32(c + 24);
            self.bytes(n, 1, 1)?;
        }
        if self.ptr(c + 36) == INLINE {
            let n = self.rd32(c + 32);
            let records = self.bytes(n, 20, 4)?;
            self.clip_note(|k| {
                k.lb_nodes = (records, n as usize);
                k.lb_inline = vec![0; n as usize];
            });
            for i in 0..n as usize {
                let r = records + 20 * i;
                let leaves = self.rd16(r + 2) as u16 as i16;
                if leaves > 0 && self.ptr(r + 8) == INLINE {
                    let at = self.bytes(leaves as u32, 2, 2)?;
                    self.clip_note(|k| k.lb_inline[i] = at);
                }
            }
        }
        if self.ptr(c + 44) == INLINE {
            let n = self.rd32(c + 40);
            let at = self.bytes(n, 2, 2)?;
            self.clip_note(|k| k.leaf_brushes = (at, n as usize));
        }
        let count = self.rd16(c + 48);
        if self.ptr(c + 52) == INLINE {
            let records = self.bytes(count, 36, 128)?;
            self.clip_note(|k| k.brushes = (records, count as usize));
            for i in 0..count as usize {
                let r = records + 36 * i;
                if self.ptr(r + 4) == INLINE {
                    let side = self.bytes(1, 8, 4)?;
                    let p = self.rd32(side);
                    if p != 0 {
                        self.qa_plane(p)?;
                    }
                }
                if self.ptr(r + 8) == INLINE {
                    self.bytes(1, 1, 1)?;
                }
            }
        }
        if self.ptr(c + 56) == INLINE {
            let at = self.bytes(count, 24, 128)?;
            self.clip_note(|k| k.bounds = at);
        }
        if self.ptr(c + 60) == INLINE {
            let at = self.bytes(count, 4, 4)?;
            self.clip_note(|k| k.contents = at);
        }
        Ok(())
    }

    /// Records into the collision-map record while the first `ClipInfo` is walked.
    fn clip_note(&mut self, f: impl FnOnce(&mut super::replay::ClipRec)) {
        if self.clip_on && self.apply {
            if let Some(k) = self.rec.clips.last_mut() {
                f(k);
            }
        }
    }

    fn qa_clip_info_pointer(&mut self, pointer: u32) -> Res {
        self.note_pointer(pointer);
        self.push(0)?;
        if pointer == INSERT {
            self.reserve(3, 4, 4, false)?;
        }
        if pointer >= INSERT {
            let info = self.bytes(1, 64, 4)?;
            self.qa_clip_info(info)?;
        }
        self.pop()
    }

    /// The stock IW4 collision map (`clipMap_t`, 256 bytes). CoDOL's QA map is laid out like IW5's (a 64-byte collision-info
    /// block at +8, a second one behind a pointer, a 72-byte model record), so this is a separate walker. The order is the
    /// loader's: planes, static models, materials, brush sides, brush edges, nodes, leafs, leaf brushes, leaf brush nodes,
    /// vertices, triangles, borders, partitions, trees, models, brushes, bounds, contents, static model nodes, map entities,
    /// dynamic entities.
    pub(super) fn stock_clip_map(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 256, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if self.ptr(h + 12) == INLINE {
            let n = self.rd32(h + 8);
            self.bytes(n, 20, 4)?;
        }
        if self.ptr(h + 20) == INLINE {
            let n = self.rd32(h + 16);
            let records = self.bytes(n, 76, 4)?;
            for i in 0..n as usize {
                self.asset_reference(self.rd32(records + 76 * i), Kind::Model)?;
            }
        }
        if self.ptr(h + 28) == INLINE {
            let n = self.rd32(h + 24);
            let records = self.bytes(n, 12, 4)?;
            for i in 0..n as usize {
                if self.ptr(records + 12 * i) == INLINE {
                    self.string()?;
                }
            }
        }
        if self.ptr(h + 36) == INLINE {
            let n = self.rd32(h + 32);
            let records = self.bytes(n, 8, 4)?;
            for i in 0..n as usize {
                self.qa_plane(self.rd32(records + 8 * i))?;
            }
        }
        if self.ptr(h + 44) == INLINE {
            let n = self.rd32(h + 40);
            self.bytes(n, 1, 1)?;
        }
        if self.ptr(h + 52) == INLINE {
            let n = self.rd32(h + 48);
            let records = self.bytes(n, 8, 4)?;
            for i in 0..n as usize {
                self.qa_plane(self.rd32(records + 8 * i))?;
            }
        }
        if self.ptr(h + 60) == INLINE {
            let n = self.rd32(h + 56);
            self.bytes(n, 40, 4)?;
        }
        if self.ptr(h + 76) == INLINE {
            let n = self.rd32(h + 72);
            self.bytes(n, 2, 2)?;
        }
        if self.ptr(h + 68) == INLINE {
            let n = self.rd32(h + 64);
            let records = self.bytes(n, 20, 4)?;
            for i in 0..n as usize {
                let count = self.rd16(records + 20 * i + 2) as u16 as i16;
                if count > 0 && self.ptr(records + 20 * i + 8) == INLINE {
                    self.bytes(count as u32, 2, 2)?;
                }
            }
        }
        if self.ptr(h + 84) == INLINE {
            let n = self.rd32(h + 80);
            self.bytes(n, 4, 4)?;
        }
        if self.ptr(h + 92) == INLINE {
            let n = self.rd32(h + 88);
            self.bytes(n, 12, 4)?;
        }
        let triangles = self.rd32(h + 96);
        if self.ptr(h + 100) == INLINE {
            self.bytes(triangles, 6, 2)?;
        }
        if self.ptr(h + 104) == INLINE {
            let n = ((3u32.wrapping_mul(triangles).wrapping_add(31)) >> 5).wrapping_mul(4);
            self.bytes(n, 1, 1)?;
        }
        if self.ptr(h + 112) == INLINE {
            let n = self.rd32(h + 108);
            self.bytes(n, 28, 4)?;
        }
        if self.ptr(h + 120) == INLINE {
            let n = self.rd32(h + 116);
            let records = self.bytes(n, 12, 4)?;
            for i in 0..n as usize {
                if self.ptr(records + 12 * i + 8) == INLINE {
                    self.bytes(1, 28, 4)?;
                }
            }
        }
        if self.ptr(h + 128) == INLINE {
            let n = self.rd32(h + 124);
            self.bytes(n, 32, 16)?;
        }
        if self.ptr(h + 136) == INLINE {
            let n = self.rd32(h + 132);
            self.bytes(n, 68, 4)?;
        }
        let brushes = self.rd16(h + 140);
        if self.ptr(h + 144) == INLINE {
            let records = self.bytes(brushes, 36, 4)?;
            for i in 0..brushes as usize {
                let r = records + 36 * i;
                if self.ptr(r + 4) == INLINE {
                    let side = self.bytes(1, 8, 4)?;
                    self.qa_plane(self.rd32(side))?;
                }
                if self.ptr(r + 8) == INLINE {
                    self.bytes(1, 1, 1)?;
                }
            }
        }
        if self.ptr(h + 148) == INLINE {
            self.bytes(brushes, 24, 4)?;
        }
        if self.ptr(h + 152) == INLINE {
            self.bytes(brushes, 4, 4)?;
        }
        if self.ptr(h + 164) == INLINE {
            let n = self.rd16(h + 160);
            self.bytes(n, 28, 4)?;
        }
        self.asset_reference(self.rd32(h + 156), Kind::AddonMapEnts)?;
        for i in 0..2usize {
            if self.ptr(h + 172 + 4 * i) == INLINE {
                let count = self.rd16(h + 168 + 2 * i);
                // 92 bytes in the retail zones (ZoneTool's struct leaves out a trailing pointer)
                let records = self.bytes(count, 92, 4)?;
                for j in 0..count as usize {
                    let r = records + 92 * j;
                    self.asset_reference(self.rd32(r + 32), Kind::Model)?;
                    self.asset_reference(self.rd32(r + 40), Kind::Effect)?;
                    self.asset_reference(self.rd32(r + 44), Kind::Preset)?;
                }
            }
        }
        for (list, stride) in [(180usize, 32usize), (188, 12), (196, 20)] {
            for k in 0..2usize {
                let count = self.rd16(h + 168 + 2 * k) as u64;
                self.world_zero(self.rd32(h + list + 4 * k), count, stride, 4)?;
            }
        }
        self.pop()
    }

    pub(super) fn collision_qa(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 256, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        self.clip_on = true;
        self.clip_tag = 0;
        self.clip_header = h;
        self.qa_clip_info(h + 8)?;
        self.clip_tag = 1;
        self.qa_clip_info_pointer(self.rd32(h + 72))?;
        if self.rd32(h + 80) != 0 {
            let n = self.rd32(h + 76);
            let records = self.bytes(n, 76, 4)?;
            if self.apply {
                self.rec.clip_statics = (records, n as usize);
            }
            for i in 0..n as usize {
                self.asset_reference(self.rd32(records + 76 * i), Kind::Model)?;
            }
        }
        if self.rd32(h + 88) != 0 {
            let n = self.rd32(h + 84);
            let records = self.bytes(n, 8, 4)?;
            for i in 0..n as usize {
                let p = self.rd32(records + 8 * i);
                if p != 0 {
                    self.qa_plane(p)?;
                }
            }
        }
        if self.rd32(h + 96) != 0 {
            let n = self.rd32(h + 92);
            self.bytes(n, 40, 4)?;
        }
        if self.rd32(h + 104) != 0 {
            let n = self.rd32(h + 100);
            self.bytes(n, 12, 4)?;
        }
        if self.rd32(h + 112) != 0 {
            let n = self.rd32(h + 108);
            self.bytes(n, 6, 2)?;
        }
        if self.rd32(h + 116) != 0 {
            let n = ((3u32.wrapping_mul(self.rd32(h + 108)).wrapping_add(31)) >> 3) & !3;
            self.bytes(n, 1, 1)?;
        }
        if self.rd32(h + 124) != 0 {
            let n = self.rd32(h + 120);
            self.bytes(n, 28, 4)?;
        }
        if self.rd32(h + 132) != 0 {
            let n = self.rd32(h + 128);
            let records = self.bytes(n, 12, 4)?;
            for i in 0..n as usize {
                if self.ptr(records + 12 * i + 8) == INLINE {
                    self.bytes(1, 28, 4)?;
                }
            }
        }
        if self.rd32(h + 140) != 0 {
            let n = self.rd32(h + 136);
            self.bytes(n, 32, 16)?;
        }
        if self.rd32(h + 148) != 0 {
            let n = self.rd32(h + 144);
            let records = self.bytes(n, 72, 4)?;
            if self.apply {
                self.rec.cmodels = (records, n as usize);
            }
            for i in 0..n as usize {
                self.clip_tag = 100 + i as u32;
                self.qa_clip_info_pointer(self.rd32(records + 72 * i + 28))?;
            }
        }
        self.clip_on = false;
        if self.rd32(h + 192) != 0 {
            let n = self.rd16(h + 188);
            self.bytes(n, 28, 4)?;
        }
        self.asset_reference(self.rd32(h + 152), Kind::AddonMapEnts)?;
        if self.rd32(h + 156) != 0 {
            let n = self.rd8(h + 160);
            let records = self.bytes(n, 20, 4)?;
            for i in 0..n as usize {
                if self.ptr(records + 20 * i) == INLINE {
                    self.string()?;
                }
            }
        }
        const STRIDES: [usize; 3] = [8, 32, 20];
        for i in 0..3 {
            if self.ptr(h + 168 + 8 * i) == INLINE {
                let n = self.rd32(h + 164 + 8 * i);
                self.bytes(n, STRIDES[i], 4)?;
            }
        }
        for i in 0..2usize {
            if self.rd32(h + 200 + 4 * i) != 0 {
                let slots = 0x3000_0001 + align4(self.positions()[3]) as u32;
                let count = self.rd16(h + 196 + 2 * i);
                let records = self.bytes(count, 92, 4)?;
                for j in 0..count as usize {
                    let r = records + j * 92;
                    let start = self.pos;
                    let m = self.rd32(r + 32);
                    self.asset_reference(m, Kind::Model)?;
                    if m >= INSERT {
                        self.record_alias(4, slots + 92 * j as u32 + 32, start);
                    }
                    let start = self.pos;
                    let e = self.rd32(r + 40);
                    self.asset_reference(e, Kind::Effect)?;
                    if e >= INSERT {
                        self.record_alias(30, slots + 92 * j as u32 + 40, start);
                    }
                    let start = self.pos;
                    let p = self.rd32(r + 44);
                    self.asset_reference(p, Kind::Preset)?;
                    if p >= INSERT {
                        self.record_alias(0, slots + 92 * j as u32 + 44, start);
                    }
                }
            }
        }
        for i in 0..3usize {
            for k in 0..2usize {
                let count = self.rd16(h + 196 + 2 * k) as u64;
                let stride = [32, 12, 20][i];
                self.world_zero(self.rd32(h + 208 + 8 * i + 4 * k), count, stride, 4)?;
            }
        }
        self.pop()
    }
}
