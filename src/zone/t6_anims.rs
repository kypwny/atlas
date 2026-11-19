//! Black Ops II animations (`XAnimParts`) read out of a zone with the schema walker's asset hook.
//!
//! The data arrays have the layout of the other Infinity Ward / Treyarch games of the family (bones grouped by how their
//! rotation is stored, keyed or constant 16-bit quaternions, keyed small or full translations scaled into a box, frame numbers
//! as bytes below 256 frames and as 16-bit values above, a delta part for root motion), so the stored arrays are handed to the
//! decoder `anim::RawAnim::decode` that CoDOL's animations use, after being put in the header layout it expects. A decode
//! that does not consume its arrays exactly fails, so a wrong assumption shows up as an error rather than as bad keys.
//! Layouts: OpenAssetTools' Black Ops II `XAnimParts`, `XAnimDeltaPart` and `XAnimPartTrans` (GPL-3.0, independent code).

use super::schema_walk::{Game, View, WalkReport, ZoneImage, walk_with};
use crate::anim::{Animation, RawAnim, RawDelta};
use anyhow::{Context, Result};

/// One animation of a zone: the decoded tracks, or why they could not be decoded.
#[derive(Debug)]
pub struct T6Anim {
    pub name: String,
    pub anim: std::result::Result<Animation, String>,
    pub bone_count: usize,
    /// The animation's data lives in a streamed file next to the zone (the zone holds only the header).
    pub streamed: bool,
}

