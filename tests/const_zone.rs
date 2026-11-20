//! `MAP_const.ff` of Call of Duty Online against the stock zones. Needs `CODOL_TEST_QA` (a QA 3.3.3.3 game folder, read only) and
//! does nothing without it. Every stock `*_const.ff` is read, laid out again from its own raw files (the image must be byte
//! identical), checked against the rule (the baselines of the zone's row in `mp/constBaselinesTable.csv`, in order, then the marker; three zones keep baselines for a mode the table does not list),
//! and the marker-only zone is compared with the stock zones. A report per zone is printed with `--nocapture`.

use atlas::zone::const_zone::{check, image, marker_only, parse, table_row, zone_of};
use atlas::zone::convert_cli::{verify, Fmt};
use atlas::zone::export::{find_key, zone_files};
use atlas::zone::fastfile::{self, ArchiveKey};
use atlas::zone::replay::{DataRec, Replay};
use atlas::zone::Zone;
use std::path::{Path, PathBuf};

fn folder() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("CODOL_TEST_QA")?);
    p.join("client_shipRetail/chinese").is_dir().then_some(p)
}

/// The text of the stock table, from whichever common zone holds it.
fn table(root: &Path, key: &ArchiveKey) -> String {
    for p in zone_files(root) {
        let Ok(z) = Zone::open(&p, key) else { continue };
        let mut r = Replay::new(&z).ok().unwrap();
        for i in 0..z.assets.len() {
            if r.replay_asset(&z, i).is_err() {
                break;
            }
        }
        for d in &r.rec.data {
            if let DataRec::StringTable { name: Some(n), columns, rows, cells } = d {
                if n.eq_ignore_ascii_case("mp/constBaselinesTable.csv") {
                    return (0..*rows).map(|y| (0..*columns).map(|x| cells[y * columns + x].clone().unwrap_or_default()).collect::<Vec<_>>().join(",") + "\n").collect();
                }
            }
        }
    }
    panic!("mp/constBaselinesTable.csv not found in the common zones");
}

#[test]
fn stock_const_zones_follow_the_rule_and_lay_out_byte_identical() {
    let Some(root) = folder() else { return };
    let key = find_key(&root).unwrap();
    let csv = table(&root, &key);
    assert!(table_row(&csv, "mp_afghan").is_some());
    let mut consts: Vec<PathBuf> = zone_files(&root).into_iter().filter(|p| p.file_stem().unwrap().to_string_lossy().ends_with("_const")).collect();
    consts.sort();
    assert!(consts.len() >= 60, "{} const zones", consts.len());
    let (mut identical, mut marker_only_same, mut deviations) = (0, 0, 0);
    for p in &consts {
        let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
        let zone = zone_of(&stem).unwrap();
        let o = fastfile::open(&p, &key).unwrap();
        assert_eq!(o.version, fastfile::VERSION_QA);
        let files = parse(&o.image).unwrap_or_else(|e| panic!("{stem}: {e}"));
        let rule = check(zone, &files, Some(&csv));
        let rebuilt = image(&files) == o.image;
        let none = files.len() == 1;
        let generated = marker_only(zone) == o.image;
        let row = table_row(&csv, zone);
        println!(
            "{stem}: {} raw files, row {}, rule {}, image from its own files {}, marker-only zone {}",
            files.len(),
            match &row { Some(r) => format!("{} modes", r.len()), None => "none".into() },
            rule.as_ref().map(|_| "ok".to_string()).unwrap_or_else(|e| e.clone()),
            if rebuilt { "identical" } else { "DIFFERS" },
            if generated { "identical" } else if none { "differs (marker text)" } else { "differs (no baselines)" },
        );
        assert!(rebuilt, "{stem}: image differs");
        // a zone whose row was not built keeps the builder's log in its marker; every other zone follows the rule
        if rule.is_err() { deviations += 1; }
        verify(Fmt::Codol, &stem, &image(&files)).unwrap();
        identical += rebuilt as usize;
        marker_only_same += generated as usize;
    }
    println!("{} const zones: {identical} laid out byte identical from their raw files, {marker_only_same} identical to the generated marker-only zone", consts.len());
    assert_eq!(identical, consts.len());
    // baselines of zones that have no row in the table (never read by the game): coop_gun_mp_suburbs, mp_bridge, za_coliseum
    assert!(deviations <= 3, "{deviations} zones break the table rule");
    assert!(marker_only_same >= 1, "mp_con_spring_const is the marker alone");
}

#[test]
fn a_sealed_const_zone_reopens_and_the_stock_seal_is_compared() {
    let Some(root) = folder() else { return };
    let key = find_key(&root).unwrap();
    let p = root.join("client_shipRetail/chinese/mp_con_spring_const.ff");
    let raw = std::fs::read(&p).unwrap();
    let o = fastfile::open_bytes(&raw, &key).unwrap();
    // the generated image is the stock image; sealing it with the stock container gives a file that opens to it again
    assert_eq!(marker_only("mp_con_spring"), o.image);
    let sealed = fastfile::seal(&o.image, o.version, &o.container).unwrap();
    assert_eq!(fastfile::open_bytes(&sealed, &key).unwrap().image, o.image);
    println!("mp_con_spring_const.ff: stock {} bytes, sealed again {} bytes, {}", raw.len(), sealed.len(), if sealed == raw { "byte identical" } else { "bytes differ (compression)" });
}
