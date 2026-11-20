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

/// The rules of the conversion: what differs in value (not only in layout) between the two games.
pub fn rules() -> Rules {
    let mut r = Rules::default();
    r.replace.insert("FxElemDef".to_string(), elem_def);
    r.replace.insert("MaterialInfo".to_string(), material_info);
    r
}

pub fn codol_to_iw4(input: &Input) -> Result<HopOut> {
    debug_assert!(input.fmt == Fmt::Codol);
    let zone = Zone::from_image(input.version, input.image.clone())?;
    let sync = convert_codol::ext_sync(&zone, input.version).map_err(|e| anyhow!(e))?;
    let source = convert_codol::schema(input.version);
    let out = ce::convert_zone_s(Game::Iw4, source, &input.zone_image(), sync, Game::Iw4, rules()).map_err(|e| anyhow!(e))?;
    let stats = out.stats.into_iter().map(|(ty, (converted, left_out))| (ty, TypeStat { converted, left_out })).collect();
    Ok(HopOut { image: out.image, stats, nulled: out.nulled, warnings: out.warnings, assets: out.assets })
}
