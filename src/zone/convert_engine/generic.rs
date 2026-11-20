//! Schema-to-schema conversion: structures of a source zone (read mode memory) are rebuilt in the target game's layout in a
//! `Heap`, member by member, matching members by name (and an optional rename table). Rules can replace or fix up the conversion
//! of any target structure; whatever has no counterpart stays zero. The result is written out by `emit`.

use super::*;
use std::collections::{BTreeMap, HashMap};

/// Read access to a memory the expressions of a schema can be evaluated over.
pub trait MemRead {
    fn m_rd(&self, addr: u32, size: usize) -> Option<u64>;
    fn m_rdp(&self, addr: u32) -> Option<u64>;
}

impl MemRead for Walker<'_> {
    fn m_rd(&self, addr: u32, size: usize) -> Option<u64> {
        self.rd(addr, size).ok()
    }
    fn m_rdp(&self, addr: u32) -> Option<u64> {
        self.rdp(addr).ok()
    }
}

impl MemRead for Heap {
    fn m_rd(&self, addr: u32, size: usize) -> Option<u64> {
        if !self.fits(addr, size) {
            return None;
        }
        let b = self.get(addr, size);
        let mut v = 0u64;
        for (i, x) in b.iter().enumerate() {
            v |= (*x as u64) << (8 * i);
        }
        Some(v)
    }
    fn m_rdp(&self, addr: u32) -> Option<u64> {
        self.m_rd(addr, self.psz)
    }
}

