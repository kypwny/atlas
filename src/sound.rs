//! Sounds: the loaded sounds of Call of Duty 4, Modern Warfare 2 and Modern Warfare 3 zones, read from the zone files alone (no
//! running game), written as WAV and decoded to PCM for playback.
//!
//! A loaded sound is a name and a Miles sound: a wave format (tag, channels, rate, bits, block size), the data length and the
//! data. CoD4 and MW2 keep it as an `AILSOUNDINFO` (the tag is a plain int), MW3 as the fields of a `WAVEFORMATEX`. The member
//! offsets come from the zone schemas, so this reads the three games with one routine. Streamed sounds (`.iwd`) and Black Ops II
//! sound banks are not read here.

use crate::zone::fastfile::{self, Family, Peek};
use crate::zone::schema_walk::{Game, View, ZoneImage, walk_with};
use anyhow::{Context, Result, bail};
use std::path::Path;

/// `WAVEFORMATEX` tags the Miles sounds use.
pub const FORMAT_PCM: u16 = 1;
pub const FORMAT_MS_ADPCM: u16 = 2;
pub const FORMAT_IMA_ADPCM: u16 = 0x11;
pub const FORMAT_MP3: u16 = 0x55;

#[derive(Clone, Debug)]
pub struct Sound {
    /// The name as the zone stores it (a path without the extension, such as `weapons/ak47/fire`).
    pub name: String,
    /// The zone file the sound was read from.
    pub zone: String,
    pub format: u16,
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    pub block_align: u16,
    pub data: Vec<u8>,
}

impl Sound {
    /// What the sound is stored as, for lists.
    pub fn format_name(&self) -> String {
        match self.format {
            FORMAT_PCM => format!("PCM {}-bit", self.bits),
            FORMAT_MS_ADPCM => "MS ADPCM".to_string(),
            FORMAT_IMA_ADPCM => "IMA ADPCM".to_string(),
            FORMAT_MP3 => "MP3".to_string(),
            other => format!("format {other:#x}"),
        }
    }

    /// Frames (a sample of every channel) in the sound, `None` for a format whose length is not known without decoding.
    pub fn frames(&self) -> Option<u64> {
        let ch = self.channels.max(1) as u64;
        match self.format {
            FORMAT_PCM => {
                let bytes = (self.bits as u64 / 8).max(1);
                Some(self.data.len() as u64 / (bytes * ch))
            }
            FORMAT_MS_ADPCM => {
                let block = self.block_align as u64;
                if block == 0 {
                    return None;
                }
                let per = ms_adpcm_samples_per_block(self.block_align, self.channels) as u64;
                let whole = self.data.len() as u64 / block;
                Some(whole * per)
            }
            FORMAT_IMA_ADPCM => {
                let block = self.block_align as u64;
                if block == 0 {
                    return None;
                }
                let per = ima_samples_per_block(self.block_align, self.channels) as u64;
                Some(self.data.len() as u64 / block * per)
            }
            _ => None,
        }
    }

    /// Length in seconds, when known.
    pub fn seconds(&self) -> Option<f32> {
        if self.rate == 0 {
            return None;
        }
        self.frames().map(|f| f as f32 / self.rate as f32)
    }

    /// The file extension and the bytes to write: a WAV when the format can be one, the bare data (`.mp3`, `.bin`) otherwise.
    pub fn export_bytes(&self) -> (&'static str, Vec<u8>) {
        match to_wav(self) {
            Some(w) => ("wav", w),
            None if self.format == FORMAT_MP3 => ("mp3", self.data.clone()),
            None => ("bin", self.data.clone()),
        }
    }

