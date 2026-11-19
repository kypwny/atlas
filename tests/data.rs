//! The data tables: string tables, localised strings and raw files collected from every zone of a game folder. Needs
//! `CODOL_TEST_GAME` (a QA build), `CODOL_TEST_319` (the 3.19 install) or `CODOL_TEST_MW2` (the x86 stock folder).

use std::path::PathBuf;

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

#[test]
fn qa_zones_give_tables_text_and_files() {
    let Some(root) = env("CODOL_TEST_GAME") else { return };
    let d = atlas::data::collect(&root, None).unwrap();
    assert_eq!((d.zones_read, d.zones_failed), (238, 0));
    assert!(d.tables.len() > 400, "{} tables", d.tables.len());
    assert!(d.localize.len() > 20_000);
    // the Chinese builds' text is decoded, not mangled
    let (text, _) = &d.localize["AC130_CIVILIAN_FIRE"];
    assert!(text.contains('平'), "{text}");
    assert!(d.rawfiles.len() > 1000);
    let out = std::env::temp_dir().join("atlas_data_test");
    let _ = std::fs::remove_dir_all(&out);
    d.write(&out).unwrap();
    assert!(out.join("index.json").is_file() && out.join("localize.csv").is_file());
    assert!(out.join("stringtables").is_dir() && out.join("rawfiles").is_dir());
    let _ = std::fs::remove_dir_all(out);
}

#[test]
fn the_319_install_has_the_weapon_tables() {
    let Some(root) = env("CODOL_TEST_319") else { return };
    let d = atlas::data::collect(&root, None).unwrap();
    assert!(d.tables.len() > 1000, "{} tables", d.tables.len());
    let classes = &d.tables["mp/classtable.csv"];
    assert!(classes.columns > 10 && classes.rows > 5);
    assert_eq!(classes.cell(0, 0), "loadoutName");
}

#[test]
fn stock_mw2_zones_give_data_where_the_walk_reaches() {
    let Some(root) = env("CODOL_TEST_MW2") else { return };
    let dir = root.join("zone/iw4x/x86/english");
    if !dir.is_dir() {
        return;
    }
    let d = atlas::data::collect(&dir, None).unwrap();
    assert_eq!(d.zones_read, 96);
    assert!(d.tables.len() > 100 && d.localize.len() > 5000 && d.rawfiles.len() > 300);
    assert_eq!(d.localize["AC130_CIVILIAN_FIRE"].0, "You harmed a civilian! Mission failed.");
}

fn column(csv: &str, weapon: &str, name: &str) -> String {
    let mut lines = csv.lines().filter(|l| !l.starts_with('#'));
    let header: Vec<&str> = lines.next().unwrap().split(',').collect();
    let col = header.iter().position(|h| *h == name).unwrap_or_else(|| panic!("no column {name}"));
    let line = lines.find(|l| l.starts_with(&format!("\"{weapon}\","))).unwrap_or_else(|| panic!("no weapon {weapon}"));
    line.split(',').nth(col).unwrap().to_string()
}

#[test]
fn weapon_tables_carry_the_known_stats() {
    // 3.19 and QA: ak47_mp has a 30 round magazine, 96 ms fire time, 2400 ms reload (the values the client's timing table agrees on)
    for (var, tag) in [("CODOL_TEST_319", "3.19"), ("CODOL_TEST_GAME", "QA")] {
        let Some(root) = env(var) else { continue };
        let d = atlas::data::collect(&root, None).unwrap();
        let out = std::env::temp_dir().join(format!("atlas_weapons_{tag}"));
        let _ = std::fs::remove_dir_all(&out);
        d.write(&out).unwrap();
        let csv = std::fs::read_to_string(out.join("weapons_summary.csv")).unwrap();
        assert_eq!(column(&csv, "ak47_mp", "iClipSize"), "30", "{tag}");
        assert_eq!(column(&csv, "ak47_mp", "iFireTime"), "96", "{tag}");
        assert_eq!(column(&csv, "ak47_mp", "iReloadTime"), "2400", "{tag}");
        assert_eq!(column(&csv, "ak47_mp", "display_text"), "\"AK-47\"", "{tag}");
        let _ = std::fs::remove_dir_all(out);
    }
    // stock MW2: the same gun is 85 ms and 2500 ms
    if let Some(root) = env("CODOL_TEST_MW2") {
        let dir = root.join("zone/iw4x/x86/english");
        if dir.is_dir() {
            let d = atlas::data::collect(&dir, None).unwrap();
            let out = std::env::temp_dir().join("atlas_weapons_mw2");
            let _ = std::fs::remove_dir_all(&out);
            d.write(&out).unwrap();
            let csv = std::fs::read_to_string(out.join("weapons_summary.csv")).unwrap();
            assert_eq!(column(&csv, "ak47_mp", "iFireTime"), "85");
            assert_eq!(column(&csv, "ak47_mp", "iReloadTime"), "2500");
            let _ = std::fs::remove_dir_all(out);
        }
    }
}
