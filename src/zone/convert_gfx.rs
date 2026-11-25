//! Phase 2 of the stock-to-QA zone conversion (`convert_zone`): the render assets. Materials, technique sets, images, pixel and
//! vertex shaders, vertex declarations.
//!
//! Stock offsets are the IW4 structures' (ZoneTool `Structs.hpp`, OpenAssetTools `IW4_Assets.h`); the QA layouts were identified by
//! pairing the headers of assets that both games hold under the same name (769 materials, 204 technique sets, 856 images in
//! `common_mp`) and are described next to each rule. Fields the QA record has and the stock one lacks are noted as such and take the
//! value the QA assets hold (almost always the same one).

use super::convert_zone::{cstr_at, Hdr, Out, Rule, Src, INLINE};
use std::collections::HashMap;

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "Material" => material,
        "MaterialTechniqueSet" => technique_set,
        "GfxImage" => image,
        "MaterialPixelShader" | "MaterialVertexShader" => shader,
        "MaterialVertexDeclaration" => vertex_declaration,
        _ => return None,
    })
}

/// Images of IWD archives, by asset name: the map's own `MAP.iwd` beside the source first, then any `--iwd` file or folder (the
/// source game's stock archives, e.g. CoD4's `main\iw_*.iwd`). A CoD4 zone names streamed images with an empty load definition
/// (the client reads `images/NAME.iwi` from its IWDs). QA tolerates a missing file; 3.19 stops with WIN_FILE_NOT_FOUND. An empty
/// image whose name is here is written inline; the first archive that has a name wins. Entries are read when used.
static IWD_IMAGES: std::sync::Mutex<Option<HashMap<String, (std::path::PathBuf, usize)>>> = std::sync::Mutex::new(None);
/// 3.19 target: the streamed 2D images as loose `images/NAME.iwi` files ("IWi" version 9) instead of embedded pixels.
/// One block of a zone holds at most 256 MB (a zone pointer keeps 28 bits of offset), which a stock CoD4 map's textures
/// overflow (mp_citystreets: 278 MB). 3.19 reads an image whose load definition is empty from images/NAME.iwi
/// (sub_C20270) through its search paths; r1-mod serves usermaps\MAP\images\ there. Cube maps stay embedded (an empty
/// cube is made blank, sub_BE7470), and so does a file over the loader's 12 MB staging buffer (sub_C1F7D0, error 498).
static LOOSE_IMAGES: std::sync::Mutex<Option<Vec<(String, Vec<u8>)>>> = std::sync::Mutex::new(None);

/// Start (Some) or stop collecting loose images; `take_loose_images` hands over what was collected.
pub fn set_loose_images(on: bool) {
    *LOOSE_IMAGES.lock().unwrap() = on.then(Vec::new);
}

pub fn take_loose_images() -> Vec<(String, Vec<u8>)> {
    LOOSE_IMAGES.lock().unwrap().as_mut().map(std::mem::take).unwrap_or_default()
}

/// CoD4 iwi v6 -> 3.19 iwi v9: header 28 -> 32 bytes (flags u32 at 4, format at 8, w/h/d at 10, the four
/// file-size-for-picmip values at 16, each counting the larger header); the pixels (smallest level first) are unchanged.
fn iwi6_to_iwi9(b: &[u8]) -> Option<Vec<u8>> {
    if b.len() < 28 || &b[..3] != b"IWi" || b[3] != 6 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() + 4);
    out.extend_from_slice(b"IWi\x09");
    out.extend_from_slice(&(b[5] as u32).to_le_bytes());
    out.push(b[4]);
    out.push(0);
    out.extend_from_slice(&b[6..12]);
    for i in 0..4 {
        let v = u32::from_le_bytes(b[12 + 4 * i..16 + 4 * i].try_into().unwrap());
        out.extend_from_slice(&(v + 4).to_le_bytes());
    }
    out.extend_from_slice(&b[28..]);
    // the loader requires size[0] == the file's length
    (u32::from_le_bytes(out[16..20].try_into().unwrap()) as usize == out.len()).then_some(out)
}