    /// Interleaved 16-bit samples for playback, `None` for formats that are not decoded here.
    pub fn pcm16(&self) -> Option<Vec<i16>> {
        let ch = self.channels.max(1) as usize;
        match self.format {
            FORMAT_PCM => match self.bits {
                16 => Some(self.data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()),
                8 => Some(self.data.iter().map(|&b| ((b as i16) - 128) << 8).collect()),
                24 => Some(self.data.chunks_exact(3).map(|b| i16::from_le_bytes([b[1], b[2]])).collect()),
                _ => None,
            },
            FORMAT_MS_ADPCM => decode_ms_adpcm(&self.data, ch, self.block_align as usize),
            FORMAT_IMA_ADPCM => decode_ima_adpcm(&self.data, ch, self.block_align as usize),
            _ => None,
        }
    }
}

/// A number the search words compare (`length`, `rate`, `channels`, `bits`, `size`, `frames`).
pub fn property(s: &Sound, name: &str) -> Option<f64> {
    match name {
        "length" => s.seconds().map(f64::from),
        "rate" => Some(s.rate as f64),
        "channels" => Some(s.channels as f64),
        "bits" => Some(s.bits as f64),
        "size" => Some(s.data.len() as f64),
        "frames" => s.frames().map(|f| f as f64),
        _ => None,
    }
}

pub const PROPERTIES: &[&str] = &["length", "rate", "channels", "bits", "size", "frames"];

// ---------------------------------------------------------------------------------------------------------------- zones

/// The game a zone belongs to, when its sounds can be read.
pub fn game_of(p: &Peek) -> Option<Game> {
    if p.big_endian {
        return None;
    }
    match (p.family, p.version) {
        (Family::Iw, 5) => Some(Game::Iw3),
        (Family::Iw, 276) => Some(Game::Iw4),
        (Family::Iw, 1) => Some(Game::Iw5),
        _ => None,
    }
}

/// Reads every loaded sound of a zone file (Call of Duty 4, Modern Warfare 2, Modern Warfare 3).
pub fn read_zone(path: &Path) -> Result<Vec<Sound>> {
    let peek = fastfile::peek(path)?;
    let Some(game) = game_of(&peek) else {
        if peek.family == Family::Treyarch {
            bail!("Black Ops II keeps its sounds in the .sabs and .sabl banks, not in the zone");
        }
        bail!("sounds are read from Call of Duty 4, Modern Warfare 2 and Modern Warfare 3 zones");
    };
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let zone = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let image = ZoneImage::open(game, &zone, &bytes)?;
    let mut out = Vec::new();
    let mut hook = |v: &View, ty: &str, hdr: u32, _id: u32| {
        if ty == "LoadedSound" {
            if let Some(mut s) = read_loaded(v, game, hdr) {
                s.zone = zone.clone();
                out.push(s);
            }
        }
    };
    walk_with(game, &image, Some(&mut hook));
    Ok(out)
}

fn read_loaded(v: &View, game: Game, hdr: u32) -> Option<Sound> {
    let name = v.ptr(hdr + v.member("LoadedSound", "name")?).and_then(|p| v.cstr(p))?;
    if name.is_empty() {
        return None;
    }
    let base = hdr + v.member("LoadedSound", "sound")?;
    let m = |n: &str| v.member("MssSound", n);
    let (format, channels, rate, bits, block_align, len);
    if game == Game::Iw5 {
        format = v.u16(base + m("format")?)?;
        channels = v.u16(base + m("channels")?)?;
        rate = v.u32(base + m("sampleRate")?)?;
        bits = v.u16(base + m("bits")?)?;
        block_align = v.u16(base + m("blockAlign")?)?;
        len = v.u32(base + m("data_len")?)? as usize;
    } else {
        let info = base + m("info")?;
        let a = |n: &str| v.member("AILSOUNDINFO", n);
        format = v.u32(info + a("format")?)? as u16;
        channels = v.u32(info + a("channels")?)? as u16;
        rate = v.u32(info + a("rate")?)?;
        bits = v.u32(info + a("bits")?)? as u16;
        block_align = v.u32(info + a("block_size")?)? as u16;
        len = v.u32(info + a("data_len")?)? as usize;
    }
    let ptr = v.ptr(base + m("data")?)?;
    if ptr == 0 || len == 0 {
        return None;
    }
    let data = v.bytes(ptr, len)?.to_vec();
    Some(Sound { name, zone: String::new(), format, channels, rate, bits, block_align, data })
}

