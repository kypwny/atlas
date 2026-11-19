//! SEAnim (version 1) reader and writer, the animation format of the SE-Tools family.
//!
//! Layout, as found in a reference dump of 7,405 SEAnim files:
//!
//! ```text
//! "SEAnim", u16 version = 1, u16 header size (counted from the end of the magic)
//! u8 type (0 absolute, 1 additive, 2 relative, 3 delta), u8 flags (1 looped),
//! u8 data flags (1 location, 2 rotation, 4 scale, 0x40 notes), u8 property flags (1 = doubles instead of floats), 2 reserved
//! f32 frame rate, u32 frame count, u32 bone count, u8 modifier count, 3 reserved, u32 note count
//! bone names (NUL-terminated), modifiers (bone index, u8 type), then per bone: u8 0, and for each present kind a key
//! count followed by keys: frame number, then vec3 (location, scale) or quaternion x y z w (rotation)
//! notes: frame number, name (NUL-terminated)
//! ```
//! Frame numbers and counts are sized by the frame count (u8 below 256, u16 below 65536, else u32); bone indices by the
//! bone count in the same way.

use anyhow::{Result, bail};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoneKeys {
    pub name: String,
    pub location: Vec<(u32, [f32; 3])>,
    pub rotation: Vec<(u32, [f32; 4])>,
    pub scale: Vec<(u32, [f32; 3])>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeAnim {
    pub anim_type: u8,
    pub looped: bool,
    pub frame_rate: f32,
    pub frame_count: u32,
    pub bones: Vec<BoneKeys>,
    /// (bone index, modifier type)
    pub modifiers: Vec<(u32, u8)>,
    pub notes: Vec<(u32, String)>,
}

fn width(count: usize) -> usize {
    if count < 0x100 {
        1
    } else if count < 0x1_0000 {
        2
    } else {
        4
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let Some(s) = self.b.get(self.p..self.p.checked_add(n).unwrap_or(usize::MAX)) else { bail!("SEAnim is cut off at byte {}", self.p) };
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn idx(&mut self, w: usize) -> Result<u32> {
        Ok(match w {
            1 => self.u8()? as u32,
            2 => u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as u32,
            _ => self.u32()?,
        })
    }
    fn vec<const N: usize>(&mut self) -> Result<[f32; N]> {
        let mut v = [0.0; N];
        for x in v.iter_mut() {
            *x = self.f32()?;
        }
        Ok(v)
    }
    fn cstr(&mut self) -> Result<String> {
        let rest = &self.b[self.p.min(self.b.len())..];
        let Some(n) = rest.iter().position(|&c| c == 0) else { bail!("unterminated string in SEAnim") };
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.p += n + 1;
        Ok(s)
    }
}

impl SeAnim {
    pub fn read(bytes: &[u8]) -> Result<SeAnim> {
        if bytes.len() < 36 || &bytes[..6] != b"SEAnim" {
            bail!("not an SEAnim file");
        }
        if u16::from_le_bytes([bytes[6], bytes[7]]) != 1 {
            bail!("SEAnim version is not supported");
        }
        let header = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let mut r = Rd { b: bytes, p: 10 };
        let (anim_type, flags, data, props) = (r.u8()?, r.u8()?, r.u8()?, r.u8()?);
        if props != 0 {
            bail!("SEAnim with double precision is not supported");
        }
        r.take(2)?;
        let frame_rate = r.f32()?;
        let (frame_count, bone_count) = (r.u32()? as usize, r.u32()? as usize);
        let modifier_count = r.u8()? as usize;
        r.take(3)?;
        let note_count = r.u32()? as usize;
        if bone_count > 100_000 || frame_count > 10_000_000 || note_count > 1_000_000 {
            bail!("implausible SEAnim counts");
        }
        r.p = 8 + header;
        let mut a = SeAnim { anim_type, looped: flags & 1 != 0, frame_rate, frame_count: frame_count as u32, ..Default::default() };
        for _ in 0..bone_count {
            a.bones.push(BoneKeys { name: r.cstr()?, ..Default::default() });
        }
        let (bw, fw) = (width(bone_count), width(frame_count));
        for _ in 0..modifier_count {
            let i = r.idx(bw)?;
            a.modifiers.push((i, r.u8()?));
        }
        for b in a.bones.iter_mut() {
            r.u8()?;
            if data & 1 != 0 {
                let n = r.idx(fw)? as usize;
                for _ in 0..n {
                    let f = r.idx(fw)?;
                    b.location.push((f, r.vec::<3>()?));
                }
            }
            if data & 2 != 0 {
                let n = r.idx(fw)? as usize;
                for _ in 0..n {
                    let f = r.idx(fw)?;
                    b.rotation.push((f, r.vec::<4>()?));
                }
            }
            if data & 4 != 0 {
                let n = r.idx(fw)? as usize;
                for _ in 0..n {
                    let f = r.idx(fw)?;
                    b.scale.push((f, r.vec::<3>()?));
                }
            }
        }
        if data & 0x40 != 0 {
            for _ in 0..note_count {
                let f = r.idx(fw)?;
                a.notes.push((f, r.cstr()?));
            }
        }
        Ok(a)
    }

    pub fn write(&self) -> Vec<u8> {
        let mut o: Vec<u8> = b"SEAnim".to_vec();
        o.extend(1u16.to_le_bytes());
        o.extend(28u16.to_le_bytes());
        let has = |f: fn(&BoneKeys) -> bool| self.bones.iter().any(f);
        let data = (has(|b| !b.location.is_empty()) as u8) | (has(|b| !b.rotation.is_empty()) as u8) << 1 | (has(|b| !b.scale.is_empty()) as u8) << 2 | (!self.notes.is_empty() as u8) << 6;
        // a data kind is written for every bone once any bone has it
        o.extend([self.anim_type, self.looped as u8, data, 0, 0, 0]);
        o.extend(self.frame_rate.to_le_bytes());
        o.extend(self.frame_count.to_le_bytes());
        o.extend((self.bones.len() as u32).to_le_bytes());
        o.extend([self.modifiers.len() as u8, 0, 0, 0]);
        o.extend((self.notes.len() as u32).to_le_bytes());
        for b in &self.bones {
            o.extend(b.name.as_bytes());
            o.push(0);
        }
        let (bw, fw) = (width(self.bones.len()), width(self.frame_count as usize));
        let put = |o: &mut Vec<u8>, v: u32, w: usize| match w {
            1 => o.push(v as u8),
            2 => o.extend((v as u16).to_le_bytes()),
            _ => o.extend(v.to_le_bytes()),
        };
        for &(i, t) in &self.modifiers {
            put(&mut o, i, bw);
            o.push(t);
        }
        let f = |o: &mut Vec<u8>, v: &[f32]| v.iter().for_each(|x| o.extend(x.to_le_bytes()));
        for b in &self.bones {
            o.push(0);
            if data & 1 != 0 {
                put(&mut o, b.location.len() as u32, fw);
                for (fr, v) in &b.location {
                    put(&mut o, *fr, fw);
                    f(&mut o, v);
                }
            }
            if data & 2 != 0 {
                put(&mut o, b.rotation.len() as u32, fw);
                for (fr, v) in &b.rotation {
                    put(&mut o, *fr, fw);
                    f(&mut o, v);
                }
            }
            if data & 4 != 0 {
                put(&mut o, b.scale.len() as u32, fw);
                for (fr, v) in &b.scale {
                    put(&mut o, *fr, fw);
                    f(&mut o, v);
                }
            }
        }
        for (fr, name) in &self.notes {
            put(&mut o, *fr, fw);
            o.extend(name.as_bytes());
            o.push(0);
        }
        o
    }
}

/// Converts a decoded animation to SEAnim. Locations are multiplied by `scale` (2.54 for the centimetres SE-Tools use).
/// A bone the animation drives in only one of rotation / location gets the other from the model's own pose when a model
/// with a skeleton is given (the reference dumps do the same for rotation). Root motion becomes a `tag_origin` bone. Notifies
/// become notes at `round(time * last frame)`. The type is 3 (delta) with root motion, otherwise 0 (absolute) for ordinary
/// animations and 2 (relative) for the other asset types.
pub fn from_animation(anim: &crate::anim::Animation, model: Option<&super::ExportModel>, scale: f32) -> SeAnim {
    use crate::anim::{Rotation, Translation};
    let mut out = SeAnim {
        // root motion makes it a delta animation (type 3), as in the reference dumps
        anim_type: if anim.delta.is_some() { 3 } else if anim.asset_type == 1 { 0 } else { 2 },
        looped: anim.looped,
        frame_rate: anim.frame_rate,
        frame_count: anim.num_frames as u32 + 1,
        ..Default::default()
    };
    for t in &anim.tracks_with_delta() {
        let bind = model.and_then(|m| m.bones.iter().find(|b| b.name == t.name));
        let mut b = BoneKeys { name: t.name.clone(), ..Default::default() };
        match &t.rot {
            Rotation::Keys { at, q } => b.rotation = at.iter().zip(q).map(|(&f, q)| (f as u32, *q)).collect(),
            Rotation::None => {
                if let Some(bone) = bind {
                    b.rotation.push((0, bone.local.1));
                }
            }
        }
        match &t.trans {
            Translation::Keys { at, v } => b.location = at.iter().zip(v).map(|(&f, v)| (f as u32, v.map(|c| c * scale))).collect(),
            Translation::None => {
                if let Some(bone) = bind {
                    b.location.push((0, bone.local.0.map(|c| c * scale)));
                }
            }
        }
        out.bones.push(b);
    }
    for (name, time) in &anim.notifies {
        out.notes.push(((time * anim.num_frames as f32).round().max(0.0) as u32, name.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SeAnim {
        SeAnim {
            anim_type: 0,
            looped: true,
            frame_rate: 30.0,
            frame_count: 5,
            bones: vec![
                BoneKeys { name: "a".into(), location: vec![(0, [1.0, 2.0, 3.0]), (4, [4.0, 5.0, 6.0])], rotation: vec![(0, [0.0, 0.0, 0.0, 1.0])], scale: vec![] },
                BoneKeys { name: "b".into(), location: vec![], rotation: vec![(2, [0.0, 0.0, 1.0, 0.0])], scale: vec![] },
            ],
            modifiers: vec![(1, 2)],
            notes: vec![(3, "end".into())],
        }
    }

    #[test]
    fn write_then_read_gives_the_same_animation() {
        let a = sample();
        assert_eq!(SeAnim::read(&a.write()).unwrap(), a);
    }

    #[test]
    fn rejects_other_data() {
        assert!(SeAnim::read(b"nope").is_err());
        let mut b = sample().write();
        b.truncate(b.len() - 6);
        assert!(SeAnim::read(&b).is_err());
    }
}
