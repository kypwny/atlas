//! Effects (`FxEffectDef`) of Call of Duty Online zones: read into plain definitions ([`def`]), played by a simulator ([`sim`])
//! that turns a running effect into camera-facing quads, ribbons and model placements the viewer draws.
//!
//! The element layout is IW4's with the CoDOL changes the zone walker knows (`zone::replay::Replay::effect`): 260-byte elements,
//! two more element types (rotated sprite 2, line 4, everything from the trail on two higher), one more element flag at 0x100 and
//! 64-byte visual-state samples in QA / 3.19 zones.

pub mod def;
pub mod sim;
pub mod view;

pub use def::{Effect, Elem, ElemType, Library, Visual};
