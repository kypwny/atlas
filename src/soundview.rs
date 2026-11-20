//! What the Sounds tab shows about a sound beyond its header: loudness and spectral figures, a waveform, a loudness envelope, an
//! average spectrum and a spectrogram. Everything is computed from the decoded 16-bit samples.

/// Spectrogram window length (a power of two) and how many frequency rows it gives.
const FFT: usize = 1024;
pub const SPEC_BINS: usize = FFT / 2;
/// Quietest level drawn (dB below full scale).
pub const FLOOR_DB: f32 = -90.0;
/// Number of log-spaced bars of the average spectrum.
pub const SPECTRUM_BARS: usize = 160;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub frames: usize,
    pub seconds: f32,
    /// Level of the loudest sample / the whole sound, in dB below full scale (`-inf` for silence).
    pub peak_db: f32,
    pub rms_db: f32,
    /// The spectrum's centre of mass, in Hz.
    pub centroid_hz: f32,
    pub crossings: usize,
    /// Nothing above -80 dB.
    pub silent: bool,
    /// Seconds from the first audible sample until the envelope reaches 90% of the peak.
    pub attack_s: f32,
    /// Seconds of silence (under -60 dB) before and after the sound.
    pub head_s: f32,
    pub tail_s: f32,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub stats: Stats,
    /// Largest absolute sample in each of 600 slices, 0..1.
    pub peaks: Vec<f32>,
    /// RMS level of each of 300 slices, in dB (floor at `FLOOR_DB`).
    pub envelope_db: Vec<f32>,
    /// Average level of `SPECTRUM_BARS` log-spaced bands from 20 Hz to Nyquist, in dB (floor at `FLOOR_DB`).
    pub spectrum_db: Vec<f32>,
    /// Spectrogram, `spec_cols` columns of `SPEC_BINS` values (0..255), lowest frequency first within a column.
    pub spectrogram: Vec<u8>,
    pub spec_cols: usize,
    pub rate: u32,
}

fn db(x: f32) -> f32 {
    if x <= 0.0 { f32::NEG_INFINITY } else { 20.0 * x.log10() }
}

/// In-place radix-2 FFT of `n` (a power of two) complex values.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

/// A colour for a spectrogram value (0 quiet .. 255 loud): black, blue, magenta, orange, white.
pub fn heat(v: u8) -> [u8; 3] {
    const STOPS: [(f32, [f32; 3]); 5] = [(0.0, [6.0, 6.0, 14.0]), (0.3, [40.0, 40.0, 170.0]), (0.55, [190.0, 40.0, 150.0]), (0.8, [250.0, 160.0, 40.0]), (1.0, [255.0, 250.0, 220.0])];
    let t = v as f32 / 255.0;
    let i = STOPS.iter().position(|s| s.0 >= t).unwrap_or(STOPS.len() - 1).max(1);
    let (a, b) = (STOPS[i - 1], STOPS[i]);
    let k = ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
    [0, 1, 2].map(|c| (a.1[c] + (b.1[c] - a.1[c]) * k) as u8)
}

