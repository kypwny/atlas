//! The read schema of Call of Duty Online zones (QA 3.3.3.3, 3.19 and 1.1.1.13) for the conversion engine (`convert_engine`).
//!
//! The engine reads a zone with a schema (`schema/*.json`: the structures of a game, their members and the rules the loader applies to
//! them). Call of Duty Online has none of its own; its structures are stock Modern Warfare 2 (IW4) structures with a list of changes,
//! measured by the walker that reads these zones (`replay*.rs`) and by the converter
//! in the other direction (`convert_zone.rs` and the `convert_*` files it uses). This file applies those changes to the IW4 schema when
//! the schema is first used: a member is moved, added, dropped or widened by name, so every change is one line next to the numbers
//! that justify it. Members keep the names they have in IW4, so the engine's by-name conversion carries their values over.
//!
//! The engine checks the result against the zone itself: every asset has to end at its directory extent and the block positions have
//! to be the ones the reference walker reports (`ext_sync`).

use super::convert_engine::{ExtSync, Schema};
use super::{replay::Replay, Zone};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Asset types read with the schema; every other type of a zone is stepped over by its directory extent.
const READ: &[u32] = &[0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 20, 21, 22, 24, 27, 30, 31, 36, 37, 40];

/// The schema for a Call of Duty Online fastfile version (334, 423 or 461).
pub fn schema(version: u32) -> &'static Schema {
    static S: [OnceLock<Schema>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let i = match version {
        334 => 0,
        461 => 2,
        _ => 1,
    };
    S[i].get_or_init(|| {
        let doc = build(version);
        Schema::from_value(&doc, false).expect("the Call of Duty Online schema")
    })
}