fn eval_expr(e: &Expr, vars: &[u32], mem: &dyn MemRead, structs: &[SStruct]) -> Option<i64> {
    match e {
        Expr::Num(n) => Some(*n),
        Expr::Op(op, a, b) => {
            let x = eval_expr(a, vars, mem, structs)?;
            if op == "&&" {
                return Some((x != 0 && eval_expr(b, vars, mem, structs)? != 0) as i64);
            }
            if op == "||" {
                return Some((x != 0 || eval_expr(b, vars, mem, structs)? != 0) as i64);
            }
            let y = eval_expr(b, vars, mem, structs)?;
            Some(match op.as_str() {
                "+" => x.wrapping_add(y),
                "-" => {
                    if let Expr::Var(sid) = &**a {
                        return Some((x - y) / structs[*sid].size.max(1) as i64);
                    }
                    x.wrapping_sub(y)
                }
                "*" => x.wrapping_mul(y),
                "/" => x.checked_div(y)?,
                "%" => x.checked_rem(y)?,
                "<<" => x.wrapping_shl(y as u32),
                ">>" => x.wrapping_shr(y as u32),
                "<" => (x < y) as i64,
                "<=" => (x <= y) as i64,
                ">" => (x > y) as i64,
                ">=" => (x >= y) as i64,
                "==" => (x == y) as i64,
                "!=" => (x != y) as i64,
                "&" => x & y,
                "^" => x ^ y,
                "|" => x | y,
                _ => return None,
            })
        }
        Expr::Var(sid) => Some(vars[*sid] as i64),
        Expr::Dyn(d) => {
            let var = vars[d.root];
            if var == 0 {
                return None;
            }
            let mut addr = var;
            for &(off, deref) in &d.refs {
                addr = addr.wrapping_add(off);
                if deref {
                    addr = mem.m_rdp(addr)? as u32;
                }
            }
            let mut extra = 0i64;
            for (k, ix) in d.idx.iter().enumerate() {
                let v = eval_expr(ix, vars, mem, structs)?;
                let stride: i64 = d.dims.iter().skip(k + 1).map(|&x| x as i64).product::<i64>() * d.esz as i64;
                extra += v * stride;
            }
            let addr = (addr as i64 + extra) as u32;
            let raw = mem.m_rd(addr, d.kind.size())?;
            Some(match d.kind {
                Kind::I8 => raw as u8 as i8 as i64,
                Kind::U8 => raw as u8 as i64,
                Kind::I16 => raw as u16 as i16 as i64,
                Kind::U16 => raw as u16 as i64,
                Kind::I32 => raw as u32 as i32 as i64,
                Kind::U32 | Kind::F32 => raw as u32 as i64,
                Kind::I64 | Kind::U64 | Kind::F64 => raw as i64,
            })
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct S {
    pub sid: usize,
    pub addr: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct T {
    pub sid: usize,
    pub addr: u32,
}

pub type R = std::result::Result<(), String>;
pub type RuleFn = fn(&mut Conv, S, T) -> R;

/// How the shapes of a member's modifiers read for the conversion.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Sh {
    Val,
    /// An embedded array (all dimensions flattened).
    Arr(usize),
    Ptr,
    PtrN,
    ArrPtr(usize),
    PtrNPtr,
    /// An embedded array of array pointers (the counts per element).
    ArrN(usize),
    /// An array whose element count is an expression (inside a structure of dynamic size).
    DynArr,
    Other,
}

fn is_array_count(c: &Option<Expr>) -> bool {
    c.as_ref().is_some_and(is_array_eval)
}

fn shape(m: &Member) -> Sh {
    match m.mods.as_slice() {
        [] => Sh::Val,
        mods if mods.iter().all(|x| matches!(x, Mod::Array { dsize: None, dcount: None, .. })) => Sh::Arr(mods.iter().map(|x| if let Mod::Array { n, .. } = x { *n as usize } else { 1 }).product()),
        [Mod::Ptr { count, .. }] => {
            if is_array_count(count) {
                Sh::PtrN
            } else {
                Sh::Ptr
            }
        }
        [Mod::Array { n, dsize: None, dcount: None }, Mod::Ptr { count, by_idx }] => {
            if is_array_count(count) || by_idx.iter().flatten().any(is_array_eval) {
                Sh::ArrN(*n as usize)
            } else {
                Sh::ArrPtr(*n as usize)
            }
        }
        [Mod::Ptr { count, .. }, Mod::Ptr { count: c2, .. }] if is_array_count(count) && !is_array_count(c2) => Sh::PtrNPtr,
        [Mod::Array { dsize: Some(_), .. }] => Sh::DynArr,
        _ => Sh::Other,
    }
}

fn pointerish(sh: Sh) -> bool {
    matches!(sh, Sh::Ptr | Sh::PtrN | Sh::ArrPtr(_) | Sh::PtrNPtr | Sh::ArrN(_) | Sh::DynArr | Sh::Other)
}

/// Rules of one conversion hop.
#[derive(Default)]
pub struct Rules {
    /// (target structure, target member) -> source member name.
    pub renames: HashMap<(String, String), String>,
    /// Target structure -> rule replacing the generic conversion (the rule may call `Conv::generic_members`).
    pub replace: HashMap<String, RuleFn>,
    /// Target structure -> rule run after the generic conversion.
    pub post: HashMap<String, RuleFn>,
    /// Source asset structure -> target asset structure (identity when absent).
    pub asset_map: HashMap<String, String>,
    /// Source asset structures that are not converted, with the reason.
    pub skip: HashMap<String, String>,
}

/// Plain arrays at least this large are kept apart from the address window of the heap.
const BLOB_BYTES: usize = 256 * 1024;

/// A converted asset that is an entry of the target's asset list.
pub struct TopAsset {
    pub ty: u32,
    pub ptr: u32,
    /// Source asset id.
    pub id: u32,
}

struct Region {
    ssz: u32,
    n: u32,
    tbase: u32,
    tsz: u32,
    ssid: usize,
    tsid: usize,
}

pub struct Conv {
    pub game: Game,
    w_ptr: *const Walker<'static>,
    /// Asset events seen so far and the entry being converted.
    events: Vec<AssetRec>,
    cur_entry: usize,
    pub ss: &'static Schema,
    pub ts: &'static Schema,
    pub heap: Heap,
    pub svars: Vec<u32>,
    pub tvars: Vec<u32>,
    pub rules: Rules,
    cache: HashMap<(u32, usize, usize), u32>,
    temp_keys: Vec<u32>,
    regions: BTreeMap<u32, Region>,
    /// Source asset id -> target header (None: left out).
    pub assets: HashMap<u32, Option<u32>>,
    /// Target header -> the heap headers of the assets it references (for ordering).
    pub deps: HashMap<u32, Vec<u32>>,
    dep_stack: Vec<Vec<u32>>,
    by_id: HashMap<u32, usize>,
    dummy: u32,
    layout_memo: HashMap<(usize, usize), bool>,
    pub warnings: BTreeMap<String, usize>,
    pub nulled: usize,
    /// Why assets were left out: source asset id -> reason.
    pub reasons: HashMap<u32, String>,
    ctx: String,
}

fn strip(name: &str) -> &str {
    name
}

impl Conv {
    pub fn new(game: Game, target: Game, rules: Rules) -> Conv {
        let ss = Schema::get(game);
        let ts = Schema::get(target);
        Conv {
            game,
            w_ptr: std::ptr::null(),
            events: Vec::new(),
            cur_entry: 0,
            ss,
            ts,
            heap: Heap::new(ts.ptr),
            svars: vec![0; ss.structs.len()],
            tvars: vec![0; ts.structs.len()],
            rules,
            cache: HashMap::new(),
            temp_keys: Vec::new(),
            regions: BTreeMap::new(),
            assets: HashMap::new(),
            deps: HashMap::new(),
            dep_stack: Vec::new(),
            by_id: HashMap::new(),
            dummy: 0,
            layout_memo: HashMap::new(),
            warnings: BTreeMap::new(),
            nulled: 0,
            reasons: HashMap::new(),
            ctx: String::new(),
        }
    }

    /// The source walker, valid while an entry is being converted (`convert_entry`).
    pub fn w(&self) -> &'static Walker<'static> {
        // SAFETY: `w_ptr` is set from a live `&Walker` at the start of `convert_entry` and the reference never outlives that call:
        // nothing keeps it, and every user of `w()` runs inside the conversion of the entry.
        unsafe { &*self.w_ptr }
    }

    /// Source addresses in the temporary block mean something only until the entry ends.
    fn is_temp_addr(&self, a: u32) -> bool {
        self.ss.blocks.get(((a.wrapping_sub(1)) >> 28) as usize).is_some_and(|b| b.1 == 0)
    }

    fn cache_put(&mut self, key: (u32, usize, usize), v: u32) {
        if self.is_temp_addr(key.0) {
            self.temp_keys.push(key.0);
        }
        self.cache.insert(key, v);
    }

    fn region_put(&mut self, sp: u32, r: Region) {
        if self.is_temp_addr(sp) {
            self.temp_keys.push(sp);
        }
        self.regions.insert(sp, r);
    }

    /// Forgets what was converted from the temporary block (its memory is reused by the next entry).
    fn forget_temp(&mut self) {
        let keys = std::mem::take(&mut self.temp_keys);
        if keys.is_empty() {
            return;
        }
        let set: std::collections::HashSet<u32> = keys.iter().copied().collect();
        self.cache.retain(|k, _| !set.contains(&k.0));
        for k in keys {
            self.regions.remove(&k);
        }
    }

    pub fn warn(&mut self, msg: String) {
        let key = format!("{}: {}", strip(&self.ctx), msg);
        *self.warnings.entry(key).or_default() += 1;
    }

    // ---------------------------------------------------------------- source memory
    pub fn s_bytes(&self, addr: u32, n: usize) -> Option<&[u8]> {
        if n == 0 {
            return Some(&[]);
        }
        let (b, o) = self.w().decode(addr, n).ok()?;
        self.w().mem[b].get(o..o + n)
    }

    pub fn s_u32(&self, addr: u32) -> u32 {
        self.w().rd(addr, 4).unwrap_or(0) as u32
    }

    pub fn s_ptr(&self, addr: u32) -> u32 {
        self.w().rdp(addr).unwrap_or(0) as u32
    }

    pub fn s_cstr(&self, addr: u32) -> Vec<u8> {
        self.w().rd_cstr(addr).unwrap_or_default()
    }

    pub fn smember(&self, sid: usize, name: &str) -> Option<&'static Member> {
        self.ss.structs[sid].members.iter().find(|m| m.name == name)
    }

    pub fn tmember(&self, tid: usize, name: &str) -> Option<&'static Member> {
        self.ts.structs[tid].members.iter().find(|m| m.name == name)
    }

    /// A scalar member of a source structure (the first element of an array), as an integer.
    pub fn si(&self, s: S, name: &str) -> i64 {
        let Some(m) = self.smember(s.sid, name) else { return 0 };
        match read_num(self.w(), s.addr + m.off, m.k) {
            Some(Num::I(v)) => v,
            Some(Num::F(f)) => f as i64,
            None => 0,
        }
    }

    pub fn sf(&self, s: S, name: &str) -> f64 {
        let Some(m) = self.smember(s.sid, name) else { return 0.0 };
        match read_num(self.w(), s.addr + m.off, m.k) {
            Some(Num::I(v)) => v as f64,
            Some(Num::F(f)) => f,
            None => 0.0,
        }
    }

    /// The pointer value (a source address) of a pointer member, 0 when null.
    pub fn sp(&self, s: S, name: &str) -> u32 {
        match self.smember(s.sid, name) {
            Some(m) => self.s_ptr(s.addr + m.off),
            None => 0,
        }
    }

    /// The embedded structure member of a source structure.
    pub fn ssub(&self, s: S, name: &str) -> Option<S> {
        let m = self.smember(s.sid, name)?;
        Some(S { sid: m.ty?, addr: s.addr + m.off })
    }

    /// The loader's element count of a source pointer member.
    pub fn scount(&self, s: S, name: &str) -> usize {
        let Some(m) = self.smember(s.sid, name) else { return 0 };
        match m.mods.first() {
            Some(Mod::Ptr { count: Some(e), .. }) => eval_expr(e, &self.svars, self.w(), &self.ss.structs).unwrap_or(0).max(0) as usize,
            Some(Mod::Ptr { count: None, .. }) => 1,
            _ => 0,
        }
    }

    pub fn sstr(&self, s: S, name: &str) -> Option<Vec<u8>> {
        let p = self.sp(s, name);
        if p == 0 {
            None
        } else {
            Some(self.s_cstr(p))
        }
    }

    pub fn t_set_i(&mut self, t: T, name: &str, v: i64) {
        if let Some(m) = self.tmember(t.sid, name) {
            write_num(&mut self.heap, t.addr + m.off, m.k, Num::I(v));
        }
    }

    pub fn t_set_f(&mut self, t: T, name: &str, v: f64) {
        if let Some(m) = self.tmember(t.sid, name) {
            write_num(&mut self.heap, t.addr + m.off, m.k, Num::F(v));
        }
    }

    pub fn t_get_i(&self, t: T, name: &str) -> i64 {
        let Some(m) = self.tmember(t.sid, name) else { return 0 };
        match read_num(&self.heap, t.addr + m.off, m.k) {
            Some(Num::I(v)) => v,
            Some(Num::F(f)) => f as i64,
            None => 0,
        }
    }

    pub fn t_sub(&self, t: T, name: &str) -> Option<T> {
        let m = self.tmember(t.sid, name)?;
        Some(T { sid: m.ty?, addr: t.addr + m.off })
    }

    /// Allocates `n` zeroed elements for the pointer member `name` of a target structure, points the member at them and returns the
    /// array (elements of the member's structure type; for primitive members `sid` is `usize::MAX`).
    pub fn t_alloc(&mut self, t: T, name: &str, n: usize) -> Option<T> {
        let m = self.tmember(t.sid, name)?;
        let es = match m.ty {
            Some(ty) => self.ts.structs[ty].size as usize,
            None => m.es as usize,
        };
        let a = self.heap.alloc(es * n.max(1), 4);
        self.heap.set_ptr(t.addr + m.off, if n == 0 { 0 } else { a });
        Some(T { sid: m.ty.unwrap_or(usize::MAX), addr: a })
    }

    pub fn t_str(&mut self, t: T, name: &str, s: &[u8]) {
        if let Some(m) = self.tmember(t.sid, name) {
            let a = self.heap.alloc_cstr(s);
            self.heap.set_ptr(t.addr + m.off, a);
        }
    }

    /// A dummy object for pointers into runtime memory (only the pointer's being set matters: the loader reserves the memory).
    pub fn dummy(&mut self) -> u32 {
        if self.dummy == 0 {
            self.dummy = self.heap.alloc(16, 4);
        }
        self.dummy
    }

    fn copy_raw(&mut self, saddr: u32, taddr: u32, n: usize) {
        if let Some(b) = self.s_bytes(saddr, n) {
            let b = b.to_vec();
            if self.heap.fits(taddr, n) {
                self.heap.put(taddr, &b);
            }
        }
    }

    // ---------------------------------------------------------------- layout comparisons
    /// Both structures are leaves (no loaded data) with the same members at the same offsets and the same size: their bytes can be copied.
    fn same_layout(&mut self, ssid: usize, tsid: usize) -> bool {
        if let Some(&r) = self.layout_memo.get(&(ssid, tsid)) {
            return r;
        }
        self.layout_memo.insert((ssid, tsid), false);
        let (a, b) = (&self.ss.structs[ssid], &self.ts.structs[tsid]);
        let mut ok = a.size == b.size && a.union == b.union && a.dynoff < 0 && b.dynoff < 0 && a.members.len() == b.members.len();
        if ok {
            for (x, y) in a.members.iter().zip(b.members.iter()) {
                if x.name != y.name || x.off != y.off || x.k != y.k || x.bits != y.bits || x.mods.len() != y.mods.len() || x.need || y.need || x.is_string != y.is_string {
                    ok = false;
                    break;
                }
                for (m1, m2) in x.mods.iter().zip(y.mods.iter()) {
                    match (m1, m2) {
                        (Mod::Array { n: n1, .. }, Mod::Array { n: n2, .. }) if n1 == n2 => {}
                        _ => ok = false,
                    }
                }
                match (x.ty, y.ty) {
                    (None, None) => {}
                    (Some(p), Some(q)) => {
                        if !self.same_layout(p, q) {
                            ok = false;
                        }
                    }
                    _ => ok = false,
                }
                if !ok {
                    break;
                }
            }
        }
        self.layout_memo.insert((ssid, tsid), ok);
        ok
    }

    fn src_loaded(&self, m: &Member) -> bool {
        if m.ign {
            return false;
        }
        match &m.cond {
            Some(c) => eval_expr(c, &self.svars, self.w(), &self.ss.structs).unwrap_or(0) != 0,
            None => true,
        }
    }

    fn target_is_runtime(&self, m: &Member) -> bool {
        m.blk.is_some_and(|b| self.ts.blocks[b as usize].1 == 1)
    }

    // ---------------------------------------------------------------- structures
    /// Converts a source structure into the (zeroed or pre-filled) target storage, applying the rules of the target structure.
    pub fn generic(&mut self, s: S, t: T) -> R {
        if t.addr < HEAP_BASE || t.addr >= HEAP_BASE + 0x0FFF_FFF0 {
            return Err(format!("{} -> {}: the target storage {:#x} is outside the heap (source {:#x}){}", self.ss.structs[s.sid].name, self.ts.structs[t.sid].name, t.addr, s.addr, if self.heap.overflow { " after the heap overflowed" } else { "" }));
        }
        let (sv, tv) = (self.svars[s.sid], self.tvars[t.sid]);
        self.svars[s.sid] = s.addr;
        self.tvars[t.sid] = t.addr;
        let name = self.ts.structs[t.sid].name.as_str();
        let r = if let Some(rule) = self.rules.replace.get(name).copied() {
            rule(self, s, t)
        } else {
            self.generic_members(s, t)
        };
        let r = match (r, self.rules.post.get(name).copied()) {
            (Ok(()), Some(post)) => post(self, s, t),
            (r, _) => r,
        };
        self.svars[s.sid] = sv;
        self.tvars[t.sid] = tv;
        r
    }

    /// The by-name conversion of the members of a structure (no rules for the structure itself; its members' structures apply theirs).
    pub fn generic_members(&mut self, s: S, t: T) -> R {
        let sst = &self.ss.structs[s.sid];
        let tst = &self.ts.structs[t.sid];
        let saved = std::mem::replace(&mut self.ctx, format!("{}", tst.name));
        let r = self.generic_members_inner(s, t, sst, tst);
        self.ctx = saved;
        r
    }

    fn generic_members_inner(&mut self, s: S, t: T, sst: &'static SStruct, tst: &'static SStruct) -> R {
        // structures made of bit fields (and the like) are plain data: same size, same bytes
        if sst.size == tst.size && tst.members.iter().any(|m| m.bits > 0) && !tst.members.iter().any(|m| m.need) {
            self.copy_raw(s.addr, t.addr, tst.size as usize);
            return Ok(());
        }
        if tst.union {
            return self.union_members(s, t, sst, tst);
        }
        // pass 1: values; pass 2: everything that points
        for pass in 0..2 {
            for tm in &tst.members {
                if tm.ign || tm.bits > 0 {
                    continue;
                }
                let Some(sm) = self.find_src(sst, tst, tm) else { continue };
                let (shs, sht) = (shape(sm), shape(tm));
                if pointerish(sht) != (pass == 1) {
                    continue;
                }
                if sm.bits > 0 {
                    continue;
                }
                if pass == 1 && !self.src_loaded(sm) {
                    continue;
                }
                self.member(s, sm, shs, t, tm, sht)?;
            }
        }
        Ok(())
    }

    fn find_src(&self, sst: &'static SStruct, tst: &'static SStruct, tm: &Member) -> Option<&'static Member> {
        if self.rules.renames.is_empty() {
            return sst.members.iter().find(|m| m.name == tm.name);
        }
        let name = self.rules.renames.get(&(tst.name.clone(), tm.name.clone())).map(|s| s.as_str()).unwrap_or(tm.name.as_str());
        sst.members.iter().find(|m| m.name == name)
    }

    fn union_members(&mut self, s: S, t: T, sst: &'static SStruct, tst: &'static SStruct) -> R {
        let loads = tst.members.iter().any(|m| m.need && !m.ign);
        if !loads || !sst.union || !sst.members.iter().any(|m| m.need && !m.ign) {
            // a plain union: the bytes
            let n = sst.size.min(tst.size) as usize;
            self.copy_raw(s.addr, t.addr, n);
            return Ok(());
        }
        // the member the loader loads
        let mut active = None;
        for m in &sst.members {
            if m.ign || !m.need {
                continue;
            }
            let ok = match &m.cond {
                Some(c) => eval_expr(c, &self.svars, self.w(), &self.ss.structs).unwrap_or(0) != 0,
                None => true,
            };
            if ok {
                active = Some(m);
                break;
            }
        }
        let Some(sm) = active else { return Ok(()) };
        // the same-named target member, or the one the rename table gives
        let tm = tst.members.iter().find(|m| self.rules.renames.get(&(tst.name.clone(), m.name.clone())).map(|s| s.as_str()).unwrap_or(m.name.as_str()) == sm.name);
        match tm {
            Some(tm) => self.member(s, sm, shape(sm), t, tm, shape(tm)),
            None => {
                self.warn(format!("union member {} has no counterpart", sm.name));
                Ok(())
            }
        }
    }

    // ---------------------------------------------------------------- members
    fn member(&mut self, s: S, sm: &'static Member, shs: Sh, t: T, tm: &'static Member, sht: Sh) -> R {
        let sa = s.addr + sm.off;
        let ta = t.addr + tm.off;
        match (shs, sht) {
            (Sh::Val, Sh::Val) => self.value(sm, sa, tm, ta),
            (Sh::Arr(ns), Sh::Arr(nt)) => self.array(sm, sa, ns, tm, ta, nt),
            (Sh::Ptr, Sh::Ptr) => self.single_pointer(sm, sa, tm, ta),
            (Sh::PtrN, Sh::PtrN) => self.array_pointer(s, sm, sa, t, tm, ta),
            (Sh::ArrPtr(ns), Sh::ArrPtr(nt)) => self.pointer_array(sm, sa, ns, tm, ta, nt, true),
            (Sh::PtrNPtr, Sh::PtrNPtr) => self.pointer_array_pointer(s, sm, sa, t, tm, ta),
            (Sh::Val, Sh::Arr(_)) | (Sh::Arr(_), Sh::Val) => {
                if sm.es == tm.es && sm.ty.is_none() && tm.ty.is_none() {
                    let n = (sm.es as usize).min(tm.es as usize);
                    self.copy_raw(sa, ta, n);
                } else {
                    self.warn(format!("member {} changes shape", tm.name));
                }
                Ok(())
            }
            (Sh::ArrN(ns), Sh::ArrN(nt)) => self.array_of_array_pointers(sm, sa, ns, tm, ta, nt),
            (Sh::DynArr, Sh::DynArr) => {
                // the elements were copied with the structure; the ones that hold pointers are converted in place
                if let (Some(st), Some(tt)) = (sm.ty, tm.ty) {
                    if !self.ss.structs[st].leaf {
                        let n = match &sm.mods[0] {
                            Mod::Array { dsize: Some(e), .. } => eval_expr(e, &self.svars, self.w(), &self.ss.structs).unwrap_or(0).max(0) as u32,
                            _ => 0,
                        };
                        let (ssz, tsz) = (self.ss.structs[st].size, self.ts.structs[tt].size);
                        for i in 0..n {
                            self.generic(S { sid: st, addr: sa + i * ssz }, T { sid: tt, addr: ta + i * tsz })?;
                        }
                    }
                }
                Ok(())
            }
            (Sh::Other, Sh::Other) if self.ts.structs[t.sid].dynoff >= 0 => Ok(()),
            _ => {
                self.warn(format!("member {} has a different shape in the two layouts ({:?} against {:?})", tm.name, shs, sht));
                Ok(())
            }
        }
    }

    fn value(&mut self, sm: &'static Member, sa: u32, tm: &'static Member, ta: u32) -> R {
        match (sm.ty, tm.ty) {
            (None, None) => {
                if let (Some(sk), Some(tk)) = (sm.k, tm.k) {
                    if sk == tk {
                        self.copy_raw(sa, ta, sk.size());
                    } else if let Some(v) = read_num(self.w(), sa, sm.k) {
                        write_num(&mut self.heap, ta, tm.k, v);
                    }
                }
                Ok(())
            }
            (Some(st), Some(tt)) => self.generic(S { sid: st, addr: sa }, T { sid: tt, addr: ta }),
            _ => {
                if sm.es == tm.es {
                    self.copy_raw(sa, ta, sm.es as usize);
                } else {
                    self.warn(format!("member {} is a structure on one side only", tm.name));
                }
                Ok(())
            }
        }
    }

    fn array(&mut self, sm: &'static Member, sa: u32, ns: usize, tm: &'static Member, ta: u32, nt: usize) -> R {
        let n = ns.min(nt);
        match (sm.ty, tm.ty) {
            (None, None) => {
                let (Some(sk), Some(tk)) = (sm.k, tm.k) else { return Ok(()) };
                if sk == tk {
                    self.copy_raw(sa, ta, n * sk.size());
                } else {
                    for i in 0..n {
                        if let Some(v) = read_num(self.w(), sa + (i * sk.size()) as u32, sm.k) {
                            write_num(&mut self.heap, ta + (i * tk.size()) as u32, tm.k, v);
                        }
                    }
                }
                Ok(())
            }
            (Some(st), Some(tt)) => {
                let (ssz, tsz) = (self.ss.structs[st].size, self.ts.structs[tt].size);
                if self.same_layout(st, tt) {
                    self.copy_raw(sa, ta, n * ssz as usize);
                    return Ok(());
                }
                for i in 0..n as u32 {
                    self.generic(S { sid: st, addr: sa + i * ssz }, T { sid: tt, addr: ta + i * tsz })?;
                }
                Ok(())
            }
            _ => {
                self.warn(format!("array {} is of structures on one side only", tm.name));
                Ok(())
            }
        }
    }

    /// The heap address standing for the source address of an asset-pointer slot (the asset converted on demand).
    fn asset_at_slot(&mut self, slot: u32) -> u32 {
        let Some(&id) = self.w().ref_id.get(&slot) else {
            self.warn("a reference to an asset that was not found".to_string());
            self.nulled += 1;
            return 0;
        };
        match self.asset_ref(id) {
            Some(a) => a,
            None => {
                self.nulled += 1;
                0
            }
        }
    }

    fn single_pointer(&mut self, sm: &'static Member, sa: u32, tm: &'static Member, ta: u32) -> R {
        let sp = self.s_ptr(sa);
        if sp == 0 {
            return Ok(());
        }
        if self.target_is_runtime(tm) {
            let d = self.dummy();
            self.heap.set_ptr(ta, d);
            return Ok(());
        }
        let psz = self.heap.psz;
        let _ = psz;
        let v = self.pointee(sm, sa, sp, 1, tm)?;
        self.heap.set_ptr(ta, v);
        Ok(())
    }

    /// What a pointer (source address `sp`, `n` elements) becomes: strings are copied, assets converted, structures and values rebuilt.
    fn pointee(&mut self, sm: &'static Member, slot: u32, sp: u32, n: usize, tm: &'static Member) -> std::result::Result<u32, String> {
        if sm.is_string && tm.is_string {
            let s = self.s_cstr(sp);
            return Ok(self.heap.alloc_cstr(&s));
        }
        let sasset = sm.ty.is_some_and(|t| self.ss.structs[t].asset);
        let tasset = tm.ty.is_some_and(|t| self.ts.structs[t].asset);
        if sasset || tasset {
            if sasset && tasset {
                return Ok(self.asset_at_slot(slot));
            }
            self.warn(format!("{} is an asset reference on one side only", tm.name));
            return Ok(0);
        }
        match (sm.ty, tm.ty) {
            (Some(st), Some(tt)) => self.struct_array(sp, n, st, tt),
            (None, None) => self.value_array(sp, n, sm, tm),
            _ => {
                self.warn(format!("{} points at a structure on one side only", tm.name));
                Ok(0)
            }
        }
    }

    /// Source structures `sp..` (n of them) as target structures; identical layouts are copied, shared data stays shared.
    pub fn struct_array(&mut self, sp: u32, n: usize, st: usize, tt: usize) -> std::result::Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        if let Some(&a) = self.cache.get(&(sp, n, tt)) {
            return Ok(a);
        }
        let (ssz, tsz) = (self.ss.structs[st].size, self.ts.structs[tt].size);
        if let Some((&start, r)) = self.regions.range(..=sp).next_back() {
            if r.ssid == st && r.tsid == tt && sp - start < r.ssz * r.n && (sp - start) % r.ssz == 0 && (sp - start) / r.ssz + n as u32 <= r.n {
                let a = r.tbase + (sp - start) / r.ssz * r.tsz;
                self.cache_put((sp, n, tt), a);
                return Ok(a);
            }
        }
        // 64-bit zones: an offset pointer to a structure of dynamic size designates the word before it
        if self.w().psz == 8 {
            if let Some((&start, r)) = self.regions.range(sp + 1..=sp + 4).next() {
                if r.ssid == st && r.tsid == tt && start - sp == 4 {
                    let a = r.tbase;
                    self.cache_put((sp, n, tt), a);
                    return Ok(a);
                }
            }
        }
        let dyn_s = self.ss.structs[st].dynoff >= 0;
        if !dyn_s && ssz as usize * n >= BLOB_BYTES && self.same_layout(st, tt) {
            // a large array of plain data: kept apart, no part of it can be pointed at
            let bytes = self.s_bytes(sp, ssz as usize * n).map(|b| b.to_vec()).unwrap_or_default();
            let a = self.heap.alloc_blob(bytes);
            self.cache_put((sp, n, tt), a);
            return Ok(a);
        }
        let size = if dyn_s { self.footprint(S { sid: st, addr: sp }) } else { tsz as usize * n };
        let base = self.heap.alloc(size.max(tsz as usize), 4);
        if self.heap.overflow {
            return Err(format!("{} x {} {} ({} bytes each) do not fit the address window of a zone", n, self.ts.structs[tt].name, self.ss.structs[st].name, tsz));
        }
        self.cache_put((sp, n, tt), base);
        self.region_put(sp, Region { ssz, n: n as u32, tbase: base, tsz, ssid: st, tsid: tt });
        if dyn_s {
            // a structure of dynamic size: head and trailing data are contiguous in memory, copy them whole and patch the pointers
            self.copy_raw(sp, base, size);
            self.generic(S { sid: st, addr: sp }, T { sid: tt, addr: base })?;
            return Ok(base);
        }
        if self.same_layout(st, tt) {
            self.copy_raw(sp, base, ssz as usize * n);
            return Ok(base);
        }
        for i in 0..n as u32 {
            self.generic(S { sid: st, addr: sp + i * ssz }, T { sid: tt, addr: base + i * tsz })?;
        }
        Ok(base)
    }

    /// Bytes a structure of dynamic size occupies in the loaded memory: its head and the data that follows it.
    fn footprint(&self, s: S) -> usize {
        let mut vars = self.svars.clone();
        self.footprint_in(s, &mut vars)
    }

    fn footprint_in(&self, s: S, vars: &mut Vec<u32>) -> usize {
        let st = &self.ss.structs[s.sid];
        if st.dynoff < 0 {
            return st.size as usize;
        }
        let saved = vars[s.sid];
        vars[s.sid] = s.addr;
        let mut total = st.dynoff as usize;
        let mut matched = false;
        for m in &st.members {
            if m.ign || !m.need || !m.after_partial {
                continue;
            }
            if st.union && matched {
                break;
            }
            let ok = match &m.cond {
                Some(c) => eval_expr(c, vars, self.w(), &self.ss.structs).unwrap_or(0) != 0,
                None => true,
            };
            if !ok {
                continue;
            }
            matched = true;
            let sub = s.addr + m.off;
            match (m.mods.first(), m.ty) {
                (Some(Mod::Array { dsize: Some(e), .. }), _) => {
                    let n = eval_expr(e, vars, self.w(), &self.ss.structs).unwrap_or(0).max(0) as usize;
                    total += n * m.es as usize;
                }
                (None, Some(ty)) => total += self.footprint_in(S { sid: ty, addr: sub }, vars),
                (None, None) => total += m.es as usize,
                _ => {}
            }
        }
        vars[s.sid] = saved;
        total
    }

    fn value_array(&mut self, sp: u32, n: usize, sm: &'static Member, tm: &'static Member) -> std::result::Result<u32, String> {
        let (Some(sk), Some(tk)) = (sm.k, tm.k) else { return Ok(0) };
        if n == 0 {
            return Ok(0);
        }
        // elements may be small arrays themselves (`byte[3]`): the member's element size is what counts
        let (ssz, tsz) = (sm.es as usize, tm.es as usize);
        let same = sk == tk && ssz == tsz;
        if !same && (ssz != sk.size() || tsz != tk.size()) {
            self.warn(format!("{}: elements of {ssz} bytes in the source, {tsz} in the target", tm.name));
            return Ok(0);
        }
        let key = (sp, n, usize::MAX - tsz - if matches!(tk, Kind::F32 | Kind::F64) { 100 } else { 0 });
        if let Some(&a) = self.cache.get(&key) {
            return Ok(a);
        }
        let kid = usize::MAX - 1000 - ssz * 10 - tsz;
        if let Some((&start, r)) = self.regions.range(..=sp).next_back() {
            if r.ssid == kid && sp - start < r.ssz * r.n && (sp - start) % r.ssz == 0 && (sp - start) / r.ssz + n as u32 <= r.n {
                let a = r.tbase + (sp - start) / r.ssz * r.tsz;
                self.cache_put(key, a);
                return Ok(a);
            }
        }
        if same && ssz * n >= BLOB_BYTES {
            let bytes = self.s_bytes(sp, ssz * n).map(|b| b.to_vec()).unwrap_or_default();
            let a = self.heap.alloc_blob(bytes);
            self.cache_put(key, a);
            return Ok(a);
        }
        let base = self.heap.alloc(tsz * n, 4);
        self.cache_put(key, base);
        self.region_put(sp, Region { ssz: ssz as u32, n: n as u32, tbase: base, tsz: tsz as u32, ssid: kid, tsid: kid });
        if same {
            self.copy_raw(sp, base, ssz * n);
        } else {
            for i in 0..n {
                if let Some(v) = read_num(self.w(), sp + (i * ssz) as u32, sm.k) {
                    write_num(&mut self.heap, base + (i * tsz) as u32, tm.k, v);
                }
            }
        }
        Ok(base)
    }

    fn array_pointer(&mut self, s: S, sm: &'static Member, sa: u32, t: T, tm: &'static Member, ta: u32) -> R {
        let sp = self.s_ptr(sa);
        if sp == 0 {
            return Ok(());
        }
        if self.target_is_runtime(tm) {
            let d = self.dummy();
            self.heap.set_ptr(ta, d);
            return Ok(());
        }
        let ns = match &sm.mods[0] {
            Mod::Ptr { count: Some(e), .. } => eval_expr(e, &self.svars, self.w(), &self.ss.structs).unwrap_or(0).max(0) as usize,
            _ => 1,
        };
        let nt = match &tm.mods[0] {
            Mod::Ptr { count: Some(e), .. } => eval_expr(e, &self.tvars, &self.heap, &self.ts.structs),
            _ => None,
        };
        let _ = (s, t);
        if ns > 0x0040_0000 || nt.is_some_and(|v| v > 0x0040_0000) {
            return Err(format!("{}.{}: a count of {ns} (target {nt:?}) is out of range", self.ts.structs[t.sid].name, tm.name));
        }
        let n = match nt {
            Some(nt) if nt >= 0 => {
                if nt as usize != ns {
                    self.warn(format!("{}: {} elements in the source, {} in the target", tm.name, ns, nt));
                }
                nt as usize
            }
            _ => ns,
        };
        if n == 0 {
            return Ok(());
        }
        // copy as many as both have; the rest of the target array stays zero
        let take = ns.min(n);
        let a = if take == n { self.pointee(sm, sa, sp, n, tm)? } else { self.pointee_padded(sm, sa, sp, take, n, tm)? };
        self.heap.set_ptr(ta, a);
        Ok(())
    }

    fn pointee_padded(&mut self, sm: &'static Member, slot: u32, sp: u32, take: usize, n: usize, tm: &'static Member) -> std::result::Result<u32, String> {
        // an array longer than what the source has: build the shorter one and copy it into a zeroed longer one
        let tsz = match tm.ty {
            Some(t) => self.ts.structs[t].size as usize,
            None => tm.es as usize,
        };
        let short = self.pointee(sm, slot, sp, take, tm)?;
        let a = self.heap.alloc(tsz * n, 4);
        if short != 0 && take > 0 {
            let b = self.heap.get(short, tsz * take).to_vec();
            self.heap.put(a, &b);
        }
        Ok(a)
    }

    fn pointer_array_pointer(&mut self, s: S, sm: &'static Member, sa: u32, t: T, tm: &'static Member, ta: u32) -> R {
        let sp = self.s_ptr(sa);
        if sp == 0 {
            return Ok(());
        }
        let ns = match &sm.mods[0] {
            Mod::Ptr { count: Some(e), .. } => eval_expr(e, &self.svars, self.w(), &self.ss.structs).unwrap_or(0).max(0) as usize,
            _ => 1,
        };
        let nt = match &tm.mods[0] {
            Mod::Ptr { count: Some(e), .. } => eval_expr(e, &self.tvars, &self.heap, &self.ts.structs),
            _ => None,
        };
        let _ = (s, t);
        let n = match nt {
            Some(nt) if nt >= 0 => {
                if nt as usize != ns {
                    self.warn(format!("{}: {} pointers in the source, {} in the target", tm.name, ns, nt));
                }
                nt as usize
            }
            _ => ns,
        };
        if n == 0 {
            return Ok(());
        }
        let psz = self.heap.psz;
        let arr = self.heap.alloc(psz * n, 4);
        self.heap.set_ptr(ta, arr);
        let spsz = self.w().psz as u32;
        for i in 0..ns.min(n) {
            let slot = sp + i as u32 * spsz;
            let p = self.s_ptr(slot);
            if p == 0 {
                continue;
            }
            let v = self.pointee(sm, slot, p, 1, tm)?;
            self.heap.set_ptr(arr + (i * psz) as u32, v);
        }
        Ok(())
    }

    fn array_of_array_pointers(&mut self, sm: &'static Member, sa: u32, ns: usize, tm: &'static Member, ta: u32, nt: usize) -> R {
        let (Some(Mod::Ptr { count: sc, by_idx: sb }), Some(Mod::Ptr { count: tc, by_idx: tb })) = (sm.mods.get(1), tm.mods.get(1)) else { return Ok(()) };
        let psz = self.heap.psz as u32;
        let spsz = self.w().psz as u32;
        for i in 0..ns.min(nt) {
            let slot = sa + i as u32 * spsz;
            let p = self.s_ptr(slot);
            if p == 0 {
                continue;
            }
            let tslot = ta + i as u32 * psz;
            if self.target_is_runtime(tm) {
                let d = self.dummy();
                self.heap.set_ptr(tslot, d);
                continue;
            }
            let se = sb.get(i).and_then(|e| e.as_ref()).or(sc.as_ref());
            let te = tb.get(i).and_then(|e| e.as_ref()).or(tc.as_ref());
            let n_s = se.and_then(|e| eval_expr(e, &self.svars, self.w(), &self.ss.structs)).unwrap_or(0).max(0) as usize;
            let n_t = te.and_then(|e| eval_expr(e, &self.tvars, &self.heap, &self.ts.structs)).map(|v| v.max(0) as usize).unwrap_or(n_s);
            if n_t != n_s {
                self.warn(format!("{}[{i}]: {n_s} elements in the source, {n_t} in the target", tm.name));
            }
            if n_t == 0 {
                continue;
            }
            let a = if n_t == n_s { self.pointee(sm, slot, p, n_s, tm)? } else { self.pointee_padded(sm, slot, p, n_s.min(n_t), n_t, tm)? };
            self.heap.set_ptr(tslot, a);
        }
        Ok(())
    }

    fn pointer_array(&mut self, sm: &'static Member, sa: u32, ns: usize, tm: &'static Member, ta: u32, nt: usize, _embedded: bool) -> R {
        let psz = self.heap.psz as u32;
        let spsz = self.w().psz as u32;
        for i in 0..ns.min(nt) as u32 {
            let slot = sa + i * spsz;
            let p = self.s_ptr(slot);
            if p == 0 {
                continue;
            }
            let v = self.pointee(sm, slot, p, 1, tm)?;
            self.heap.set_ptr(ta + i * psz, v);
        }
        Ok(())
    }

    // ---------------------------------------------------------------- assets
    /// The target header an asset (by source id) became, converting it now when that has not happened; also notes the dependency.
    pub fn asset_ref(&mut self, id: u32) -> Option<u32> {
        let r = self.convert_asset(id);
        if let (Some(a), Some(d)) = (r, self.dep_stack.last_mut()) {
            d.push(a);
        }
        r
    }

    fn convert_asset(&mut self, id: u32) -> Option<u32> {
        if let Some(&r) = self.assets.get(&id) {
            return r;
        }
        let rec = self.events[*self.by_id.get(&id)?].clone();
        if rec.top != self.cur_entry {
            // the memory of an earlier entry's temporary block is gone: an asset nobody converted while its holder was read is lost
            self.warn("a reference to an asset loaded inside an earlier entry that was not converted then".to_string());
            self.assets.insert(id, None);
            return None;
        }
        let sname = self.ss.structs[rec.sid].name.clone();
        if let Some(why) = self.rules.skip.get(&sname) {
            self.reasons.insert(id, why.clone());
            self.assets.insert(id, None);
            return None;
        }
        let tname = self.rules.asset_map.get(&sname).cloned().unwrap_or_else(|| sname.clone());
        let Some(&tsid) = self.ts.by_name.get(&tname) else {
            self.reasons.insert(id, format!("no {tname} in the target game"));
            self.assets.insert(id, None);
            return None;
        };
        let hdr = self.heap.alloc(self.ts.structs[tsid].size as usize, 4);
        self.assets.insert(id, Some(hdr));
        let saved = (std::mem::replace(&mut self.svars, vec![0; self.ss.structs.len()]), std::mem::replace(&mut self.tvars, vec![0; self.ts.structs.len()]));
        self.dep_stack.push(Vec::new());
        self.ctx = tname.clone();
        let r = self.generic(S { sid: rec.sid, addr: rec.hdr }, T { sid: tsid, addr: hdr });
        let deps = self.dep_stack.pop().unwrap_or_default();
        self.svars = saved.0;
        self.tvars = saved.1;
        match r {
            Ok(()) => {
                self.deps.insert(hdr, deps);
                Some(hdr)
            }
            Err(e) => {
                self.reasons.insert(id, e);
                self.assets.insert(id, None);
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------- numbers
#[derive(Clone, Copy, Debug)]
pub enum Num {
    I(i64),
    F(f64),
}

fn read_num(mem: &dyn MemRead, addr: u32, k: Option<Kind>) -> Option<Num> {
    let k = k?;
    let raw = mem.m_rd(addr, k.size())?;
    Some(match k {
        Kind::I8 => Num::I(raw as u8 as i8 as i64),
        Kind::U8 => Num::I(raw as u8 as i64),
        Kind::I16 => Num::I(raw as u16 as i16 as i64),
        Kind::U16 => Num::I(raw as u16 as i64),
        Kind::I32 => Num::I(raw as u32 as i32 as i64),
        Kind::U32 => Num::I(raw as u32 as i64),
        Kind::I64 | Kind::U64 => Num::I(raw as i64),
        Kind::F32 => Num::F(f32::from_bits(raw as u32) as f64),
        Kind::F64 => Num::F(f64::from_bits(raw)),
    })
}

fn write_num(heap: &mut Heap, addr: u32, k: Option<Kind>, v: Num) {
    let Some(k) = k else { return };
    if !heap.fits(addr, k.size()) {
        return;
    }
    let (iv, fv) = match v {
        Num::I(i) => (i, i as f64),
        Num::F(f) => (f as i64, f),
    };
    match k {
        Kind::I8 | Kind::U8 => heap.set_u8(addr, iv as u8),
        Kind::I16 | Kind::U16 => heap.set_u16(addr, iv as u16),
        Kind::I32 | Kind::U32 => heap.set_u32(addr, iv as u32),
        Kind::I64 | Kind::U64 => heap.put(addr, &(iv as u64).to_le_bytes()),
        Kind::F32 => heap.set_u32(addr, (fv as f32).to_bits()),
        Kind::F64 => heap.put(addr, &fv.to_bits().to_le_bytes()),
    }
}

// ---------------------------------------------------------------------------------------------------------------- whole zones
/// The outcome of converting a zone.
pub struct ZoneOut {
    pub image: Vec<u8>,
    /// Per source asset type: (converted, left out by reason).
    pub stats: BTreeMap<u32, (usize, BTreeMap<String, usize>)>,
    pub nulled: usize,
    pub warnings: BTreeMap<String, usize>,
    pub assets: usize,
}

/// Maps the asset type of a source entry to the target's by name (`ASSET_TYPE_*`).
fn target_type(ss: &Schema, ts: &Schema, ty: u32, tsid: usize) -> Option<u32> {
    let name = ss.asset_names.get(ty as usize)?;
    if let Some(i) = ts.asset_names.iter().position(|n| n == name) {
        if ts.assets.get(&(i as u32)) == Some(&tsid) {
            return Some(i as u32);
        }
    }
    let mut types: Vec<u32> = ts.assets.iter().filter(|&(_, &v)| v == tsid).map(|(&k, _)| k).collect();
    types.sort();
    types.first().copied()
}

type Stats = BTreeMap<u32, (usize, BTreeMap<String, usize>)>;

impl Conv {
    /// Converts the asset the entry `i` of the source's asset list loaded (the walker has just finished it).
    fn convert_entry(&mut self, w: &Walker, i: usize, top: &mut Vec<TopAsset>, stats: &mut Stats) {
        // SAFETY: see `w()`; the lifetime is only erased for the duration of this call
        self.w_ptr = unsafe { std::mem::transmute::<*const Walker, *const Walker<'static>>(w as *const Walker) };
        self.cur_entry = i;
        self.forget_temp();
        for e in &w.events[self.events.len()..] {
            self.by_id.insert(e.id, self.events.len());
            self.events.push(e.clone());
        }
        let Some(rec) = self.events.iter().rev().find(|e| e.top == i && w.top_slots.contains(&e.slot)).cloned() else {
            self.w_ptr = std::ptr::null();
            return;
        };
        let ty = w.assets.get(i).map(|a| a.ty).unwrap_or(0);
        let st = stats.entry(ty).or_default();
        match self.asset_ref(rec.id) {
            Some(ptr) => {
                let sname = &self.ss.structs[rec.sid].name;
                let tname = self.rules.asset_map.get(sname).cloned().unwrap_or_else(|| sname.clone());
                let tsid = self.ts.by_name[&tname];
                match target_type(self.ss, self.ts, ty, tsid) {
                    Some(tty) => {
                        st.0 += 1;
                        top.push(TopAsset { ty: tty, ptr, id: rec.id });
                    }
                    None => *st.1.entry("no asset type for it in the target game".to_string()).or_default() += 1,
                }
            }
            None => {
                let why = self.reasons.get(&rec.id).cloned().unwrap_or_else(|| "not converted".to_string());
                *st.1.entry(why).or_default() += 1;
            }
        }
        self.w_ptr = std::ptr::null();
    }
}

/// Converts every asset of the source zone with `rules` and writes the target zone image.
pub fn convert_zone(game: Game, zone: &ZoneImage, target: Game, rules: Rules) -> std::result::Result<ZoneOut, String> {
    let mut c = Conv::new(game, target, rules);
    let mut stats: Stats = BTreeMap::new();
    let mut top: Vec<TopAsset> = Vec::new();
    let mut strings: Vec<Vec<u8>> = Vec::new();
    {
        let mut cb = |w: &Walker, i: usize| {
            if strings.is_empty() {
                let (arr, n) = w.script_strings;
                for k in 0..n {
                    let p = w.rdp(arr + (k * w.psz) as u32).unwrap_or(0) as u32;
                    strings.push(if p == 0 { Vec::new() } else { w.rd_cstr(p).unwrap_or_default() });
                }
            }
            c.convert_entry(w, i, &mut top, &mut stats);
        };
        walk_entries(game, zone, &mut cb)?;
    }
    if c.heap.overflow {
        return Err("the converted structures do not fit the 256 MB address window of a zone".to_string());
    }
    // dependencies first
    let mut order: Vec<usize> = Vec::new();
    {
        let pos: HashMap<u32, usize> = top.iter().enumerate().map(|(i, t)| (t.ptr, i)).collect();
        let mut state = vec![0u8; top.len()];
        fn visit(i: usize, top: &[TopAsset], pos: &HashMap<u32, usize>, deps: &HashMap<u32, Vec<u32>>, state: &mut Vec<u8>, order: &mut Vec<usize>) {
            if state[i] != 0 {
                return;
            }
            state[i] = 1;
            let mut stack = vec![top[i].ptr];
            let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
            while let Some(h) = stack.pop() {
                if !seen.insert(h) {
                    continue;
                }
                for &d in deps.get(&h).map(|v| v.as_slice()).unwrap_or(&[]) {
                    match pos.get(&d) {
                        Some(&j) => visit(j, top, pos, deps, state, order),
                        None => stack.push(d),
                    }
                }
            }
            order.push(i);
        }
        for i in 0..top.len() {
            visit(i, &top, &pos, &c.deps, &mut state, &mut order);
        }
    }
    let strs: Vec<u32> = strings.iter().map(|s| c.heap.alloc_cstr(s)).collect();
    let assets: Vec<(u32, u32)> = order.iter().map(|&i| (top[i].ty, top[i].ptr)).collect();
    let heap = std::mem::replace(&mut c.heap, Heap::new(4));
    let image = emit(target, heap, &strs, &assets)?;
    Ok(ZoneOut { image, stats, nulled: c.nulled, warnings: std::mem::take(&mut c.warnings), assets: assets.len() })
}
