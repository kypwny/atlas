//! An independent reader of the Cast container (header, nodes, properties), shared by the export tests. It knows nothing of the
//! writer: it follows the format description (little endian, `cast` magic, version 1, 24-byte node headers whose size covers the
//! children, properties as a two-byte type code, name length, element count, name and elements).
#![allow(dead_code)]

#[derive(Default, Debug)]
pub struct Seen {
    /// (node id, hash, parent index into `nodes`)
    pub nodes: Vec<(u32, u64, Option<usize>)>,
    pub props: Vec<Prop>,
}

#[derive(Debug, Clone)]
pub struct Prop {
    pub node: usize,
    pub node_id: u32,
    pub name: String,
    pub code: String,
    pub len: u32,
    pub data: Vec<u8>,
}

impl Prop {
    pub fn string(&self) -> String {
        String::from_utf8_lossy(&self.data[..self.data.iter().position(|&c| c == 0).unwrap_or(self.data.len())]).into_owned()
    }
    pub fn u32s(&self) -> Vec<u32> {
        self.data.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
    }
    pub fn f32s(&self) -> Vec<f32> {
        self.data.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
    }
}

pub const ROOT: u32 = 0x746F6F72;
pub const MODEL: u32 = 0x6C646F6D;
pub const MESH: u32 = 0x6873656D;
pub const SKELETON: u32 = 0x6C656B73;
pub const BONE: u32 = 0x656E6F62;
pub const ANIMATION: u32 = 0x6D696E61;
pub const CURVE: u32 = 0x76727563;
pub const MATERIAL: u32 = 0x6C74616D;
pub const FILE: u32 = 0x656C6966;

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn read_nodes(b: &[u8], mut at: usize, end: usize, count: u32, parent: Option<usize>, seen: &mut Seen) {
    for _ in 0..count {
        assert!(at + 24 <= end, "node header past its parent");
        let id = u32le(b, at);
        let size = u32le(b, at + 4) as usize;
        let hash = u64::from_le_bytes(b[at + 8..at + 16].try_into().unwrap());
        let (props, children) = (u32le(b, at + 16), u32le(b, at + 20));
        let body = at + 24;
        assert!(body + size <= end, "node {id:#x} runs past its parent ({} > {})", body + size, end);
        let me = seen.nodes.len();
        seen.nodes.push((id, hash, parent));
        let mut p = body;
        for _ in 0..props {
            let code = [b[p], b[p + 1]];
            let name_len = u16::from_le_bytes([b[p + 2], b[p + 3]]) as usize;
            let len = u32le(b, p + 4);
            let name = String::from_utf8(b[p + 8..p + 8 + name_len].to_vec()).unwrap();
            p += 8 + name_len;
            let tcode = String::from_utf8(code.iter().take_while(|&&c| c != 0).copied().collect()).unwrap();
            let elem = match tcode.as_str() {
                "b" => 1,
                "h" => 2,
                "i" | "f" => 4,
                "l" | "d" => 8,
                "2v" => 8,
                "3v" => 12,
                "4v" => 16,
                "s" => 0,
                other => panic!("property {name}: unknown type {other:?}"),
            };
            let start = p;
            if tcode == "s" {
                p += b[p..].iter().position(|&c| c == 0).expect("unterminated string") + 1;
            } else {
                p += elem * len as usize;
            }
            assert!(p <= body + size, "property {name} runs past its node");
            seen.props.push(Prop { node: me, node_id: id, name, code: tcode, len, data: b[start..p].to_vec() });
        }
        read_nodes(b, p, body + size, children, Some(me), seen);
        at = body + size;
    }
}

pub fn parse_cast(b: &[u8]) -> Seen {
    assert_eq!(&b[..4], b"cast");
    assert_eq!(u32le(b, 4), 1, "version");
    let mut seen = Seen::default();
    read_nodes(b, 16, b.len(), u32le(b, 8), None, &mut seen);
    seen
}

impl Seen {
    pub fn count(&self, id: u32) -> usize {
        self.nodes.iter().filter(|n| n.0 == id).count()
    }
    pub fn props_of(&self, node: usize) -> impl Iterator<Item = &Prop> {
        self.props.iter().filter(move |p| p.node == node)
    }
    pub fn prop(&self, node: usize, name: &str) -> Option<&Prop> {
        self.props_of(node).find(|p| p.name == name)
    }
}