/// Block positions after each directory entry, from the reference walker (`Replay`): they stand in for the entries the schema does
/// not read and check the ones it does.
pub fn ext_sync(zone: &Zone, version: u32) -> Result<ExtSync, String> {
    let mut r = Replay::new(zone).map_err(|f| format!("the reference walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    let mut out = ExtSync { pos: Vec::with_capacity(zone.assets.len()), note: None };
    for i in 0..zone.assets.len() {
        let e = zone.assets[i];
        if out.note.is_none() {
            if let Err(f) = r.replay_asset(zone, i) {
                out.note = Some(format!("the reference walker stops at asset {i} (type {}, extent {}): {} at {:#x}", e.asset_type, e.extent, f.what, f.at));
            }
        }
        out.pos.push(if out.note.is_none() { Some(r.positions()) } else { None });
    }
    let _ = version;
    Ok(out)
}

/// `atlas --convertzone codoltrace GAME_FOLDER ZONE INDEX`: the block-3 allocations the reference walker makes for one asset
/// (address, length, where in the stream it started and whether it read stream bytes), to compare with the engine's trace
/// (`ATLAS_CODOL_TRACE=INDEX`).
pub fn replay_trace(args: &[String]) -> anyhow::Result<()> {
    use anyhow::Context;
    let root = std::path::Path::new(args.first().context("game folder")?);
    let key = super::export::find_key(root)?;
    let zone = Zone::open(std::path::Path::new(args.get(1).context("zone")?), &key)?;
    let index: usize = args.get(2).context("asset index")?.parse()?;
    for i in index.saturating_sub(2)..(index + 4).min(zone.assets.len()) {
        let a = zone.assets[i];
        println!("entry {i}: type {} reference {:#x} extent {}", a.asset_type, a.reference, a.extent);
    }
    let mut r = Replay::new(&zone).map_err(|f| anyhow::anyhow!("{} at {}", f.what, f.at))?;
    r.enable_tracing();
    for i in 0..=index.min(zone.assets.len() - 1) {
        r.replay_asset(&zone, i).map_err(|f| anyhow::anyhow!("asset {i}: {} at {:#x}", f.what, f.at))?;
    }
    if let (Some(at), Some(n)) = (args.get(3), args.get(4)) {
        let at = usize::from_str_radix(at.trim_start_matches("0x"), 16)?;
        let n: usize = n.parse()?;
        println!("words at {at:#x}: {:x?}", (0..n).map(|k| super::u32_at(&zone.data, at + 4 * k)).collect::<Vec<_>>());
    }
    println!("positions {:?}
peaks     {:?}
header    {:?}", r.positions(), r.block_positions(), zone.block_sizes);
    let e = zone.assets[index];
    println!("asset {index}: type {} at {:#x}, extent {:#x} (ends {:#x})", e.asset_type, e.start, e.extent, e.start + e.extent);
    for (a, info) in r.block3_allocations().iter().zip(r.block3_allocation_info()) {
        if a.0 == index {
            println!("  b3 at {} len {} stream {:#x} {}", (a.1 & 0x0fff_ffff) - 1, a.2, info.0, if info.1 { "" } else { "(slot)" });
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------ the patcher

/// The IW4 schema as a JSON document with the operations the changes need. Members are identified by an id while the document is
/// patched, so the offsets inside the expressions that read members (counts, conditions) follow a member that moves.
struct Doc {
    v: Value,
}

fn scalar(name: &str, off: i64, kind: &str) -> Value {
    let (size, tk) = match kind {
        "i8" => (1, "char"),
        "u8" => (1, "unsigned char"),
        "i16" => (2, "short"),
        "u16" => (2, "uint16_t"),
        "i32" => (4, "int"),
        "f32" => (4, "float"),
        _ => (4, "unsigned int"),
    };
    json!({"n": name, "o": off, "k": kind, "ty": -1, "mods": [], "str": 0, "scr": 0, "re": 0, "c": null, "blk": null, "aa": null, "leaf": 1,
           "ign": 0, "dyn": 0, "ap": 0, "need": 0, "es": size, "ea": size, "bits": 0, "ep": 0, "ma": size, "td": 0, "tk": tk})
}

impl Doc {
    fn new() -> Doc {
        let mut v: Value = serde_json::from_str(include_str!("schema/iw4.json")).expect("iw4 schema");
        // give every member an id and turn the member offsets in the expressions into those ids
        let mut next = 0u64;
        for s in v["structs"].as_array_mut().unwrap() {
            for m in s["m"].as_array_mut().unwrap() {
                m["_id"] = json!(next);
                next += 1;
            }
        }
        let structs = v["structs"].clone();
        fn visit(x: &mut Value, structs: &Value) {
            match x {
                Value::Array(a) => {
                    if a.len() == 7 && a[0] == "d" && a[2].is_array() {
                        let mut cur = a[1].as_u64().map(|r| r as usize);
                        let refs = a[2].as_array_mut().unwrap();
                        for r in refs.iter_mut() {
                            let (off, deref) = (r[0].as_i64().unwrap(), r[1].clone());
                            let Some(sid) = cur else { break };
                            let hit = structs[sid]["m"].as_array().unwrap().iter().find(|m| m["o"].as_i64() == Some(off));
                            match hit {
                                Some(m) => {
                                    *r = json!(["@", m["_id"].clone(), deref]);
                                    cur = m["ty"].as_i64().filter(|&t| t >= 0).map(|t| t as usize);
                                }
                                None => break,
                            }
                        }
                    }
                    for e in a.iter_mut() {
                        visit(e, structs);
                    }
                }
                Value::Object(o) => {
                    for (_, e) in o.iter_mut() {
                        visit(e, structs);
                    }
                }
                _ => {}
            }
        }
        let mut st = v["structs"].take();
        visit(&mut st, &structs);
        v["structs"] = st;
        Doc { v }
    }

    fn sid(&self, name: &str) -> usize {
        self.v["structs"].as_array().unwrap().iter().position(|s| s["n"] == name).unwrap_or_else(|| panic!("no structure {name}"))
    }

    fn members(&mut self, s: &str) -> &mut Vec<Value> {
        let i = self.sid(s);
        self.v["structs"][i]["m"].as_array_mut().unwrap()
    }

    fn member(&mut self, s: &str, m: &str) -> &mut Value {
        self.members(s).iter_mut().find(|x| x["n"] == m).unwrap_or_else(|| panic!("no member {s}.{m}"))
    }

    fn off(&mut self, s: &str, m: &str) -> i64 {
        self.member(s, m)["o"].as_i64().unwrap()
    }

    fn set_size(&mut self, s: &str, size: i64) {
        let i = self.sid(s);
        self.v["structs"][i]["s"] = json!(size);
    }

    /// Moves a member to `off`.
    fn mv(&mut self, s: &str, m: &str, off: i64) {
        self.member(s, m)["o"] = json!(off);
    }

    /// Moves every member at or behind `from` by `by` bytes and grows the structure.
    fn shift(&mut self, s: &str, from: i64, by: i64) {
        for m in self.members(s) {
            let o = m["o"].as_i64().unwrap();
            if o >= from {
                m["o"] = json!(o + by);
            }
        }
        let i = self.sid(s);
        let size = self.v["structs"][i]["s"].as_i64().unwrap();
        self.v["structs"][i]["s"] = json!(size + by);
    }

    /// The length of the (first) array of a member.
    fn arr(&mut self, s: &str, m: &str, n: i64) {
        let mods = self.member(s, m)["mods"].as_array_mut().unwrap();
        let a = mods.iter_mut().find(|x| x[0] == "a").expect("an array member");
        a[1] = json!(n);
    }

    fn kind(&mut self, s: &str, m: &str, kind: &str) {
        let proto = scalar("", 0, kind);
        let mem = self.member(s, m);
        for k in ["k", "es", "ea", "ma", "tk"] {
            mem[k] = proto[k].clone();
        }
    }

    fn drop(&mut self, s: &str, m: &str) {
        self.members(s).retain(|x| x["n"] != m);
    }

    fn add(&mut self, s: &str, mut member: Value) {
        member["_id"] = json!(self.next_id());
        self.members(s).push(member);
    }

    fn add_scalar(&mut self, s: &str, name: &str, off: i64, kind: &str) {
        self.add(s, scalar(name, off, kind));
    }

    /// Adds a copy of member `from` of structure `from_s` at `off` under a new name, loaded in order after `after` (or last).
    fn add_like(&mut self, s: &str, name: &str, off: i64, from_s: &str, from: &str, after: Option<&str>) {
        let mut m = self.member(from_s, from).clone();
        m["n"] = json!(name);
        m["o"] = json!(off);
        m["_id"] = json!(self.next_id());
        let list = self.members(s);
        let at = after.and_then(|a| list.iter().position(|x| x["n"] == a)).map(|p| p + 1).unwrap_or(list.len());
        list.insert(at, m);
    }

    /// An asset pointer member like `from`, pointing at structure `target` instead.
    fn add_asset_like(&mut self, s: &str, name: &str, off: i64, from_s: &str, from: &str, target: &str, after: Option<&str>) {
        let t = self.sid(target);
        let (size, align) = (self.v["structs"][t]["s"].clone(), self.v["structs"][t]["a"].clone());
        self.add_like(s, name, off, from_s, from, after);
        let m = self.member(s, name);
        m["ty"] = json!(t);
        m["tk"] = json!(target);
        m["es"] = size;
        m["ea"] = align;
    }

    /// The element count of a member that is a pointer to a fixed number of elements.
    fn ptr_count(&mut self, s: &str, m: &str, n: i64) {
        let mods = self.member(s, m)["mods"].as_array_mut().unwrap();
        let p = mods.iter_mut().find(|x| x[0] == "p").expect("a pointer member");
        p[1] = json!(["n", n]);
    }

    /// Renumbers the element types a member's condition compares with (`elemType == k`).
    fn map_elem_types(&mut self, s: &str, m: &str, f: &dyn Fn(i64) -> i64) {
        fn walk(x: &mut Value, f: &dyn Fn(i64) -> i64) {
            if let Value::Array(a) = x {
                if a.len() == 4 && a[0] == "o" && a[1] == "==" && a[3][0] == "n" {
                    let k = a[3][1].as_i64().unwrap();
                    a[3][1] = json!(f(k));
                } else {
                    for e in a.iter_mut() {
                        walk(e, f);
                    }
                }
            }
        }
        let c = &mut self.member(s, m)["c"];
        walk(c, f);
    }

    /// Widens a condition that is a chain of `elemType == k` terms with the terms for `extra`.
    fn or_elem_types(&mut self, s: &str, m: &str, extra: &[i64]) {
        fn first(x: &Value) -> Option<Value> {
            let a = x.as_array()?;
            if a.len() == 4 && a[0] == "o" && a[1] == "==" {
                return Some(x.clone());
            }
            a.iter().find_map(first)
        }
        let c = self.member(s, m)["c"].clone();
        let leaf = first(&c).expect("an elemType term");
        let mut out = c;
        for &k in extra {
            let mut t = leaf.clone();
            t[3][1] = json!(k);
            out = json!(["o", "||", out, t]);
        }
        self.member(s, m)["c"] = out;
    }

    /// Appends a new structure and returns its index.
    fn new_struct(&mut self, name: &str, size: i64, align: i64) -> usize {
        let list = self.v["structs"].as_array_mut().unwrap();
        list.push(json!({"n": name, "u": 0, "s": size, "a": align, "leaf": 0, "asset": "", "blk": null, "aa": null, "dynoff": -1, "dyn": 0, "m": []}));
        list.len() - 1
    }

    /// Copies member `m` of structure `from` into structure `to` at `off` (appended; `order` sorts later). The counts of the copy that
    /// read members of `from` read the copies in `to` made before it (`map` holds the old and new ids).
    fn copy_member(&mut self, from: &str, m: &str, to: &str, off: i64, map: &mut HashMap<u64, u64>) {
        let (fs, ts) = (self.sid(from), self.sid(to));
        let mut c = self.member(from, m).clone();
        let old = c["_id"].as_u64().unwrap();
        let new = self.next_id();
        c["_id"] = json!(new);
        c["o"] = json!(off);
        fn retarget(x: &mut Value, fs: usize, ts: usize, map: &HashMap<u64, u64>) {
            if let Value::Array(a) = x {
                if a.len() == 7 && a[0] == "d" && a[1].as_u64() == Some(fs as u64) {
                    a[1] = json!(ts);
                    if let Some(r) = a[2].as_array_mut().and_then(|r| r.first_mut()).and_then(|r| r.as_array_mut()) {
                        if r[0] == "@" {
                            let id = r[1].as_u64().unwrap();
                            r[1] = json!(map.get(&id).copied().unwrap_or(id));
                        }
                    }
                }
                for e in a.iter_mut() {
                    retarget(e, fs, ts, map);
                }
            }
        }
        retarget(&mut c["mods"], fs, ts, map);
        retarget(&mut c["c"], fs, ts, map);
        map.insert(old, new);
        self.members(to).push(c);
    }

    /// Removes the named members from a structure.
    fn drop_all(&mut self, s: &str, names: &[&str]) {
        for n in names {
            self.drop(s, n);
        }
    }

    /// Puts the members in the given order (the loader's order); every member must be named.
    fn order(&mut self, s: &str, names: &[&str]) {
        let list = self.members(s);
        let mut out = Vec::new();
        for n in names {
            let at = list.iter().position(|m| m["n"] == *n).unwrap_or_else(|| panic!("no member {s}.{n}"));
            out.push(list.remove(at));
        }
        assert!(list.is_empty(), "members of {s} not in the order: {:?}", list.iter().map(|m| m["n"].clone()).collect::<Vec<_>>());
        *list = out;
    }

    /// Points a member's structure type at another structure.
    fn retype(&mut self, s: &str, m: &str, target: &str) {
        let t = self.sid(target);
        let (size, align) = (self.v["structs"][t]["s"].clone(), self.v["structs"][t]["a"].clone());
        let mem = self.member(s, m);
        mem["ty"] = json!(t);
        mem["tk"] = json!(target);
        mem["es"] = size;
        mem["ea"] = align;
    }

    /// Adds an array member of a scalar kind: `n` elements at `off`.
    fn add_array(&mut self, s: &str, name: &str, off: i64, kind: &str, n: i64) {
        let mut m = scalar(name, off, kind);
        m["mods"] = json!([["a", n, null, null]]);
        self.add(s, m);
    }

    /// Makes the pointer member `m` of `s` point at `count_member` (a member of the same structure) elements.
    fn set_count(&mut self, s: &str, m: &str, count_member: &str) {
        let sid = self.sid(s);
        let (id, kind, size) = {
            let c = self.member(s, count_member);
            (c["_id"].clone(), c["k"].clone(), c["es"].clone())
        };
        let expr = json!(["d", sid, [["@", id, 0]], kind, [], size, []]);
        let mods = self.member(s, m)["mods"].as_array_mut().unwrap();
        let p = mods.iter_mut().find(|x| x[0] == "p").expect("a pointer member");
        p[1] = expr;
    }

    /// Adds a pointer to `n` elements of a scalar kind (counted by `count_member`), copied from member `from` of `from_s`.
    fn add_ptr(&mut self, s: &str, name: &str, off: i64, from_s: &str, from: &str, kind: &str, count_member: &str) {
        self.add_like(s, name, off, from_s, from, None);
        self.kind(s, name, kind);
        self.set_count(s, name, count_member);
    }

    fn next_id(&self) -> u64 {
        self.v["structs"].as_array().unwrap().iter().flat_map(|s| s["m"].as_array().unwrap().iter()).filter_map(|m| m["_id"].as_u64()).max().unwrap_or(0) + 1
    }

    /// Writes the expression offsets back and refreshes the element sizes of members whose structure changed size.
    fn finish(mut self, version: u32) -> Value {
        let mut at: HashMap<u64, i64> = HashMap::new();
        let sizes: Vec<i64> = self.v["structs"].as_array().unwrap().iter().map(|s| s["s"].as_i64().unwrap()).collect();
        for s in self.v["structs"].as_array_mut().unwrap() {
            for m in s["m"].as_array_mut().unwrap() {
                at.insert(m["_id"].as_u64().unwrap(), m["o"].as_i64().unwrap());
                if let Some(t) = m["ty"].as_i64().filter(|&t| t >= 0) {
                    m["es"] = json!(sizes[t as usize]);
                }
            }
        }
        fn back(x: &mut Value, at: &HashMap<u64, i64>) {
            match x {
                Value::Array(a) => {
                    if a.len() == 3 && a[0] == "@" {
                        let id = a[1].as_u64().unwrap();
                        *x = json!([at[&id], a[2].clone()]);
                        return;
                    }
                    for e in a.iter_mut() {
                        back(e, at);
                    }
                }
                Value::Object(o) => {
                    for (_, e) in o.iter_mut() {
                        back(e, at);
                    }
                }
                _ => {}
            }
        }
        let mut st = self.v["structs"].take();
        back(&mut st, &at);
        self.v["structs"] = st;
        self.v["extents"] = json!(true);
        self.v["game"] = json!(format!("CODOL{version}"));
        // the asset types the schema reads; the rest are stepped over
        let all = self.v["assets"].as_object().unwrap().clone();
        let mut keep = serde_json::Map::new();
        for (k, val) in all {
            if READ.contains(&k.parse::<u32>().unwrap()) {
                keep.insert(k, val);
            }
        }
        self.v["assets"] = Value::Object(keep);
        self.v
    }
}

// ------------------------------------------------------------------------------------------------------------ the changes

fn build(version: u32) -> Value {
    let mut d = Doc::new();
    qa_changes(&mut d, version);
    d.finish(version)
}

/// QA 3.3.3.3 (423) and 3.19 (461).
fn qa_changes(d: &mut Doc, version: u32) {
    // changes shared by every Call of Duty Online build (1.1.1.13, 3.3.3.3, 3.19)
    // PhysPreset: 68 bytes in the IW5 layout. Six scaling members after the first ten (all zero unless the preset names a sound
    // prefix, then 250, 250, 1, 1, 0, 0: 23 of 23 presets with a prefix), the two flag bytes behind them (convert_model.rs).
    d.mv("PhysPreset", "tempDefaultToCylinder", 64);
    d.mv("PhysPreset", "perSurfaceSndAlias", 65);
    d.set_size("PhysPreset", 68);
    for (i, n) in ["minMomentum", "maxMomentum", "minPitch", "maxPitch"].iter().enumerate() {
        d.add_scalar("PhysPreset", n, 40 + 4 * i as i64, "f32");
    }
    d.add_scalar("PhysPreset", "volumeType", 56, "i32");
    d.add_scalar("PhysPreset", "pitchType", 60, "i32");

    // XModelSurfs: three more part-bit words (zero in every QA record); XSurface 64 -> 84 (convert_model.rs, "XModelSurfs").
    d.arr("XModelSurfs", "partBits", 9);
    d.set_size("XModelSurfs", 48);
    d.drop("XSurface", "zoneHandle");
    d.add_scalar("XSurface", "flags", 2, "u16");
    d.mv("XSurface", "vertCount", 4);
    d.mv("XSurface", "triCount", 6);
    d.mv("XSurface", "baseTriIndex", 8);
    d.kind("XSurface", "baseTriIndex", "u32");
    d.mv("XSurface", "baseVertIndex", 12);
    d.kind("XSurface", "baseVertIndex", "u32");
    d.mv("XSurface", "triIndices", 16);
    d.mv("XSurface", "vertInfo", 20);
    d.mv("XSurface", "verts0", 32);
    d.add_scalar("XSurface", "unknown36", 36, "u32");
    d.mv("XSurface", "vertListCount", 40);
    d.mv("XSurface", "vertList", 44);
    d.add_scalar("XSurface", "unknown48", 48, "u32");
    d.mv("XSurface", "partBits", 52);
    d.arr("XSurface", "partBits", 8);
    d.set_size("XSurface", 84);

    // XModel 304 -> 360: the part-bit arrays are wider (noScalePartBits 6 -> 8 words, the skeleton pointers sit 8 bytes later), a lod
    // record is 56 bytes (a surface name word after the surface record, nine part-bit words, no runtime surface pointer) and every
    // member behind the lods is 56 bytes further (convert_model.rs, "XModel").
    d.arr("XModel", "noScalePartBits", 8);
    d.shift("XModel", 36, 8);
    d.shift("XModel", 248, 48);
    d.arr("XModelLodInfo", "partBits", 9);
    d.drop("XModelLodInfo", "surfs");
    d.mv("XModelLodInfo", "partBits", 16);
    d.mv("XModelLodInfo", "lod", 52);
    d.mv("XModelLodInfo", "smcBaseIndexPlusOne", 53);
    d.mv("XModelLodInfo", "smcSubIndexMask", 54);
    d.mv("XModelLodInfo", "smcBucket", 55);
    d.set_size("XModelLodInfo", 56);
    d.add_like("XModelLodInfo", "surfName", 12, "XModelSurfs", "name", Some("modelSurfs"));
    // snd_alias_t 100 -> 108: two words after the flags (+60: 50 in 10,089 of 10,768 aliases; +64: nearly always 0), everything
    // behind them eight bytes up (convert_sound.rs).
    d.shift("snd_alias_t", 60, 8);
    d.add_scalar("snd_alias_t", "unknown60", 60, "u32");
    d.add_scalar("snd_alias_t", "unknown64", 64, "u32");

    // LoadedSound 44 -> 48: one word before the data pointer.
    d.shift("MssSound", 36, 4);
    d.shift("LoadedSound", 40, 4);
    d.add_scalar("MssSound", "unknown36", 36, "u32");

    // FxElemDef 252 -> 260: a word at +252 and a string at +256 (loaded after the extended definition). The element types are
    // numbered as in IW4 plus none or two from the trail on, and two new types sit at 2 and 4 (sprites that use a material).
    d.set_size("FxElemDef", 260);
    d.add_scalar("FxElemDef", "unknown252", 252, "u32");
    d.add_like("FxElemDef", "name256", 256, "FxEffectDefRef", "name", Some("extended"));
    for (s, m) in [("FxElemVisuals", "material"), ("FxElemVisuals", "model"), ("FxElemVisuals", "effectDef"), ("FxElemVisuals", "soundName"), ("FxElemDefVisuals", "markArray"), ("FxElemExtendedDefPtr", "trailDef"), ("FxElemExtendedDefPtr", "sparkFountainDef")] {
        d.map_elem_types(s, m, &|k| match k {
            0 | 1 => k,
            2 => 3,
            n => n + 2,
        });
    }
    d.or_elem_types("FxElemVisuals", "material", &[2, 4]);

    if version == 334 {
        c1_changes(d);
        return;
    }

    // QA 3.3.3.3 and 3.19
    // a collision surface's triangles are read only when the pointer is -1 (the reference walker), anything else is a reference
    d.member("XModelCollSurf_s", "collTris")["re"] = json!(1);

    // GfxImage 32 -> 52 (the header maps of convert.rs, measured on 7 aligned words: the name moves to +48, the texture pointer
    // stays, the load definition and its data are in block 3, not temporary). The platform members sit where the maps put them:
    // picmip, noPicmip and track at +12, the card memory at +24 and the sizes at +32.
    d.mv("GfxImage", "picmip", 12);
    d.mv("GfxImage", "noPicmip", 14);
    d.mv("GfxImage", "track", 15);
    d.mv("GfxImage", "cardMemory", 24);
    d.mv("GfxImage", "width", 32);
    d.mv("GfxImage", "height", 34);
    d.mv("GfxImage", "depth", 36);
    d.mv("GfxImage", "delayLoadPixels", 38);
    d.mv("GfxImage", "name", 48);
    d.set_size("GfxImage", 52);
    d.member("GfxTexture", "loadDef")["blk"] = json!(3);

    // MaterialTechniqueSet 204 -> 208: a word after the name; every member behind it moves up by four (header map: 51 of 51 words).
    d.shift("MaterialTechniqueSet", 4, 4);

    // Material 96 -> 104: the 24-byte info becomes 32 bytes with the name at +16 (measured on 500 materials present in both stock
    // and QA `common_mp`: the surface type bits are the word at +20, the game flags the byte at +26, the sort key the byte at +28
    // and the two atlas counts the bytes behind it; QA's draw surface words at +0 are zero in all of them).
    d.mv("MaterialInfo", "name", 16);
    d.mv("MaterialInfo", "surfaceTypeBits", 20);
    d.mv("MaterialInfo", "gameFlags", 26);
    d.mv("MaterialInfo", "sortKey", 28);
    d.mv("MaterialInfo", "textureAtlasRowCount", 29);
    d.mv("MaterialInfo", "textureAtlasColumnCount", 30);
    d.mv("MaterialInfo", "drawSurf", 0);
    d.drop("MaterialInfo", "hashIndex");
    d.set_size("MaterialInfo", 32);
    d.shift("Material", 24, 8);

    if version == 461 {
        // 3.19 impact records have four more surface types: 35 non-flesh references and 4 flesh ones (156 bytes; measured on the
        // impact table of `common_mp`: 2,973 bytes = 8 + name + 19 x 156).
        d.arr("FxImpactEntry", "nonflesh", 35);
        d.mv("FxImpactEntry", "flesh", 140);
        d.set_size("FxImpactEntry", 156);
        // 3.19: one more technique slot (the technique set is 212 bytes), one more state-bits entry (the counts and flags behind the
        // entries sit one byte later; the header stays 104 bytes) (replay.rs: `technique_set`, `material`).
        d.arr("MaterialTechniqueSet", "techniques", 49);
        d.set_size("MaterialTechniqueSet", 212);
        d.arr("Material", "stateBitsEntry", 49);
        for m in ["textureCount", "constantCount", "stateBitsCount", "stateFlags", "cameraRegion"] {
            let o = d.off("Material", m);
            d.mv("Material", m, o + 1);
        }
    }

    // TracerDef 112 -> 116: an effect reference at +8.
    d.shift("TracerDef", 8, 4);
    d.add_asset_like("TracerDef", "effect", 8, "TracerDef", "material", "FxEffectDef", Some("material"));

    // FxImpactTable: 19 records of 35 effect references (stock 15).
    d.ptr_count("FxImpactTable", "table", 19);

    // FxElemVisStateSample 48 -> 64: each visual state is followed by two words
    d.set_size("FxElemVisStateSample", 64);
    d.mv("FxElemVisStateSample", "amplitude", 32);
    clip_map(d);
    gfx_world(d, version);
}

/// 1.1.1.13 (version 334): the structures of models, sounds and effects are the QA ones (above); materials, technique sets, the collision
/// map and the render world are stock IW4's (`replay.rs`: `iw4_sizes`); the images are 36 bytes and the impact table has 16 records.
fn c1_changes(d: &mut Doc) {
    d.ptr_count("FxImpactTable", "table", 16);
    // GfxImage 32 -> 36: a word between the last size bytes and the name (measured on 5 images of `code_post_gfx_mp` present in
    // the stock zone: every word before it is where IW4 has it), the load definition and its data in block 3
    d.shift("GfxImage", 28, 4);
    d.add_scalar("GfxImage", "unknown28", 28, "u32");
    d.member("GfxTexture", "loadDef")["blk"] = json!(3);
}

/// The collision map (type 15): laid out like IW5's `clipMap_t` (convert_clip.rs, replay_world.rs `collision_qa`). The arrays a brush
/// model needs live in a 64-byte `ClipInfo` at +8 (planes, materials, brush sides, brush edges, leaf-brush nodes, leaf brushes,
/// brushes, their bounds and contents), a second one is behind a pointer at +72 (loaded in the temporary block), and every collision
/// model (72 bytes) has one behind a pointer at +28. The trigger arrays and the stages of the map entities are members of the map, and
/// the map-entities record in the stream is 36 bytes (its trigger part is empty).
fn clip_map(d: &mut Doc) {
    // the ClipInfo members, in the order the loader reads them
    d.new_struct("ClipInfo", 64, 4);
    let mut map = HashMap::new();
    for (m, off) in [
        ("planeCount", 0),
        ("planes", 4),
        ("numMaterials", 8),
        ("materials", 12),
        ("numBrushSides", 16),
        ("brushsides", 20),
        ("numBrushEdges", 24),
        ("brushEdges", 28),
        ("leafbrushNodesCount", 32),
        ("leafbrushNodes", 36),
        ("numLeafBrushes", 40),
        ("leafbrushes", 44),
        ("numBrushes", 48),
        ("brushes", 52),
        ("brushBounds", 56),
        ("brushContents", 60),
    ] {
        d.copy_member("clipMap_t", m, "ClipInfo", off, &mut map);
        // the reference walker loads each array only when its pointer is -1; any other value refers to data loaded before
        if !m.starts_with("num") && !m.ends_with("Count") {
            d.member("ClipInfo", m)["re"] = json!(1);
        }
    }
    d.drop_all(
        "clipMap_t",
        &["planeCount", "planes", "numMaterials", "materials", "numBrushSides", "brushsides", "numBrushEdges", "brushEdges", "leafbrushNodesCount", "leafbrushNodes", "numLeafBrushes", "leafbrushes", "numBrushes", "brushes", "brushBounds", "brushContents", "numLeafSurfaces", "leafsurfaces"],
    );
    // the stages and triggers move here from the map entities
    let mut stage_map = HashMap::new();
    d.copy_member("MapEnts", "stageCount", "clipMap_t", 160, &mut stage_map);
    d.copy_member("MapEnts", "stages", "clipMap_t", 156, &mut stage_map);
    let mut trig_map = HashMap::new();
    d.copy_member("MapEnts", "trigger", "clipMap_t", 164, &mut trig_map);
    d.drop_all("MapEnts", &["stages", "stageCount"]);
    // the trigger arrays are inline in the map-entities record; the map's own trigger words refer to them
    for m in ["models", "hulls", "slabs"] {
        d.member("MapTriggers", m)["re"] = json!(1);
    }
    d.set_size("MapEnts", 36);
    // the new members of the map and of the collision models
    d.add_like("clipMap_t", "info", 8, "MapEnts", "trigger", None);
    d.retype("clipMap_t", "info", "ClipInfo");
    d.add_like("clipMap_t", "pInfo", 72, "XModelLodInfo", "modelSurfs", None);
    d.retype("clipMap_t", "pInfo", "ClipInfo");
    d.add_like("cmodel_t", "info", 28, "XModelLodInfo", "modelSurfs", None);
    d.retype("cmodel_t", "info", "ClipInfo");
    d.mv("cmodel_t", "leaf", 32);
    d.set_size("cmodel_t", 72);
    let cm = d.sid("cmodel_t");
    d.v["structs"][cm]["leaf"] = json!(0);
    for (m, off) in [
        ("isInUse", 4),
        ("numStaticModels", 76),
        ("staticModelList", 80),
        ("numNodes", 84),
        ("nodes", 88),
        ("numLeafs", 92),
        ("leafs", 96),
        ("vertCount", 100),
        ("verts", 104),
        ("triCount", 108),
        ("triIndices", 112),
        ("triEdgeIsWalkable", 116),
        ("borderCount", 120),
        ("borders", 124),
        ("partitionCount", 128),
        ("partitions", 132),
        ("aabbTreeCount", 136),
        ("aabbTrees", 140),
        ("numSubModels", 144),
        ("cmodels", 148),
        ("mapEnts", 152),
        ("smodelNodeCount", 188),
        ("smodelNodes", 192),
        ("dynEntCount", 196),
        ("dynEntDefList", 200),
        ("dynEntPoseList", 208),
        ("dynEntClientList", 216),
        ("dynEntCollList", 224),
        ("checksum", 232),
        ("padding", 236),
    ] {
        d.mv("clipMap_t", m, off);
    }
    d.arr("clipMap_t", "padding", 20);
    d.order(
        "clipMap_t",
        &[
            "name", "isInUse", "info", "pInfo", "numStaticModels", "staticModelList", "numNodes", "nodes", "numLeafs", "leafs", "vertCount", "verts", "triCount", "triIndices", "triEdgeIsWalkable", "borderCount", "borders",
            "partitionCount", "partitions", "aabbTreeCount", "aabbTrees", "numSubModels", "cmodels", "smodelNodeCount", "smodelNodes", "mapEnts", "stageCount", "stages", "trigger", "dynEntCount", "dynEntDefList", "dynEntPoseList",
            "dynEntClientList", "dynEntCollList", "checksum", "padding",
        ],
    );
}

/// The render world (type 21): 1,608 bytes (stock 628), 1,612 in 3.19 (convert_world.rs, replay_world.rs). Against the stock layout the
/// draw block has three more words behind the reflection probes (a count and two lists, 12 and 1 bytes per record), the light grid and
/// everything behind it sit 12 bytes later, the sun flare holds two more materials (104 bytes), 960 bytes of values (the "insert",
/// `zone::world::world_insert`) sit between the sun flare and the outdoor lookup matrix, cells are 48 bytes (a count and a list of
/// 1-byte records at +40) and reflection probes 20 (a list of lists at +12, count +16). The draw-surface key is 16 bytes (the
/// material header's `drawSurf` too).
fn gfx_world(d: &mut Doc, version: u32) {
    d.shift("GfxWorldDraw", 16, 12);
    d.add_scalar("GfxWorldDraw", "extraCount", 16, "u32");
    let rec = d.new_struct("GfxWorldRecord12", 12, 4);
    d.v["structs"][rec]["leaf"] = json!(1);
    d.add_array("GfxWorldRecord12", "raw", 0, "u8", 12);
    d.add_ptr("GfxWorldDraw", "extraRecords12", 20, "GfxWorldDpvsStatic", "sortedSurfIndex", "u16", "extraCount");
    d.member("GfxWorldDraw", "extraRecords12")["ty"] = json!(rec);
    d.member("GfxWorldDraw", "extraRecords12")["k"] = json!("");
    d.member("GfxWorldDraw", "extraRecords12")["tk"] = json!("GfxWorldRecord12");
    d.member("GfxWorldDraw", "extraRecords12")["es"] = json!(12);
    d.member("GfxWorldDraw", "extraRecords12")["ea"] = json!(4);
    d.add_ptr("GfxWorldDraw", "extraRecords1", 24, "GfxCell", "reflectionProbes", "i8", "extraCount");
    d.order(
        "GfxWorldDraw",
        &[
            "reflectionProbeCount", "reflectionProbes", "reflectionProbeOrigins", "reflectionProbeTextures", "extraCount", "extraRecords12", "extraRecords1", "lightmapCount", "lightmaps", "lightmapPrimaryTextures",
            "lightmapSecondaryTextures", "lightmapOverridePrimary", "lightmapOverrideSecondary", "vertexCount", "vd", "vertexLayerDataSize", "vld", "indexCount", "indices",
        ],
    );
    d.shift("GfxWorld", 152, 12);

    // sun flare: two more materials after the first two, the rest eight bytes later
    d.shift("sunflare_t", 12, 8);
    d.add_like("sunflare_t", "spriteMaterial3", 12, "sunflare_t", "flareMaterial", Some("flareMaterial"));
    d.add_like("sunflare_t", "spriteMaterial4", 16, "sunflare_t", "flareMaterial", Some("spriteMaterial3"));
    // the insert, then the outdoor lookup matrix and everything behind it
    d.shift("GfxWorld", 360, 968);
    d.add_array("GfxWorld", "unknown368", 368, "u8", 960);

    // cells: 48 bytes with a count at +40 and a list of 1-byte records at +44, loaded behind the reflection probe list
    d.set_size("GfxCell", 48);
    d.add_scalar("GfxCell", "extraCount", 40, "i8");
    d.add_ptr("GfxCell", "extraRecords", 44, "GfxCell", "reflectionProbes", "i8", "extraCount");

    // reflection probes: 20 bytes, a list of lists at +12 (count +16): each list is a pointer and a count of 2-byte records
    let pl = d.new_struct("GfxProbeList", 8, 4);
    d.add_scalar("GfxProbeList", "count", 4, "u32");
    d.add_ptr("GfxProbeList", "records", 0, "GfxWorldDpvsStatic", "sortedSurfIndex", "u16", "count");
    d.order("GfxProbeList", &["records", "count"]);
    let _ = pl;
    d.set_size("GfxReflectionProbe", 20);
    d.add_scalar("GfxReflectionProbe", "listCount", 16, "u32");
    d.add_like("GfxReflectionProbe", "lists", 12, "GfxWorldDraw", "extraRecords12", None);
    d.retype("GfxReflectionProbe", "lists", "GfxProbeList");
    d.set_count("GfxReflectionProbe", "lists", "listCount");
    let g = d.sid("GfxReflectionProbe");
    d.v["structs"][g]["leaf"] = json!(0);
    // the draw-surface key is 16 bytes
    d.set_size("GfxDrawSurf", 16);
    let ds = d.sid("GfxDrawSurf");
    d.v["structs"][ds]["a"] = json!(8);

    if version == 461 {
        // 3.19: a word at +0x34 (a sort key) pushes everything behind it four bytes up
        d.shift("GfxWorld", 52, 4);
        d.add_scalar("GfxWorld", "sortKeyUnknown", 52, "u32");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_document_is_the_iw4_schema() {
        let d = Doc::new();
        let v = d.finish(423);
        let orig: Value = serde_json::from_str(include_str!("schema/iw4.json")).unwrap();
        let mut a = v;
        // ids and the asset filter are the only differences
        for s in a["structs"].as_array_mut().unwrap() {
            for m in s["m"].as_array_mut().unwrap() {
                m.as_object_mut().unwrap().remove("_id");
            }
        }
        assert_eq!(a["structs"], orig["structs"]);
    }

    #[test]
    fn the_schema_loads() {
        for v in [334, 423, 461] {
            let _ = schema(v);
        }
    }
}
