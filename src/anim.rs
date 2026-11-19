//! Animations (`XAnimParts`, the IW4 layout CoDOL uses): decoding the flat data arrays into per-bone keyframe tracks and
//! sampling them per frame.
//!
//! An animation lists the bones it drives (script-string names) in groups by how their rotation is stored: none, 2-value
//! "half" quaternion (a rotation about z), full 4-value quaternion, each either keyed or a single constant; then a
//! translation entry per bone: keyed small (u8) or full (u16) offsets scaled into a box (`mins + size * value`), a constant,
//! or none. Keyed tracks carry the frame numbers they apply to; frames in between interpolate. The layout of the arrays
//! (which values come from which array, the 16-bit frame-number checkpoints) follows OpenAssetTools' `FlatXAnimReader`
//! (GPL-3.0), checked against the files.

use anyhow::{Result, bail};

pub const QUAT_SCALE: f32 = 32767.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Rotation {
    /// Not driven by the animation: the model's own pose applies.
    None,
    /// `frames[k]` (x, y, z, w) applies at frame `frames_at[k]`; one entry means constant.
    Keys { at: Vec<u16>, q: Vec<[f32; 4]> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Translation {
    None,
    Keys { at: Vec<u16>, v: Vec<[f32; 3]> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoneTrack {
    pub name: String,
    pub rot: Rotation,
    pub trans: Translation,
}

/// Root motion: the movement and turning of the animation's origin over its frames, in the same key form as a bone's tracks.
/// Locations are in the game's units; the rotation is about z only for 2D deltas and a full quaternion for 3D ones.
#[derive(Clone, Debug, PartialEq)]
pub struct DeltaTrack {
    pub rot: Rotation,
    pub trans: Translation,
}

#[derive(Clone, Debug)]
pub struct Animation {
    pub name: String,
    /// Last frame number; the animation has `num_frames + 1` frames.
    pub num_frames: u16,
    pub frame_rate: f32,
    pub looped: bool,
    /// The asset's own type byte (1 for normal animations; others are relative or delta kinds).
    pub asset_type: u8,
    pub tracks: Vec<BoneTrack>,
    /// Present when the animation carries root motion.
    pub delta: Option<DeltaTrack>,
    pub notifies: Vec<(String, f32)>,
}

/// One sub-track of the delta part (see `zone::replay::DeltaRec`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawDelta {
    pub header: Vec<u8>,
    pub constant: Vec<u8>,
    pub frames: Vec<u8>,
    pub indices: Vec<u8>,
    pub data: Vec<u8>,
}

/// The raw arrays of one animation as recorded from a zone (see `zone::replay::AnimRec`), with names resolved.
#[derive(Clone, Debug, Default)]
pub struct RawAnim {
    pub name: String,
    pub header: Vec<u8>,
    pub bone_names: Vec<String>,
    pub notifies: Vec<(String, f32)>,
    pub data_byte: Vec<u8>,
    pub data_short: Vec<i16>,
    pub data_int: Vec<i32>,
    pub random_short: Vec<i16>,
    pub random_byte: Vec<u8>,
    pub indices: Vec<u16>,
    /// Delta part: translation, 2D rotation, 3D rotation.
    pub delta: [Option<RawDelta>; 3],
}

struct Cursor<'a, T> {
    d: &'a [T],
    p: usize,
}

impl<'a, T: Copy> Cursor<'a, T> {
    fn new(d: &'a [T]) -> Self {
        Self { d, p: 0 }
    }
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [T]> {
        let Some(s) = self.d.get(self.p..self.p + n) else { bail!("animation data ran out: {what} ({n} wanted, {} left)", self.d.len() - self.p.min(self.d.len())) };
        self.p += n;
        Ok(s)
    }
    fn one(&mut self, what: &str) -> Result<T> {
        Ok(self.take(1, what)?[0])
    }
    fn left(&self) -> usize {
        self.d.len() - self.p.min(self.d.len())
    }
}

fn h16(h: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([h[o], h[o + 1]])
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

impl RawAnim {
    /// Parses an `anim` line of the animation index (`zone::export::write_anims`).
    pub fn from_line(line: &str) -> Option<RawAnim> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 11 || f[0] != "anim" {
            return None;
        }
        let le16 = |b: Vec<u8>| -> Vec<i16> { b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect() };
        Some(RawAnim {
            name: f[1].to_string(),
            header: hex(f[2]),
            bone_names: if f[3].is_empty() { Vec::new() } else { f[3].split(',').map(str::to_string).collect() },
            notifies: f[4].split(',').filter_map(|n| n.rsplit_once(':').map(|(a, b)| (a.to_string(), b.parse().unwrap_or(0.0)))).collect(),
            data_byte: hex(f[5]),
            data_short: le16(hex(f[6])),
            data_int: hex(f[7]).chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
            random_short: le16(hex(f[8])),
            random_byte: hex(f[9]),
            indices: hex(f[10]).chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect(),
            delta: [11usize, 12, 13].map(|i| {
                let p: Vec<&str> = f.get(i)?.split(':').collect();
                (p.len() == 5).then(|| RawDelta { header: hex(p[0]), constant: hex(p[1]), frames: hex(p[2]), indices: hex(p[3]), data: hex(p[4]) })
            }),
        })
    }

    /// Decodes the tracks. Fails when the arrays do not fit the layout exactly (they must be consumed completely).
    pub fn decode(&self) -> Result<Animation> {
        let h = &self.header;
        if h.len() < 88 {
            bail!("animation header is too short");
        }
        let num_frames = h16(h, 14);
        let flags = h[16];
        let counts: Vec<usize> = h[17..27].iter().map(|&c| c as usize).collect();
        let total = counts[9];
        if total != self.bone_names.len() {
            bail!("animation lists {} bone names for {total} bones", self.bone_names.len());
        }
        let use_byte_indices = num_frames < 256;
        let (mut byte, mut short, mut int) = (Cursor::new(&self.data_byte), Cursor::new(&self.data_short), Cursor::new(&self.data_int));
        let (mut rnd_byte, mut rnd_short, mut idx) = (Cursor::new(&self.random_byte), Cursor::new(&self.random_short), Cursor::new(&self.indices));

        let mut tracks: Vec<BoneTrack> = self.bone_names.iter().map(|n| BoneTrack { name: n.clone(), rot: Rotation::None, trans: Translation::None }).collect();

        // frame numbers of a keyed track; `stored` is the key count minus one
        let read_indices = |stored: u16, byte: &mut Cursor<u8>, short: &mut Cursor<i16>, idx: &mut Cursor<u16>| -> Result<Vec<u16>> {
            let count = stored as usize + 1;
            if use_byte_indices {
                return Ok(byte.take(count, "dataByte indices")?.iter().map(|&b| b as u16).collect());
            }
            if stored >= 64 {
                let v = idx.take(count, "indices")?.to_vec();
                // checkpoint values the game inserts: first entry, every 256th, and the last
                short.take((count - 2) / 256 + 2, "dataShort checkpoints")?;
                return Ok(v);
            }
            Ok(short.take(count, "dataShort indices")?.iter().map(|&s| s as u16).collect())
        };

        let mut bone = 0usize;
        // rotations, grouped by kind: none, half, full, half constant, full constant
        bone += counts[0];
        for _ in 0..counts[1] {
            let stored = short.one("half quat size")? as u16;
            let at = read_indices(stored, &mut byte, &mut short, &mut idx)?;
            let raw = rnd_short.take(at.len() * 2, "randomDataShort half quat")?;
            let q = raw.chunks_exact(2).map(|c| [0.0, 0.0, c[0] as f32 / QUAT_SCALE, c[1] as f32 / QUAT_SCALE]).collect();
            tracks[bone].rot = Rotation::Keys { at, q };
            bone += 1;
        }
        for _ in 0..counts[2] {
            let stored = short.one("full quat size")? as u16;
            let at = read_indices(stored, &mut byte, &mut short, &mut idx)?;
            let raw = rnd_short.take(at.len() * 4, "randomDataShort full quat")?;
            let q = raw.chunks_exact(4).map(|c| [c[0] as f32 / QUAT_SCALE, c[1] as f32 / QUAT_SCALE, c[2] as f32 / QUAT_SCALE, c[3] as f32 / QUAT_SCALE]).collect();
            tracks[bone].rot = Rotation::Keys { at, q };
            bone += 1;
        }
        for _ in 0..counts[3] {
            let c = short.take(2, "half quat constant")?;
            tracks[bone].rot = Rotation::Keys { at: vec![0], q: vec![[0.0, 0.0, c[0] as f32 / QUAT_SCALE, c[1] as f32 / QUAT_SCALE]] };
            bone += 1;
        }
        for _ in 0..counts[4] {
            let c = short.take(4, "full quat constant")?;
            tracks[bone].rot = Rotation::Keys { at: vec![0], q: vec![[c[0] as f32 / QUAT_SCALE, c[1] as f32 / QUAT_SCALE, c[2] as f32 / QUAT_SCALE, c[3] as f32 / QUAT_SCALE]] };
            bone += 1;
        }
        if bone != total {
            bail!("rotation groups cover {bone} bones, not {total}");
        }

        let float3 = |int: &mut Cursor<i32>, what: &str| -> Result<[f32; 3]> {
            let v = int.take(3, what)?;
            Ok([f32::from_bits(v[0] as u32), f32::from_bits(v[1] as u32), f32::from_bits(v[2] as u32)])
        };
        let mut assigned = vec![false; total];
        for kind in 5..=8usize {
            for _ in 0..counts[kind] {
                let b = byte.one("translation bone")? as usize;
                if b >= total || assigned[b] {
                    bail!("translation entry for bone {b} is out of range or repeated");
                }
                assigned[b] = true;
                match kind {
                    5 | 6 => {
                        let stored = short.one("translation size")? as u16;
                        let mins = float3(&mut int, "translation mins")?;
                        let size = float3(&mut int, "translation size")?;
                        let at = read_indices(stored, &mut byte, &mut short, &mut idx)?;
                        let mut v = Vec::with_capacity(at.len());
                        if kind == 5 {
                            for c in rnd_byte.take(at.len() * 3, "randomDataByte translation")?.chunks_exact(3) {
                                v.push([mins[0] + size[0] * c[0] as f32, mins[1] + size[1] * c[1] as f32, mins[2] + size[2] * c[2] as f32]);
                            }
                        } else {
                            for c in rnd_short.take(at.len() * 3, "randomDataShort translation")?.chunks_exact(3) {
                                v.push([mins[0] + size[0] * c[0] as u16 as f32, mins[1] + size[1] * c[1] as u16 as f32, mins[2] + size[2] * c[2] as u16 as f32]);
                            }
                        }
                        tracks[b].trans = Translation::Keys { at, v };
                    }
                    7 => {
                        let c = float3(&mut int, "constant translation")?;
                        tracks[b].trans = Translation::Keys { at: vec![0], v: vec![c] };
                    }
                    _ => {}
                }
            }
        }
        if assigned.iter().any(|a| !a) {
            bail!("some bones have no translation entry");
        }
        for (name, left) in [("dataByte", byte.left()), ("dataShort", short.left()), ("dataInt", int.left()), ("randomDataByte", rnd_byte.left()), ("randomDataShort", rnd_short.left()), ("indices", idx.left())] {
            if left != 0 {
                bail!("{left} entries of {name} are left over");
            }
        }
        Ok(Animation { name: self.name.clone(), num_frames, frame_rate: f32::from_le_bytes([h[40], h[41], h[42], h[43]]), looped: flags & 1 != 0, asset_type: h[28], tracks, delta: self.decode_delta(num_frames)?, notifies: self.notifies.clone() })
    }

    fn decode_delta(&self, num_frames: u16) -> Result<Option<DeltaTrack>> {
        let [trans, quat2, quat] = &self.delta;
        if trans.is_none() && quat2.is_none() && quat.is_none() {
            return Ok(None);
        }
        let index_width = if num_frames < 256 { 1 } else { 2 };
        let indices = |d: &RawDelta, count: usize| -> Result<Vec<u16>> {
            if d.indices.len() != count * index_width {
                bail!("delta frame numbers are {} bytes, expected {}", d.indices.len(), count * index_width);
            }
            Ok(if index_width == 1 { d.indices.iter().map(|&b| b as u16).collect() } else { d.indices.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect() })
        };
        let f32_at = |b: &[u8], o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let i16_at = |b: &[u8], o: usize| i16::from_le_bytes([b[o], b[o + 1]]) as f32 / QUAT_SCALE;
        let size_of = |d: &RawDelta| -> Result<usize> {
            if d.header.len() < 4 {
                bail!("delta header is cut off");
            }
            Ok(h16(&d.header, 0) as usize)
        };
        let mut out = DeltaTrack { rot: Rotation::None, trans: Translation::None };
        if let Some(d) = trans {
            let size = size_of(d)?;
            if size == 0 {
                if d.constant.len() < 12 {
                    bail!("delta translation constant is cut off");
                }
                out.trans = Translation::Keys { at: vec![0], v: vec![[f32_at(&d.constant, 0), f32_at(&d.constant, 4), f32_at(&d.constant, 8)]] };
            } else {
                let count = size + 1;
                let small = d.header[2] != 0;
                let stride = if small { 3 } else { 6 };
                if d.frames.len() < 24 || d.data.len() != count * stride {
                    bail!("delta translation data is {} bytes, expected {}", d.data.len(), count * stride);
                }
                let mins = [f32_at(&d.frames, 0), f32_at(&d.frames, 4), f32_at(&d.frames, 8)];
                let scale = [f32_at(&d.frames, 12), f32_at(&d.frames, 16), f32_at(&d.frames, 20)];
                let v = (0..count)
                    .map(|k| {
                        let val = |c: usize| if small { d.data[3 * k + c] as f32 } else { u16::from_le_bytes([d.data[6 * k + 2 * c], d.data[6 * k + 2 * c + 1]]) as f32 };
                        [mins[0] + scale[0] * val(0), mins[1] + scale[1] * val(1), mins[2] + scale[2] * val(2)]
                    })
                    .collect();
                out.trans = Translation::Keys { at: indices(d, count)?, v };
            }
        }
        for (d, components) in [(quat2, 2usize), (quat, 4usize)] {
            let Some(d) = d else { continue };
            let size = size_of(d)?;
            let q = |b: &[u8], k: usize| -> [f32; 4] {
                if components == 2 {
                    [0.0, 0.0, i16_at(b, 4 * k), i16_at(b, 4 * k + 2)]
                } else {
                    [i16_at(b, 8 * k), i16_at(b, 8 * k + 2), i16_at(b, 8 * k + 4), i16_at(b, 8 * k + 6)]
                }
            };
            let width = 2 * components;
            out.rot = if size == 0 {
                if d.constant.len() < width {
                    bail!("delta rotation constant is cut off");
                }
                Rotation::Keys { at: vec![0], q: vec![q(&d.constant, 0)] }
            } else {
                let count = size + 1;
                if d.data.len() != count * width {
                    bail!("delta rotation data is {} bytes, expected {}", d.data.len(), count * width);
                }
                Rotation::Keys { at: indices(d, count)?, q: (0..count).map(|k| q(&d.data, k)).collect() }
            };
        }
        Ok(Some(out))
    }
}

impl Animation {
    /// The bone tracks plus the delta (root motion) as a track for the bone `tag_origin`, the way the reference exports
    /// carry it. An animation that already drives a `tag_origin` bone keeps its own track.
    pub fn tracks_with_delta(&self) -> Vec<BoneTrack> {
        let mut tracks = self.tracks.clone();
        if let Some(d) = &self.delta {
            if !tracks.iter().any(|t| t.name == "tag_origin") {
                tracks.push(BoneTrack { name: "tag_origin".into(), rot: d.rot.clone(), trans: d.trans.clone() });
            }
        }
        tracks
    }
}

/// Key lookup: the two keys around `frame` and the blend between them. A single key is constant.
fn bracket(at: &[u16], frame: f32) -> (usize, usize, f32) {
    if at.len() <= 1 || frame <= at[0] as f32 {
        return (0, 0, 0.0);
    }
    let last = at.len() - 1;
    if frame >= at[last] as f32 {
        return (last, last, 0.0);
    }
    // keys are in increasing frame order
    let k = at.partition_point(|&a| (a as f32) <= frame) - 1;
    let (a, b) = (at[k] as f32, at[k + 1] as f32);
    (k, k + 1, if b > a { (frame - a) / (b - a) } else { 0.0 })
}

fn nlerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    // take the short way round
    let dot: f32 = (0..4).map(|i| a[i] * b[i]).sum();
    let sgn = if dot < 0.0 { -1.0 } else { 1.0 };
    let mut q = [0.0; 4];
    for i in 0..4 {
        q[i] = a[i] * (1.0 - t) + b[i] * sgn * t;
    }
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt().max(1e-12);
    q.map(|x| x / n)
}

impl BoneTrack {
    /// Rotation at `frame` (x, y, z, w), normalised; None when the animation does not drive it.
    pub fn rotation_at(&self, frame: f32) -> Option<[f32; 4]> {
        let Rotation::Keys { at, q } = &self.rot else { return None };
        let (i, j, t) = bracket(at, frame);
        let n = |v: [f32; 4]| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt().max(1e-12);
            v.map(|x| x / l)
        };
        Some(if i == j { n(q[i]) } else { nlerp(n(q[i]), n(q[j]), t) })
    }

    /// Translation at `frame`; None when the animation does not drive it.
    pub fn translation_at(&self, frame: f32) -> Option<[f32; 3]> {
        let Translation::Keys { at, v } = &self.trans else { return None };
        let (i, j, t) = bracket(at, frame);
        Some([0, 1, 2].map(|k| v[i][k] + (v[j][k] - v[i][k]) * t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(num_frames: u16, counts: [u8; 10]) -> Vec<u8> {
        let mut h = vec![0u8; 88];
        h[14..16].copy_from_slice(&num_frames.to_le_bytes());
        h[17..27].copy_from_slice(&counts);
        h[40..44].copy_from_slice(&30.0f32.to_le_bytes());
        h
    }

    #[test]
    fn decodes_constant_rotation_and_keyed_translation() {
        // two bones: bone 0 has a full constant quaternion, bone 1 a half constant; translations: bone 0 small keyed
        // (frames 0 and 4), bone 1 none
        let mut a = RawAnim { name: "t".into(), bone_names: vec!["a".into(), "b".into()], ..Default::default() };
        a.header = header(4, [0, 0, 0, 1, 1, 1, 0, 0, 1, 2]);
        // rotation constants come first (half then full), then the translation size (2 keys); bytes hold the bone
        // numbers and the keyed frame numbers
        a.data_short = vec![0, 32767, 0, 0, 0, 32767, 1];
        a.data_byte = vec![0, 0, 4, 1]; // translation bone 0, its frames 0 and 4, then the no-translation bone 1
        a.data_int = [0.0f32, 0.0, 0.0, 0.01, 0.0, 0.0].iter().map(|f| f.to_bits() as i32).collect();
        a.random_byte = vec![0, 0, 0, 100, 0, 0];
        let anim = a.decode().expect("decodes");
        assert_eq!(anim.tracks.len(), 2);
        assert_eq!(anim.tracks[0].rotation_at(0.0), Some([0.0, 0.0, 0.0, 1.0]));
        let t = anim.tracks[0].translation_at(2.0).unwrap();
        assert!((t[0] - 0.5).abs() < 1e-5, "{t:?}"); // halfway between 0 and 100 * 0.01 = 1.0
        assert!(anim.tracks[1].translation_at(2.0).is_none());
    }

    #[test]
    fn leftover_data_is_an_error() {
        let mut a = RawAnim { name: "t".into(), bone_names: vec!["a".into()], ..Default::default() };
        a.header = header(1, [1, 0, 0, 0, 0, 0, 0, 0, 1, 1]);
        a.data_byte = vec![0, 9];
        assert!(a.decode().is_err());
    }

    #[test]
    fn interpolation_brackets_the_keys() {
        assert_eq!(bracket(&[0, 10], 5.0), (0, 1, 0.5));
        assert_eq!(bracket(&[0, 10], -3.0), (0, 0, 0.0));
        assert_eq!(bracket(&[0, 10], 12.0), (1, 1, 0.0));
        assert_eq!(bracket(&[3], 9.0), (0, 0, 0.0));
        let q = nlerp([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 0.0], 0.5);
        assert!((q[2] - q[3]).abs() < 1e-6);
    }
}
