//! Posing a skeleton with an animation and skinning a model with it (on the CPU).
//!
//! Each bone's local transform is the animation's rotation / translation where it drives that bone (matched by bone
//! name) and the model's own local pose otherwise; world transforms follow down the parent chain. A vertex is moved by
//! the weighted sum of `posed * inverse(bind)` of its bones.

use crate::anim::Animation;
use crate::model::Model;
use crate::modelindex::Skeleton;
use glam::{Quat, Vec3};

#[derive(Clone, Copy, Debug)]
pub struct Xform {
    pub rot: Quat,
    pub pos: Vec3,
}

impl Xform {
    pub const IDENTITY: Xform = Xform { rot: Quat::IDENTITY, pos: Vec3::ZERO };

    /// `self` applied after `child` (parent * child).
    pub fn then(self, child: Xform) -> Xform {
        Xform { rot: self.rot * child.rot, pos: self.pos + self.rot * child.pos }
    }

    pub fn inverse(self) -> Xform {
        let r = self.rot.inverse();
        Xform { rot: r, pos: -(r * self.pos) }
    }

    pub fn apply(self, p: Vec3) -> Vec3 {
        self.rot * p + self.pos
    }
}

fn quat(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize()
}

/// The bind pose in model space.
pub fn bind_pose(skel: &Skeleton) -> Vec<Xform> {
    skel.global.iter().map(|(q, p)| Xform { rot: quat(*q), pos: Vec3::from(*p) }).collect()
}

/// Model-space transform of every bone at `frame` of `anim`.
pub fn pose(skel: &Skeleton, anim: &Animation, frame: f32) -> Vec<Xform> {
    let n = skel.names.len();
    let mut world: Vec<Xform> = Vec::with_capacity(n);
    for i in 0..n {
        let track = anim.tracks.iter().find(|t| t.name == skel.names[i]);
        let (bind_q, bind_p) = skel.local[i];
        let rot = track.and_then(|t| t.rotation_at(frame)).map(quat).unwrap_or_else(|| quat(bind_q));
        let pos = track.and_then(|t| t.translation_at(frame)).map(Vec3::from).unwrap_or_else(|| Vec3::from(bind_p));
        let local = Xform { rot, pos };
        // bones come after their parents in the stored order
        let w = match skel.parents[i] {
            p if p >= 0 && (p as usize) < world.len() => world[p as usize].then(local),
            _ => local,
        };
        world.push(w);
    }
    world
}

/// A copy of `model` with positions and normals moved by the pose.
pub fn skin(model: &Model, bind: &[Xform], posed: &[Xform]) -> Model {
    let mut out = model.clone();
    let skin_mats: Vec<Xform> = bind.iter().zip(posed).map(|(b, p)| p.then(b.inverse())).collect();
    for (surface, weights) in out.surfaces.iter_mut().zip(model.skin_weights()) {
        for (v, w) in surface.vertices.iter_mut().zip(weights) {
            if w.is_empty() {
                continue;
            }
            let (p, nrm) = (Vec3::from(v.pos), Vec3::from(v.normal));
            let (mut np, mut nn) = (Vec3::ZERO, Vec3::ZERO);
            for (bone, weight) in w {
                let m = skin_mats.get(bone as usize).copied().unwrap_or(Xform::IDENTITY);
                np += m.apply(p) * weight;
                nn += (m.rot * nrm) * weight;
            }
            v.pos = np.into();
            v.normal = nn.normalize_or_zero().into();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::{BoneTrack, Rotation, Translation};

    fn skeleton() -> Skeleton {
        // root at the origin, child one unit along x
        Skeleton {
            names: vec!["root".into(), "child".into()],
            parents: vec![-1, 0],
            local: vec![([0.0, 0.0, 0.0, 1.0], [0.0; 3]), ([0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0])],
            global: vec![([0.0, 0.0, 0.0, 1.0], [0.0; 3]), ([0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0])],
        }
    }

    fn anim(tracks: Vec<BoneTrack>) -> Animation {
        Animation { name: "a".into(), num_frames: 1, frame_rate: 30.0, looped: false, asset_type: 1, delta: None, tracks, notifies: vec![] }
    }

    #[test]
    fn an_animation_that_drives_nothing_leaves_the_bind_pose() {
        let s = skeleton();
        let posed = pose(&s, &anim(vec![]), 0.0);
        for (a, b) in posed.iter().zip(bind_pose(&s)) {
            assert!((a.pos - b.pos).length() < 1e-6);
        }
    }

    #[test]
    fn rotating_the_root_carries_the_child_round() {
        let s = skeleton();
        // 90 degrees about z: the child's offset (1, 0, 0) becomes (0, 1, 0)
        let a = anim(vec![BoneTrack {
            name: "root".into(),
            rot: Rotation::Keys { at: vec![0], q: vec![[0.0, 0.0, 0.70710677, 0.70710677]] },
            trans: Translation::None,
        }]);
        let posed = pose(&s, &a, 0.0);
        assert!((posed[1].pos - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-5, "{:?}", posed[1].pos);
    }
}
