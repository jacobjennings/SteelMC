//! Native entry points for structure state transforms.
//!
//! The mirror and rotation rules themselves now live in
//! `steel_worldgen::structure::state_transform`, because a browser host has to
//! place structure pieces without linking `steel-core`. This file keeps the
//! call sites the native template engine already uses and forwards to that one
//! implementation, so the two hosts cannot drift apart.

use steel_worldgen::structure::state_transform;

use super::{
    BlockPos, BlockRef, BlockStateId, Direction, Registry, Rotation, StructureMirror,
    StructureTemplate,
};

impl StructureTemplate {
    pub(crate) fn transform_state(
        registry: &Registry,
        state: BlockStateId,
        mirror: StructureMirror,
        rotation: Rotation,
    ) -> BlockStateId {
        state_transform::transform_state(registry, state, mirror, rotation)
    }

    pub(super) fn block_for_state(registry: &Registry, state: BlockStateId) -> BlockRef {
        let Some(block) = registry.blocks.by_state_id(state) else {
            panic!(
                "structure template references invalid block state {}",
                state.0
            );
        };
        block
    }

    pub(super) const fn mirror_direction(
        direction: Direction,
        mirror: StructureMirror,
    ) -> Direction {
        state_transform::mirror_direction(direction, mirror)
    }

    pub(super) fn block_pos_seed(pos: BlockPos) -> i64 {
        let mut seed = i64::from(pos.x().wrapping_mul(3_129_871))
            ^ i64::from(pos.z()).wrapping_mul(116_129_781)
            ^ i64::from(pos.y());
        seed = seed
            .wrapping_mul(seed)
            .wrapping_mul(42_317_861)
            .wrapping_add(seed.wrapping_mul(11));
        seed >> 16
    }

    pub(super) fn clamped_lerp_inverse(
        value: i32,
        min_dist: i32,
        max_dist: i32,
        min: f32,
        max: f32,
    ) -> f32 {
        if min_dist == max_dist {
            return max;
        }
        let delta = ((value - min_dist) as f32 / (max_dist - min_dist) as f32).clamp(0.0, 1.0);
        min + delta * (max - min)
    }
}
