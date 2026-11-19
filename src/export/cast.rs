//! Cast (`.cast`) writer, the model and animation format of dtzxporter's Cast (https://github.com/dtzxporter/cast, MIT), which
//! has importers for Blender 3.6 and later and for Maya. A file is a header and a tree of nodes with typed properties; the
//! layout follows the format description in that project's README (little endian, `cast` magic, version 1, 24-byte node
//! headers, node sizes that include the children).
//!
//! A model becomes a `Root` with one `Model` holding a `Skeleton` of `Bone`s, one `Mesh` per surface (positions, normals, UV
//! layer, 32-bit face indices in counter-clockwise order, skin weights) and `Material`s whose textures are `File` nodes.
//! Animations become `Animation` nodes of `Curve`s (rotation as `rq`, location as `tx`/`ty`/`tz`, scale as `sx`/`sy`/`sz`).

use super::seanim::SeAnim;
use super::ExportModel;

const ROOT: u32 = 0x746F6F72;
const MODEL: u32 = 0x6C646F6D;
const MESH: u32 = 0x6873656D;
const SKELETON: u32 = 0x6C656B73;
const BONE: u32 = 0x656E6F62;
const ANIMATION: u32 = 0x6D696E61;
const CURVE: u32 = 0x76727563;
const MATERIAL: u32 = 0x6C74616D;
const FILE: u32 = 0x656C6966;
const METADATA: u32 = 0x6174656D;

/// One property: a name, the element type code and the packed elements.
struct Prop {
    name: &'static str,
    code: &'static str,
    length: u32,
    data: Vec<u8>,
}