// ------------------------------------------------------------------------------------------------------------------ WAV

fn riff(fmt: &[u8], data: &[u8], fact: Option<u32>) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64 + fmt.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(fmt);
    if fmt.len() % 2 == 1 {
        out.push(0);
    }
    if let Some(f) = fact {
        out.extend_from_slice(b"fact");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&f.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    let total = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&total.to_le_bytes());
    out
}

const MS_COEF: [(i16, i16); 7] = [(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];

fn ms_adpcm_samples_per_block(block_align: u16, channels: u16) -> u32 {
    let ch = channels.max(1) as u32;
    (block_align as u32).saturating_sub(7 * ch) * 2 / ch + 2
}

fn ima_samples_per_block(block_align: u16, channels: u16) -> u32 {
    let ch = channels.max(1) as u32;
    (block_align as u32).saturating_sub(4 * ch) * 8 / (4 * ch) + 1
}

/// A WAV file for the sound: PCM, Microsoft ADPCM and IMA ADPCM keep their data and get the matching header, so no decoding
/// loses anything. `None` for other formats.
pub fn to_wav(s: &Sound) -> Option<Vec<u8>> {
    let ch = s.channels.max(1);
    let mut fmt = Vec::new();
    let push16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let push32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    match s.format {
        FORMAT_PCM => {
            if !matches!(s.bits, 8 | 16 | 24 | 32) {
                return None;
            }
            let align = ch * s.bits / 8;
            push16(&mut fmt, 1);
            push16(&mut fmt, ch);
            push32(&mut fmt, s.rate);
            push32(&mut fmt, s.rate * align as u32);
            push16(&mut fmt, align);
            push16(&mut fmt, s.bits);
            Some(riff(&fmt, &s.data, None))
        }
        FORMAT_MS_ADPCM => {
            if s.block_align == 0 {
                return None;
            }
            let per = ms_adpcm_samples_per_block(s.block_align, ch);
            push16(&mut fmt, 2);
            push16(&mut fmt, ch);
            push32(&mut fmt, s.rate);
            push32(&mut fmt, (s.rate as u64 * s.block_align as u64 / per as u64) as u32);
            push16(&mut fmt, s.block_align);
            push16(&mut fmt, 4);
            push16(&mut fmt, 32);
            push16(&mut fmt, per as u16);
            push16(&mut fmt, 7);
            for (a, b) in MS_COEF {
                push16(&mut fmt, a as u16);
                push16(&mut fmt, b as u16);
            }
            let frames = (s.data.len() / s.block_align as usize) as u32 * per;
            Some(riff(&fmt, &s.data, Some(frames)))
        }
        FORMAT_IMA_ADPCM => {
            if s.block_align == 0 {
                return None;
            }
            let per = ima_samples_per_block(s.block_align, ch);
            push16(&mut fmt, 0x11);
            push16(&mut fmt, ch);
            push32(&mut fmt, s.rate);
            push32(&mut fmt, (s.rate as u64 * s.block_align as u64 / per as u64) as u32);
            push16(&mut fmt, s.block_align);
            push16(&mut fmt, 4);
            push16(&mut fmt, 2);
            push16(&mut fmt, per as u16);
            let frames = (s.data.len() / s.block_align as usize) as u32 * per;
            Some(riff(&fmt, &s.data, Some(frames)))
        }
        _ => None,
    }
}

// --------------------------------------------------------------------------------------------------------------- decoding

fn rd16(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}

/// Microsoft ADPCM to interleaved 16-bit samples. The last partial block is dropped.
pub fn decode_ms_adpcm(data: &[u8], channels: usize, block_align: usize) -> Option<Vec<i16>> {
    const ADAPT: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];
    if !(1..=2).contains(&channels) || block_align < 7 * channels + 1 {
        return None;
    }
    let mut out = Vec::new();
    for block in data.chunks_exact(block_align) {
        let mut pred = [0usize; 2];
        let mut delta = [0i32; 2];
        let (mut s1, mut s2) = ([0i32; 2], [0i32; 2]);
        for c in 0..channels {
            pred[c] = (block[c] as usize).min(6);
        }
        let mut o = channels;
        for c in 0..channels {
            delta[c] = rd16(block, o) as i32;
            o += 2;
        }
        for c in 0..channels {
            s1[c] = rd16(block, o) as i32;
            o += 2;
        }
        for c in 0..channels {
            s2[c] = rd16(block, o) as i32;
            o += 2;
        }
        // the two stored samples come first, oldest first
        for c in 0..channels {
            out.push(s2[c] as i16);
        }
        for c in 0..channels {
            out.push(s1[c] as i16);
        }
        let mut ch = 0usize;
        for &byte in &block[o..] {
            for nib in [byte >> 4, byte & 15] {
                let (c1, c2) = MS_COEF[pred[ch]];
                let signed = if nib >= 8 { nib as i32 - 16 } else { nib as i32 };
                let p = ((s1[ch] * c1 as i32 + s2[ch] * c2 as i32) >> 8) + signed * delta[ch];
                let p = p.clamp(-32768, 32767);
                s2[ch] = s1[ch];
                s1[ch] = p;
                delta[ch] = ((ADAPT[nib as usize] * delta[ch]) >> 8).max(16);
                out.push(p as i16);
                ch = (ch + 1) % channels;
            }
        }
    }
    Some(out)
}

