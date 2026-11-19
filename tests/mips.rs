//! Compares generated mip chains with the ones shipped in the game files.
//! Runs only when CODOL_TEST_GAME points at a game folder.

use atlas::{bc, iwi, source};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn psnr(a: &[u8], b: &[u8], channels: usize) -> f64 {
    let (mut se, mut n) = (0.0f64, 0.0f64);
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..channels {
            let d = pa[c] as f64 - pb[c] as f64;
            se += d * d;
            n += 1.0;
        }
    }
    if se == 0.0 { 60.0 } else { 10.0 * (255.0f64 * 255.0 / (se / n)).log10() }
}

#[test]
fn generated_mips_resemble_shipped_mips() {
    let Some(root) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else {
        eprintln!("CODOL_TEST_GAME not set; skipping");
        return;
    };
    let cat = source::scan(source::discover(&root), &source::Progress::default());
    // (format name) -> per-level (sum of PSNR, count)
    let mut stats: BTreeMap<&'static str, BTreeMap<usize, (f64, usize)>> = BTreeMap::new();
    let mut taken: BTreeMap<&'static str, usize> = BTreeMap::new();
    for e in &cat.entries {
        let Some(v) = e.variants.first() else { continue };
        let Some(h) = &v.header else { continue };
        let Some(fmt) = h.format() else { continue };
        let kind = match fmt {
            iwi::Format::Dxt1 => bc::Bc::Dxt1,
            iwi::Format::Dxt3 => bc::Bc::Dxt3,
            iwi::Format::Dxt5 => bc::Bc::Dxt5,
            _ => continue,
        };
        if h.map_type() != iwi::MapType::Tex2D || !h.has_mipmaps() || h.width.min(h.height) < 64 || h.width.max(h.height) > 512 {
            continue;
        }
        let n = taken.entry(fmt.name()).or_default();
        if *n >= 60 {
            continue;
        }
        *n += 1;
        let data = cat.providers[v.provider].read(&v.key).unwrap();
        let shipped = iwi::read_raw(&data, h).unwrap();
        let mut regen = shipped.clone();
        iwi::generate_mips(&mut regen).unwrap();
        assert_eq!(regen.mips.len(), shipped.mips.len(), "{}", e.name);
        assert_eq!(regen.mips[0], shipped.mips[0], "level 0 must be untouched ({})", e.name);
        // The result must also serialise.
        iwi::write(&regen).unwrap();
        for l in 1..shipped.mips.len().min(6) {
            let (w, hh) = ((h.width >> l).max(1), (h.height >> l).max(1));
            let a = bc::decode(kind, &shipped.mips[l][0], w, hh);
            let b = bc::decode(kind, &regen.mips[l][0], w, hh);
            let ch = if fmt == iwi::Format::Dxt1 { 3 } else { 4 };
            let s = stats.entry(fmt.name()).or_default().entry(l).or_default();
            s.0 += psnr(&a, &b, ch);
            s.1 += 1;
        }
    }
    let mut worst = f64::MAX;
    for (fmt, levels) in &stats {
        let line: Vec<String> = levels.iter().map(|(l, (sum, n))| format!("L{l} {:.1} dB", sum / *n as f64)).collect();
        eprintln!("{fmt:<5} ({} images): {}", taken[fmt], line.join("  "));
        for (_, (sum, n)) in levels {
            worst = worst.min(sum / *n as f64);
        }
    }
    assert!(!stats.is_empty(), "no images were compared");
    assert!(worst > 24.0, "generated mips are too far from the shipped ones ({worst:.1} dB)");
}