/// 4x4 grey DXT1 as an iwi v9 file (one level, no picmip).
fn placeholder_iwi9() -> Vec<u8> {
    let mut out = b"IWi\x09".to_vec();
    out.extend_from_slice(&0x03u32.to_le_bytes());
    out.push(11);
    out.push(0);
    for v in [4u16, 4, 1] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for _ in 0..4 {
        out.extend_from_slice(&40u32.to_le_bytes());
    }
    out.extend_from_slice(&[0x10, 0x84, 0x10, 0x84, 0, 0, 0, 0]);
    out
}

fn iwd_raw(name: &str) -> Option<Vec<u8>> {
    let (p, i) = IWD_IMAGES.lock().unwrap().as_ref()?.get(&name.to_ascii_lowercase())?.clone();
    read_iwd_entry(&p, i)
}

/// Whether an empty 2D image found in no archive gets a 4x4 grey placeholder (3.19 target).
static PLACEHOLDERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_image_placeholders(on: bool) {
    PLACEHOLDERS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Registers every `images/*.iwi` of an IWD (zip), or of every `*.iwd` in a folder (in name order). Returns how many names were new.
pub fn add_iwd_images(path: &std::path::Path) -> Result<usize, String> {
    if path.is_dir() {
        let mut files: Vec<_> = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iwd")))
            .collect();
        files.sort();
        let mut n = 0;
        for f in files {
            n += add_iwd_images(&f)?;
        }
        return Ok(n);
    }
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut map = IWD_IMAGES.lock().unwrap();
    let m = map.get_or_insert_with(HashMap::new);
    let mut n = 0;
    for i in 0..z.len() {
        let e = z.by_index_raw(i).map_err(|e| e.to_string())?;
        let name = e.name().map_err(|e| e.to_string())?.replace('\\', "/").to_ascii_lowercase();
        let Some(stem) = name.strip_prefix("images/").and_then(|x| x.strip_suffix(".iwi")) else { continue };
        if !m.contains_key(stem) {
            m.insert(stem.to_string(), (path.to_path_buf(), i));
            n += 1;
        }
    }
    Ok(n)
}

fn read_iwd_entry(path: &std::path::Path, i: usize) -> Option<Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut e = z.by_index(i).ok()?;
    let mut b = Vec::new();
    std::io::Read::read_to_end(&mut e, &mut b).ok()?;
    Some(b)
}

/// 4x4 DXT1, one level, mid grey: stands in for a texture none of the archives has.
fn placeholder_load_def() -> (Vec<u8>, u32, u32, u32) {
    let mut out = vec![1u8, 0, 0, 0];
    out.extend_from_slice(&0x03u32.to_le_bytes());
    out.extend_from_slice(&0x3154_5844u32.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&[0x10, 0x84, 0x10, 0x84, 0, 0, 0, 0]);
    (out, 8, 4, 4)
}

