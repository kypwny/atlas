//! Modern Warfare 3 (IW5) <-> stock Modern Warfare 2 (IW4), the two hops of the hub-and-spoke conversion that involve IW5.
//!
//! The zones on disk are the 64-bit build, so IW5 structures have 8-byte pointers; the engine (`convert_engine`) converts members
//! by name and the rules here cover what differs.

use super::convert_cli::{engine_hop, HopOut, Input};
use super::convert_engine::{Conv, Rules, R, S, T};
use super::schema_walk::Game;
use anyhow::Result;

/// Assets whose content is specific to the game (their fields have no counterpart that keeps their meaning).
fn game_specific(r: &mut Rules) {
    for (name, why) in [
        ("WeaponCompleteDef", "weapons are game specific"),
        ("WeaponAttachment", "weapons are game specific"),
        ("VehicleDef", "vehicles are game specific"),
        ("menuDef_t", "menus are game specific"),
        ("MenuList", "menus are game specific"),
        ("LeaderboardDef", "leaderboards are game specific"),
        ("StructuredDataDefSet", "structured data is game specific"),
    ] {
        r.skip.insert(name.to_string(), why.to_string());
    }
}

/// MssSound in IW4: an `AILSOUNDINFO` header and the data; in IW5 the fields of a wave format header (plus two unknown words).
fn mss_to_iw5(c: &mut Conv, s: S, t: T) -> R {
    let Some(info) = c.ssub(s, "info") else { return Err("MssSound without its info".to_string()) };
    let (rate, bits, channels) = (c.si(info, "rate"), c.si(info, "bits"), c.si(info, "channels"));
    let len = c.si(info, "data_len");
    c.t_set_i(t, "format", c.si(info, "format"));
    c.t_set_i(t, "channels", channels);
    c.t_set_i(t, "sampleRate", rate);
    c.t_set_i(t, "byteRate", rate * channels * bits / 8);
    c.t_set_i(t, "blockAlign", c.si(info, "block_size"));
    c.t_set_i(t, "bits", bits);
    c.t_set_i(t, "data_len", len);
    let sp = c.sp(s, "data");
    c.t_copy_array(s, t, "data", sp, len.max(0) as usize)
}

fn mss_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    let Some(info) = c.t_sub(t, "info") else { return Err("MssSound without its info".to_string()) };
    let (rate, bits, channels, len) = (c.si(s, "sampleRate"), c.si(s, "bits"), c.si(s, "channels"), c.si(s, "data_len"));
    c.t_set_i(info, "format", c.si(s, "format"));
    c.t_set_i(info, "channels", channels);
    c.t_set_i(info, "rate", rate);
    c.t_set_i(info, "bits", bits);
    c.t_set_i(info, "block_size", c.si(s, "blockAlign"));
    c.t_set_i(info, "data_len", len);
    let width = (channels * bits / 8).max(1);
    c.t_set_i(info, "samples", len / width);
    let sp = c.sp(s, "data");
    c.t_copy_array(s, t, "data", sp, len.max(0) as usize)
}

/// IW4 channel maps keep six speaker entries of two levels; IW5 a counted list with one level each.
fn channel_map_to_iw5(c: &mut Conv, s: S, t: T) -> R {
    let n = c.si(s, "speakerCount").clamp(0, 6) as usize;
    c.t_set_i(t, "speakerCount", n as i64);
    c.t_alloc(t, "speakers", n);
    for i in 0..n {
        let (Some(se), Some(te)) = (c.s_elem(s, "speakers", i), c.t_elem(t, "speakers", i)) else { break };
        c.t_set_i(te, "speaker", c.si(se, "speaker"));
        let level = c.sf(se, "levels");
        c.t_set_f(te, "level", level);
    }
    Ok(())
}

fn channel_map_to_iw4(c: &mut Conv, s: S, t: T) -> R {
    let n = c.si(s, "speakerCount").clamp(0, 6) as usize;
    c.t_set_i(t, "speakerCount", n as i64);
    for i in 0..n {
        let (Some(se), Some(te)) = (c.s_elem(s, "speakers", i), c.t_elem(t, "speakers", i)) else { break };
        c.t_set_i(te, "speaker", c.si(se, "speaker"));
        c.t_set_i(te, "numLevels", 1);
        let level = c.sf(se, "level");
        c.t_set_f(te, "levels", level);
    }
    Ok(())
}

pub fn rules_to_iw4() -> Rules {
    let mut r = Rules::default();
    r.replace.insert("MssSound".to_string(), mss_to_iw4);
    r.replace.insert("MSSChannelMap".to_string(), channel_map_to_iw4);
    game_specific(&mut r);
    r.type_map.insert("CLIPMAP".to_string(), "CLIPMAP_MP".to_string());
    r
}

pub fn rules_from_iw4() -> Rules {
    let mut r = Rules::default();
    r.replace.insert("MssSound".to_string(), mss_to_iw5);
    r.replace.insert("MSSChannelMap".to_string(), channel_map_to_iw5);
    game_specific(&mut r);
    r
}

pub fn iw5_to_iw4(input: &Input) -> Result<HopOut> {
    engine_hop(Game::Iw5, Game::Iw4, rules_to_iw4(), input)
}

pub fn iw4_to_iw5(input: &Input) -> Result<HopOut> {
    engine_hop(Game::Iw4, Game::Iw5, rules_from_iw4(), input)
}
