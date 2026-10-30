//! Isolated calls on retained Carvers terrain, not biome decoration or Full scenes.

use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::feature::*;
use steel_registry::{REGISTRY, vanilla_blocks};
use steel_utils::random::worldgen_random::WorldgenRandom;
use steel_utils::value_providers::IntProvider;
use steel_utils::{BlockPos, BlockStateId};
use steel_worldgen::portable_feature::{self, GroundCoverAccess};
use steel_worldgen::portable_terrain::{PortableChunk, RegionWindow};
use steel_worldgen::surface::SurfaceBlockAccess;

pub trait BlockStorage {
    fn read(&mut self, pos: BlockPos) -> BlockStateId;
    fn write(&mut self, pos: BlockPos, state: BlockStateId) -> bool;
}

impl BlockStorage for PortableChunk<'_> {
    fn read(&mut self, pos: BlockPos) -> BlockStateId {
        let x = pos.x() - self.chunk_min_x();
        let z = pos.z() - self.chunk_min_z();
        let y = pos.y() - self.min_y();
        assert!((0..16).contains(&x) && (0..16).contains(&z) && (0..384).contains(&y));
        let Some(state) = self.get_relative_block(x as usize, y as usize, z as usize) else {
            panic!("outside retained comparison chunk");
        };
        state
    }
    fn write(&mut self, pos: BlockPos, state: BlockStateId) -> bool {
        let x = pos.x() - self.chunk_min_x();
        let z = pos.z() - self.chunk_min_z();
        let y = pos.y() - self.min_y();
        assert!((0..16).contains(&x) && (0..16).contains(&z) && (0..384).contains(&y));
        self.set_relative_block(x as usize, y as usize, z as usize, state);
        true
    }
}

impl BlockStorage for RegionWindow<'_, '_> {
    fn read(&mut self, pos: BlockPos) -> BlockStateId {
        match self.block(pos) {
            Ok(state) => state,
            Err(error) => panic!("{error}"),
        }
    }
    fn write(&mut self, pos: BlockPos, state: BlockStateId) -> bool {
        match self.write_state(pos, state) {
            Ok(accepted) => accepted,
            Err(error) => panic!("{error}"),
        }
    }
}

pub struct ObservedStorage<'a, W: BlockStorage> {
    pub inner: &'a mut W,
    pub accepted: usize,
    pub rejected: usize,
}

impl<'a, W: BlockStorage> ObservedStorage<'a, W> {
    pub fn new(inner: &'a mut W) -> Self {
        Self {
            inner,
            accepted: 0,
            rejected: 0,
        }
    }
}

impl<W: BlockStorage> BlockStorage for ObservedStorage<'_, W> {
    fn read(&mut self, pos: BlockPos) -> BlockStateId {
        self.inner.read(pos)
    }
    fn write(&mut self, pos: BlockPos, state: BlockStateId) -> bool {
        let accepted = self.inner.write(pos, state);
        if accepted {
            self.accepted += 1;
        } else {
            self.rejected += 1;
        }
        accepted
    }
}

pub struct ChunkAccess<'a, W: BlockStorage>(pub &'a mut W);

impl<W: BlockStorage> GroundCoverAccess for ChunkAccess<'_, W> {
    fn test(&mut self, predicate: &BlockPredicate, pos: BlockPos) -> bool {
        let BlockPredicate::MatchingBlocks { blocks, offset } = predicate else {
            panic!("unsupported comparison predicate");
        };
        let state = self.0.read(pos.offset(offset.x, offset.y, offset.z));
        blocks.0.contains(&state.get_block())
    }

    fn state(
        &mut self,
        provider: &BlockStateProviderKind,
        _random: &mut WorldgenRandom,
        _pos: BlockPos,
    ) -> BlockStateId {
        let BlockStateProviderKind::Simple { state } = provider else {
            panic!("unsupported comparison provider");
        };
        steel_worldgen::state_resolver::WorldgenStateResolver::feature_block_state_from_data(
            &REGISTRY,
            state,
            "ground-cover comparison",
        )
    }

    fn optional_state(
        &mut self,
        provider: &BlockStateProviderKind,
        random: &mut WorldgenRandom,
        pos: BlockPos,
    ) -> Option<BlockStateId> {
        Some(self.state(provider, random, pos))
    }

    fn accepted_neighbor(&mut self, accepted: &BlockHolderSet, pos: BlockPos) -> bool {
        let BlockHolderSet::Entries(blocks) = accepted else {
            panic!("unsupported comparison holder set");
        };
        let state = self.0.read(pos);
        blocks.contains(&state.get_block())
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockStateId) {
        self.0.write(pos, state);
    }
}

pub fn projected<W: BlockStorage>(
    world: &mut ChunkAccess<'_, W>,
    random: &mut WorldgenRandom,
    origin: BlockPos,
) {
    let config = ProjectedRandomPatchySquareConfiguration {
        block: BlockStateProviderKind::Simple {
            state: BlockStateData {
                block: &vanilla_blocks::SHORT_GRASS,
                properties: &[],
            },
        },
        project_through: air(),
        size: IntProvider::Constant(2),
        max_projection_height: 3,
    };
    portable_feature::projected_random_patchy_square(world, random, &config, origin);
}

pub fn spread<W: BlockStorage>(
    world: &mut ChunkAccess<'_, W>,
    random: &mut WorldgenRandom,
    origin: BlockPos,
) {
    let config = RandomNeighborSpreadConfiguration {
        block: BlockStateProviderKind::Simple {
            state: BlockStateData {
                block: &vanilla_blocks::OAK_LEAVES,
                properties: &[],
            },
        },
        accepted_neighbors: BlockHolderSet::Entries(vec![&vanilla_blocks::OAK_LEAVES]),
        can_replace: air(),
        attempts: IntProvider::Constant(64),
        xz_offset: IntProvider::Uniform {
            min_inclusive: -2,
            max_inclusive: 2,
        },
        y_offset: IntProvider::Uniform {
            min_inclusive: -1,
            max_inclusive: 1,
        },
    };
    portable_feature::random_neighbor_spread(world, random, &config, origin);
}

fn air() -> BlockPredicate {
    BlockPredicate::MatchingBlocks {
        blocks: BlockRefList(vec![&vanilla_blocks::AIR]),
        offset: Default::default(),
    }
}
