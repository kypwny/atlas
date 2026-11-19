//! IPAK reading against real Black Ops II archives. Needs `CODOL_TEST_BO2` (the game folder, holding
//! `zone/all/*.ipak`); skipped otherwise.

use atlas::ipak::Ipak;
use atlas::iwi::{self, Header};
use std::path::PathBuf;

#[test]
fn every_entry_is_a_valid_version_27_image_of_exactly_the_size_its_header_implies() {
    let Some(root) = std::env::var_os("CODOL_TEST_BO2").map(PathBuf::from) else { return };
    let dir = root.join("zone").join("all");
    for name in ["mp_nuketown_2020.ipak", "patch_zm.ipak", "dlc0_load_mp.ipak"] {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let ipak = Ipak::open(&path).unwrap();
        assert!(!ipak.entries.is_empty());
        assert!(ipak.entries.windows(2).all(|w| w[0].name_hash <= w[1].name_hash), "{name}: index is sorted by name hash");
        for e in &ipak.entries {
            let data = ipak.read(e).unwrap_or_else(|err| panic!("{name} {:08x}: {err:#}", e.name_hash));
            let h = Header::parse(&data).unwrap();
            assert_eq!(h.version, 27);
            let fmt = h.format().unwrap_or_else(|| panic!("{name} {:08x}: format code {}", e.name_hash, h.format));
            assert_eq!(iwi::expected_size(&h), Some(data.len()), "{name} {:08x} ({})", e.name_hash, fmt.name());
            iwi::decode_level(&data, &h, 0).unwrap();
            // the header-only path used by the browser's scan agrees
            let head = ipak.read_prefix(e, 64).unwrap();
            assert_eq!(&head[..64], &data[..64]);
        }
    }
}
