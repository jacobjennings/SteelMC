//! Server binding for the shared portable ground-cover algorithms.

use super::super::prelude::*;
use super::super::runner::FeatureDecorationRunner;
use steel_worldgen::portable_feature::GroundCoverAccess;

pub(super) struct GroundCoverRegion<'a, 'world> {
    pub region: &'a mut WorldGenRegion<'world>,
    pub registry: &'a Registry,
}

impl GroundCoverAccess for GroundCoverRegion<'_, '_> {
    fn test(&mut self, predicate: &BlockPredicate, pos: BlockPos) -> bool {
        FeatureDecorationRunner::test_block_predicate(self.region, self.registry, predicate, pos)
    }

    fn state(
        &mut self,
        provider: &BlockStateProviderKind,
        random: &mut WorldgenRandom,
        pos: BlockPos,
    ) -> BlockStateId {
        FeatureDecorationRunner::sample_block_state_provider(
            self.region,
            self.registry,
            random,
            provider,
            pos,
        )
    }

    fn optional_state(
        &mut self,
        provider: &BlockStateProviderKind,
        random: &mut WorldgenRandom,
        pos: BlockPos,
    ) -> Option<BlockStateId> {
        FeatureDecorationRunner::sample_block_state_provider_optional(
            self.region,
            self.registry,
            random,
            provider,
            pos,
        )
    }

    fn accepted_neighbor(&mut self, accepted: &BlockHolderSet, pos: BlockPos) -> bool {
        FeatureDecorationRunner::block_matches_holder_set(
            self.region.block_state(pos).get_block(),
            accepted,
        )
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockStateId) {
        let _ = self
            .region
            .set_block_state(pos, state, UpdateFlags::UPDATE_CLIENTS);
    }
}
