use super::super::prelude::*;
use super::super::runner::FeatureDecorationRunner;
use super::ground_cover::GroundCoverRegion;

impl FeatureDecorationRunner {
    pub(in crate::worldgen::feature) fn place_random_neighbor_spread_feature(
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        random: &mut WorldgenRandom,
        config: &RandomNeighborSpreadConfiguration,
        origin: BlockPos,
    ) -> bool {
        steel_worldgen::portable_feature::random_neighbor_spread(
            &mut GroundCoverRegion { region, registry },
            random,
            config,
            origin,
        )
    }
}
