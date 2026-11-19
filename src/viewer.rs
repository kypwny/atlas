//! Offscreen 3D renderer for models (wgpu, Vulkan by default). The scene is
//! drawn into a texture that the window shows as an image.
//!
//! Conventions: model space is the game's (right-handed, Z up). Clip space is
//! wgpu's (Y up, depth 0..1), which matches Direct3D 9. Colours are written
//! straight into a non-sRGB target, i.e. gamma space with no conversion, the
//! way Direct3D 9 renders without the sRGB write state.

use crate::model::Model;
use crate::modelindex::{MaterialState, SurfaceTextures};
use crate::modeltex::{MipLevel, TexImage};
use bytemuck::{Pod, Zeroable};
use eframe::egui::{self, TextureId};
use eframe::egui_wgpu::{self, RenderState, wgpu};
use glam::{Mat4, Vec3, Vec4};
use eframe::egui::mutex::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    pos: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [u8; 4],
    tint: [u8; 4],
    /// [alpha test mode (0 none, 1 >0, 2 <128, 3 >=128), premultiplied alpha, 0, 0]
    flags: [u8; 4],
    /// tangent xyz and the bitangent sign
    tangent: [f32; 4],
    /// envMapParms (Fresnel minimum, maximum, exponent) of the surface's material
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    light: [f32; 4],
    eye: [f32; 4],
    mode: u32,
    /// bit 0: use the normal map, bit 1: use the specular map
    features: u32,
    _pad: [u32; 2],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shading {
    Lit,
    Normals,
    UvChecker,
    UvColor,
    VertexColor,
    Surfaces,
    Textured,
}

