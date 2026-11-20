//! Call of Duty 4 (IW3) <-> stock Modern Warfare 2 (IW4), the two hops of the hub-and-spoke conversion that involve IW3.

use super::convert_cli::{engine_hop, HopOut, Input};
use super::convert_engine::Rules;
use super::schema_walk::Game;
use anyhow::Result;

/// Assets whose content is specific to the game (their fields have no counterpart that keeps their meaning).
fn game_specific(r: &mut Rules) {
    for (name, why) in [
        ("WeaponDef", "weapons are game specific"),
        ("WeaponCompleteDef", "weapons are game specific"),
        ("VehicleDef", "vehicles are game specific"),
        ("menuDef_t", "menus are game specific"),
        ("MenuList", "menus are game specific"),
        ("LeaderboardDef", "leaderboards are game specific"),
        ("StructuredDataDefSet", "structured data is game specific"),
    ] {
        r.skip.insert(name.to_string(), why.to_string());
    }
}

pub fn rules_to_iw4() -> Rules {
    let mut r = Rules::default();
    game_specific(&mut r);
    r.type_map.insert("CLIPMAP".to_string(), "CLIPMAP_SP".to_string());
    r.type_map.insert("CLIPMAP_PVS".to_string(), "CLIPMAP_MP".to_string());
    r
}

pub fn rules_from_iw4() -> Rules {
    let mut r = Rules::default();
    game_specific(&mut r);
    r.type_map.insert("CLIPMAP_SP".to_string(), "CLIPMAP".to_string());
    r.type_map.insert("CLIPMAP_MP".to_string(), "CLIPMAP_PVS".to_string());
    r
}

pub fn iw3_to_iw4(input: &Input) -> Result<HopOut> {
    engine_hop(Game::Iw3, Game::Iw4, rules_to_iw4(), input)
}

pub fn iw4_to_iw3(input: &Input) -> Result<HopOut> {
    engine_hop(Game::Iw4, Game::Iw3, rules_from_iw4(), input)
}
