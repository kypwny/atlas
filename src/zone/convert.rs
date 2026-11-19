//! Converting the fixed-size asset headers of a stock Modern Warfare 2 (IW4) zone to the QA (3.3.3.3) layout.
//!
//! The field maps were derived by aligning the headers of 4,000+ assets that both games contain under the same name
//! (`tools/data/header_maps_iw4_qa.tsv`: every 4-byte source field that has a counterpart, and where it moved). The target
//! fields that have no stock source take the value most QA headers hold there (`tools/data/header_defaults_qa.tsv`; mostly
//! zero, a few format words). Pointer words are copied as they are: a pointer into the stream has to be re-resolved by the
//! walker that emits the converted zone, not here.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Sizes of the stock and the QA header of a kind.
pub fn sizes(kind: &str) -> Option<(usize, usize)> {
    Some(match kind {
        "material" => (96, 104),
        "techset" => (204, 208),
        "image" => (32, 52),
        "surfaces" => (36, 48),
        "model" => (304, 360),
        _ => return None,
    })
}

struct Tables {
    /// kind -> (source offset, target offset)
    moves: HashMap<String, Vec<(usize, usize)>>,
    /// kind -> (target offset, value)
    defaults: HashMap<String, Vec<(usize, u32)>>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut moves: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for l in include_str!("../../tools/data/header_maps_iw4_qa.tsv").lines().skip(1) {
            let f: Vec<&str> = l.split('\t').collect();
            if let [k, a, b] = f[..] {
                moves.entry(k.to_string()).or_default().push((a.parse().unwrap(), b.parse().unwrap()));
            }
        }
        let mut defaults: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
        for l in include_str!("../../tools/data/header_defaults_qa.tsv").lines().skip(1) {
            let f: Vec<&str> = l.split('\t').collect();
            if let [k, o, v] = f[..] {
                defaults.entry(k.to_string()).or_default().push((o.parse().unwrap(), v.parse().unwrap()));
            }
        }
        Tables { moves, defaults }
    })
}

/// Converts a stock header of `kind` to the QA layout; `None` for an unknown kind or a header of the wrong size.
pub fn header_to_qa(kind: &str, stock: &[u8]) -> Option<Vec<u8>> {
    let (from, to) = sizes(kind)?;
    if stock.len() != from {
        return None;
    }
    let t = tables();
    let mut out = vec![0u8; to];
    for &(o, v) in t.defaults.get(kind).into_iter().flatten() {
        out[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    for &(a, b) in t.moves.get(kind).into_iter().flatten() {
        out[b..b + 4].copy_from_slice(&stock[a..a + 4]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_field_is_covered_once() {
        for kind in ["material", "techset", "image", "surfaces", "model"] {
            let (from, to) = sizes(kind).unwrap();
            let t = tables();
            let mut seen = vec![0u8; to / 4];
            for &(a, b) in &t.moves[kind] {
                assert!(a + 4 <= from && b + 4 <= to, "{kind} {a}->{b}");
                seen[b / 4] += 1;
            }
            for &(o, _) in t.defaults.get(kind).into_iter().flatten() {
                seen[o / 4] += 1;
            }
            // a field is either moved to or defaulted, never both; a few stay zero because no source or default is known
            assert!(seen.iter().all(|&n| n <= 1), "{kind}: a field is filled twice");
            let empty = seen.iter().filter(|&&n| n == 0).count();
            assert!(empty * 4 <= seen.len(), "{kind}: {empty} of {} target fields are left empty", seen.len());
        }
    }

    #[test]
    fn a_material_name_moves_behind_the_new_words() {
        let mut stock = vec![0u8; 96];
        stock[0..4].copy_from_slice(&u32::MAX.to_le_bytes()); // inline name pointer
        let qa = header_to_qa("material", &stock).unwrap();
        assert_eq!(qa.len(), 104);
        assert_eq!(&qa[16..20], &u32::MAX.to_le_bytes());
        assert_eq!(&qa[0..16], &[0u8; 16]);
        assert!(header_to_qa("material", &stock[..90]).is_none());
    }
}