impl Shading {
    pub const ALL: [(Shading, &'static str); 7] = [
        (Shading::Textured, "Textured"),
        (Shading::Lit, "Lit"),
        (Shading::Normals, "Normals"),
        (Shading::UvChecker, "UV checker"),
        (Shading::UvColor, "UV colours"),
        (Shading::VertexColor, "Vertex colour"),
        (Shading::Surfaces, "Surfaces"),
    ];

    fn code(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Cull {
    /// Use each material's own setting (textured shading), back-face culling otherwise.
    Material,
    None,
    Back,
    Front,
}

impl Cull {
    pub const ALL: [(Cull, &'static str); 4] = [(Cull::Material, "Cull: material"), (Cull::None, "No culling"), (Cull::Back, "Cull back"), (Cull::Front, "Cull front")];
}

/// What kind of light a marker stands for (they are drawn differently and can be switched on and off by kind).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LightClass {
    /// The sun: an arrow from the marker's position towards the sun, ending in a disc.
    Sun,
    Omni,
    Spot,
    /// A hero-only light (lights the player and weapon models).
    Hero,
}

impl LightClass {
    pub const ALL: [LightClass; 4] = [LightClass::Sun, LightClass::Omni, LightClass::Spot, LightClass::Hero];

    pub fn bit(self) -> u8 {
        1 << self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            LightClass::Sun => "sun",
            LightClass::Omni => "omni",
            LightClass::Spot => "spot",
            LightClass::Hero => "hero",
        }
    }
}

/// A light of the map as the viewer draws it: a marker in the light's colour (a diamond; a square for hero lights), a line along
/// a spot's axis, and the sun as an arrow. All in model space.
#[derive(Clone, Debug)]
pub struct LightMarker {
    pub pos: [f32; 3],
    /// Linear colour; only its hue and relative brightness are used (the largest channel is scaled to 1).
    pub color: [f32; 3],
    /// A spot's axis, or the way from `pos` to the sun.
    pub dir: [f32; 3],
    /// Reach in model units; for the sun the length of the arrow.
    pub radius: f32,
    pub class: LightClass,
    /// Half the opening angle of a spot's cone, in radians.
    pub half_angle: f32,
}

/// Which lights are drawn.
#[derive(Clone, Copy, Debug)]
pub struct LightView {
    pub show: bool,
    /// Bit `LightClass::bit` for each kind drawn.
    pub mask: u8,
    /// A light (index into the markers) to draw with its reach (rings, and a spot's cone) and an outline.
    pub highlight: Option<usize>,
}

impl Default for LightView {
    fn default() -> Self {
        LightView { show: false, mask: 0xF, highlight: None }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightVertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub target: Vec3,
    pub radius: f32,
}

const FOV_Y: f32 = 50.0 * std::f32::consts::PI / 180.0;

impl Default for Camera {
    fn default() -> Self {
        Self { yaw: -0.7, pitch: 0.35, dist: 5.0, target: Vec3::ZERO, radius: 1.0 }
    }
}

impl Camera {
    pub fn fit(&mut self, lo: [f32; 3], hi: [f32; 3]) {
        let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
        self.target = (lo + hi) * 0.5;
        self.radius = ((hi - lo).length() * 0.5).max(0.01);
        self.dist = self.radius / (FOV_Y * 0.5).tan() * 1.15;
        self.yaw = -0.7;
        self.pitch = 0.35;
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-1.5, 1.5);
    }

    pub fn zoom(&mut self, scroll: f32) {
        self.dist = (self.dist * (-scroll * 0.0025).exp()).clamp(self.radius * 0.05, self.radius * 60.0);
    }

    fn axes(&self) -> (Vec3, Vec3, Vec3) {
        let (sy, cy, sp, cp) = (self.yaw.sin(), self.yaw.cos(), self.pitch.sin(), self.pitch.cos());
        let dir = Vec3::new(cp * cy, cp * sy, sp); // from target towards the eye
        let right = Vec3::Z.cross(dir).normalize_or_zero();
        let right = if right == Vec3::ZERO { Vec3::Y } else { right };
        let up = dir.cross(right).normalize();
        (dir, right, up)
    }

    pub fn pan(&mut self, dx: f32, dy: f32, viewport_h: f32) {
        let (_, right, up) = self.axes();
        let k = 2.0 * self.dist * (FOV_Y * 0.5).tan() / viewport_h.max(1.0);
        self.target += (-right * dx + up * dy) * k;
    }

    /// Where a model-space point lands in a viewport of `size` pixels (origin top left); None behind the camera.
    pub fn project(&self, p: [f32; 3], size: (u32, u32)) -> Option<[f32; 2]> {
        let (vp, _, _) = self.matrices(size.0.max(1) as f32 / size.1.max(1) as f32);
        let c = vp * Vec4::new(p[0], p[1], p[2], 1.0);
        if c.w <= 1e-6 {
            return None;
        }
        let (x, y) = (c.x / c.w, c.y / c.w);
        Some([(x * 0.5 + 0.5) * size.0 as f32, (0.5 - y * 0.5) * size.1 as f32])
    }

    fn matrices(&self, aspect: f32) -> (Mat4, Vec3, Vec3) {
        let (dir, right, up) = self.axes();
        let eye = self.target + dir * self.dist;
        let view = glam::camera::rh::view::look_at_mat4(eye, self.target, up);
        let near = (self.dist * 0.01).max(self.radius * 0.002);
        // DirectX-style clip space (depth 0..1, Y up): the convention Direct3D 9 uses.
        let proj = glam::camera::rh::proj::directx::perspective(FOV_Y, aspect, near, (self.dist + self.radius) * 4.0);
        // Key light from above-left of the viewer, so shapes read the same from any angle.
        let light = (dir + right * -0.5 + up * 0.8).normalize();
        (proj * view, eye, light)
    }
}

struct SurfaceDraw {
    textures: [Option<String>; 3],
    state: Option<MaterialState>,
    first_index: u32,
    index_count: u32,
    first_edge: u32,
    edge_count: u32,
}

struct Scene {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    edges: wgpu::Buffer,
    draws: Vec<SurfaceDraw>,
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    id: Option<TextureId>,
    size: (u32, u32),
}

pub struct Viewer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Option<Arc<RwLock<egui_wgpu::Renderer>>>,
    layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    textures: HashMap<String, GpuTex>,
    groups: HashMap<[Option<String>; 3], wgpu::BindGroup>,
    samplers: [wgpu::Sampler; 4],
    white: GpuTex,
    flat_normal: GpuTex,
    black: GpuTex,
    features: u32,
    module: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<PipeKey, wgpu::RenderPipeline>,
    lines: wgpu::RenderPipeline,
    /// Light markers: filled shapes and lines, each drawn once without the depth test (faint) and once with it.
    light_pipes: [wgpu::RenderPipeline; 4],
    lights: Vec<LightMarker>,
    light_view: LightView,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: Option<Target>,
    scene: Option<Scene>,
}

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

const SHADER: &str = r#"
struct U {
    view_proj: mat4x4<f32>,
    light: vec4<f32>,
    eye: vec4<f32>,
    mode: u32,
    features: u32,
    pad1: u32,
    pad2: u32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var ntex: texture_2d<f32>;
@group(1) @binding(2) var stex: texture_2d<f32>;
@group(1) @binding(3) var samp: sampler;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) tint: vec4<f32>,
    @location(5) flags: vec4<u32>,
    @location(6) tangent: vec4<f32>,
    @location(7) params: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) tint: vec4<f32>,
    @location(4) world: vec3<f32>,
    @location(5) @interpolate(flat) flags: vec4<u32>,
    @location(6) tangent: vec4<f32>,
    @location(7) @interpolate(flat) params: vec4<f32>,
};

@vertex
fn vs_main(i: VIn) -> VOut {
    var o: VOut;
    o.clip = u.view_proj * vec4<f32>(i.pos, 1.0);
    o.normal = i.normal;
    o.uv = i.uv;
    o.color = i.color;
    o.tint = i.tint;
    o.world = i.pos;
    o.flags = i.flags;
    o.tangent = i.tangent;
    o.params = i.params;
    return o;
}