/// A CoD4 `.iwi` (version 6: "IWi", version, format, flags, width, height, depth, four file sizes; mips smallest first) as an IW4
/// load definition {u8 levelCount, 3 pad, u32 flags, u32 D3D format, u32 size} + the levels largest first; returns (def, size,
/// width, height). 3.19 (sub_BE7470) reads the u32 at +4 as the flags and counts the levels from it (bit 0x02 = one level), so the
/// CoD4 zone layout {levelCount, flags, w, h, d} there makes it copy a full mip chain out of a one-level image and fault. None for a format
/// with no direct D3D equivalent (RGB24) or another version.
fn iwi6_load_def(b: &[u8]) -> Option<(Vec<u8>, u32, u32, u32)> {
    if b.len() < 28 || &b[..3] != b"IWi" || b[3] != 6 {
        return None;
    }
    let (fmt, flags) = (b[4], b[5]);
    let (w, h, d) = (u16::from_le_bytes([b[6], b[7]]) as u32, u16::from_le_bytes([b[8], b[9]]) as u32, u16::from_le_bytes([b[10], b[11]]) as u32);
    let (d3d, bpb, block) = match fmt {
        1 => (0x15u32, 4u32, false),
        3 => (0x33, 2, false),
        4 => (0x1c, 1, false),
        11 => (0x3154_5844, 8, true),
        12 => (0x3354_5844, 16, true),
        13 => (0x3554_5844, 16, true),
        _ => return None,
    };
    if d > 1 {
        return None; // volume
    }
    let faces: u32 = if flags & 0x04 != 0 { 6 } else { 1 };
    let size = |lw: u32, lh: u32| if block { lw.div_ceil(4).max(1) * lh.div_ceil(4).max(1) * bpb } else { lw.max(1) * lh.max(1) * bpb };
    // the engine counts levels from the flags (Image_CountMipmaps: IMG_FLAG_NOMIPMAPS 0x02 = one level, else the full chain)
    // (sub_BE8C30: one level, then one more per doubling while 1 << n < width or height; non-power-of-two sizes round up)
    let levels = if flags & 0x02 != 0 { 1 } else { 1 + w.max(h).max(1).next_power_of_two().trailing_zeros() };
    let level_size = |l: u32| size((w >> l).max(1), (h >> l).max(1));
    let total: u32 = (0..levels).map(level_size).sum::<u32>() * faces;
    let data = &b[28..];
    if (data.len() as u32) < total {
        return None;
    }
    // the file: smallest level first, the faces inside each level. The load definition (R_LoadImage's upload loop): face by
    // face, each face's levels largest first.
    let mut at = std::collections::HashMap::new();
    let mut o = 0u32;
    for l in (0..levels).rev() {
        for f in 0..faces {
            at.insert((f, l), o);
            o += level_size(l);
        }
    }
    let mut out = Vec::with_capacity(16 + total as usize);
    out.extend_from_slice(&[levels as u8, 0, 0, 0]);
    // iwi flag bits 0x01..0x80 (nopicmip, nomipmaps, cube, volume, streaming, legacy normals, clamp u/v) are IW4's
    out.extend_from_slice(&(flags as u32).to_le_bytes());
    out.extend_from_slice(&d3d.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    for f in 0..faces {
        for l in 0..levels {
            let o0 = at[&(f, l)] as usize;
            out.extend_from_slice(&data[o0..o0 + level_size(l) as usize]);
        }
    }
    Some((out, total, w, h))
}

fn iwd_image(name: &str) -> Option<(Vec<u8>, u32, u32, u32)> {
    let (p, i) = IWD_IMAGES.lock().unwrap().as_ref()?.get(&name.to_ascii_lowercase())?.clone();
    iwi6_load_def(&read_iwd_entry(&p, i)?)
}

fn follows(s: &Src, off: u32) -> u32 {
    if s.has(off) { INLINE } else { 0 }
}

/// Material. Stock (96 bytes): `info` {name, gameFlags, sortKey, atlas rows, atlas columns, drawSurf (8), surfaceTypeBits,
/// hashIndex}, `stateBitsEntry[48]` at +24, the four counts at +72, cameraRegion +76, the technique set +80 and the texture,
/// constant and state-bits tables +84, +88, +92.
///
/// QA (104 bytes): drawSurf (16, runtime, zero) at +0, name +16, surfaceTypeBits +20, a word at +24 whose third byte is the game
/// flags, then sortKey +28, atlas rows +29, atlas columns +30; `stateBitsEntry[48]` at +32, the counts at +80, cameraRegion +84,
/// technique set +88, tables +92, +96, +100. The QA sort-key numbering has one more key than IW4's from 30 on (stock 33 is 34, 47 is
/// 48; 0 to 29 are the same in all 1,966 pairs checked).
fn material(s: &Src, o: &mut Out) -> Result<(), String> {
    let (ntex, nconst, nbits) = (s.u8(72) as usize, s.u8(73) as usize, s.u8(74) as usize);
    // textures: 12 bytes each; a water texture (semantic 11) has a water record behind it: not converted
    let textures = s.array(84, ntex, 12);
    if s.has(84) && textures.len() != ntex * 12 {
        return Err("texture table outside the loaded memory".into());
    }
    let mut h = Hdr::new(104);
    h.u32(16, INLINE);
    h.u32(20, s.u32(16)); // surfaceTypeBits
    h.u8(26, s.u8(4)); // gameFlags
    let sort = s.u8(5);
    h.u8(28, if sort >= 30 { sort + 1 } else { sort });
    h.u8(29, s.u8(6));
    h.u8(30, s.u8(7));
    if super::convert_zone::target_319() {
        // 3.19: 49 state-bits entries (the new technique slot 46 takes the lit techniques' entry, as stock 3.19 materials do), the
        // counts at +81, the camera region a byte at +85 numbered one higher from 3 on (QA 4 -> 3.19 5 in all 1,747 shared materials)
        let e = s.bytes(24, 48);
        let mut v = e[..46].to_vec();
        v.push(e[9]);
        v.extend_from_slice(&e[46..48]);
        h.raw(32, &v);
        h.raw(81, &s.bytes(72, 4));
        let cam = s.u32(76);
        h.u8(85, (if cam >= 3 { cam + 1 } else { cam }) as u8);
    } else {
        h.raw(32, &s.bytes(24, 48)); // stateBitsEntry
        h.raw(80, &s.bytes(72, 4)); // textureCount, constantCount, stateBitsCount, stateFlags
        h.u32(84, s.u32(76)); // cameraRegion
    }
    h.u32(92, follows(s, 84));
    h.u32(96, follows(s, 88));
    h.u32(100, follows(s, 92));
    let at = o.put(&h.0);
    // name, the technique set (a reference), the texture table with the images' references, constants, state bits
    let name = s.cstr(0).unwrap_or_default();
    o.put_str(&name);
    o.reference(at + 88, s.asset(80));
    if s.has(84) {
        let t = o.put(&textures);
        let base = s.u32(84);
        for i in 0..ntex {
            if textures[12 * i + 7] == 11 {
                // a water texture: a `water_t` record follows (see `water`); its word is "data follows"
                o.set(t + 12 * i + 8, INLINE);
                continue;
            }
            let id = s.at(base + 12 * i as u32).asset(8);
            o.reference(t + 12 * i + 8, id);
        }
        for i in 0..ntex {
            if textures[12 * i + 7] == 11 {
                water(&s.at(base + 12 * i as u32), o)?;
            }
        }
    }
    if s.has(88) {
        let c = s.array(88, nconst, 32);
        if c.len() != nconst * 32 {
            return Err("constant table outside the loaded memory".into());
        }
        o.put(&c);
    }
    if s.has(92) {
        let b = s.array(92, nbits, 8);
        if b.len() != nbits * 8 {
            return Err("state bits outside the loaded memory".into());
        }
        o.put(&b);
    }
    Ok(())
}

/// Technique set. Stock (204 bytes): name, worldVertFormat/hasBeenUploaded/unused (4), remappedTechniqueSet (runtime), 48 technique
/// pointers at +12. QA (208): the same with a word inserted at +4 (zero in all 1,683 QA sets), so the format word is at +8, the
/// remapped pointer at +12 and the techniques at +16. A technique that several slots share is stored once; the other slots point at it.
fn technique_set(s: &Src, o: &mut Out) -> Result<(), String> {
    // 3.19 (212 bytes) has a 49th slot inserted at 46, in front of the debug-bumpmap pair (139 of 139 technique sets of
    // mp_crash_tropical in both builds); stock sets share one technique there, a debug view: converted sets leave it empty
    let v319 = super::convert_zone::target_319();
    let slot_at = |i: usize| 16 + 4 * if v319 && i >= 46 { i + 1 } else { i };
    let mut h = Hdr::new(if v319 { 212 } else { 208 });
    h.u32(0, follows(s, 0));
    h.raw(8, &s.bytes(4, 4));
    let slots: Vec<u32> = (0..48).map(|i| s.u32(12 + 4 * i)).collect();
    for (i, &p) in slots.iter().enumerate() {
        h.u32(slot_at(i), if p != 0 { INLINE } else { 0 });
    }
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    let mut first_at: HashMap<u32, usize> = HashMap::new();
    for (i, &p) in slots.iter().enumerate() {
        if p == 0 {
            continue;
        }
        if let Some(&target) = first_at.get(&p) {
            o.inner(slot_at(i), target);
            continue;
        }
        first_at.insert(p, o.pos());
        technique(&s.at(p), o)?;
    }
    Ok(())
}

/// IW4 code constant -> QA 3.3.3.3. QA inserts constants of its own (ssaoParms, renderSourceSize, scanlines, glow colours ...)
/// between IW4's, and its matrices start at 247: QA = 249 + 4 * group + slot from IW4's view matrix (0x4C) on, with the
/// transpose bit flipped (QA's world shader, the same byte code as CoD4's, reads `viewProjectionMatrix` from 257 where IW4
/// names TRANSPOSE_VIEW_PROJECTION_MATRIX). Read from the constant tables of QA's own shaders against their arguments
/// (mp_crash_tropical, code_post_gfx_mp; 2026-11-23). 3.19 numbers them the same (79 of 79 shared names, its own zones). Unconverted, the world's matrices were read as empty vectors and
/// nothing was drawn.
fn qa_code_const(i: u16) -> u16 {
    match i {
        0..=7 => i,
        8..=0x16 => i + 1,
        0x17..=0x2B => i + 2,
        0x2C..=0x2D => i + 3,
        0x2E..=0x35 => i + 10,
        0x36..=0x39 => i + 11,
        0x3A..=0x4B => i + 139,
        _ => {
            let (g, slot) = ((i - 0x4C) / 4, (i - 0x4C) % 4);
            249 + 4 * g + [2, 3, 0, 1][slot as usize]
        }
    }
}

/// A technique: {name, flags, passCount} then `passCount` passes of 20 bytes {vertex declaration, vertex shader, pixel shader,
/// three argument counts, custom sampler flags, arguments}; QA is the same. Literal constants (argument types 1 and 7) are
/// stored behind their arguments.
fn technique(t: &Src, o: &mut Out) -> Result<(), String> {
    let passes = t.u16(6) as usize;
    let mut head = Hdr::new(8);
    head.u32(0, follows(t, 0));
    head.u16(4, t.u16(4));
    head.u16(6, passes as u16);
    o.put(&head.0);
    let pass_base = t.hdr + 8;
    let mut records = Vec::with_capacity(20 * passes);
    for j in 0..passes {
        let p = t.at(pass_base + 20 * j as u32);
        let mut r = [0u8; 20];
        r[12..16].copy_from_slice(&p.bytes(12, 4));
        if p.has(16) {
            r[16..20].copy_from_slice(&INLINE.to_le_bytes());
        }
        records.extend_from_slice(&r);
    }
    let at = o.put(&records);
    for j in 0..passes {
        let p = t.at(pass_base + 20 * j as u32);
        for (k, off) in [0u32, 4, 8].into_iter().enumerate() {
            o.reference(at + 20 * j + 4 * k, p.asset(off));
        }
    }
    for j in 0..passes {
        let p = t.at(pass_base + 20 * j as u32);
        if !p.has(16) {
            continue;
        }
        let n = p.u8(12) as usize + p.u8(13) as usize + p.u8(14) as usize;
        let args = p.array(16, n, 8);
        if args.len() != n * 8 {
            return Err("shader arguments outside the loaded memory".into());
        }
        let base = p.u32(16);
        let mut out = args.clone();
        let mut literals: Vec<Vec<u8>> = Vec::new();
        for k in 0..n {
            let ty = u16::from_le_bytes([args[8 * k], args[8 * k + 1]]);
            if ty == 3 || ty == 5 {
                let idx = u16::from_le_bytes([args[8 * k + 4], args[8 * k + 5]]);
                out[8 * k + 4..8 * k + 6].copy_from_slice(&qa_code_const(idx).to_le_bytes());
            }
            if ty == 1 || ty == 7 {
                let lp = p.at(base + 8 * k as u32).u32(4);
                let lit = t.v.bytes(lp, 16).ok_or("literal constant outside the loaded memory")?;
                literals.push(lit.to_vec());
                out[8 * k + 4..8 * k + 8].copy_from_slice(&INLINE.to_le_bytes());
            }
        }
        o.put(&out);
        for l in literals {
            o.put(&l);
        }
    }
    if t.has(0) {
        o.put_str(&t.cstr(0).unwrap_or_default());
    }
    Ok(())
}

/// GfxImage. Stock (32 bytes): the load definition pointer, mapType/semantic/category/useSrgbReads (4 bytes), picmip (2), noPicmip,
/// track, cardMemory (8), width, height, depth, delayLoadPixels, name at +28.
///
/// QA (52): load definition +0, the four type bytes +4 (same values), +8 a word (0x00010200 for an image that may be reduced in
/// quality, 0 otherwise) and +12 the no-picmip flag, +16 four card-memory words, width and height +32, depth and delay-load +36, two
/// words +40 (2, the usual value, and 0), the name +48. The four card-memory words are the stock total for the first and last and the
/// totals without the top one and the top two mip levels for the other two (equal for single-level images and for
/// images that may not be reduced).
fn image(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(52);
    h.u32(0, follows(s, 0));
    let name = s.cstr(28).unwrap_or_default();
    let empty = !s.has(0) || s.v.u32(s.u32(0) + 12).unwrap_or(0) == 0;
    let name_s = String::from_utf8_lossy(&name).into_owned();
    // names with ',' (a reference) or '$' (made by the engine) are never files
    let fileish = !name_s.starts_with(',') && !name_s.starts_with('$');
    let map_2d = s.u8(4) == 3;
    // 3.19: a 2D image goes out as a loose file and stays streamed in the zone
    let loose_on = LOOSE_IMAGES.lock().unwrap().is_some();
    if empty && fileish && map_2d && loose_on {
        let file = match iwd_raw(&name_s) {
            Some(raw) => iwi6_to_iwi9(&raw).filter(|f| f.len() <= 0xC0_0000),
            None => PLACEHOLDERS.load(std::sync::atomic::Ordering::Relaxed).then(|| {
                if std::env::var_os("ATLAS_LIST_EMPTY_IMAGES").is_some() {
                    eprintln!("placeholder image: {name_s}");
                }
                placeholder_iwi9()
            }),
        };
        if let Some(f) = file {
            if let Some(v) = LOOSE_IMAGES.lock().unwrap().as_mut() {
                if !v.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name_s)) {
                    v.push((name_s.clone(), f));
                }
            }
            return stock_image(s, o, &name);
        }
    }
    let injected = if !empty || !fileish {
        None
    } else {
        iwd_image(&name_s).or_else(|| {
            (map_2d && PLACEHOLDERS.load(std::sync::atomic::Ordering::Relaxed)).then(|| {
                if std::env::var_os("ATLAS_LIST_EMPTY_IMAGES").is_some() {
                    eprintln!("placeholder image: {name_s}");
                }
                placeholder_load_def()
            })
        })
    };
    if empty && injected.is_none() && std::env::var_os("ATLAS_LIST_EMPTY_IMAGES").is_some() {
        eprintln!("empty image (read from disk by the game): {name_s} (map type {})", s.u8(4));
    }
    if let Some((def, total, w, hh)) = &injected {
        // pixels from the map's IWD: inline, not reducible, card memory = the data
        h.u32(0, INLINE);
        h.raw(4, &s.bytes(4, 4));
        h.u32(12, 1);
        for off in [16, 20, 24, 28] {
            h.u32(off, *total);
        }
        h.u32(32, w | hh << 16);
        h.u32(36, 1);
        h.u32(40, 2);
        h.u32(48, INLINE);
        o.put(&h.0);
        o.put_str(&name);
        o.put(def);
        return Ok(());
    }
    stock_image(s, o, &name)
}