/// IMA ADPCM (the WAV flavour) to interleaved 16-bit samples.
pub fn decode_ima_adpcm(data: &[u8], channels: usize, block_align: usize) -> Option<Vec<i16>> {
    const STEP: [i32; 89] = [
        7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307,
        337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
        6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
    ];
    const INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
    if channels == 0 || channels > 2 || block_align < 4 * channels + 4 * channels {
        return None;
    }
    let mut out = Vec::new();
    for block in data.chunks_exact(block_align) {
        let per = (block_align - 4 * channels) * 2 / channels;
        let mut pcm = vec![vec![0i16; per + 1]; channels];
        let mut state = [(0i32, 0i32); 2];
        for c in 0..channels {
            let o = c * 4;
            state[c] = (rd16(block, o) as i32, (block[o + 2] as i32).min(88));
            pcm[c][0] = state[c].0 as i16;
        }
        // groups of eight samples (four bytes) per channel, channels alternating
        let body = &block[4 * channels..];
        for (g, group) in body.chunks_exact(4 * channels).enumerate() {
            for c in 0..channels {
                let (mut pred, mut idx) = state[c];
                for (k, &byte) in group[c * 4..c * 4 + 4].iter().enumerate() {
                    for (j, nib) in [byte & 15, byte >> 4].into_iter().enumerate() {
                        let step = STEP[idx as usize];
                        let mut diff = step >> 3;
                        if nib & 1 != 0 {
                            diff += step >> 2;
                        }
                        if nib & 2 != 0 {
                            diff += step >> 1;
                        }
                        if nib & 4 != 0 {
                            diff += step;
                        }
                        pred = if nib & 8 != 0 { pred - diff } else { pred + diff }.clamp(-32768, 32767);
                        idx = (idx + INDEX[nib as usize]).clamp(0, 88);
                        pcm[c][1 + g * 8 + k * 2 + j] = pred as i16;
                    }
                }
                state[c] = (pred, idx);
            }
        }
        for i in 0..=per {
            for c in 0..channels {
                out.push(pcm[c][i]);
            }
        }
    }
    Some(out)
}

/// A name that is safe to use as a file path under an output folder: no drive, no `..`, forward and back slashes both split.
pub fn safe_name(name: &str) -> std::path::PathBuf {
    crate::data::safe_path(name)
}

