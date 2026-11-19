// SPDX-License-Identifier: GPL-3.0-only (ported by its copyright holder from his own server code, codol-qa-server)
//! More walkers for `replay.rs`: menus, map data and the small world assets (see that file for the model).

use super::replay::{Fail, INLINE, INSERT, Kind, Replay, Res};

impl<'a> Replay<'a> {
    pub(super) fn light_definition(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 16, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        self.asset_reference(self.rd32(h + 4), Kind::Image)?;
        self.pop()
    }

    pub(super) fn common_world(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 16, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        let count = self.rd32(h + 8);
        if count > i32::MAX as u32 {
            return Err(self.fail("com_map light count"));
        }
        if self.rd32(h + 12) != 0 {
            let lights = self.bytes(count, 68, 4)?;
            for i in 0..count as usize {
                let (l, at) = (lights + i * 68, self.pos);
                let named = self.rd32(l + 64) == INLINE;
                if named {
                    self.string()?;
                }
                if self.apply {
                    // ComPrimaryLight: type, canUseShadowMap, exponent, unused, color, dir, origin, radius, cosHalfFovOuter, cosHalfFovInner, ...
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
                        exponent: self.rd8(l + 2) as i32,
                        hero: false,
                        name: if named { self.str_of(INLINE, at) } else { None },
                    };
                    self.rec.lights.primary.push(light);
                }
            }
        }
        self.pop()
    }

    pub(super) fn addon_map_entities(&mut self) -> Res {
        const STRIDES: [usize; 3] = [8, 32, 20];
        let h = self.pos;
        // newer builds (the 2026-11 QA zones, 3.19) carry 16 more header bytes: a stage pointer (-1) and zeros; the older
        // 36-byte record has the map name's first characters at +36, which are never 0xffffffff
        // stock zones: the stage fields end a 44-byte header (a pointer that is 0 or -1 at +36); the `so_*` addon records are 36 bytes,
        // so what stands at +36 is the first characters of the name
        let header = if self.stock && !matches!(self.rd32(h + 36), 0 | INLINE) { 36 } else if self.iw4_sizes() { 44 } else if self.rd32(h + 36) == INLINE { 52 } else { 36 };
        self.reserve(0, header, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 4) != 0 {
            let n = self.rd32(h + 8);
            self.bytes(n, 1, 1)?;
        }
        for i in 0..3 {
            if self.rd32(h + 16 + 8 * i) != 0 {
                let n = self.rd32(h + 12 + 8 * i);
                self.bytes(n, STRIDES[i], 4)?;
            }
        }
        if header >= 44 && self.rd32(h + 36) != 0 && self.rd8(h + 40) != 0 {
            let n = self.rd8(h + 40);
            let stages = self.bytes(n, 20, 4)?;
            for i in 0..n as usize {
                if self.rd32(stages + 20 * i) == INLINE {
                    self.string()?;
                }
            }
        }
        if header == 52 {
            // an 84-byte record follows the arrays in these builds (its layout is not decoded; only its size is needed)
            let rest = self.remaining();
            self.bytes(rest as u32, 1, 4)?;
        }
        self.pop()
    }

    /// Type 42 in the `asym_addon_mp_*` zones is not the 36-byte addon entity record of the co-op zones: a 52-byte header
    /// (an inline pointer at +0, a count at +40, the name pointer at +48), the map name, then `count` records of 60 bytes
    /// (all zero in every shipped zone). The entity-string word at +4 is 0 here, which is how the two are told apart.
    pub(super) fn asym_addon_data(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 52, 4, true)?;
        self.push(3)?;
        if self.rd32(h + 48) == INLINE {
            self.string()?;
        }
        if self.rd32(h) != 0 {
            let n = self.rd32(h + 40);
            self.bytes(n, 60, 4)?;
        }
        self.pop()
    }

    /// One segment of a vehicle track (`GameWorldMp`'s path data in the 1.1.1.13 client).
    fn vehicle_segment(&mut self, r: usize, depth: usize) -> Res {
        if depth >= 64 {
            return Err(self.fail("vehicle track nesting"));
        }
        if self.rd32(r) == INLINE {
            self.string()?;
        }
        if self.rd32(r + 4) != 0 {
            let n = self.rd32(r + 8);
            let sectors = self.bytes(n, 60, 4)?;
            for i in 0..n as usize {
                let sector = sectors + 60 * i;
                if self.rd32(sector + 52) != 0 {
                    let m = self.rd32(sector + 56);
                    self.bytes(m, 12, 4)?;
                }
            }
        }
        for j in [12usize, 20] {
            if self.rd32(r + j) != 0 {
                let n = self.rd32(r + j + 4);
                let refs = self.bytes(n, 4, 4)?;
                for i in 0..n as usize {
                    if self.rd32(refs + 4 * i) == INLINE {
                        let child = self.bytes(1, 44, 4)?;
                        self.vehicle_segment(child, depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The 1.1.1.13 client's multiplayer game world: an 84-byte record with a vehicle track and the glass data.
    pub(super) fn c1_game_world(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 84, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 72) == INLINE {
            let n = self.rd32(h + 76);
            let records = self.bytes(n, 44, 4)?;
            for i in 0..n as usize {
                self.vehicle_segment(records + 44 * i, 0)?;
            }
        }
        if self.rd32(h + 80) != 0 {
            let glass = self.bytes(1, 128, 4)?;
            if self.rd32(glass) != 0 {
                let n = self.rd32(glass + 4);
                self.bytes(n, 12, 4)?;
            }
            if self.rd32(glass + 16) != 0 {
                let n = self.rd32(glass + 12);
                let names = self.bytes(n, 12, 4)?;
                for i in 0..n as usize {
                    let r = names + 12 * i;
                    if self.rd32(r) == INLINE {
                        self.string()?;
                    }
                    if self.rd32(r + 8) != 0 {
                        let m = self.rd16(r + 6);
                        self.bytes(m, 2, 2)?;
                    }
                }
            }
        }
        self.pop()
    }

    /// The stock IW4 single-player game world (56 bytes): a name, the path data (nodes with their links, base nodes, chain
    /// tables, visibility bytes, the node tree), the vehicle tracks and the glass data.
    pub(super) fn stock_sp_game_world(&mut self) -> Res {
        // pathnode_t is 136 bytes. Stream order: nodes with their links (12 bytes each), the visibility bytes, the chain tables,
        // the node tree (children are references to earlier trees); the base-node pointer at +12 has no data of its own.
        let node_size: usize = 136;
        let h = self.pos;
        self.reserve(0, 56, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        let count = self.rd32(h + 4);
        if count > i32::MAX as u32 {
            return Err(self.fail("path node count"));
        }
        if self.rd32(h + 8) != 0 {
            let nodes = self.bytes(count, node_size, 4)?;
            for i in 0..count as usize {
                let node = nodes + i * node_size;
                // totalLinkCount (16 bits) at +56 and the link array pointer at +60 end the constant part; a link is 12 bytes
                if self.rd32(node + 60) != 0 {
                    let n = self.rd16(node + 56);
                    self.bytes(n, 12, 4)?;
                }
            }
        }
        if self.rd32(h + 32) != 0 {
            let n = self.rd32(h + 28);
            self.bytes(n, 1, 1)?;
        }
        let chains = self.rd32(h + 16);
        if chains > i32::MAX as u32 {
            return Err(self.fail("path chain count"));
        }
        if self.rd32(h + 20) != 0 {
            self.bytes(count, 2, 2)?;
        }
        if self.rd32(h + 24) != 0 {
            self.bytes(chains, 2, 2)?;
        }
        if self.rd32(h + 40) != 0 {
            let n = self.rd32(h + 36);
            let trees = self.bytes(n, 16, 4)?;
            for i in 0..n as usize {
                self.path_tree(trees + 16 * i, 0)?;
            }
        }
        if self.rd32(h + 44) == INLINE {
            let n = self.rd32(h + 48);
            let records = self.bytes(n, 44, 4)?;
            for i in 0..n as usize {
                self.vehicle_segment(records + 44 * i, 0)?;
            }
        }
        self.glass_data(h + 52)?;
        self.pop()
    }

    /// One node of the path tree: a leaf (negative axis) lists node numbers, an inner node points at two children.
    fn path_tree(&mut self, t: usize, depth: usize) -> Res {
        if depth >= 64 {
            return Err(self.fail("path tree nesting"));
        }
        if (self.rd32(t) as i32) < 0 {
            if self.rd32(t + 12) != 0 {
                let n = self.rd32(t + 8);
                self.bytes(n, 2, 2)?;
            }
        } else {
            for k in 0..2 {
                if self.rd32(t + 8 + 4 * k) == INLINE {
                    let child = self.bytes(1, 16, 4)?;
                    self.path_tree(child, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// The glass data pointed at by the word at `field`.
    fn glass_data(&mut self, field: usize) -> Res {
        if self.rd32(field) != 0 {
            let glass = self.bytes(1, 128, 4)?;
            if self.rd32(glass) != 0 {
                let n = self.rd32(glass + 4);
                self.bytes(n, 12, 4)?;
            }
            if self.rd32(glass + 16) != 0 {
                let n = self.rd32(glass + 12);
                let names = self.bytes(n, 12, 4)?;
                for i in 0..n as usize {
                    let r = names + 12 * i;
                    if self.rd32(r) == INLINE {
                        self.string()?;
                    }
                    if self.rd32(r + 8) != 0 {
                        let m = self.rd16(r + 6);
                        self.bytes(m, 2, 2)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The stock IW4 multiplayer game world: a name and the glass data (128 bytes, pieces and named groups). CoDOL has no
    /// loader for this type.
    pub(super) fn stock_game_world(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 8, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 4) != 0 {
            let glass = self.bytes(1, 128, 4)?;
            if self.rd32(glass) != 0 {
                let n = self.rd32(glass + 4);
                self.bytes(n, 12, 4)?;
            }
            if self.rd32(glass + 16) != 0 {
                let n = self.rd32(glass + 12);
                let names = self.bytes(n, 12, 4)?;
                for i in 0..n as usize {
                    let r = names + 12 * i;
                    if self.rd32(r) == INLINE {
                        self.string()?;
                    }
                    if self.rd32(r + 8) != 0 {
                        let m = self.rd16(r + 6);
                        self.bytes(m, 2, 2)?;
                    }
                }
            }
        }
        self.pop()
    }

    pub(super) fn effect_world(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 116, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        let g = h + 4;
        let count = self.rd32(g + 8);
        if count > i32::MAX as u32 {
            return Err(self.fail("fx_map definition count"));
        }
        if self.rd32(g + 48) != 0 {
            let slots = 0x3000_0001 + super::replay::align4(self.positions()[3]) as u32;
            let defs = self.bytes(count, 36, 4)?;
            for i in 0..count as usize {
                let d = defs + i * 36;
                let start = self.pos;
                self.asset_reference(self.rd32(d + 32), Kind::Preset)?;
                if self.rd32(d + 32) >= INSERT {
                    self.record_alias(0, slots + 36 * i as u32 + 32, start);
                }
                self.asset_reference(self.rd32(d + 24), Kind::Material)?;
                self.asset_reference(self.rd32(d + 28), Kind::Material)?;
            }
        }
        let pieces = self.rd32(g + 12) as u64;
        let words = self.rd32(g + 16) as u64;
        let mut sizes = [0u64; 9];
        sizes[0] = pieces * 32;
        sizes[1] = pieces * 32;
        sizes[2] = pieces * 36;
        sizes[3] = self.rd32(g + 36) as u64 * 4;
        sizes[4] = words * 4;
        sizes[5] = words * self.rd32(g + 24) as u64 * 4;
        sizes[6] = (pieces + 15) & !15;
        sizes[7] = pieces * 12;
        sizes[8] = ((pieces + 3) & !3) * 4;
        for i in 0..9 {
            self.push(2)?;
            if self.rd32(g + 52 + 4 * i) != 0 {
                let a = if i == 6 || i == 8 { 16 } else { 4 };
                self.reserve(2, sizes[i] as usize, a, true)?;
            }
            self.pop()?;
        }
        if self.rd32(g + 88) != 0 {
            let n = self.rd32(g + 20);
            self.bytes(n, 2, 2)?;
        }
        if self.rd32(g + 92) != 0 {
            let n = self.rd32(g + 20);
            self.bytes(n, 52, 4)?;
        }
        if self.rd32(g + 96) != 0 {
            let n = self.rd32(g + 44);
            self.bytes(n, 4, 4)?;
        }
        self.pop()
    }

    // ---- menus -------------------------------------------------------------------------------------------

    fn menu_expression(&mut self, depth: usize) -> Res {
        let h = self.pos;
        if depth >= 64 {
            return Err(self.fail("menu expression nesting"));
        }
        // IW4's statement is 24 bytes (an execution time and the last result follow the data pointer)
        self.reserve(3, if self.stock { 24 } else { 20 }, 4, true)?;
        if self.stock && self.rd32(h + 8) == INLINE {
            self.menu_supporting_data()?;
        }
        if self.rd32(h + 4) != 0 {
            let count = self.rd32(h);
            if count > i32::MAX as u32 {
                return Err(self.fail("menu expression entries"));
            }
            let entries = self.bytes(count, 12, 4)?;
            for i in 0..count as usize {
                let r = entries + 12 * i;
                if self.rd32(r) == 0 || self.rd32(r + 8) != INLINE {
                    continue;
                }
                if self.rd32(r + 4) == 2 {
                    self.string()?;
                }
                if self.rd32(r + 4) == 3 {
                    self.menu_expression(depth + 1)?;
                }
            }
        }
        Ok(())
    }

    fn menu_events(&mut self, depth: usize) -> Res {
        let h = self.pos;
        if depth >= 64 {
            return Err(self.fail("menu event nesting"));
        }
        self.reserve(3, 8, 4, true)?;
        if self.rd32(h + 4) == 0 {
            return Ok(());
        }
        let count = self.rd32(h);
        if count > i32::MAX as u32 {
            return Err(self.fail("menu event count"));
        }
        let refs = self.bytes(count, 4, 4)?;
        for i in 0..count as usize {
            if self.rd32(refs + 4 * i) == 0 {
                continue;
            }
            let event = self.bytes(1, 8, 4)?;
            let ty = self.rd8(event + 4);
            if ty == 0 {
                if self.rd32(event) == INLINE {
                    self.string()?;
                }
            } else if self.rd32(event) != 0 {
                if ty == 2 {
                    self.menu_events(depth + 1)?;
                } else if ty == 1 || (3..=6).contains(&ty) {
                    let payload = self.bytes(1, 8, 4)?;
                    if ty != 1 && self.rd32(payload) == INLINE {
                        self.string()?;
                    }
                    if self.rd32(payload + 4) == INLINE {
                        self.menu_expression(depth + 1)?;
                    }
                    if ty == 1 && self.rd32(payload) != 0 {
                        self.menu_events(depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn menu_keys(&mut self) -> Res {
        for depth in 0..64 {
            let key = self.bytes(1, 12, 4)?;
            if self.rd32(key + 4) != 0 {
                self.menu_events(depth + 1)?;
            }
            if self.rd32(key + 8) == 0 {
                return Ok(());
            }
        }
        Err(self.fail("menu key chain"))
    }

    fn menu_window(&mut self, h: usize) -> Res {
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 44) == INLINE {
            self.string()?;
        }
        self.asset_reference(self.rd32(h + 160), Kind::Material)?;
        if self.qa() {
            self.asset_reference(self.rd32(h + 164), Kind::Image)?;
        }
        Ok(())
    }

    fn menu_item(&mut self) -> Res {
        let d = if self.qa() { 4 } else { 0 };
        let start = self.bytes(1, if self.qa() { 392 } else { 380 }, 4)?;
        self.menu_window(start)?;
        let h = start + d;
        if self.rd32(h + 0xe4) == INLINE {
            self.string()?;
        }
        if self.stock {
            // IW4: two strings (mouse enter and exit text) come before the five event sets
            for i in [0xf0usize, 0xf4] {
                if self.rd32(h + i) == INLINE {
                    self.string()?;
                }
            }
        }
        for i in (if self.stock { 0xf8 } else { 0xf0 }..=0x10c).step_by(4) {
            if self.rd32(h + i) != 0 {
                self.menu_events(0)?;
            }
        }
        for i in (0x110..=0x114).step_by(4) {
            if self.rd32(h + i) == INLINE {
                self.string()?;
            }
        }
        if self.rd32(h + 0x118) != 0 {
            self.menu_keys()?;
        }
        for i in (0x11c..=0x120).step_by(4) {
            if self.rd32(h + i) == INLINE {
                self.string()?;
            }
        }
        self.asset_reference(self.rd32(h + 0x128), Kind::SoundAlias)?;
        let ty = self.rd32(h + 0xb8);
        if self.rd32(h + 0x134) != 0 {
            match ty {
                6 => {
                    let payload = self.bytes(1, 324, 4)?;
                    if self.rd32(payload + 288) != 0 {
                        self.menu_events(0)?;
                    }
                    self.asset_reference(self.rd32(payload + 320), Kind::Material)?;
                }
                0 | 4 | 9 | 10 | 11 | 14 | 16 | 17 | 18 | 22 | 23 => {
                    self.bytes(1, 32, 4)?;
                }
                12 => {
                    let payload = self.bytes(1, 392, 4)?;
                    for i in 0..64 {
                        if self.rd32(payload + 4 * i) == INLINE {
                            self.string()?;
                        }
                    }
                }
                13 => {
                    if self.rd32(h + 0x134) == INLINE {
                        self.string()?;
                    }
                }
                20 => {
                    self.bytes(1, 28, 4)?;
                }
                21 => {
                    self.bytes(1, 4, 4)?;
                }
                _ => {}
            }
        }
        if self.rd32(h + 0x140) != 0 {
            let count = self.rd32(h + 0x13c);
            if count > i32::MAX as u32 {
                return Err(self.fail("menu item expressions"));
            }
            let expressions = self.bytes(count, 8, 4)?;
            for i in 0..count as usize {
                if self.rd32(expressions + 8 * i + 4) == INLINE {
                    self.menu_expression(0)?;
                }
            }
        }
        let last = if self.qa() { 0x158 } else { 0x150 };
        for i in (0x144..=last).step_by(4) {
            if self.rd32(h + i) == INLINE {
                self.menu_expression(0)?;
            }
        }
        Ok(())
    }

    /// The supporting data a stock IW4 menu starts with (`ExpressionSupportingData`: shared functions, static dvars and
    /// strings the menu's expressions refer to).
    fn menu_supporting_data(&mut self) -> Res {
        let s = self.bytes(1, 24, 4)?;
        if self.rd32(s + 4) == INLINE {
            let n = self.rd32(s);
            if n > i32::MAX as u32 {
                return Err(self.fail("menu function count"));
            }
            let list = self.bytes(n, 4, 4)?;
            for i in 0..n as usize {
                if self.rd32(list + 4 * i) == INLINE {
                    self.menu_expression(0)?;
                }
            }
        }
        if self.rd32(s + 12) == INLINE {
            let n = self.rd32(s + 8);
            if n > i32::MAX as u32 {
                return Err(self.fail("menu static dvar count"));
            }
            let list = self.bytes(n, 4, 4)?;
            for i in 0..n as usize {
                if self.rd32(list + 4 * i) == INLINE {
                    let dvar = self.bytes(1, 8, 4)?;
                    if self.rd32(dvar + 4) == INLINE {
                        self.string()?;
                    }
                }
            }
        }
        if self.rd32(s + 20) == INLINE {
            let n = self.rd32(s + 16);
            if n > i32::MAX as u32 {
                return Err(self.fail("menu string count"));
            }
            let list = self.bytes(n, 4, 4)?;
            for i in 0..n as usize {
                if self.rd32(list + 4 * i) == INLINE {
                    self.string()?;
                }
            }
        }
        Ok(())
    }

    /// The stock IW4 menu (400 bytes). Load order from OpenAssetTools' zone code: expression data, window, font, event sets
    /// (open, close, close request, escape), key handlers, expressions, strings, items.
    fn stock_menu(&mut self) -> Res {
        let start = self.pos;
        self.reserve(0, 400, 4, true)?;
        self.push(3)?;
        if self.rd32(start + 396) == INLINE {
            self.menu_supporting_data()?;
        }
        self.menu_window(start)?;
        if self.rd32(start + 164) == INLINE {
            self.string()?;
        }
        for o in [204usize, 212, 208, 216] {
            if self.rd32(start + o) != 0 {
                self.menu_events(0)?;
            }
        }
        if self.rd32(start + 220) != 0 {
            self.menu_keys()?;
        }
        if self.rd32(start + 224) == INLINE {
            self.menu_expression(0)?;
        }
        for i in [228usize, 232, 236] {
            if self.rd32(start + i) == INLINE {
                self.string()?;
            }
        }
        for i in (256..=276).step_by(4) {
            if self.rd32(start + i) == INLINE {
                self.menu_expression(0)?;
            }
        }
        if self.rd32(start + 280) != 0 {
            let count = self.rd32(start + 172);
            if count > i32::MAX as u32 {
                return Err(self.fail("menu item count"));
            }
            let items = self.bytes(count, 4, 4)?;
            for i in 0..count as usize {
                if self.rd32(items + 4 * i) != 0 {
                    self.menu_item()?;
                }
            }
        }
        self.pop()
    }

    pub(super) fn menu(&mut self) -> Res {
        if self.stock {
            return self.stock_menu();
        }
        const EVENTS: [usize; 4] = [0xd0, 0xd8, 0xd4, 0xdc];
        let start = self.pos;
        self.reserve(0, if self.qa() { 404 } else { 400 }, 4, true)?;
        self.push(3)?;
        self.menu_window(start)?;
        let h = start + if self.qa() { 4 } else { 0 };
        if self.rd32(h + 0xa4) == INLINE {
            self.string()?;
        }
        for o in EVENTS {
            if self.rd32(h + o) != 0 {
                self.menu_events(0)?;
            }
        }
        if self.rd32(h + 0xe0) != 0 {
            self.menu_keys()?;
        }
        if self.rd32(h + 0xe4) == INLINE {
            self.menu_expression(0)?;
        }
        for i in [0xe8usize, 0xec] {
            if self.rd32(h + i) == INLINE {
                self.string()?;
            }
        }
        for i in (0x104..=0x118).step_by(4) {
            if self.rd32(h + i) == INLINE {
                self.menu_expression(0)?;
            }
        }
        if self.rd32(h + 0x11c) != 0 {
            let count = self.rd32(h + 0xb0);
            if count > i32::MAX as u32 {
                return Err(self.fail("menu item count"));
            }
            let items = self.bytes(count, 4, 4)?;
            for i in 0..count as usize {
                if self.rd32(items + 4 * i) != 0 {
                    self.menu_item()?;
                }
            }
        }
        self.pop()
    }

    pub(super) fn menu_list(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 12, 4, true)?;
        self.push(3)?;
        if self.rd32(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 8) != 0 {
            let count = self.rd32(h + 4);
            if count > i32::MAX as u32 {
                return Err(self.fail("menu list count"));
            }
            let menus = self.bytes(count, 4, 4)?;
            for i in 0..count as usize {
                self.asset_reference(self.rd32(menus + 4 * i), Kind::Menu)?;
            }
        }
        self.pop()
    }

    #[allow(dead_code)]
    fn unsupported(&self) -> Fail {
        self.fail("unsupported")
    }
}
