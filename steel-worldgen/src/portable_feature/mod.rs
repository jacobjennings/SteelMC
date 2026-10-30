//! Server-independent Java 26.3 ground-cover feature algorithms.
//!
//! Hosts supply live block predicates and state providers. They must expose
//! writes immediately, including writes outside the origin chunk. These kernels
//! do not select biome features, run placement modifiers or qualify Full terrain.

use steel_registry::feature::{
    BlockHolderSet, BlockPredicate, BlockStateProviderKind,
    ProjectedRandomPatchySquareConfiguration, RandomNeighborSpreadConfiguration,
};
use steel_utils::random::worldgen_random::WorldgenRandom;
use steel_utils::{BlockPos, BlockStateId, Direction};

/// World and registry operations required by the portable ground-cover kernels.
///
/// Provider sampling may inspect the live world and consume the supplied RNG.
/// A missing optional state skips a projected placement. Required sampling in
/// neighbor spreading retains the native provider's required-state semantics.
/// Write rejection must not roll back RNG or earlier writes.
pub trait GroundCoverAccess {
    /// Evaluate a native block predicate against the current world.
    fn test(&mut self, predicate: &BlockPredicate, pos: BlockPos) -> bool;
    /// Resolve a required provider state at the placement position.
    fn state(
        &mut self,
        provider: &BlockStateProviderKind,
        random: &mut WorldgenRandom,
        pos: BlockPos,
    ) -> BlockStateId;
    /// Resolve a provider that may decline a projected placement.
    fn optional_state(
        &mut self,
        provider: &BlockStateProviderKind,
        random: &mut WorldgenRandom,
        pos: BlockPos,
    ) -> Option<BlockStateId>;
    /// Test the block at a neighbor position against the configured holder set.
    fn accepted_neighbor(&mut self, accepted: &BlockHolderSet, pos: BlockPos) -> bool;
    /// Attempt a block write with native UPDATE_CLIENTS (2) semantics.
    fn set_block(&mut self, pos: BlockPos, state: BlockStateId);
}

/// Native `ProjectedRandomPatchySquare.place`, with live projection and providers.
///
/// Configuration is the generated native configuration, whose codec bounds size
/// to 1..=16 and projection height to nonnegative values. Projection tests below
/// before decrementing its budget. Even a zero budget can move down once.
pub fn projected_random_patchy_square(
    world: &mut impl GroundCoverAccess,
    random: &mut WorldgenRandom,
    config: &ProjectedRandomPatchySquareConfiguration,
    origin: BlockPos,
) -> bool {
    use steel_utils::random::Random as _;

    let size = config.size.sample(random);
    let bound = size * size + 1;
    for dx in -size..=size {
        for dz in -size..=size {
            let probability = dx.abs() * dz.abs();
            if random.next_i32_bounded(bound) >= bound - probability {
                continue;
            }
            let mut base = origin.offset(dx, 0, dz);
            let mut drop = config.max_projection_height;
            while world.test(&config.project_through, base.below()) {
                base = base.below();
                drop -= 1;
                if drop <= 0 {
                    break;
                }
            }
            if let Some(state) = world.optional_state(&config.block, random, base) {
                world.set_block(base, state);
            }
        }
    }
    true
}

/// Native `RandomNeighborSpreadFeature.place`, including its unconditional origin.
///
/// Exactly one accepted neighbor permits a placement. Direction traversal and
/// provider calls retain native ordering. A rejected write still consumes the
/// provider draw. The native result is true even when every write is rejected.
pub fn random_neighbor_spread(
    world: &mut impl GroundCoverAccess,
    random: &mut WorldgenRandom,
    config: &RandomNeighborSpreadConfiguration,
    origin: BlockPos,
) -> bool {
    let state = world.state(&config.block, random, origin);
    world.set_block(origin, state);
    let attempts = config.attempts.sample(random);
    for _ in 0..attempts {
        let pos = origin.offset(
            config.xz_offset.sample(random),
            config.y_offset.sample(random),
            config.xz_offset.sample(random),
        );
        if !world.test(&config.can_replace, pos) {
            continue;
        }
        let mut neighbors = 0;
        for direction in [
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::West,
            Direction::East,
        ] {
            if world.accepted_neighbor(&config.accepted_neighbors, pos.relative(direction)) {
                neighbors += 1;
            }
            if neighbors > 1 {
                break;
            }
        }
        if neighbors == 1 {
            let state = world.state(&config.block, random, pos);
            world.set_block(pos, state);
        }
    }
    true
}
