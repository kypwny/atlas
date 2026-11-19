//! A small in-memory log that long tasks write to and the Log window shows. Lines also go to stderr.

use std::sync::Mutex;
use std::time::Instant;

const KEEP: usize = 2000;

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Adds a line, prefixed with the seconds since the program started.
pub fn log(message: impl AsRef<str>) {
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let line = format!("[{t:7.1}s] {}", message.as_ref());
    eprintln!("{line}");
    let mut l = LINES.lock().unwrap();
    l.push(line);
    if l.len() > KEEP {
        let drop = l.len() - KEEP;
        l.drain(..drop);
    }
}

/// How many lines there are (to notice new ones cheaply).
pub fn count() -> usize {
    LINES.lock().unwrap().len()
}

pub fn lines() -> Vec<String> {
    LINES.lock().unwrap().clone()
}

pub fn clear() {
    LINES.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_kept_in_order_and_capped() {
        clear();
        for i in 0..(KEEP + 50) {
            log(format!("line {i}"));
        }
        let l = lines();
        assert_eq!(l.len(), KEEP);
        assert!(l.first().unwrap().ends_with("line 50") && l.last().unwrap().ends_with(&format!("line {}", KEEP + 49)));
        clear();
        assert_eq!(count(), 0);
    }
}