@fragment
fn fs_main(i: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    var n = normalize(i.normal);
    if (!front) { n = -n; }
    let view = normalize(u.eye.xyz - i.world);
    let lambert = max(dot(n, normalize(u.light.xyz)), 0.0);
    let rim = pow(1.0 - max(dot(n, view), 0.0), 3.0);
    let light = 0.22 + 0.68 * lambert + 0.10 * rim;
    var rgb = vec3<f32>(0.74, 0.76, 0.80) * light;
    var alpha = 1.0;
    switch u.mode {
        case 1u: { rgb = n * 0.5 + vec3<f32>(0.5); }
        case 2u: {
            let c = floor(i.uv * 8.0);
            let k = abs((c.x + c.y) % 2.0);
            rgb = mix(vec3<f32>(0.22), vec3<f32>(0.85), k) * (0.5 + 0.5 * light);
        }
        case 3u: { rgb = vec3<f32>(fract(i.uv), 0.2); }
        case 4u: { rgb = i.color.rgb * light; }
        case 5u: { rgb = i.tint.rgb * light; }
        case 6u: {
            let c = textureSample(tex, samp, i.uv);
            // Direct3D 9 alpha test: greater than 0, less than 128 or greater or equal 128 (of 255).
            if (i.flags.x == 1u && c.a <= 0.0) { discard; }
            if (i.flags.x == 2u && c.a >= 0.5019608) { discard; }
            if (i.flags.x == 3u && c.a < 0.5019608) { discard; }
            // The game's lit pixel shader (lp_sun_*_r0c0n0s0), read from its bytecode: the normal map stores
            // slopes (x in alpha, y in green, scaled by 4.08 / 4.06452 and offset), the shaded normal is
            // normalize(N + x*T + y*B) with B = cross(N, T) * sign and no rebuilt z.
            var nn = n;
            if ((u.features & 1u) != 0u) {
                let nm = textureSample(ntex, samp, i.uv);
                let slope = vec2<f32>(nm.a * 4.08 - 2.08, nm.g * 4.06452 - 2.06452);
                let bn = cross(normalize(i.normal), i.tangent.xyz) * i.tangent.w;
                nn = normalize(n + slope.x * i.tangent.xyz + slope.y * bn);
            }
            // Colour and specular maps are stored with gamma 2.0; the shader squares them (linear light).
            let base = c.rgb * i.color.rgb;
            let albedo = base * base;
            let l_dir = normalize(u.light.xyz);
            let v_dir = normalize(i.world - u.eye.xyz);
            let ndl = clamp(dot(nn, l_dir), 0.0, 1.0);
            var lit = albedo * (0.30 + 0.85 * ndl);
            if ((u.features & 2u) != 0u) {
                // Specular map: colour in rgb, gloss in alpha; cosine power 2^(9.37752 * gloss). The sun lobe is
                // the game's exp((R.L - 0.99925) * (power + 7)); Fresnel uses the material's envMapParms.
                let sp = textureSample(stex, samp, i.uv);
                let spec_color = sp.rgb * sp.rgb;
                let power = exp2(9.37752 * sp.a);
                let refl = v_dir - 2.0 * dot(nn, v_dir) * nn;
                let lobe = clamp(exp((dot(refl, l_dir) - 0.99925) * (power + 7.0)), 0.0, 1.0);
                let fresnel = mix(i.params.x, i.params.y, pow(clamp(dot(v_dir, nn) + 1.0, 0.0, 1.0), i.params.z));
                // A soft sky/ground gradient stands in for the game's reflection probes.
                let sky = mix(0.08, 0.55, clamp(refl.z * 0.5 + 0.5, 0.0, 1.0));
                lit = lit + spec_color * fresnel * (lobe * 1.2 + sky * (1.0 - 0.5 * sp.a) * 0.6);
            }
            rgb = sqrt(max(lit, vec3<f32>(0.0, 0.0, 0.0)));
            // Lit transparent materials expect premultiplied colour (blend one / inverse source alpha).
            if (i.flags.y == 1u) { rgb = rgb * c.a; }
            alpha = c.a;
        }
        default: {}
    }
    return vec4<f32>(rgb, alpha);
}

@fragment
fn fs_line(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(0.02, 0.02, 0.03, 1.0);
}

struct LIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};
struct LOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_light(i: LIn) -> LOut {
    var o: LOut;
    o.clip = u.view_proj * vec4<f32>(i.pos, 1.0);
    o.color = i.color;
    return o;
}

@fragment
fn fs_light(i: LOut) -> @location(0) vec4<f32> {
    return i.color;
}

@fragment
fn fs_light_ghost(i: LOut) -> @location(0) vec4<f32> {
    return vec4<f32>(i.color.rgb, i.color.a * 0.55);
}
"#;

const PALETTE: [[u8; 3]; 8] =
    [[235, 110, 100], [110, 180, 235], [140, 215, 130], [235, 200, 100], [190, 140, 230], [100, 215, 200], [235, 150, 190], [170, 170, 170]];

