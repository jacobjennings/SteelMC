//! Portable swamp hut piece placement.
//!
//! The block layout is vanilla `SwampHutPiece.postProcess`, one call at a time.
//! The witch and the black cat are entities, which a terrain view does not
//! carry, so they are skipped here and their "already spawned" flags are left
//! alone. No block state depends on them.

use steel_registry::Registry;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, StairsShape};
use steel_registry::vanilla_blocks;
use steel_utils::{BlockStateId, BoundingBox, Direction};

use crate::structure::swamp_hut::SwampHutPieceData;

use super::{ScatteredFeaturePlacer, StructureBlockAccess};

pub(super) fn place_swamp_hut_piece<H: StructureBlockAccess>(
    region: &mut H,
    registry: &Registry,
    bounding_box: &mut BoundingBox,
    orientation: Option<Direction>,
    data: &mut SwampHutPieceData,
    clip: BoundingBox,
) -> bool {
    let mut placer =
        ScatteredFeaturePlacer::new(region, registry, bounding_box, orientation, clip);
    if !placer.update_average_ground_height(&mut data.height_position, 0) {
        return false;
    }

    place_swamp_hut(&mut placer);
    true
}

fn place_swamp_hut<H: StructureBlockAccess>(placer: &mut ScatteredFeaturePlacer<'_, H>) {
    let spruce_planks = vanilla_blocks::SPRUCE_PLANKS.default_state();
    let oak_log = vanilla_blocks::OAK_LOG.default_state();
    let oak_fence = vanilla_blocks::OAK_FENCE.default_state();
    let air = vanilla_blocks::AIR.default_state();

    placer.generate_box(1, 1, 1, 5, 1, 7, spruce_planks, spruce_planks, false);
    placer.generate_box(1, 4, 2, 5, 4, 7, spruce_planks, spruce_planks, false);
    placer.generate_box(2, 1, 0, 4, 1, 0, spruce_planks, spruce_planks, false);
    placer.generate_box(2, 2, 2, 3, 3, 2, spruce_planks, spruce_planks, false);
    placer.generate_box(1, 2, 3, 1, 3, 6, spruce_planks, spruce_planks, false);
    placer.generate_box(5, 2, 3, 5, 3, 6, spruce_planks, spruce_planks, false);
    placer.generate_box(2, 2, 7, 4, 3, 7, spruce_planks, spruce_planks, false);
    placer.generate_box(1, 0, 2, 1, 3, 2, oak_log, oak_log, false);
    placer.generate_box(5, 0, 2, 5, 3, 2, oak_log, oak_log, false);
    placer.generate_box(1, 0, 7, 1, 3, 7, oak_log, oak_log, false);
    placer.generate_box(5, 0, 7, 5, 3, 7, oak_log, oak_log, false);
    placer.place_block(oak_fence, 2, 3, 2);
    placer.place_block(oak_fence, 3, 3, 7);
    placer.place_block(air, 1, 3, 4);
    placer.place_block(air, 5, 3, 4);
    placer.place_block(air, 5, 3, 5);
    placer.place_block(vanilla_blocks::POTTED_RED_MUSHROOM.default_state(), 1, 3, 5);
    placer.place_block(vanilla_blocks::CRAFTING_TABLE.default_state(), 3, 2, 6);
    placer.place_block(vanilla_blocks::CAULDRON.default_state(), 4, 2, 6);
    placer.place_block(oak_fence, 1, 2, 1);
    placer.place_block(oak_fence, 5, 2, 1);

    let north_stairs = stairs(Direction::North);
    let east_stairs = stairs(Direction::East);
    let west_stairs = stairs(Direction::West);
    let south_stairs = stairs(Direction::South);
    placer.generate_box(0, 4, 1, 6, 4, 1, north_stairs, north_stairs, false);
    placer.generate_box(0, 4, 2, 0, 4, 7, east_stairs, east_stairs, false);
    placer.generate_box(6, 4, 2, 6, 4, 7, west_stairs, west_stairs, false);
    placer.generate_box(0, 4, 8, 6, 4, 8, south_stairs, south_stairs, false);
    placer.place_block(stairs_shape(north_stairs, StairsShape::OuterRight), 0, 4, 1);
    placer.place_block(stairs_shape(north_stairs, StairsShape::OuterLeft), 6, 4, 1);
    placer.place_block(stairs_shape(south_stairs, StairsShape::OuterLeft), 0, 4, 8);
    placer.place_block(stairs_shape(south_stairs, StairsShape::OuterRight), 6, 4, 8);

    for z in [2, 7] {
        for x in [1, 5] {
            placer.fill_column_down(oak_log, x, -1, z);
        }
    }
}

fn stairs(facing: Direction) -> BlockStateId {
    vanilla_blocks::SPRUCE_STAIRS
        .default_state()
        .set_value(&BlockStateProperties::FACING, facing)
}

fn stairs_shape(state: BlockStateId, shape: StairsShape) -> BlockStateId {
    state.set_value(&BlockStateProperties::STAIRS_SHAPE, shape)
}