impl Prop {
    fn string(name: &'static str, v: &str) -> Prop {
        let mut data = v.as_bytes().to_vec();
        data.push(0);
        Prop { name, code: "s", length: 1, data }
    }
    fn bytes(name: &'static str, v: &[u8]) -> Prop {
        Prop { name, code: "b", length: v.len() as u32, data: v.to_vec() }
    }
    fn u32s(name: &'static str, v: &[u32]) -> Prop {
        Prop { name, code: "i", length: v.len() as u32, data: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
    }
    fn u16s(name: &'static str, v: &[u16]) -> Prop {
        Prop { name, code: "h", length: v.len() as u32, data: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
    }
    fn u64(name: &'static str, v: u64) -> Prop {
        Prop { name, code: "l", length: 1, data: v.to_le_bytes().to_vec() }
    }
    fn float(name: &'static str, v: f32) -> Prop {
        Prop { name, code: "f", length: 1, data: v.to_le_bytes().to_vec() }
    }
    fn floats(name: &'static str, v: &[f32]) -> Prop {
        Prop { name, code: "f", length: v.len() as u32, data: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
    }
    fn vec<const N: usize>(name: &'static str, code: &'static str, v: &[[f32; N]]) -> Prop {
        Prop { name, code, length: v.len() as u32, data: v.iter().flatten().flat_map(|x| x.to_le_bytes()).collect() }
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.name.len() + self.data.len());
        // the type code is a two-byte field: the ASCII code, zero-padded ("b" is 0x0062)
        let mut code = [0u8; 2];
        for (i, c) in self.code.bytes().take(2).enumerate() {
            code[i] = c;
        }
        out.extend_from_slice(&code);
        out.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(self.name.as_bytes());
        out.extend_from_slice(&self.data);
        out
    }
}

struct Node {
    id: u32,
    hash: u64,
    props: Vec<Prop>,
    children: Vec<Node>,
}

impl Node {
    fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for p in &self.props {
            body.extend(p.encode());
        }
        for c in &self.children {
            body.extend(c.encode());
        }
        let mut out = Vec::with_capacity(24 + body.len());
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.hash.to_le_bytes());
        out.extend_from_slice(&(self.props.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.children.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }
}

/// Hands out the unique hashes nodes are linked by.
struct Hashes(u64);

impl Hashes {
    fn next(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

fn file(bytes: &[Node]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"cast");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for n in bytes {
        out.extend(n.encode());
    }
    out
}

fn model_node(model: &ExportModel, scale: f32, h: &mut Hashes) -> Node {
    let s = |p: [f32; 3]| p.map(|c| c * scale);
    let mut node = Node { id: MODEL, hash: h.next(), props: vec![Prop::string("n", &model.name)], children: Vec::new() };
    if !model.bones.is_empty() {
        let mut sk = Node { id: SKELETON, hash: h.next(), props: Vec::new(), children: Vec::new() };
        for b in &model.bones {
            sk.children.push(Node {
                id: BONE,
                hash: h.next(),
                props: vec![
                    Prop::string("n", &b.name),
                    Prop::u32s("p", &[b.parent as u32]),
                    Prop::vec("lp", "3v", &[s(b.local.0)]),
                    Prop::vec("lr", "4v", &[b.local.1]),
                    Prop::vec("wp", "3v", &[s(b.global.0)]),
                    Prop::vec("wr", "4v", &[b.global.1]),
                ],
                children: Vec::new(),
            });
        }
        node.children.push(sk);
    }
    // materials first so meshes can link to them by hash
    let mut material_hashes = Vec::new();
    for m in &model.materials {
        let hash = h.next();
        material_hashes.push(hash);
        let mut mat = Node { id: MATERIAL, hash, props: vec![Prop::string("n", &m.name), Prop::string("t", "pbr")], children: Vec::new() };
        for (slot, image) in [("albedo", &m.textures.color), ("normal", &m.textures.normal), ("specular", &m.textures.spec)] {
            if let Some(name) = image {
                let f = h.next();
                mat.children.push(Node { id: FILE, hash: f, props: vec![Prop::string("p", &format!("../images/{}", ExportModel::image_file(name.trim_start_matches(','))))], children: Vec::new() });
                mat.props.push(Prop::u64(slot, f));
            }
        }
        node.children.push(mat);
    }
    let influences = model.parts.iter().flat_map(|p| p.weights.iter().map(|w| w.len())).max().unwrap_or(0).clamp(1, 8);
    for p in &model.parts {
        if p.positions.is_empty() || p.faces.is_empty() {
            continue;
        }
        let mut props = vec![Prop::string("n", &p.name), Prop::vec("vp", "3v", &p.positions.iter().map(|&v| s(v)).collect::<Vec<_>>())];
        if p.normals.len() == p.positions.len() {
            props.push(Prop::vec("vn", "3v", &p.normals));
        }
        if p.uvs.len() == p.positions.len() {
            props.push(Prop::vec("u0", "2v", &p.uvs.iter().map(|uv| [uv[0], uv[1]]).collect::<Vec<_>>()));
            props.push(Prop::bytes("ul", &[1]));
        }
        // the game winds its triangles clockwise; Cast wants counter-clockwise
        let faces: Vec<u32> = p.faces.iter().flat_map(|f| [f[0], f[2], f[1]]).collect();
        props.push(Prop::u32s("f", &faces));
        if p.weights.len() == p.positions.len() && p.weights.iter().any(|w| !w.is_empty()) {
            let (mut bones, mut values) = (Vec::new(), Vec::new());
            for w in &p.weights {
                for k in 0..influences {
                    let (b, v) = w.get(k).copied().unwrap_or((0, 0.0));
                    bones.push(b);
                    values.push(v);
                }
            }
            props.push(Prop::u32s("wb", &bones));
            props.push(Prop::floats("wv", &values));
            props.push(Prop::bytes("mi", &[influences as u8]));
        }
        if let Some(&m) = material_hashes.get(p.material) {
            props.push(Prop::u64("m", m));
        }
        node.children.push(Node { id: MESH, hash: h.next(), props, children: Vec::new() });
    }
    node
}

fn animation_node(a: &SeAnim, h: &mut Hashes) -> Node {
    // type 4 (Black Ops II): rotations replace the bone's, locations are offsets from its bind pose
    let loc_mode = if a.anim_type == 4 { "relative" } else { "" };
    let mode = match a.anim_type {
        1 => "additive",
        0 | 4 => "absolute",
        _ => "relative",
    };
    let mut node = Node { id: ANIMATION, hash: h.next(), props: vec![Prop::float("fr", a.frame_rate), Prop::bytes("lo", &[a.looped as u8])], children: Vec::new() };
    let mut curve = |bone: &str, key: &'static str, frames: Vec<u32>, value: Prop, mode: &str| {
        node.children.push(Node {
            id: CURVE,
            hash: h.next(),
            props: vec![Prop::string("nn", bone), Prop::string("kp", key), Prop::u32s("kb", &frames), value, Prop::string("m", mode)],
            children: Vec::new(),
        });
    };
    for b in &a.bones {
        if !b.rotation.is_empty() {
            curve(&b.name, "rq", b.rotation.iter().map(|k| k.0).collect(), Prop::vec("kv", "4v", &b.rotation.iter().map(|k| k.1).collect::<Vec<_>>()), mode);
        }
        if !b.location.is_empty() {
            for (axis, key) in ["tx", "ty", "tz"].into_iter().enumerate() {
                curve(&b.name, key, b.location.iter().map(|k| k.0).collect(), Prop::floats("kv", &b.location.iter().map(|k| k.1[axis]).collect::<Vec<_>>()), if loc_mode.is_empty() { mode } else { loc_mode });
            }
        }
        if !b.scale.is_empty() {
            for (axis, key) in ["sx", "sy", "sz"].into_iter().enumerate() {
                curve(&b.name, key, b.scale.iter().map(|k| k.0).collect(), Prop::floats("kv", &b.scale.iter().map(|k| k.1[axis]).collect::<Vec<_>>()), mode);
            }
        }
    }
    node
}

/// A `.cast` file with the model (positions multiplied by `scale`) and the given animations (already converted to bone keys in
/// the same units). The up axis hint is Z, which is how the game stores its worlds.
pub fn write(model: &ExportModel, animations: &[SeAnim], scale: f32) -> Vec<u8> {
    let mut h = Hashes(0);
    let mut root = Node { id: ROOT, hash: h.next(), props: Vec::new(), children: Vec::new() };
    root.children.push(Node { id: METADATA, hash: h.next(), props: vec![Prop::string("up", "z"), Prop::string("a", "Atlas")], children: Vec::new() });
    root.children.push(model_node(model, scale, &mut h));
    for a in animations {
        root.children.push(animation_node(a, &mut h));
    }
    file(&[root])
}

/// A `.cast` file holding only one animation.
pub fn write_animation(a: &SeAnim) -> Vec<u8> {
    let mut h = Hashes(0);
    let mut root = Node { id: ROOT, hash: h.next(), props: Vec::new(), children: Vec::new() };
    root.children.push(animation_node(a, &mut h));
    file(&[root])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{ExportBone, ExportMaterial, Part};

    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }

    /// Walks the node tree and checks every node's size against its contents.
    fn check(b: &[u8], mut at: usize, end: usize, count: u32, found: &mut Vec<u32>) {
        for _ in 0..count {
            let (id, size) = (u32_at(b, at), u32_at(b, at + 4) as usize);
            let (props, children) = (u32_at(b, at + 16), u32_at(b, at + 20));
            found.push(id);
            let body = at + 24;
            assert!(body + size <= end, "node {id:#x} runs past its parent");
            // skip the properties
            let mut p = body;
            for _ in 0..props {
                let (name, len) = (u16::from_le_bytes(b[p + 2..p + 4].try_into().unwrap()) as usize, u32_at(b, p + 4) as usize);
                let code = b[p];
                let elem = match code {
                    b'b' => 1,
                    b'h' => 2,
                    b'i' | b'f' => 4,
                    b'l' | b'd' => 8,
                    // vector types are named `2v`, `3v`, `4v` (the Cast add-on's ids)
                    b'2' | b'3' | b'4' => 4 * (code - b'0') as usize,
                    b's' => 0,
                    other => panic!("type {other}"),
                };
                p += 8 + name;
                if code == b's' {
                    p += b[p..].iter().position(|&c| c == 0).unwrap() + 1;
                } else {
                    p += elem * len;
                }
            }
            check(b, p, body + size, children, found);
            at = body + size;
        }
    }

    #[test]
    fn a_model_and_an_animation_make_a_well_formed_tree() {
        let model = ExportModel {
            name: "cube".into(),
            parts: vec![Part { name: "p".into(), positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], normals: vec![[0.0, 0.0, 1.0]; 3], uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], faces: vec![[0, 1, 2]], weights: vec![vec![(0, 1.0)]; 3], material: 0, ..Default::default() }],
            materials: vec![ExportMaterial { name: "m".into(), ..Default::default() }],
            bones: vec![ExportBone { name: "root".into(), parent: -1, ..Default::default() }],
        };
        let anim = SeAnim { frame_rate: 30.0, frame_count: 2, bones: vec![crate::export::seanim::BoneKeys { name: "root".into(), location: vec![(0, [0.0; 3]), (1, [1.0, 2.0, 3.0])], rotation: vec![(0, [0.0, 0.0, 0.0, 1.0])], scale: vec![] }], ..Default::default() };
        let bytes = write(&model, &[anim], 1.0);
        assert_eq!(&bytes[..4], b"cast");
        assert_eq!(u32_at(&bytes, 4), 1);
        assert_eq!(u32_at(&bytes, 8), 1, "one root node");
        let mut found = Vec::new();
        check(&bytes, 16, bytes.len(), 1, &mut found);
        // root, metadata, model, material, mesh... (skeleton, bone), animation, curves
        for id in [ROOT, METADATA, MODEL, SKELETON, BONE, MATERIAL, MESH, ANIMATION, CURVE] {
            assert!(found.contains(&id), "missing node {id:#x}");
        }
        assert_eq!(found.iter().filter(|&&i| i == CURVE).count(), 4, "one rotation curve and three location curves");
    }
}
