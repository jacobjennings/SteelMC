//! Portable structure piece placement.
//!
//! Structure starts are planned before noise, but vanilla writes the piece
//! blocks during biome decoration, at the very start of the Features stage.
//! This module is the browser-side half of that pass: it emits exactly the
//! block states the native placer emits, through the same sparse generated
//! block list the portable Features slice already uses, so a host without
//! `steel-core` can draw a structure.
//!
//! What it deliberately does not carry is everything a live server needs and a
//! map viewer does not: entities, block entities, loot tables, scheduled fluid
//! ticks, and the post-processing marks that later re-evaluate fence and wall
//! shapes. Those change no block state during the Features stage itself, which
//! is what the parity fixture in `steel-core` compares.

mod igloo;
mod pool_element;
mod scattered_feature;
mod swamp_hut;

use steel_registry::feature::FeatureHeightmap;
use steel_utils::{BlockPos, BlockStateId, BoundingBox, Direction, Rotation};

use crate::structure::{ProceduralPieceData, StructureMirror, StructurePiece, StructurePiecePayload};
use crate::vegetation::VegetationBlockAccess;

pub use scattered_feature::ScatteredFeaturePlacer;

/// A mutable post-Carvers terrain view for portable structure placement.
///
/// This is the same view the portable Features slice writes through. Structure
/// pieces and placed features run against one shared halo in vanilla, so they
/// must run against one shared host here too, or a feature would not see the
/// hut it is standing on.
pub trait StructureBlockAccess {
    /// First writable block Y.
    fn min_y(&self) -> i32;
    /// Current state at a world position. Outside the supplied halo is air.
    fn block_state(&self, pos: BlockPos) -> BlockStateId;
    /// Writes a state at a world position inside the supplied halo.
    fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId);
    /// Exact heightmap lookup.
    fn height_at(&self, kind: FeatureHeightmap, x: i32, z: i32) -> i32;
}

impl<T: VegetationBlockAccess> StructureBlockAccess for T {
    fn min_y(&self) -> i32 {
        VegetationBlockAccess::min_y(self)
    }

    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        VegetationBlockAccess::block_state(self, pos)
    }

    fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId) {
        VegetationBlockAccess::set_block_state(self, pos, state);
    }

    fn height_at(&self, kind: FeatureHeightmap, x: i32, z: i32) -> i32 {
        VegetationBlockAccess::height_at(self, kind, x, z)
    }
}

/// Vanilla `StructurePiece.getOrientation` mirror and rotation pair.
#[must_use]
pub const fn orientation_transform(orientation: Option<Direction>) -> (StructureMirror, Rotation) {
    match orientation {
        None | Some(Direction::North | Direction::Up | Direction::Down) => {
            (StructureMirror::None, Rotation::None)
        }
        Some(Direction::South) => (StructureMirror::LeftRight, Rotation::None),
        Some(Direction::West) => (StructureMirror::LeftRight, Rotation::Clockwise90),
        Some(Direction::East) => (StructureMirror::None, Rotation::Clockwise90),
    }
}

/// Places one already-clipped structure piece into a portable region.
///
/// Returns whether this piece family is implemented portably and placed. A
/// `false` result means the piece was skipped, never that it was placed
/// partially: an unimplemented family writes nothing at all, so a host can tell
/// a missing structure from a wrong one.
pub fn place_piece<H: StructureBlockAccess>(
    region: &mut H,
    registry: &steel_registry::Registry,
    piece: &mut StructurePiece,
    clip: BoundingBox,
) -> bool {
    let mut bounding_box = piece.bounding_box;
    let orientation = piece.orientation;
    let placed = match &mut piece.payload {
        StructurePiecePayload::Procedural(ProceduralPieceData::SwampHut(data)) => {
            swamp_hut::place_swamp_hut_piece(
                region,
                registry,
                &mut bounding_box,
                orientation,
                data,
                clip,
            )
        }
        StructurePiecePayload::Template(data) if igloo::is_portable_template_piece(data) => {
            igloo::place_igloo_piece(region, registry, data, &mut bounding_box, clip)
        }
        StructurePiecePayload::Jigsaw(data) if pool_element::is_calibration_piece(data) => {
            pool_element::place_calibration_piece(region, registry, data, clip)
        }
        _ => false,
    };
    piece.bounding_box = bounding_box;
    placed
}

/// The block families this portable placer implements.
///
/// A host asks this before drawing so it can say "not generated yet" instead of
/// quietly drawing an empty site where a structure belongs.
#[must_use]
pub fn is_portable_piece(piece: &StructurePiece) -> bool {
    match &piece.payload {
        StructurePiecePayload::Procedural(ProceduralPieceData::SwampHut(_)) => true,
        StructurePiecePayload::Template(data) => igloo::is_portable_template_piece(data),
        StructurePiecePayload::Jigsaw(data) => pool_element::is_calibration_piece(data),
        _ => false,
    }
}