/// Writes the sounds under `out`, one file per sound named after it (`name.wav`, folders kept). Returns how many were written.
pub fn write_all(sounds: &[&Sound], out: &Path) -> Result<usize> {
    let mut n = 0;
    for s in sounds {
        let (ext, bytes) = s.export_bytes();
        // names like `nvintro.wav` already carry an extension of the source file
        let stem = s.name.rsplit_once('.').filter(|(_, e)| matches!(e.to_ascii_lowercase().as_str(), "wav" | "mp3" | "ogg" | "flac")).map_or(s.name.as_str(), |(a, _)| a);
        let mut path = out.join(safe_name(stem));
        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "sound".into());
        path.set_file_name(format!("{file}.{ext}"));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, bytes).with_context(|| path.display().to_string())?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(frames: usize) -> Sound {
        let data: Vec<u8> = (0..frames as i16).flat_map(|i| (i * 100).to_le_bytes()).collect();
        Sound { name: "a/b".into(), zone: "z.ff".into(), format: FORMAT_PCM, channels: 1, rate: 22050, bits: 16, block_align: 2, data }
    }

    #[test]
    fn pcm_makes_a_canonical_wav() {
        let s = pcm(10);
        let w = to_wav(&s).unwrap();
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, w.len() - 8);
        assert_eq!(u16::from_le_bytes(w[20..22].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 22050);
        assert_eq!(&w[36..40], b"data");
        assert_eq!(&w[44..], &s.data[..]);
        assert_eq!(s.frames(), Some(10));
        assert_eq!(s.pcm16().unwrap()[3], 300);
    }

    #[test]
    fn ms_adpcm_blocks_hold_the_stated_number_of_samples() {
        // mono block of 256 bytes: 7 header bytes then 249 bytes of nibbles = 498 samples + the two stored ones
        assert_eq!(ms_adpcm_samples_per_block(256, 1), 500);
        let mut block = vec![0u8; 256];
        block[0] = 0; // predictor
        block[1..3].copy_from_slice(&16i16.to_le_bytes()); // delta
        block[3..5].copy_from_slice(&1000i16.to_le_bytes()); // sample 1
        block[5..7].copy_from_slice(&900i16.to_le_bytes()); // sample 2
        let out = decode_ms_adpcm(&block, 1, 256).unwrap();
        assert_eq!(out.len(), 500);
        assert_eq!(&out[..2], &[900, 1000]);
        // zero nibbles repeat the prediction (2 * s1 - s2 for the first predictor, scaled) and stay in range
        assert!(out.iter().all(|&v| v > -32768));
    }

    #[test]
    fn ms_adpcm_wav_carries_the_coefficients() {
        let s = Sound { name: "x".into(), zone: "z".into(), format: FORMAT_MS_ADPCM, channels: 2, rate: 44100, bits: 4, block_align: 512, data: vec![0; 1024] };
        let w = to_wav(&s).unwrap();
        // fmt chunk: 14 + 4 + 2 + 2 + 14*2... the extra size is 32 and the format tag 2
        assert_eq!(u16::from_le_bytes(w[20..22].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(w[36..38].try_into().unwrap()), 32);
        assert_eq!(s.frames(), Some(2 * ms_adpcm_samples_per_block(512, 2) as u64));
    }

    #[test]
    fn ima_adpcm_decodes_silence_from_a_silent_block() {
        let mut block = vec![0u8; 256];
        block[0..2].copy_from_slice(&500i16.to_le_bytes());
        let out = decode_ima_adpcm(&block, 1, 256).unwrap();
        assert_eq!(out.len(), ima_samples_per_block(256, 1) as usize);
        assert_eq!(out[0], 500);
    }

    #[test]
    fn export_falls_back_to_the_raw_data() {
        let mut s = pcm(4);
        s.format = FORMAT_MP3;
        assert_eq!(s.export_bytes().0, "mp3");
        s.format = 0x1234;
        assert_eq!(s.export_bytes().0, "bin");
    }
}