/// The image as the source has it (its own load definition, or none for a streamed one).
fn stock_image(s: &Src, o: &mut Out, name: &[u8]) -> Result<(), String> {
    let mut h = Hdr::new(52);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 4));
    if s.u8(10) != 0 {
        h.u32(12, 1);
    } else {
        h.u32(8, 0x0001_0200);
    }
    let total = s.u32(16);
    let (def, levels, format) = if s.has(0) { (s.u32(0), s.v.u8(s.u32(0)).unwrap_or(1) as u32, s.v.u32(s.u32(0) + 8).unwrap_or(0)) } else { (0, 1, 0) };
    let _ = def;
    let (w, hgt) = (s.u16(20) as u32, s.u16(22) as u32);
    let (d1, d2) = if s.u8(10) != 0 { (total, total) } else { card_memory(total, levels, format, w, hgt) };
    h.u32(16, total);
    h.u32(20, d2);
    h.u32(24, d1);
    h.u32(28, total);
    h.u32(32, s.u32(20));
    h.u32(36, s.u32(24));
    h.u32(40, 2);
    h.u32(48, INLINE);
    o.put(&h.0);
    o.put_str(name);
    if s.has(0) {
        let def = s.u32(0);
        let head = s.v.bytes(def, 16).ok_or("image load definition outside the loaded memory")?.to_vec();
        let size = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
        let data = s.v.bytes(def + 16, size).ok_or("image data outside the loaded memory")?.to_vec();
        o.put(&head);
        o.put(&data);
    }
    Ok(())
}

