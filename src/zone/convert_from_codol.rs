//! Call of Duty Online (QA 3.3.3.3, 3.19, 1.1.1.13) -> stock IW4.
//!
//! The zone is read by the conversion engine with the Call of Duty Online schema (`convert_codol`: the IW4 schema with the changes
//! measured on real zones), the structures are rebuilt in the stock layout by member name, and the stock stream is
//! written with the loader's own rules (`convert_engine`). The types the schema has no loader for (weapons, menus, vehicles,
//! structured data, the zone-specific types) are stepped over by their directory extents and left out.

use super::convert_cli::{Fmt, HopOut, Input};
use super::convert_codol;
use super::convert_engine::{self as ce, Conv, Rules, R, S, T};
use super::convert_zone::TypeStat;
use super::schema_walk::Game;
use super::Zone;
use anyhow::{anyhow, Result};

/// The stock element type of a QA one (`convert_fx::qa_elem_type` the other way): QA has two more types, at 2 (a rotated sprite) and
/// at 4 (a line); they become the nearest stock ones, a billboard sprite and a tail.
fn stock_elem_type(qa: i64) -> i64 {
    match qa {
        0 | 1 => qa,
        2 => 0,
        3 => 2,
        4 => 2,
        n => n - 2,
    }
}

/// The stock element flags of QA ones (`convert_fx::qa_flags` the other way): QA's bit 8 is the inserted flag, bits 9 to 16 are
/// the stock bits 8 to 15.
fn stock_flags(qa: u32) -> u32 {
    (qa & 0xFFFE_0000) | ((qa & 0x0001_FE00) >> 1) | (qa & 0xFF)
}

fn elem_def(c: &mut Conv, s: S, t: T) -> R {
    c.t_set_i(t, "elemType", stock_elem_type(c.si(s, "elemType")));
    c.t_set_i(t, "flags", stock_flags(c.si(s, "flags") as u32) as i64);
    c.generic_members_except(s, t, &["elemType", "flags"])
}

/// QA's material sort keys are the stock ones from 30 up plus one (measured on 500 materials present in both: 33 -> 34, 34 -> 35,
/// 43 -> 44, 47 -> 48, 48 -> 49, 50 -> 51, 51 -> 52, 53 -> 54; 29 and below equal).
fn material_info(c: &mut Conv, s: S, t: T) -> R {
    let k = c.si(s, "sortKey");
    c.t_set_i(t, "sortKey", if k >= 30 { k - 1 } else { k });
    c.generic_members_except(s, t, &["sortKey"])
}

/// The collision map. The QA map keeps the arrays of its brush model in a `ClipInfo` (planes, materials, brush sides and edges,
/// leaf-brush nodes and leaf brushes, brushes with their bounds and contents) and the stock map has them as its own members, so the
/// members of the `ClipInfo` convert into the map by name. Every collision model of the maps measured points at that same `ClipInfo`
/// (the second one, loaded in the temporary block, aliases it); one that keeps data of its own is not merged and is reported.
/// The map-entities record is loaded inside the map and gets an asset entry of its own, with the stages the QA map holds.
fn clip_map(c: &mut Conv, s: S, t: T) -> R {
    let ci = c.ssid("ClipInfo").ok_or("no ClipInfo in the source layout")?;
    let info = c.ssub(s, "info").ok_or("a collision map without its ClipInfo")?;
    c.generic_members(s, t)?;
    let saved = c.svars[ci];
    c.svars[ci] = info.addr;
    let r = c.generic_members(info, t);
    c.svars[ci] = saved;
    r?;
    // collision models with data of their own
    let names = ["planes", "materials", "brushsides", "brushEdges", "leafbrushNodes", "leafbrushes", "brushes", "brushBounds", "brushContents"];
    for i in 0..c.si(s, "numSubModels").max(0) as usize {
        let Some(m) = c.s_elem(s, "cmodels", i) else { break };
        let ip = c.sp(m, "info");
        if ip != 0 && ip != info.addr {
            let p = S { sid: ci, addr: ip };
            if names.iter().any(|n| c.sp(p, n) != c.sp(info, n) && c.sp(p, n) != 0) {
                c.warn("a collision model keeps brush data of its own (not merged into the map's arrays)".to_string());
                break;
            }
        }
    }
    // the map entities: the stages (a member of the map here) and the trigger arrays
    if let (Some(id), Some(me_t)) = (c.s_asset_id(s, "mapEnts"), c.tsid("MapEnts")) {
        if let Some(hdr) = c.asset_ref(id) {
            let me = T { sid: me_t, addr: hdr };
            let n = c.si(s, "stageCount").max(0) as usize;
            let sp = c.sp(s, "stages");
            if let (true, Some(st), Some(tt)) = (n > 0 && sp != 0, c.ssid("Stage"), c.tsid("Stage")) {
                let a = c.struct_array(sp, n, st, tt)?;
                c.t_set_ptr(me, "stages", a);
                c.t_set_i(me, "stageCount", n as i64);
            }
            c.add_top("MapEnts", hdr);
        }
    }
    Ok(())
}

/// The stock card memory of an image is its total (the QA word at +16) in the second slot.
fn image(c: &mut Conv, s: S, t: T) -> R {
    c.generic_members(s, t)?;
    let total = c.si(s, "cardTotal") as u32;
    if let (true, Some(cm)) = (total != 0, c.t_sub(t, "cardMemory")) {
        c.heap.set_u32(cm.addr + 4, total);
    }
    Ok(())
}

/// The rules of the conversion: what differs in value (not only in layout) between the two games.
pub fn rules() -> Rules {
    rules_with(false)
}

pub fn rules_with(canon: bool) -> Rules {
    let mut r = Rules { canon, ..Default::default() };
    r.replace.insert("FxElemDef".to_string(), elem_def);
    r.replace.insert("MaterialInfo".to_string(), material_info);
    r.replace.insert("clipMap_t".to_string(), clip_map);
    r.replace.insert("GfxImage".to_string(), image);
    r
}

pub fn codol_to_iw4(input: &Input) -> Result<HopOut> {
    codol_to_iw4_with(input, false)
}

/// The conversion, with the content hash of every converted asset when `canon` (for the round-trip measurement).
pub fn codol_to_iw4_with(input: &Input, canon: bool) -> Result<HopOut> {
    debug_assert!(input.fmt == Fmt::Codol);
    let zone = Zone::from_image(input.version, input.image.clone())?;
    let sync = convert_codol::ext_sync(&zone, input.version).map_err(|e| anyhow!(e))?;
    let source = convert_codol::schema(input.version);
    let note = sync.note.clone();
    let mut out = ce::convert_zone_s(Game::Iw4, source, &input.zone_image(), sync, Game::Iw4, rules_with(canon)).map_err(|e| anyhow!(e))?;
    if let Some(n) = note {
        *out.warnings.entry(format!("block positions are not checked from there on: {n}")).or_default() += 1;
    }
    let stats = out.stats.into_iter().map(|(ty, (converted, left_out))| (ty, TypeStat { converted, left_out })).collect();
    Ok(HopOut { image: out.image, stats, nulled: out.nulled, warnings: out.warnings, assets: out.assets, canon: out.canon })
}
