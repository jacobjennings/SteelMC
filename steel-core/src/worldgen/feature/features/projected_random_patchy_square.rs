use super::super::prelude::*;
use super::super::runner::FeatureDecorationRunner;
use super::ground_cover::GroundCoverRegion;

impl FeatureDecorationRunner {
    pub(in crate::worldgen::feature) fn place_projected_random_patchy_square_feature(
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        random: &mut WorldgenRandom,
        config: &ProjectedRandomPatchySquareConfiguration,
        origin: BlockPos,
    ) -> bool {
        steel_worldgen::portable_feature::projected_random_patchy_square(
            &mut GroundCoverRegion { region, registry },
            random,
            config,
            origin,
        )
    }
}