/// Everything that selects a pipeline: the Direct3D-style blend state, cull mode and depth state.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct PipeKey {
    /// (source factor, destination factor, operation) in the engine's numbering; None = blending off
    blend: Option<(u8, u8, u8)>,
    /// 0 none, 1 back, 2 front
    cull: u8,
    depth_write: bool,
    offset: bool,
}

fn blend_factor(v: u8) -> wgpu::BlendFactor {
    match v {
        1 => wgpu::BlendFactor::Zero,
        2 => wgpu::BlendFactor::One,
        3 => wgpu::BlendFactor::Src,
        4 => wgpu::BlendFactor::OneMinusSrc,
        5 => wgpu::BlendFactor::SrcAlpha,
        6 => wgpu::BlendFactor::OneMinusSrcAlpha,
        7 => wgpu::BlendFactor::DstAlpha,
        8 => wgpu::BlendFactor::OneMinusDstAlpha,
        9 => wgpu::BlendFactor::Dst,
        10 => wgpu::BlendFactor::OneMinusDst,
        _ => wgpu::BlendFactor::One,
    }
}

fn blend_operation(v: u8) -> wgpu::BlendOperation {
    match v {
        2 => wgpu::BlendOperation::Subtract,
        3 => wgpu::BlendOperation::ReverseSubtract,
        4 => wgpu::BlendOperation::Min,
        5 => wgpu::BlendOperation::Max,
        _ => wgpu::BlendOperation::Add,
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    key: &PipeKey,
    topology: wgpu::PrimitiveTopology,
    fragment: &str,
) -> wgpu::RenderPipeline {
    let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Unorm8x4, 4 => Unorm8x4, 5 => Uint8x4, 6 => Float32x4, 7 => Float32x4];
    let blend = key.blend.map(|(src, dst, op)| {
        let c = wgpu::BlendComponent { src_factor: blend_factor(src), dst_factor: blend_factor(dst), operation: blend_operation(op) };
        wgpu::BlendState { color: c, alpha: c }
    });
    // Opaque geometry is pushed back a little so the wireframe overlay wins; decals (polygon offset) are pulled forward.
    let bias = if topology == wgpu::PrimitiveTopology::LineList {
        wgpu::DepthBiasState::default()
    } else if key.offset {
        wgpu::DepthBiasState { constant: -2, slope_scale: -1.0, clamp: 0.0 }
    } else {
        wgpu::DepthBiasState { constant: 2, slope_scale: 1.0, clamp: 0.0 }
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("model_pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<GpuVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes })],
        },
        primitive: wgpu::PrimitiveState {
            topology,
            strip_index_format: None,
            // The files store triangles clockwise as seen from the front.
            front_face: wgpu::FrontFace::Cw,
            cull_mode: match key.cull {
                1 => Some(wgpu::Face::Back),
                2 => Some(wgpu::Face::Front),
                _ => None,
            },
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(key.depth_write),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias,
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            // Alpha is never written: the image shown in the window must stay opaque.
            targets: &[Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend, write_mask: wgpu::ColorWrites::COLOR })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn build_light_pipeline(device: &wgpu::Device, module: &wgpu::ShaderModule, layout: &wgpu::PipelineLayout, topology: wgpu::PrimitiveTopology, ghost: bool) -> wgpu::RenderPipeline {
    let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
    let blend = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("light_pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_light"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<LightVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes })],
        },
        primitive: wgpu::PrimitiveState { topology, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(if ghost { wgpu::CompareFunction::Always } else { wgpu::CompareFunction::LessEqual }),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(if ghost { "fs_light_ghost" } else { "fs_light" }),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend: Some(wgpu::BlendState { color: blend, alpha: blend }), write_mask: wgpu::ColorWrites::COLOR })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn push_line(lines: &mut Vec<LightVertex>, a: Vec3, b: Vec3, color: [f32; 4]) {
    lines.push(LightVertex { pos: a.to_array(), color });
    lines.push(LightVertex { pos: b.to_array(), color });
}

/// A circle of radius `r` around `centre` in the plane spanned by the unit vectors `u` and `v`, as 32 line segments.
fn push_ring(lines: &mut Vec<LightVertex>, centre: Vec3, u: Vec3, v: Vec3, r: f32, color: [f32; 4]) {
    const N: usize = 32;
    let at = |k: usize| {
        let a = k as f32 / N as f32 * std::f32::consts::TAU;
        centre + (u * a.cos() + v * a.sin()) * r
    };
    for k in 0..N {
        push_line(lines, at(k), at(k + 1), color);
    }
}