/// Bytes the top mip level takes in the stock card-memory total (a guess from the format: Direct3D format codes DXT1, DXT3, DXT5
/// and the uncompressed 32-bit and 8-bit formats), used for the QA record's reduced-quality totals.
fn level_bytes(format: u32, w: u32, h: u32) -> u32 {
    let blocks = |bytes: u32| w.div_ceil(4).max(1) * h.div_ceil(4).max(1) * bytes;
    match format {
        0x3154_5844 => blocks(8),
        0x3354_5844 | 0x3554_5844 | 0x3254_5844 | 0x3454_5844 => blocks(16),
        0x15 | 0x16 | 0x14 => w.max(1) * h.max(1) * 4,
        0x32 | 0x1c | 0x29 => w.max(1) * h.max(1),
        0x33 | 0x34 => w.max(1) * h.max(1) * 2,
        _ => w.max(1) * h.max(1),
    }
}

/// (total without the top level, total without the top two levels) in the image's own card-memory units.
fn card_memory(total: u32, levels: u32, format: u32, w: u32, h: u32) -> (u32, u32) {
    // a level count of 0 means the full chain
    let levels = if levels == 0 { 32 - w.max(h).max(1).leading_zeros() } else { levels };
    if levels <= 1 {
        return (total, total);
    }
    let top = level_bytes(format, w, h);
    let second = level_bytes(format, (w / 2).max(1), (h / 2).max(1));
    (total.saturating_sub(top), total.saturating_sub(top + second))
}

