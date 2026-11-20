//! Loaded sounds of real zones. Each game needs its folder in an environment variable and is skipped without it:
//! - `CODOL_TEST_IW3`: Call of Duty 4 (the folder that has `zone\english`)
//! - `CODOL_TEST_IW5`: Modern Warfare 3 (the folder that has `zone\english`)

use atlas::sound;
use std::path::PathBuf;

fn zones(var: &str) -> Vec<PathBuf> {
    let Some(root) = std::env::var_os(var).map(PathBuf::from) else { return Vec::new() };
    let mut out: Vec<PathBuf> = std::fs::read_dir(root.join("zone").join("english")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "ff")).collect();
    out.sort();
    out
}

fn check(var: &str) {
    let mut seen = 0;
    for z in zones(var) {
        let Ok(list) = sound::read_zone(&z) else { continue };
        for s in list.iter().take(40) {
            assert!(!s.name.is_empty() && s.rate >= 8000 && s.rate <= 96000 && (1..=8).contains(&s.channels), "{}: {} {:?}", z.display(), s.name, (s.rate, s.channels, s.format));
            let wav = sound::to_wav(s);
            if let Some(w) = wav {
                assert_eq!(&w[..4], b"RIFF");
                assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, w.len() - 8);
                assert!(s.pcm16().is_some_and(|p| !p.is_empty()), "{}: {} does not decode", z.display(), s.name);
            }
            seen += 1;
        }
        if seen > 200 {
            break;
        }
    }
    if std::env::var_os(var).is_some() {
        assert!(seen > 0, "no sounds found under {var}");
    }
}

#[test]
fn modern_warfare_3_sounds_read_and_decode() {
    check("CODOL_TEST_IW5");
}

#[test]
fn call_of_duty_4_sounds_read_and_decode() {
    check("CODOL_TEST_IW3");
}

#[test]
fn other_games_are_refused_with_a_reason() {
    let dir = std::env::temp_dir().join(format!("atlas-snd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.ff");
    std::fs::write(&p, b"TAff0100\x93\0\0\0aaaaaaaaaaaaaaaa").unwrap();
    let e = sound::read_zone(&p).unwrap_err().to_string();
    assert!(e.contains("sabs") || e.contains("sounds are read"), "{e}");
    std::fs::remove_dir_all(&dir).ok();
}