/// The triangles and lines that draw `lights` for `camera` in a viewport of `size` pixels. Markers keep their size on screen.
fn light_geometry(lights: &[LightMarker], view: &LightView, camera: &Camera, size: (u32, u32)) -> (Vec<LightVertex>, Vec<LightVertex>) {
    let (mut tris, mut lines): (Vec<LightVertex>, Vec<LightVertex>) = (Vec::new(), Vec::new());
    let (_, right, up) = camera.axes();
    let (_, eye, _) = camera.matrices(size.0.max(1) as f32 / size.1.max(1) as f32);
    let per_pixel = 2.0 * (FOV_Y * 0.5).tan() / size.1.max(1) as f32;
    let ink = [0.02, 0.02, 0.03, 1.0];
    let fill = |tris: &mut Vec<LightVertex>, corners: [Vec3; 4], color: [f32; 4]| {
        for v in [corners[0], corners[1], corners[2], corners[0], corners[2], corners[3]] {
            tris.push(LightVertex { pos: v.to_array(), color });
        }
    };
    let outline = |lines: &mut Vec<LightVertex>, c: [Vec3; 4], color: [f32; 4]| {
        for k in 0..4 {
            push_line(lines, c[k], c[(k + 1) % 4], color);
        }
    };
    for (i, l) in lights.iter().enumerate() {
        if view.mask & l.class.bit() == 0 {
            continue;
        }
        let dir = Vec3::from(l.dir).normalize_or_zero();
        let m = l.color[0].max(l.color[1]).max(l.color[2]);
        let rgb = if m > 1e-4 { [l.color[0] / m, l.color[1] / m, l.color[2] / m] } else { [1.0; 3] };
        // a little white keeps dark hues readable on the dark background
        let col = [rgb[0] * 0.85 + 0.15, rgb[1] * 0.85 + 0.15, rgb[2] * 0.85 + 0.15, 1.0];
        let mut centre = Vec3::from(l.pos);
        if l.class == LightClass::Sun {
            // the arrow runs from the position towards the sun; the disc sits at its end
            let tip = centre + dir * l.radius;
            push_line(&mut lines, centre, tip, col);
            let side = dir.cross(up).normalize_or_zero();
            let back = tip - dir * (l.radius * 0.08);
            for s in [-1.0f32, 1.0] {
                push_line(&mut lines, tip, back + side * (s * l.radius * 0.04), col);
            }
            centre = tip;
            let h = 11.0 * per_pixel * (centre - eye).length();
            let corners = [centre + up * h, centre + right * h, centre - up * h, centre - right * h];
            fill(&mut tris, corners, col);
            outline(&mut lines, corners, ink);
            // rays around the disc
            for q in 0..8 {
                let a = q as f32 / 8.0 * std::f32::consts::TAU;
                let d = right * a.cos() + up * a.sin();
                push_line(&mut lines, centre + d * (h * 1.3), centre + d * (h * 1.9), col);
            }
            continue;
        }
        let reach = (l.radius / 500.0).clamp(0.0, 1.0);
        let hero = l.class == LightClass::Hero;
        let h = (if hero { 4.0 } else { 7.0 + 4.0 * reach }) * per_pixel * (centre - eye).length();
        let corners = if hero {
            let (a, b) = (right * (h * 0.8), up * (h * 0.8));
            [centre + a + b, centre + a - b, centre - a - b, centre - a + b]
        } else {
            [centre + up * h, centre + right * h, centre - up * h, centre - right * h]
        };
        fill(&mut tris, corners, col);
        outline(&mut lines, corners, ink);
        if l.class == LightClass::Spot && dir != Vec3::ZERO {
            push_line(&mut lines, centre, centre + dir * (l.radius * 0.5).clamp(30.0, 400.0), [col[0], col[1], col[2], 0.7]);
        }
        if view.highlight == Some(i) {
            // the reach: three circles through the position, and for a spot the cone out to the reach
            let ring_col = [col[0], col[1], col[2], 0.85];
            if l.radius > 0.0 {
                push_ring(&mut lines, centre, Vec3::X, Vec3::Y, l.radius, ring_col);
                push_ring(&mut lines, centre, Vec3::X, Vec3::Z, l.radius, ring_col);
                push_ring(&mut lines, centre, Vec3::Y, Vec3::Z, l.radius, ring_col);
            }
            if l.class == LightClass::Spot && dir != Vec3::ZERO && l.radius > 0.0 {
                let u = dir.cross(if dir.z.abs() < 0.9 { Vec3::Z } else { Vec3::X }).normalize();
                let v = dir.cross(u);
                let (base, r) = (centre + dir * (l.radius * l.half_angle.cos()), l.radius * l.half_angle.sin());
                push_ring(&mut lines, base, u, v, r, ring_col);
                for q in 0..4 {
                    let a = q as f32 * std::f32::consts::FRAC_PI_2;
                    push_line(&mut lines, centre, base + (u * a.cos() + v * a.sin()) * r, ring_col);
                }
            }
            // a white outline around the marker
            let o = h * 1.6;
            outline(&mut lines, [centre + up * o, centre + right * o, centre - up * o, centre - right * o], [1.0, 1.0, 1.0, 1.0]);
        }
    }
    (tris, lines)
}

/// A texture on the GPU; bind groups combining up to three of them are built per surface.
struct GpuTex {
    view: wgpu::TextureView,
    clamp: (bool, bool),
}