/// Pixel and vertex shaders: {name, runtime pointer, program pointer, program size in words, renderer}. Identical in QA.
fn shader(s: &Src, o: &mut Out) -> Result<(), String> {
    let words = s.u16(12) as usize;
    let mut h = Hdr::new(16);
    h.u32(0, follows(s, 0));
    h.u32(8, follows(s, 8));
    h.raw(12, &s.bytes(12, 4));
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    if s.has(8) {
        let code = s.array(8, words, 4);
        if code.len() != words * 4 {
            return Err("shader program outside the loaded memory".into());
        }
        o.put(&code);
    }
    Ok(())
}

/// Vertex declaration: name, stream count, optional-source flag and the routing table (92 bytes, the last pointer is runtime).
/// Identical in QA.
fn vertex_declaration(s: &Src, o: &mut Out) -> Result<(), String> {
    let mut h = Hdr::new(100);
    h.u32(0, follows(s, 0));
    h.raw(4, &s.bytes(4, 96));
    h.u32(36, 0);
    o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    let _ = cstr_at;
    Ok(())
}

/// `water_t` behind a water texture (semantic 11): 68 bytes, the same in both games: floatTime +0, the two wave-spectrum arrays
/// (`H0X`, `H0Y` at +4 and +8: M * N records of 8 and 4 bytes; M +12, N +16), the animation values, and the water image at +64
/// (a reference). `t` is the texture record (the record's +8 word is the water pointer).
fn water(t: &Src, o: &mut Out) -> Result<(), String> {
    if !t.has(8) {
        return Ok(());
    }
    let w = t.at(t.u32(8));
    let (m, n) = (w.u32(12) as usize, w.u32(16) as usize);
    let cells = m * n;
    let mut h = Hdr::new(68);
    h.raw(0, &w.bytes(0, 68));
    h.u32(4, follows(&w, 4));
    h.u32(8, follows(&w, 8));
    h.u32(64, 0);
    let at = o.put(&h.0);
    o.reference(at + 64, w.asset(64));
    if w.has(4) {
        let a = w.array(4, cells, 8);
        if a.len() != cells * 8 {
            return Err("water spectrum outside the loaded memory".into());
        }
        o.put(&a);
    }
    if w.has(8) {
        let a = w.array(8, cells, 4);
        if a.len() != cells * 4 {
            return Err("water spectrum outside the loaded memory".into());
        }
        o.put(&a);
    }
    Ok(())
}
