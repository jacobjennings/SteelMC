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
        match predicate {
            BlockPredicate::MatchingBlocks { blocks, offset } => {
                let state = self.0.read(pos.offset(offset.x, offset.y, offset.z));
                blocks.0.contains(&state.get_block())
            }
            BlockPredicate::MatchingBlockTag { tag, offset } => self
                .0
                .read(pos.offset(offset.x, offset.y, offset.z))
                .get_block()
                .has_tag(tag),
            BlockPredicate::Not { predicate } => !GroundCoverAccess::test(self, predicate, pos),
            _ => panic!("unsupported comparison predicate"),
        }
    }

    fn state(
        &mut self,
        provider: &BlockStateProviderKind,
        random: &mut WorldgenRandom,
        pos: BlockPos,
    ) -> BlockStateId {
        match portable_feature::providers::state(self, &REGISTRY, provider, random, pos) {
            Ok(state) => state,
            Err(error) => panic!("{error}"),
        }
    }

    fn optional_state(
        &mut self,
        provider: &BlockStateProviderKind,
        random: &mut WorldgenRandom,
        pos: BlockPos,
    ) -> Option<BlockStateId> {
        match portable_feature::providers::optional_state(self, &REGISTRY, provider, random, pos) {
            Ok(state) => state,
            Err(error) => panic!("{error}"),
        }
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
    providers: bool,
) {
    let config = ProjectedRandomPatchySquareConfiguration {
        block: if providers {
            optional_weighted()
        } else {
            simple(&vanilla_blocks::SHORT_GRASS)
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
    providers: bool,
) {
    let config = RandomNeighborSpreadConfiguration {
        block: if providers {
            weighted(
                64,
                &vanilla_blocks::OAK_LEAVES,
                &vanilla_blocks::BIRCH_LEAVES,
            )
        } else {
            simple(&vanilla_blocks::OAK_LEAVES)
        },
        accepted_neighbors: BlockHolderSet::Entries(if providers {
            vec![&vanilla_blocks::OAK_LEAVES, &vanilla_blocks::BIRCH_LEAVES]
        } else {
            vec![&vanilla_blocks::OAK_LEAVES]
        }),
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

impl<W: BlockStorage> portable_feature::providers::ProviderAccess for ChunkAccess<'_, W> {
    fn test(&mut self, predicate: &BlockPredicate, pos: BlockPos) -> Result<bool, &'static str> {
        Ok(GroundCoverAccess::test(self, predicate, pos))
    }

    fn block(&mut self, pos: BlockPos) -> Result<BlockStateId, &'static str> {
        Ok(self.0.read(pos))
    }
}

fn simple(block: steel_registry::blocks::BlockRef) -> BlockStateProviderKind {
    BlockStateProviderKind::Simple {
        state: BlockStateData {
            block,
            properties: &[],
        },
    }
}

fn weighted(
    total: i32,
    first: steel_registry::blocks::BlockRef,
    second: steel_registry::blocks::BlockRef,
) -> BlockStateProviderKind {
    BlockStateProviderKind::Weighted {
        entries: vec![
            WeightedBlockState {
                data: BlockStateData {
                    block: &vanilla_blocks::STONE,
                    properties: &[],
                },
                weight: 0,
            },
            WeightedBlockState {
                data: BlockStateData {
                    block: first,
                    properties: &[],
                },
                weight: (total / 3).max(1),
            },
            WeightedBlockState {
                data: BlockStateData {
                    block: second,
                    properties: &[],
                },
                weight: total - (total / 3).max(1),
            },
        ],
    }
}

fn decline() -> BlockStateProviderKind {
    BlockStateProviderKind::RuleBased {
        fallback: None,
        rules: vec![],
    }
}

fn optional_weighted() -> BlockStateProviderKind {
    BlockStateProviderKind::RuleBased {
        fallback: None,
        rules: vec![
            RuleBasedStateProviderRule {
                if_true: air(),
                then: decline(),
            },
            RuleBasedStateProviderRule {
                if_true: air(),
                then: weighted(3, &vanilla_blocks::SHORT_GRASS, &vanilla_blocks::FERN),
            },
        ],
    }
}

// Direct readbacks exercise refusal, nested continuation, fallback and both
// native weighted selector sizes. They do not write or select biome features.
pub fn provider_readbacks<W: BlockStorage>(
    world: &mut ChunkAccess<'_, W>,
    seed: u64,
    pos: BlockPos,
) -> serde_json::Value {
    use steel_utils::random::Random as _;
    let mut random = WorldgenRandom::from_seed(seed);
    let providers = [
        BlockStateProviderKind::Reference(
            &steel_registry::vanilla_block_state_providers::SOIL_BENEATH_TREE,
        ),
        decline(),
        optional_weighted(),
        BlockStateProviderKind::RuleBased {
            fallback: Some(Box::new(simple(&vanilla_blocks::DIRT))),
            rules: vec![],
        },
        weighted(1, &vanilla_blocks::SHORT_GRASS, &vanilla_blocks::FERN),
        weighted(3, &vanilla_blocks::SHORT_GRASS, &vanilla_blocks::FERN),
        weighted(
            64,
            &vanilla_blocks::OAK_LEAVES,
            &vanilla_blocks::BIRCH_LEAVES,
        ),
    ];
    let mut samples = Vec::new();
    for provider in providers {
        for _ in 0..12 {
            let optional = world.optional_state(&provider, &mut random, pos);
            let required = world.state(&provider, &mut random, pos);
            samples.push(serde_json::json!({
                "optional": optional.map(|state| state.get_block().key.to_string()),
                "required": required.get_block().key.to_string(),
            }));
        }
    }
    serde_json::json!({"samples": samples, "nextInt": random.next_i32()})
}