fn create_gpu_texture(device: &wgpu::Device, queue: &wgpu::Queue, img: &TexImage) -> GpuTex {
    let first = &img.mips[0];
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model_texture_image"),
        size: wgpu::Extent3d { width: first.width, height: first.height, depth_or_array_layers: 1 },
        mip_level_count: img.mips.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Raw bytes, no sRGB decoding: what Direct3D 9 samples unless the sRGB sampler state is on.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, m) in img.mips.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &m.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(m.width * 4), rows_per_image: Some(m.height) },
            wgpu::Extent3d { width: m.width, height: m.height, depth_or_array_layers: 1 },
        );
    }
    GpuTex { view: texture.create_view(&wgpu::TextureViewDescriptor::default()), clamp: (img.clamp_u, img.clamp_v) }
}

fn solid_texture(device: &wgpu::Device, queue: &wgpu::Queue, rgba: [u8; 4]) -> GpuTex {
    create_gpu_texture(device, queue, &TexImage { mips: vec![MipLevel { width: 1, height: 1, rgba: rgba.to_vec() }], clamp_u: false, clamp_v: false })
}

fn make_samplers(device: &wgpu::Device) -> [wgpu::Sampler; 4] {
    let address = |clamp: bool| if clamp { wgpu::AddressMode::ClampToEdge } else { wgpu::AddressMode::Repeat };
    [0usize, 1, 2, 3].map(|i| {
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model_texture_sampler"),
            address_mode_u: address(i & 1 != 0),
            address_mode_v: address(i & 2 != 0),
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        })
    })
}

/// Tint used for surface `i` in the "Surfaces" shading and the surface list.
pub fn surface_tint(i: usize) -> [u8; 3] {
    PALETTE[i % PALETTE.len()]
}

impl Viewer {
    /// For the window: shares the egui renderer's device and shows the result as an egui texture.
    pub fn new(state: &RenderState) -> Self {
        Self::build(state.device.clone(), state.queue.clone(), Some(state.renderer.clone()))
    }

    /// Without a window: its own Vulkan device (or the platform default); read frames with [`Viewer::read_back`].
    pub fn headless() -> anyhow::Result<Self> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        if std::env::var_os("WGPU_BACKEND").is_none() {
            desc.backends = wgpu::Backends::VULKAN;
        }
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|e| anyhow::anyhow!("no suitable GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| anyhow::anyhow!("cannot create a GPU device: {e}"))?;
        eprintln!("headless renderer on {:?}", adapter.get_info());
        Ok(Self::build(device, queue, None))
    }

    fn build(device: wgpu::Device, queue: wgpu::Queue, renderer: Option<Arc<RwLock<egui_wgpu::Renderer>>>) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("model_shader"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model_uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model_texture"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("model_pipeline_layout"),
            bind_group_layouts: &[Some(&layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let lines_key = PipeKey { blend: None, cull: 0, depth_write: true, offset: false };
        let lines = build_pipeline(&device, &module, &pipeline_layout, &lines_key, wgpu::PrimitiveTopology::LineList, "fs_line");
        let light_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("light_pipeline_layout"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let light_pipes = [(wgpu::PrimitiveTopology::TriangleList, true), (wgpu::PrimitiveTopology::LineList, true), (wgpu::PrimitiveTopology::TriangleList, false), (wgpu::PrimitiveTopology::LineList, false)]
            .map(|(topology, ghost)| build_light_pipeline(&device, &module, &light_layout, topology, ghost));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model_uniform_buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model_bind_group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let white = solid_texture(&device, &queue, [255, 255, 255, 255]);
        // Flat normal in the DXT5nm layout (x in alpha, y in green): both 0.5; black = no specular.
        let flat_normal = solid_texture(&device, &queue, [128, 128, 128, 128]);
        let black = solid_texture(&device, &queue, [0, 0, 0, 0]);
        let samplers = make_samplers(&device);
        Self { device, queue, renderer, layout, tex_layout, textures: HashMap::new(), groups: HashMap::new(), samplers, white, flat_normal, black, features: 3, module, pipeline_layout, pipelines: HashMap::new(), lines, light_pipes, lights: Vec::new(), light_view: LightView::default(), uniforms, bind_group, target: None, scene: None }
    }

    /// Uploads a model, replacing the previous one. Returns one entry per surface.
    pub fn set_model(&mut self, model: Option<&Model>, surface_textures: &[SurfaceTextures], states: &[Option<MaterialState>], env: &[Option<[f32; 4]>]) {
        use wgpu::util::DeviceExt;
        let Some(model) = model else {
            self.scene = None;
            return;
        };
        let mut vertices: Vec<GpuVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut edges: Vec<u32> = Vec::new();
        let mut draws = Vec::new();
        for (si, s) in model.surfaces.iter().enumerate() {
            let base = vertices.len() as u32;
            let tint = PALETTE[si % PALETTE.len()];
            let state = states.get(si).copied().flatten();
            let flags = state.map_or([0u8; 4], |st| [st.alpha_test, st.premultiplied() as u8, 0, 0]);
            let params = env.get(si).copied().flatten().unwrap_or([0.0; 4]);
            vertices.extend(s.vertices.iter().map(|v| GpuVertex { pos: v.pos, normal: v.normal, uv: v.uv, color: v.color, tint: [tint[0], tint[1], tint[2], 255], flags, tangent: [v.tangent[0], v.tangent[1], v.tangent[2], v.binormal_sign], params }));
            let first_index = indices.len() as u32;
            indices.extend(s.indices.iter().map(|i| i + base));
            let first_edge = edges.len() as u32;
            let mut seen = HashSet::new();
            for t in s.indices.chunks_exact(3) {
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    if a != b && seen.insert((a.min(b), a.max(b))) {
                        edges.push(a + base);
                        edges.push(b + base);
                    }
                }
            }
            draws.push(SurfaceDraw {
                textures: {
                    let t = surface_textures.get(si).cloned().unwrap_or_default();
                    [t.color, t.normal, t.spec].map(|n| n.map(|n| n.to_ascii_lowercase()))
                },
                state,
                first_index,
                index_count: indices.len() as u32 - first_index,
                first_edge,
                edge_count: edges.len() as u32 - first_edge,
            });
        }
        let buf = |label: &str, data: &[u8], usage| self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: data, usage });
        // Empty buffers are not allowed in some backends; pad with one zeroed element.
        if vertices.is_empty() {
            vertices.push(GpuVertex::zeroed());
        }
        if indices.is_empty() {
            indices.push(0);
        }
        if edges.is_empty() {
            edges.push(0);
        }
        self.scene = Some(Scene {
            vertices: buf("model_vertices", bytemuck::cast_slice(&vertices), wgpu::BufferUsages::VERTEX),
            indices: buf("model_indices", bytemuck::cast_slice(&indices), wgpu::BufferUsages::INDEX),
            edges: buf("model_edges", bytemuck::cast_slice(&edges), wgpu::BufferUsages::INDEX),
            draws,
        });
    }

