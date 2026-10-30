//! Isolated calls on retained Carvers terrain, not biome decoration or Full scenes.

use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::feature::*;
use steel_registry::{REGISTRY, vanilla_blocks};
use steel_utils::random::worldgen_random::WorldgenRandom;
use steel_utils::value_providers::IntProvider;
use steel_utils::{BlockPos, BlockStateId};
use steel_worldgen::portable_feature::{self, GroundCoverAccess};
use steel_worldgen::portable_terrain::PortableChunk;
use steel_worldgen::surface::SurfaceBlockAccess;

pub struct ChunkAccess<'a, 'generator>(pub &'a mut PortableChunk<'generator>);

impl ChunkAccess<'_, '_> {
    fn relative(&self, pos: BlockPos) -> (usize, usize, usize) {
        let x = pos.x() - self.0.chunk_min_x();
        let z = pos.z() - self.0.chunk_min_z();
        let y = pos.y() - self.0.min_y();
        assert!((0..16).contains(&x) && (0..16).contains(&z) && (0..384).contains(&y));
        (x as usize, y as usize, z as usize)
    }
}

impl GroundCoverAccess for ChunkAccess<'_, '_> {
    fn test(&mut self, predicate: &BlockPredicate, pos: BlockPos) -> bool {
        let BlockPredicate::MatchingBlocks { blocks, offset } = predicate else {
            panic!("unsupported comparison predicate");
        };
        let (x, y, z) = self.relative(pos.offset(offset.x, offset.y, offset.z));
        let Some(state) = self.0.get_relative_block(x, y, z) else {
            panic!("outside retained comparison chunk");
        };
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
        let (x, y, z) = self.relative(pos);
        let Some(state) = self.0.get_relative_block(x, y, z) else {
            panic!("outside retained comparison chunk");
        };
        blocks.contains(&state.get_block())
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockStateId) {
        let (x, y, z) = self.relative(pos);
        self.0.set_relative_block(x, y, z, state);
    }
}

pub fn projected(world: &mut ChunkAccess<'_, '_>, random: &mut WorldgenRandom, origin: BlockPos) {
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

pub fn spread(world: &mut ChunkAccess<'_, '_>, random: &mut WorldgenRandom, origin: BlockPos) {
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