fn raw_of(v: &View, hdr: u32, strings: &[String]) -> Option<(RawAnim, bool)> {
    let m = |n: &str| v.member("XAnimParts", n);
    let u16f = |n: &str| v.u16(hdr + m(n)?);
    let name = v.cstr(v.ptr(hdr + m("name")?)?)?;
    let num_frames = u16f("numframes")?;
    let looped = v.u8(hdr + m("bLoop")?)? != 0;
    let bone_counts: Vec<u8> = (0..10).map(|i| v.u8(hdr + m("boneCount")? + i)).collect::<Option<_>>()?;
    let notify_count = v.u8(hdr + m("notifyCount")?)? as u32;
    let asset_type = v.u8(hdr + m("assetType")?)?;
    let framerate = v.f32(hdr + m("framerate")?)?;
    let streamed = v.u32(hdr + m("streamedFileSize")?)? != 0;
    // the header in the layout RawAnim::decode reads
    let mut header = vec![0u8; 88];
    header[14..16].copy_from_slice(&num_frames.to_le_bytes());
    header[16] = looped as u8;
    header[17..27].copy_from_slice(&bone_counts);
    header[28] = asset_type;
    header[40..44].copy_from_slice(&framerate.to_le_bytes());
    let total = bone_counts[9] as u32;

    let array = |ptr: &str, count: u32, width: u32| -> Option<Vec<u8>> {
        let p = v.ptr(hdr + m(ptr)?)?;
        if p == 0 || count == 0 {
            return Some(Vec::new());
        }
        Some(v.bytes(p, (count * width) as usize)?.to_vec())
    };
    let le16 = |b: Vec<u8>| -> Vec<i16> { b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect() };
    let width = if num_frames < 256 { 1 } else { 2 };
    let index_count = v.u32(hdr + m("indexCount")?)?;
    let idx_ptr = v.ptr(hdr + m("indices")?)?;
    let indices: Vec<u16> = if idx_ptr == 0 || index_count == 0 {
        Vec::new()
    } else {
        let b = v.bytes(idx_ptr, (index_count * width) as usize)?;
        if width == 1 { b.iter().map(|&x| x as u16).collect() } else { b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect() }
    };

    // bone names: script string indices
    let names_ptr = v.ptr(hdr + m("names")?)?;
    let mut bone_names = Vec::new();
    for i in 0..total {
        let s = if names_ptr == 0 { 0 } else { v.u16(names_ptr + 2 * i)? as usize };
        bone_names.push(strings.get(s).cloned().unwrap_or_default());
    }
    let mut notifies = Vec::new();
    let notify_ptr = v.ptr(hdr + m("notify")?)?;
    if notify_ptr != 0 {
        for i in 0..notify_count {
            let at = notify_ptr + 8 * i;
            notifies.push((strings.get(v.u16(at)? as usize).cloned().unwrap_or_default(), v.f32(at + 4)?));
        }
    }

    let mut raw = RawAnim {
        name,
        header,
        bone_names,
        notifies,
        data_byte: array("dataByte", u16f("dataByteCount")? as u32, 1)?,
        data_short: le16(array("dataShort", u16f("dataShortCount")? as u32, 2)?),
        data_int: array("dataInt", u16f("dataIntCount")? as u32, 4)?.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
        random_short: le16(array("randomDataShort", v.u32(hdr + m("randomDataShortCount")?)?, 2)?),
        random_byte: array("randomDataByte", u16f("randomDataByteCount")? as u32, 1)?,
        indices,
        delta: [None, None, None],
    };

    // delta (root motion): translation, 2D rotation, 3D rotation
    let delta_ptr = v.ptr(hdr + m("deltaPart")?)?;
    if delta_ptr != 0 {
        let dm = |n: &str| v.member("XAnimDeltaPart", n);
        let slots = [v.ptr(delta_ptr + dm("trans")?)?, v.ptr(delta_ptr + dm("quat2")?)?, v.ptr(delta_ptr + dm("quat")?)?];
        let index_bytes = |at: u32, n: usize| -> Option<Vec<u8>> { Some(v.bytes(at, n * width as usize)?.to_vec()) };
        for (i, &t) in slots.iter().enumerate() {
            if t == 0 {
                continue;
            }
            let header = v.bytes(t, 4)?.to_vec();
            let size = u16::from_le_bytes([header[0], header[1]]) as usize;
            let u = t + v.member(["XAnimPartTrans", "XAnimDeltaPartQuat2", "XAnimDeltaPartQuat"][i], "u")?;
            let mut d = RawDelta { header, ..Default::default() };
            if size == 0 {
                d.constant = v.bytes(u, [12, 4, 8][i])?.to_vec();
            } else {
                let count = size + 1;
                let (flen, idx_off) = if i == 0 { (28, v.member("XAnimPartTransFrames", "indices")?) } else { (4, v.member("XAnimDeltaPartQuatDataFrames", "indices")?) };
                d.frames = v.bytes(u, flen)?.to_vec();
                d.indices = index_bytes(u + idx_off, count)?;
                let frames_ptr = v.ptr(u + flen as u32 - 4)?;
                if frames_ptr != 0 {
                    let small = d.header[2] != 0;
                    let stride = match i {
                        0 => if small { 3 } else { 6 },
                        1 => 4,
                        _ => 8,
                    };
                    d.data = v.bytes(frames_ptr, count * stride)?.to_vec();
                }
            }
            raw.delta[i] = Some(d);
        }
    }
    Some((raw, streamed))
}

/// Reads every animation of a zone.
pub fn read_anims(zone: &ZoneImage) -> (Vec<T6Anim>, WalkReport) {
    let mut out: Vec<T6Anim> = Vec::new();
    let mut hook = |v: &View, ty: &str, hdr: u32, _id: u32| {
        if ty != "XAnimParts" {
            return;
        }
        let strings = v.script_strings();
        if let Some((raw, streamed)) = raw_of(v, hdr, &strings) {
            let bone_count = raw.bone_names.len();
            let anim = raw.decode().map_err(|e| format!("{e:#}"));
            out.push(T6Anim { name: raw.name, anim, bone_count, streamed });
        }
    };
    let report = walk_with(Game::T6, zone, Some(&mut hook));
    (out, report)
}

/// Opens a Black Ops II fastfile and reads its animations.
pub fn read_anims_file(path: &std::path::Path) -> Result<(Vec<T6Anim>, WalkReport)> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let zone = ZoneImage::open(Game::T6, &name, &bytes).with_context(|| format!("opening {}", path.display()))?;
    Ok(read_anims(&zone))
}