    /// Replaces the light markers (drawn by [`Viewer::render`] when the light view says so).
    pub fn set_lights(&mut self, lights: Vec<LightMarker>) {
        self.lights = lights;
    }

    pub fn set_light_view(&mut self, view: LightView) {
        self.light_view = view;
    }

    pub fn upload_texture(&mut self, name: &str, img: &TexImage) {
        let tex = create_gpu_texture(&self.device, &self.queue, img);
        self.textures.insert(name.to_ascii_lowercase(), tex);
        self.groups.clear();
    }

    /// Turns the normal-map and specular-map contributions on or off.
    pub fn set_features(&mut self, normal: bool, spec: bool) {
        self.features = normal as u32 | (spec as u32) << 1;
    }

    /// Makes sure the bind group for this combination of colour / normal / specular maps exists.
    fn ensure_group(&mut self, key: &[Option<String>; 3]) {
        if self.groups.contains_key(key) {
            return;
        }
        let color = key[0].as_ref().and_then(|n| self.textures.get(n)).unwrap_or(&self.white);
        let normal = key[1].as_ref().and_then(|n| self.textures.get(n)).unwrap_or(&self.flat_normal);
        let spec = key[2].as_ref().and_then(|n| self.textures.get(n)).unwrap_or(&self.black);
        let sampler = &self.samplers[color.clamp.0 as usize | (color.clamp.1 as usize) << 1];
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model_texture_group"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&color.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&normal.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&spec.view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });
        self.groups.insert(key.clone(), group);
    }

    pub fn has_texture(&self, name: &str) -> bool {
        self.textures.contains_key(&name.to_ascii_lowercase())
    }

    fn ensure_target(&mut self, size: (u32, u32)) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        if let Some(old) = self.target.take() {
            if let (Some(r), Some(id)) = (&self.renderer, old.id) {
                r.write().free_texture(&id);
            }
        }
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model_color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = self
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("model_depth"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let id = self.renderer.as_ref().map(|r| r.write().register_native_texture(&self.device, &view, wgpu::FilterMode::Linear));
        self.target = Some(Target { texture, view, depth, id, size });
    }

