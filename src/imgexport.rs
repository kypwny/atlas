//! Writing decoded images in the formats artists use (PNG, TGA, TIFF), with an optional repair of normal maps.

use crate::iwi::Level;
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ImageFormat {
    #[default]
    Png,
    Tga,
    Tiff,
}

impl ImageFormat {
    pub const ALL: [(ImageFormat, &'static str); 3] = [(ImageFormat::Png, "PNG"), (ImageFormat::Tga, "TGA"), (ImageFormat::Tiff, "TIFF")];

    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Tga => "tga",
            ImageFormat::Tiff => "tiff",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Tga => "TGA",
            ImageFormat::Tiff => "TIFF",
        }
    }

    pub fn code(self) -> &'static str {
        self.extension()
    }

    pub fn from_code(s: &str) -> Option<Self> {
        Some(match s {
            "png" => ImageFormat::Png,
            "tga" => ImageFormat::Tga,
            "tiff" | "tif" => ImageFormat::Tiff,
            _ => return None,
        })
    }

    fn image_format(self) -> image::ImageFormat {
        match self {
            ImageFormat::Png => image::ImageFormat::Png,
            ImageFormat::Tga => image::ImageFormat::Tga,
            ImageFormat::Tiff => image::ImageFormat::Tiff,
        }
    }
}

/// How normal maps are written: as decoded, or with the Z channel rebuilt from X and Y (red and green) for the convention of the
/// target program: OpenGL (Blender, Maya: green up) or DirectX (Unreal, 3ds Max: green down).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NormalFix {
    #[default]
    None,
    OpenGl,
    DirectX,
}

impl NormalFix {
    pub const ALL: [(NormalFix, &'static str); 3] = [(NormalFix::None, "Leave as stored"), (NormalFix::OpenGl, "OpenGL"), (NormalFix::DirectX, "DirectX")];

    pub fn code(self) -> &'static str {
        match self {
            NormalFix::None => "none",
            NormalFix::OpenGl => "opengl",
            NormalFix::DirectX => "directx",
        }
    }

    pub fn from_code(s: &str) -> Option<Self> {
        Some(match s {
            "none" => NormalFix::None,
            "opengl" => NormalFix::OpenGl,
            "directx" => NormalFix::DirectX,
            _ => return None,
        })
    }
}

/// Whether an RGBA8 normal map is stored the DXT5nm way: X in alpha, Y in green, and red and blue holding the same coarser copy
/// of X (the Infinity Ward games' normal maps are; red equals blue in every pixel).
fn is_swizzled(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|p| p[0] == p[2])
}

/// Rebuilds Z from X and Y of an RGBA8 normal map and writes the result as red, green, blue (alpha opaque): X comes from alpha
/// for a DXT5nm-style map and from red otherwise, Y from green. DirectX also flips green.
pub fn fix_normals(rgba: &mut [u8], fix: NormalFix) {
    if fix == NormalFix::None {
        return;
    }
    let swizzled = is_swizzled(rgba);
    for p in rgba.chunks_exact_mut(4) {
        let x = p[if swizzled { 3 } else { 0 }] as f32 / 127.5 - 1.0;
        let mut y = p[1] as f32 / 127.5 - 1.0;
        let z = (1.0 - x * x - y * y).max(0.0).sqrt();
        if fix == NormalFix::DirectX {
            y = -y;
        }
        p[0] = ((x * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
        p[1] = ((y * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
        p[2] = ((z * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
        p[3] = 255;
    }
}

/// Writes one face of a decoded level. `normal` is the repair to apply when the image is a normal map.
pub fn write_face(path: &Path, level: &Level, face: usize, format: ImageFormat, is_normal: bool, fix: NormalFix) -> Result<()> {
    let mut data = level.faces.get(face).context("no such face")?.clone();
    if is_normal {
        fix_normals(&mut data, fix);
    }
    image::save_buffer_with_format(path, &data, level.width, level.height, image::ColorType::Rgba8, format.image_format()).with_context(|| path.display().to_string())
}

/// Whether an image counts as a normal map: the header flag, or the naming the games use.
pub fn looks_like_normal_map(name: &str, flagged: bool) -> bool {
    let n = name.to_ascii_lowercase();
    flagged || ["_n", "_nml", "_n2", "_n3", "_normal"].iter().any(|s| n.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_normal_stays_flat_and_directx_flips_green() {
        // (128, 128) is the straight-on normal: Z comes out at its maximum
        let mut a = [128u8, 128, 0, 0];
        fix_normals(&mut a, NormalFix::OpenGl);
        assert_eq!((a[0], a[1], a[3]), (128, 128, 255));
        assert!(a[2] > 250);
        // a tilted one: green 200 becomes 55 or so for DirectX
        let mut b = [128u8, 200, 0, 255];
        fix_normals(&mut b, NormalFix::DirectX);
        assert!((b[1] as i32 - 55).abs() <= 2, "{}", b[1]);
        let mut c = [128u8, 200, 9, 7];
        fix_normals(&mut c, NormalFix::None);
        assert_eq!(c, [128, 200, 9, 7]);
    }

    #[test]
    fn a_dxt5nm_map_takes_x_from_alpha() {
        // red == blue everywhere: X is the alpha, the coarse red is replaced
        let mut a = [100u8, 128, 100, 200];
        fix_normals(&mut a, NormalFix::OpenGl);
        assert_eq!(a[0], 200);
        assert_eq!(a[3], 255);
    }

    #[test]
    fn formats_write_files_the_image_crate_can_read_back() {
        let level = Level { width: 2, height: 2, faces: vec![vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 255]] };
        let dir = std::env::temp_dir().join(format!("atlas-imgexport-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (f, _) in ImageFormat::ALL {
            let p = dir.join(format!("t.{}", f.extension()));
            write_face(&p, &level, 0, f, false, NormalFix::None).unwrap();
            let back = image::open(&p).unwrap().to_rgba8();
            assert_eq!(back.get_pixel(1, 0).0, [0, 255, 0, 255], "{f:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normal_maps_are_found_by_flag_or_name() {
        assert!(looks_like_normal_map("x_nml", false));
        assert!(looks_like_normal_map("whatever", true));
        assert!(!looks_like_normal_map("x_col", false));
    }
}