/// Analyses interleaved 16-bit samples (`channels` per frame). `None` when there are no samples.
pub fn analyse(pcm: &[i16], channels: usize, rate: u32) -> Option<View> {
    let ch = channels.max(1);
    let frames = pcm.len() / ch;
    if frames == 0 || rate == 0 {
        return None;
    }
    let mono: Vec<f32> = pcm.chunks_exact(ch).map(|f| f.iter().map(|&s| s as f32).sum::<f32>() / ch as f32 / 32768.0).collect();

    // waveform: the loudest sample of each slice over all channels
    let slices = 600usize.min(frames);
    let peaks: Vec<f32> = (0..slices)
        .map(|b| {
            let (a, z) = (b * frames / slices, ((b + 1) * frames / slices).max(b * frames / slices + 1).min(frames));
            pcm[a * ch..z * ch].iter().map(|&s| (s as i32).abs()).max().unwrap_or(0) as f32 / 32768.0
        })
        .collect();

    // loudness
    let peak = mono.iter().fold(0f32, |m, &x| m.max(x.abs()));
    let rms = (mono.iter().map(|&x| x as f64 * x as f64).sum::<f64>() / frames as f64).sqrt() as f32;
    let crossings = mono.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
    let env_slices = 300usize.min(frames);
    let env: Vec<f32> = (0..env_slices)
        .map(|b| {
            let (a, z) = (b * frames / env_slices, ((b + 1) * frames / env_slices).max(b * frames / env_slices + 1).min(frames));
            let s = &mono[a..z];
            (s.iter().map(|&x| x * x).sum::<f32>() / s.len() as f32).sqrt()
        })
        .collect();
    let envelope_db: Vec<f32> = env.iter().map(|&e| db(e).max(FLOOR_DB)).collect();

    // times: the first and last sample above -60 dB, and when the envelope first reaches 90% of its maximum
    let gate = 10f32.powf(-60.0 / 20.0);
    let first = mono.iter().position(|x| x.abs() > gate);
    let last = mono.iter().rposition(|x| x.abs() > gate);
    let seconds = frames as f32 / rate as f32;
    let (head_s, tail_s) = match (first, last) {
        (Some(f), Some(l)) => (f as f32 / rate as f32, (frames - 1 - l) as f32 / rate as f32),
        _ => (seconds, 0.0),
    };
    let env_max = env.iter().fold(0f32, |m, &e| m.max(e));
    let attack_slice = env.iter().position(|&e| e >= 0.9 * env_max).unwrap_or(0);
    let first_slice = first.map_or(0, |f| f * env_slices / frames);
    let attack_s = attack_slice.saturating_sub(first_slice) as f32 * seconds / env_slices as f32;

    // spectrogram and the average spectrum
    let cols = 700usize.min(frames.div_ceil(256).max(1));
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT as f32).cos()).collect();
    let mut spectrogram = vec![0u8; cols * SPEC_BINS];
    let mut mean_power = vec![0f64; SPEC_BINS];
    let (mut re, mut im) = (vec![0f32; FFT], vec![0f32; FFT]);
    for c in 0..cols {
        let centre = (c * frames + frames / 2) / cols;
        let start = centre as isize - (FFT / 2) as isize;
        for i in 0..FFT {
            let p = start + i as isize;
            re[i] = if p >= 0 && (p as usize) < frames { mono[p as usize] * window[i] } else { 0.0 };
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        for k in 0..SPEC_BINS {
            // a full-scale sine reads about 0 dB: its peak bin holds amplitude * N / 4 with a Hann window
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() / (FFT as f32 / 4.0);
            mean_power[k] += (mag * mag) as f64;
            let d = db(mag).clamp(FLOOR_DB, 0.0);
            spectrogram[c * SPEC_BINS + k] = ((d - FLOOR_DB) / -FLOOR_DB * 255.0).round() as u8;
        }
    }
    for p in &mut mean_power {
        *p /= cols as f64;
    }
    let bin_hz = rate as f32 / FFT as f32;
    let total: f64 = mean_power.iter().sum();
    let centroid_hz = if total > 0.0 { (mean_power.iter().enumerate().map(|(k, p)| k as f64 * bin_hz as f64 * p).sum::<f64>() / total) as f32 } else { 0.0 };
    let nyquist = rate as f32 / 2.0;
    let (lo, hi) = (20f32.ln(), nyquist.max(40.0).ln());
    let spectrum_db: Vec<f32> = (0..SPECTRUM_BARS)
        .map(|b| {
            let f0 = (lo + (hi - lo) * b as f32 / SPECTRUM_BARS as f32).exp();
            let f1 = (lo + (hi - lo) * (b + 1) as f32 / SPECTRUM_BARS as f32).exp();
            let (b0, b1) = (f0 / bin_hz, f1 / bin_hz);
            let p = if b1 - b0 < 1.0 {
                // narrower than one FFT bin: interpolate between the neighbouring bins instead of repeating one
                let mid = (0.5 * (b0 + b1)).min((SPEC_BINS - 1) as f32);
                let (i, t) = (mid as usize, mid.fract() as f64);
                mean_power[i] * (1.0 - t) + mean_power[(i + 1).min(SPEC_BINS - 1)] * t
            } else {
                let (k0, k1) = ((b0 as usize).min(SPEC_BINS - 1), (b1.ceil() as usize).min(SPEC_BINS));
                mean_power[k0..k1.max(k0 + 1)].iter().sum::<f64>() / (k1.max(k0 + 1) - k0) as f64
            };
            db(p.sqrt() as f32).max(FLOOR_DB)
        })
        .collect();

    Some(View {
        stats: Stats {
            frames,
            seconds,
            peak_db: db(peak),
            rms_db: db(rms),
            centroid_hz,
            crossings,
            silent: peak < 10f32.powf(-80.0 / 20.0),
            attack_s,
            head_s,
            tail_s,
        },
        peaks,
        envelope_db,
        spectrum_db,
        spectrogram,
        spec_cols: cols,
        rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, rate: u32, seconds: f32, amp: f32) -> Vec<i16> {
        (0..(rate as f32 * seconds) as usize).map(|i| ((2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * amp * 32767.0) as i16).collect()
    }

    #[test]
    fn a_sine_has_the_expected_level_pitch_and_crossings() {
        let v = analyse(&sine(1000.0, 44100, 1.0, 0.5), 1, 44100).unwrap();
        assert!((v.stats.peak_db + 6.02).abs() < 0.1, "{}", v.stats.peak_db);
        assert!((v.stats.rms_db + 9.03).abs() < 0.2, "{}", v.stats.rms_db);
        assert!((v.stats.centroid_hz - 1000.0).abs() < 60.0, "{}", v.stats.centroid_hz);
        assert!((v.stats.crossings as i32 - 2000).abs() <= 3, "{}", v.stats.crossings);
        assert!(!v.stats.silent);
        // the loudest bar of the spectrum sits near 1 kHz
        let top = v.spectrum_db.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
        let f = (20f32.ln() + (22050f32.ln() - 20f32.ln()) * (top as f32 + 0.5) / SPECTRUM_BARS as f32).exp();
        assert!((f - 1000.0).abs() < 150.0, "{f}");
        // and the spectrogram's hottest row is the 1 kHz bin (1000 / 43.07 = 23)
        let col = &v.spectrogram[(v.spec_cols / 2) * SPEC_BINS..(v.spec_cols / 2 + 1) * SPEC_BINS];
        let hot = col.iter().enumerate().max_by_key(|(_, x)| **x).unwrap().0;
        assert!((hot as i32 - 23).abs() <= 1, "{hot}");
    }

    #[test]
    fn silence_and_gaps_are_measured() {
        let mut pcm = vec![0i16; 4410];
        pcm.extend(sine(440.0, 44100, 0.5, 0.8));
        pcm.extend(vec![0i16; 8820]);
        let v = analyse(&pcm, 1, 44100).unwrap();
        assert!((v.stats.head_s - 0.1).abs() < 0.01, "{}", v.stats.head_s);
        assert!((v.stats.tail_s - 0.2).abs() < 0.01, "{}", v.stats.tail_s);
        let quiet = analyse(&vec![0i16; 1000], 1, 44100).unwrap();
        assert!(quiet.stats.silent && quiet.stats.peak_db == f32::NEG_INFINITY);
        assert!(analyse(&[], 1, 44100).is_none());
    }

    #[test]
    fn stereo_is_mixed_and_the_waveform_keeps_the_loudest_channel() {
        let l = sine(500.0, 22050, 0.2, 0.9);
        let pcm: Vec<i16> = l.iter().flat_map(|&x| [x, 0]).collect();
        let v = analyse(&pcm, 2, 22050).unwrap();
        assert!(v.peaks.iter().cloned().fold(0.0, f32::max) > 0.85);
        assert_eq!(v.peaks.len(), 600.min(l.len()));
    }

    #[test]
    fn the_heat_ramp_runs_dark_to_bright() {
        let (a, b) = (heat(0), heat(255));
        assert!(a.iter().map(|&x| x as u32).sum::<u32>() < 60 && b.iter().map(|&x| x as u32).sum::<u32>() > 700);
    }
}