    /// Draws the scene; returns the egui texture to show (None when headless). `visible[i]` selects surfaces.
    pub fn render(&mut self, size: (u32, u32), camera: &Camera, shading: Shading, cull: Cull, wireframe: bool, visible: &[bool]) -> Option<TextureId> {
        let size = (size.0.clamp(16, 8192), size.1.clamp(16, 8192));
        self.ensure_target(size);
        let (view_proj, eye, light) = camera.matrices(size.0 as f32 / size.1 as f32);
        let uniforms = Uniforms { view_proj: view_proj.to_cols_array_2d(), light: light.extend(0.0).to_array(), eye: eye.extend(1.0).to_array(), mode: shading.code(), features: self.features, _pad: [0; 2] };
        self.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
        // Plan the draws: with textured shading every surface uses its material's blend, alpha test, cull and depth
        // state and surfaces are drawn in sort-key order (opaque first); other modes draw plain opaque geometry.
        let mut plan: Vec<(usize, PipeKey)> = Vec::new();
        if let Some(scene) = &self.scene {
            for (i, d) in scene.draws.iter().enumerate() {
                if !(visible.get(i).copied().unwrap_or(true) && d.index_count > 0) {
                    continue;
                }
                let state = if shading == Shading::Textured { d.state } else { None };
                let forced = match cull {
                    Cull::Material => None,
                    Cull::None => Some(0u8),
                    Cull::Back => Some(1),
                    Cull::Front => Some(2),
                };
                let material_cull = state.map_or(1, |st| match st.cull {
                    1 => 0u8,
                    3 => 2,
                    _ => 1,
                });
                let key = PipeKey {
                    blend: state.filter(|st| st.blended()).map(|st| (st.src, st.dst, st.op)),
                    cull: forced.unwrap_or(material_cull),
                    depth_write: state.is_none_or(|st| st.depth_write),
                    offset: state.is_some_and(|st| st.polygon_offset),
                };
                plan.push((i, key));
            }
            if shading == Shading::Textured {
                plan.sort_by_key(|(i, _)| scene.draws[*i].state.map_or(0, |st| st.sort_key));
            }
        }
        let none_key: [Option<String>; 3] = [None, None, None];
        self.ensure_group(&none_key);
        if let Some(scene) = &self.scene {
            let keys: Vec<[Option<String>; 3]> = plan.iter().map(|(i, _)| scene.draws[*i].textures.clone()).collect();
            for k in &keys {
                self.ensure_group(k);
            }
        }
        for (_, key) in &plan {
            if !self.pipelines.contains_key(key) {
                let p = build_pipeline(&self.device, &self.module, &self.pipeline_layout, key, wgpu::PrimitiveTopology::TriangleList, "fs_main");
                self.pipelines.insert(*key, p);
            }
        }
        let light_bufs = if self.light_view.show && !self.lights.is_empty() {
            use wgpu::util::DeviceExt;
            let (tris, lines) = light_geometry(&self.lights, &self.light_view, camera, size);
            let buf = |data: &[LightVertex]| (!data.is_empty()).then(|| self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("light_vertices"), contents: bytemuck::cast_slice(data), usage: wgpu::BufferUsages::VERTEX }));
            Some((buf(&tris), tris.len() as u32, buf(&lines), lines.len() as u32))
        } else {
            None
        };
        let target = self.target.as_ref().unwrap();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("model_encoder") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("model_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.09, g: 0.09, b: 0.10, a: 1.0 }), store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(scene) = &self.scene {
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, scene.vertices.slice(..));
                pass.set_index_buffer(scene.indices.slice(..), wgpu::IndexFormat::Uint32);
                for (i, key) in &plan {
                    let d = &scene.draws[*i];
                    pass.set_pipeline(&self.pipelines[key]);
                    pass.set_bind_group(1, &self.groups[&d.textures], &[]);
                    pass.draw_indexed(d.first_index..d.first_index + d.index_count, 0, 0..1);
                }
                if wireframe {
                    pass.set_pipeline(&self.lines);
                    pass.set_bind_group(1, &self.groups[&none_key], &[]);
                    pass.set_index_buffer(scene.edges.slice(..), wgpu::IndexFormat::Uint32);
                    for (i, d) in scene.draws.iter().enumerate() {
                        if visible.get(i).copied().unwrap_or(true) && d.edge_count > 0 {
                            pass.draw_indexed(d.first_edge..d.first_edge + d.edge_count, 0, 0..1);
                        }
                    }
                }
            }
            if let Some((tris, tri_count, lines, line_count)) = &light_bufs {
                pass.set_bind_group(0, &self.bind_group, &[]);
                // faint through the walls first, then solid where nothing is in front
                for first in [0, 2] {
                    if let Some(b) = tris {
                        pass.set_pipeline(&self.light_pipes[first]);
                        pass.set_vertex_buffer(0, b.slice(..));
                        pass.draw(0..*tri_count, 0..1);
                    }
                    if let Some(b) = lines {
                        pass.set_pipeline(&self.light_pipes[first + 1]);
                        pass.set_vertex_buffer(0, b.slice(..));
                        pass.draw(0..*line_count, 0..1);
                    }
                }
            }
        }
        self.queue.submit(Some(encoder.finish()));
        target.id
    }

    /// Reads the last rendered frame back as RGBA8 (for tests and screenshots).
    pub fn read_back(&self) -> Option<(u32, u32, Vec<u8>)> {
        let t = self.target.as_ref()?;
        let (w, h) = t.size;
        let row = (w * 4).next_multiple_of(256);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model_readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("model_readback_encoder") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            out.extend_from_slice(&data[(y * row) as usize..(y * row + w * 4) as usize]);
        }
        Some((w, h, out))
    }

    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }
}

/// Shows the rendered texture filling `ui`'s remaining space; returns the
/// response (for dragging) and the size in physical pixels.
pub fn viewport_rect(ui: &mut egui::Ui) -> (egui::Response, (u32, u32)) {
    let avail = ui.available_size();
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
    let ppp = ui.ctx().pixels_per_point();
    (resp, ((rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32))
}
