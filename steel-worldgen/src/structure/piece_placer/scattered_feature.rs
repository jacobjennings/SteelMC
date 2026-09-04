//! Portable `ScatteredFeaturePiece` helpers.
//!
//! Vanilla's scattered feature pieces (swamp hut, desert pyramid, jungle
//! temple, igloo's ground level) share one base class whose whole job is to
//! translate piece-local coordinates into world coordinates through the piece's
//! orientation, and to drop the piece onto the average ground height under its
//! footprint. This is that base class with the server-only surface removed.

use glam::IVec3;
use steel_registry::Registry;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::feature::FeatureHeightmap;
use steel_registry::vanilla_blocks;
use steel_utils::{BlockPos, BlockStateId, BoundingBox, Direction};

use crate::structure::state_transform::transform_state;

use super::{StructureBlockAccess, orientation_transform};

/// One piece's placement cursor over a portable region.
pub struct ScatteredFeaturePlacer<'a, H: StructureBlockAccess> {
    region: &'a mut H,
    registry: &'a Registry,
    bounding_box: &'a mut BoundingBox,
    orientation: Option<Direction>,
    clip: BoundingBox,
}

impl<'a, H: StructureBlockAccess> ScatteredFeaturePlacer<'a, H> {
    /// Creates a placement cursor for one piece.
    pub const fn new(
        region: &'a mut H,
        registry: &'a Registry,
        bounding_box: &'a mut BoundingBox,
        orientation: Option<Direction>,
        clip: BoundingBox,
    ) -> Self {
        Self {
            region,
            registry,
            bounding_box,
            orientation,
            clip,
        }
    }

    /// Vanilla `ScatteredFeaturePiece.updateAverageGroundHeight`.
    ///
    /// The answer is remembered on the piece, so a hut that straddles a chunk
    /// boundary uses the height the first decorated chunk measured rather than
    /// re-levelling itself for each chunk it touches.
    pub fn update_average_ground_height(
        &mut self,
        height_position: &mut Option<i32>,
        offset: i32,
    ) -> bool {
        if height_position.is_some() {
            return true;
        }

        let mut total = 0;
        let mut count = 0;
        for z in self.bounding_box.min_z()..=self.bounding_box.max_z() {
            for x in self.bounding_box.min_x()..=self.bounding_box.max_x() {
                if self.clip.contains_blockpos(BlockPos::new(x, 64, z)) {
                    total += self
                        .region
                        .height_at(FeatureHeightmap::MotionBlockingNoLeaves, x, z);
                    count += 1;
                }
            }
        }

        if count == 0 {
            return false;
        }

        let adjusted = total / count;
        *height_position = Some(adjusted);
        let dy = adjusted - self.bounding_box.min_y() + offset;
        *self.bounding_box = self.bounding_box.translate(IVec3::new(0, dy, 0));
        true
    }

    /// Vanilla `StructurePiece.generateBox`.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors vanilla StructurePiece.generateBox parameters"
    )]
    pub fn generate_box(
        &mut self,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        edge: BlockStateId,
        fill: BlockStateId,
        skip_air: bool,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if skip_air && self.get_block(x, y, z).is_air() {
                        continue;
                    }
                    let state = if y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1 {
                        fill
                    } else {
                        edge
                    };
                    self.place_block(state, x, y, z);
                }
            }
        }
    }

    /// Vanilla `StructurePiece.fillColumnDown`.
    pub fn fill_column_down(&mut self, state: BlockStateId, x: i32, start_y: i32, z: i32) {
        let mut pos = self.world_pos(x, start_y, z);
        if !self.clip.contains_blockpos(pos) {
            return;
        }

        while Self::is_replaceable_by_structures(self.region.block_state(pos))
            && pos.y() > self.region.min_y() + 1
        {
            self.region.set_block_state(pos, state);
            pos = pos.below();
        }
    }

    /// Vanilla `StructurePiece.placeBlock`.
    pub fn place_block(&mut self, state: BlockStateId, x: i32, y: i32, z: i32) {
        let pos = self.world_pos(x, y, z);
        if !self.clip.contains_blockpos(pos) {
            return;
        }

        let state = self.transform_state(state);
        self.region.set_block_state(pos, state);
    }

    /// Vanilla `StructurePiece.getWorldPos`.
    pub const fn world_pos(&self, x: i32, y: i32, z: i32) -> BlockPos {
        let world_y = if self.orientation.is_some() {
            y + self.bounding_box.min_y()
        } else {
            y
        };
        let (world_x, world_z) = match self.orientation {
            None | Some(Direction::Up | Direction::Down) => (x, z),
            Some(Direction::North) => {
                (self.bounding_box.min_x() + x, self.bounding_box.max_z() - z)
            }
            Some(Direction::South) => {
                (self.bounding_box.min_x() + x, self.bounding_box.min_z() + z)
            }
            Some(Direction::West) => (self.bounding_box.max_x() - z, self.bounding_box.min_z() + x),
            Some(Direction::East) => (self.bounding_box.min_x() + z, self.bounding_box.min_z() + x),
        };
        BlockPos::new(world_x, world_y, world_z)
    }

    /// The chunk box this placement is clipped to.
    pub const fn clip(&self) -> BoundingBox {
        self.clip
    }

    fn get_block(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        let pos = self.world_pos(x, y, z);
        if self.clip.contains_blockpos(pos) {
            self.region.block_state(pos)
        } else {
            vanilla_blocks::AIR.default_state()
        }
    }

    fn transform_state(&self, state: BlockStateId) -> BlockStateId {
        let (mirror, rotation) = orientation_transform(self.orientation);
        transform_state(self.registry, state, mirror, rotation)
    }

    fn is_replaceable_by_structures(state: BlockStateId) -> bool {
        state.is_air()
            || state.has_fluid()
            || state.get_block() == &vanilla_blocks::GLOW_LICHEN
            || state.get_block() == &vanilla_blocks::SEAGRASS
            || state.get_block() == &vanilla_blocks::TALL_SEAGRASS
    }
}
