//! Future Warfare (NX1, Xbox 360 development build) -> stock Modern Warfare 2 (IW4).
//!
//! The zone is read by the conversion engine with the NX1 schema (`schema/nx1.json`, big-endian) and its structures are rebuilt in the
//! stock layout by member name (`convert_engine`); what has no counterpart is left out. Nothing is converted back: NX1 is only a source.

use super::convert_cli::{HopOut, Input};
use super::convert_engine::{self as ce, Conv, ExtSync, Rules, Schema, R, S, T};
use super::convert_zone::TypeStat;
use super::schema_walk::Game;
use anyhow::{anyhow, Result};
use std::sync::OnceLock;

/// The NX1 read schema.
pub fn schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        let doc: serde_json::Value = serde_json::from_str(include_str!("schema/nx1.json")).expect("schema/nx1.json");
        Schema::from_value(&doc, false).expect("the NX1 schema")
    })
}

/// The stock element type of an NX1 one: NX1 has a laser and a tracer (4 and 5) that stock Modern Warfare 2 lacks, which become tails;
/// from the cloud on the numbers are two higher.
fn stock_elem_type(nx: i64) -> i64 {
    match nx {
        0..=3 => nx,
        4 | 5 => 2,
        n => n - 2,
    }
}

fn elem_def(c: &mut Conv, s: S, t: T) -> R {
    c.t_set_i(t, "elemType", stock_elem_type(c.si(s, "elemType")));
    c.generic_members_except(s, t, &["elemType"])
}

pub fn rules() -> Rules {
    let mut r = Rules::default();
    r.replace.insert("FxElemDef".to_string(), elem_def);
    r.asset_map.insert("SndAliasList".to_string(), "snd_alias_list_t".to_string());
    for (t, tm, sm) in [
        ("snd_alias_list_t", "aliasName", "p_aliasName"),
        ("snd_alias_list_t", "head", "p_head"),
        ("snd_alias_t", "aliasName", "p_aliasName"),
        ("snd_alias_t", "subtitle", "p_subtitle"),
        ("snd_alias_t", "secondaryAliasName", "p_secondaryAliasName"),
        ("snd_alias_t", "chainAliasName", "p_chainAliasName"),
        ("snd_alias_t", "mixerGroup", "p_mixerGroup"),
        ("snd_alias_t", "soundFile", "p_soundFile"),
        ("MaterialTextureDef", "semantic", "m_semantic"),
    ] {
        r.renames.insert((t.to_string(), tm.to_string()), sm.to_string());
    }
    for (name, why) in [
        ("WeaponCompleteDef", "weapons are game specific"),
        ("VehicleDef", "vehicles are game specific"),
        ("menuDef_t", "menus are game specific"),
        ("MenuList", "menus are game specific"),
        ("LeaderboardDef", "leaderboards are game specific"),
        ("StructuredDataDefSet", "structured data is game specific"),
    ] {
        r.skip.insert(name.to_string(), why.to_string());
    }
    r.type_map.insert("CLIPMAP_MP".to_string(), "CLIPMAP_MP".to_string());
    r.type_map.insert("SND_VOLUME_FALLOFF_CURVE".to_string(), "SOUND_CURVE".to_string());
    r
}

pub fn nx1_to_iw4(input: &Input) -> Result<HopOut> {
    let out = ce::convert_zone_s(Game::Iw4, schema(), &input.zone_image(), ExtSync::default(), Game::Iw4, rules()).map_err(|e| anyhow!(e))?;
    let stats = out.stats.into_iter().map(|(ty, (converted, left_out))| (ty, TypeStat { converted, left_out })).collect();
    Ok(HopOut { image: out.image, stats, nulled: out.nulled, warnings: out.warnings, assets: out.assets, canon: out.canon })
}
