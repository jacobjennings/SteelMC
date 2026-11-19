use glam::IVec3;
use smallvec::SmallVec;
use steel_registry::biome::BiomeRef;
use steel_registry::structure::StructureRef;
use steel_utils::{BoundingBox, ChunkPos};

use super::prelude::*;
use steel_utils::value_providers::HeightProvider;
use super::sorter::{FeatureSorter, FeatureStepData};
use crate::worldgen::structure::piece_placer::StructurePiecePlacer;
#[cfg(test)]
use steel_worldgen::structure::StructureReferenceMap;
use steel_worldgen::structure::StructureStart;

/// Runs the structure-piece and placed-feature decoration pass for a generator.
#[derive(Debug)]
pub(crate) struct FeatureDecorationRunner {
    pub(super) sorter: FeatureSorter,
    source_biome_lookup: Box<[bool]>,
    structures_by_step: Box<[Box<[StructureRef]>]>,
}

impl FeatureDecorationRunner {
    pub(super) const VANILLA_DIRECTION_VALUES: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];

    pub(super) const VANILLA_HORIZONTAL_DIRECTIONS: [Direction; 4] = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ];

    pub(super) fn random_horizontal_direction(random: &mut WorldgenRandom) -> Direction {
        Self::VANILLA_HORIZONTAL_DIRECTIONS[random.next_i32_bounded(4) as usize]
    }

    pub(super) fn random_direction(random: &mut WorldgenRandom) -> Direction {
        Self::VANILLA_DIRECTION_VALUES[random.next_i32_bounded(6) as usize]
    }

    pub(super) fn shuffled_directions<const N: usize>(
        random: &mut WorldgenRandom,
        mut directions: [Direction; N],
    ) -> [Direction; N] {
        for i in (1..N).rev() {
            let Ok(bound) = i32::try_from(i + 1) else {
                panic!("direction shuffle length {N} exceeds i32 range");
            };
            let j = random.next_i32_bounded(bound) as usize;
            directions.swap(i, j);
        }
        directions
    }

    pub(super) const fn manhattan_distance(left: BlockPos, right: BlockPos) -> i32 {
        Self::abs_diff(left.x(), right.x())
            + Self::abs_diff(left.y(), right.y())
            + Self::abs_diff(left.z(), right.z())
    }

    pub(super) fn for_each_vanilla_within_manhattan(
        origin: BlockPos,
        reach_x: i32,
        reach_y: i32,
        reach_z: i32,
        mut visitor: impl FnMut(BlockPos) -> bool,
    ) {
        let max_depth = reach_x + reach_y + reach_z;
        for current_depth in 0..=max_depth {
            let max_x = reach_x.min(current_depth);
            for x in -max_x..=max_x {
                let max_y = reach_y.min(current_depth - x.abs());
                for y in -max_y..=max_y {
                    let z = current_depth - x.abs() - y.abs();
                    if z > reach_z {
                        continue;
                    }

                    if !visitor(origin.offset(x, y, z)) {
                        return;
                    }

                    if z != 0 && !visitor(origin.offset(x, y, -z)) {
                        return;
                    }
                }
            }
        }
    }

    pub(super) fn for_each_vanilla_between_closed(
        min: BlockPos,
        max: BlockPos,
        mut visitor: impl FnMut(BlockPos),
    ) {
        let width = max.x() - min.x() + 1;
        let height = max.y() - min.y() + 1;
        let depth = max.z() - min.z() + 1;
        debug_assert!(width > 0 && height > 0 && depth > 0);

        let end = i64::from(width) * i64::from(height) * i64::from(depth);
        for index in 0..end {
            let x = (index % i64::from(width)) as i32;
            let slice = index / i64::from(width);
            let y = (slice % i64::from(height)) as i32;
            let z = (slice / i64::from(height)) as i32;
            visitor(BlockPos::new(min.x() + x, min.y() + y, min.z() + z));
        }
    }

    const fn abs_diff(left: i32, right: i32) -> i32 {
        if left >= right {
            left - right
        } else {
            right - left
        }
    }

    #[must_use]
    pub(crate) fn new(possible_biomes: &[BiomeRef], registry: &Registry) -> Self {
        let mut source_biome_ids = FxHashSet::default();
        let mut unique_biomes = Vec::new();
        let mut max_biome_id = 0;

        for &biome in possible_biomes {
            let Some(biome_id) = biome.try_id() else {
                panic!("possible biome {} is not registered", biome.key);
            };
            max_biome_id = max_biome_id.max(biome_id);

            if source_biome_ids.insert(biome_id) {
                unique_biomes.push(biome);
            }
        }

        let mut source_biome_lookup = vec![false; max_biome_id + 1].into_boxed_slice();
        for biome_id in source_biome_ids {
            source_biome_lookup[biome_id] = true;
        }

        let sorter = FeatureSorter::build(&unique_biomes, registry);
        let step_count = DECORATION_STEP_COUNT.max(sorter.step_count());
        let structures_by_step = (0..step_count)
            .map(|step| Self::structures_for_decoration_step(registry, step).into_boxed_slice())
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Self {
            sorter,
            source_biome_lookup,
            structures_by_step,
        }
    }

    pub(crate) fn decorate(
        &self,
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        seed: i64,
        biome_zoom_seed: i64,
    ) {
        let center = region.center();
        let origin = BlockPos::new(center.0.x * 16, region.min_y(), center.0.y * 16);
        let (possible_biomes, all_warm_for_freeze) =
            self.collect_possible_biomes_with_max_y(region, registry);

        let mut random = WorldgenRandom::from_seed(0);
        let decoration_seed = random.set_decoration_seed(seed, origin.x(), origin.z());
        let step_count = self.structures_by_step.len();
        let has_structures = !region
            .chunk(center.0.x, center.0.y, ChunkStatus::StructureStarts)
            .structure_references()
            .is_empty();

        for step in 0..step_count {
            if has_structures {
                self.place_structures_for_step_cached(
                    region,
                    registry,
                    decoration_seed,
                    &mut random,
                    step,
                    biome_zoom_seed,
                );
            }

            let Some(step_features) = self.sorter.step(step) else {
                continue;
            };
            Self::place_features_for_step_with_bounds(
                region,
                registry,
                decoration_seed,
                &mut random,
                origin,
                step,
                step_features,
                &possible_biomes,
                all_warm_for_freeze,
                biome_zoom_seed,
            );
        }
    }

    fn collect_possible_biomes_with_max_y(
        &self,
        region: &WorldGenRegion<'_>,
        registry: &Registry,
    ) -> (SmallVec<[(usize, i32); 16]>, bool) {
        let center = region.center();
        let lookup_len = self.source_biome_lookup.len();
        let mut max_block_y = SmallVec::<[i32; 128]>::from_elem(i32::MIN, lookup_len);
        let min_quart_y = region.min_y() >> 2;

        for chunk_z in center.0.y - 1..=center.0.y + 1 {
            for chunk_x in center.0.x - 1..=center.0.x + 1 {
                let chunk_ref = region.chunk(chunk_x, chunk_z, ChunkStatus::Biomes);
                let summary = chunk_ref.raw_chunk().biome_max_quart_y.get_or_init(|| {
                    let sections = chunk_ref.sections();
                    let section_count = sections.sections.len();
                    let mut per_chunk = SmallVec::<[i16; 128]>::from_elem(i16::MIN, lookup_len);
                    sections.for_each_biome_id_with_quart_y(|section_index, qy, biome_id| {
                        let idx = usize::from(biome_id);
                        if idx < per_chunk.len() {
                            let q_upper = if section_index + 1 >= section_count && qy >= 3 {
                                i16::MAX
                            } else {
                                (min_quart_y + (section_index as i32) * 4 + (qy as i32)) as i16
                            };
                            if q_upper > per_chunk[idx] {
                                per_chunk[idx] = q_upper;
                            }
                        }
                    });
                    let compact: Vec<(u16, i16)> = per_chunk
                        .iter()
                        .enumerate()
                        .filter_map(|(id, &qy)| (qy != i16::MIN).then_some((id as u16, qy)))
                        .collect();
                    std::sync::Arc::new(compact.into_boxed_slice())
                });
                for &(raw_id, max_qy) in summary.iter() {
                    let biome_id = usize::from(raw_id);
                    if self
                        .source_biome_lookup
                        .get(biome_id)
                        .copied()
                        .unwrap_or(false)
                    {
                        let upper_y = if max_qy == i16::MAX {
                            i32::MAX / 4
                        } else {
                            (i32::from(max_qy) << 2) + 8
                        };
                        if upper_y > max_block_y[biome_id] {
                            max_block_y[biome_id] = upper_y;
                        }
                    }
                }
            }
        }

        let mut biomes = SmallVec::<[(usize, i32); 16]>::new();
        let mut all_warm_for_freeze = true;
        for (biome_id, &upper_y) in max_block_y.iter().enumerate() {
            if upper_y != i32::MIN {
                biomes.push((biome_id, upper_y));
                if all_warm_for_freeze
                    && let Some(biome) = registry.biomes.by_id(biome_id)
                    && (biome.temperature < 0.62
                        || !matches!(biome.temperature_modifier, TemperatureModifier::None))
                {
                    all_warm_for_freeze = false;
                }
            }
        }
        (biomes, all_warm_for_freeze)
    }

    #[cfg(test)]
    pub(super) fn collect_possible_biome_ids(&self, region: &WorldGenRegion<'_>) -> Vec<usize> {
        let (biomes, _) = self.collect_possible_biomes_with_max_y(region, &REGISTRY);
        biomes.into_iter().map(|(id, _)| id).collect()
    }

    pub(super) fn structures_for_decoration_step(
        registry: &Registry,
        step: usize,
    ) -> Vec<StructureRef> {
        registry
            .structures
            .iter()
            .map(|(_, structure)| structure)
            .filter(|structure| structure.step.decoration_ordinal() == step)
            .collect()
    }

    pub(super) const fn center_chunk_writable_box(region: &WorldGenRegion<'_>) -> BoundingBox {
        Self::chunk_writable_box(region.center(), region.min_y(), region.max_y_exclusive())
    }

    pub(super) const fn chunk_writable_box(
        center: ChunkPos,
        min_y: i32,
        max_y_exclusive: i32,
    ) -> BoundingBox {
        let min_x = center.0.x * 16;
        let min_z = center.0.y * 16;
        BoundingBox::new(
            IVec3::new(min_x, min_y + 1, min_z),
            IVec3::new(min_x + 15, max_y_exclusive - 1, min_z + 15),
        )
    }

    #[cfg(test)]
    pub(super) fn resolve_structure_starts_from_references(
        references: &StructureReferenceMap,
        structure_id: &Identifier,
        mut start_lookup: impl FnMut(steel_utils::ChunkPos, &Identifier) -> Option<StructureStart>,
    ) -> Vec<StructureStart> {
        let Some(source_positions) = references.get(structure_id) else {
            return Vec::new();
        };

        let mut starts = Vec::new();
        for &source_pos in source_positions {
            let Some(start) = start_lookup(source_pos, structure_id) else {
                continue;
            };
            if start.chunk_pos == source_pos && !start.pieces.is_empty() {
                starts.push(start);
            }
        }
        starts
    }

    fn structure_source_positions_in_region(
        region: &WorldGenRegion<'_>,
        structure_id: &Identifier,
    ) -> Vec<steel_utils::ChunkPos> {
        let center = region.center();
        let center_chunk = region.chunk(center.0.x, center.0.y, ChunkStatus::StructureStarts);
        let references = center_chunk.structure_references();
        let source_positions = references
            .get(structure_id)
            .map(|positions| positions.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        drop(references);
        source_positions
    }

    fn place_structures_for_step_cached(
        &self,
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        decoration_seed: i64,
        random: &mut WorldgenRandom,
        step: usize,
        biome_zoom_seed: i64,
    ) {
        let center = region.center();
        let center_chunk = region.chunk(center.0.x, center.0.y, ChunkStatus::StructureStarts);
        if center_chunk.structure_references().is_empty() {
            return;
        }
        let Some(structures) = self.structures_by_step.get(step) else {
            return;
        };
        Self::place_structures_slice(
            region,
            registry,
            decoration_seed,
            random,
            step,
            structures,
            biome_zoom_seed,
        );
    }

    #[cfg(test)]
    pub(super) fn place_structures_for_step(
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        decoration_seed: i64,
        random: &mut WorldgenRandom,
        step: usize,
        biome_zoom_seed: i64,
    ) {
        let structures = Self::structures_for_decoration_step(registry, step);
        Self::place_structures_slice(
            region,
            registry,
            decoration_seed,
            random,
            step,
            &structures,
            biome_zoom_seed,
        );
    }

    fn place_structures_slice(
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        decoration_seed: i64,
        random: &mut WorldgenRandom,
        step: usize,
        structures: &[StructureRef],
        biome_zoom_seed: i64,
    ) {
        let writable_box = Self::center_chunk_writable_box(region);

        for (structure_index, &structure) in structures.iter().enumerate() {
            Self::set_structure_seed(random, decoration_seed, structure_index, step);

            let source_positions =
                Self::structure_source_positions_in_region(region, &structure.key);
            for source_pos in source_positions {
                let Some(source_chunk) =
                    region.try_chunk(source_pos.0.x, source_pos.0.y, ChunkStatus::StructureStarts)
                else {
                    continue;
                };
                let mut source_starts = source_chunk.structure_starts_mut();
                let Some(start) = source_starts.get_mut(&structure.key) else {
                    continue;
                };
                if start.chunk_pos != source_pos || start.pieces.is_empty() {
                    continue;
                }
                let Some(reference_pos) = start.placement_reference_pos() else {
                    continue;
                };
                for piece in &mut start.pieces {
                    if piece.bounding_box.intersects(writable_box) {
                        StructurePiecePlacer::place_piece(
                            region,
                            registry,
                            piece,
                            reference_pos,
                            writable_box,
                            random,
                            biome_zoom_seed,
                        );
                    }
                }
                StructurePiecePlacer::after_place_structure(
                    region,
                    structure,
                    &mut start.pieces,
                    writable_box,
                );
                start.bounding_box =
                    StructureStart::compute_bounding_box(&start.pieces, start.bb_inflate);
            }
        }
    }

    pub(super) fn set_structure_seed(
        random: &mut WorldgenRandom,
        decoration_seed: i64,
        structure_index: usize,
        step: usize,
    ) {
        let Ok(structure_index_i32) = i32::try_from(structure_index) else {
            panic!("structure index {structure_index} exceeds i32 range");
        };
        let Ok(step_i32) = i32::try_from(step) else {
            panic!("decoration step {step} exceeds i32 range");
        };
        random.set_feature_seed(decoration_seed, structure_index_i32, step_i32);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors vanilla's decoration loop state without hiding generation inputs"
    )]
    fn place_features_for_step_with_bounds(
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        decoration_seed: i64,
        random: &mut WorldgenRandom,
        origin: BlockPos,
        step: usize,
        step_features: &FeatureStepData,
        possible_biomes: &[(usize, i32)],
        all_warm_for_freeze: bool,
        biome_zoom_seed: i64,
    ) {
        let step_len = step_features.len();
        if step_len == 0 {
            return;
        }
        let mut max_biome_y = SmallVec::<[i32; 128]>::from_elem(i32::MIN, step_len);
        for &(biome_id, biome_max_y) in possible_biomes {
            if let Some(indices) = step_features.feature_indices_for_biome(biome_id) {
                for &idx in indices {
                    if biome_max_y > max_biome_y[idx] {
                        max_biome_y[idx] = biome_max_y;
                    }
                }
            }
        }

        let cutoff_y = region.surface_cutoff_y();
        let last_root_system_idx = if cutoff_y.is_some() {
            (0..step_len).rev().find(|&idx| {
                max_biome_y[idx] != i32::MIN
                    && step_features.feature(idx).is_some_and(|feat| {
                        matches!(
                            *Self::configured_feature_kind(&feat.data.feature),
                            ConfiguredFeatureKind::RootSystem(_)
                        )
                    })
            })
        } else {
            None
        };

        let gen_min_y = region.generation_min_y();
        let gen_height = region.generation_height();
        let sea_level = region.sea_level();
        let Ok(step_i32) = i32::try_from(step) else {
            panic!("decoration step {step} exceeds i32 range");
        };
        for (feature_index, &biome_upper_y) in max_biome_y.iter().enumerate() {
            if biome_upper_y == i32::MIN {
                continue;
            }
            let Some(feature) = step_features.feature(feature_index) else {
                panic!("decoration step {step} references missing feature index {feature_index}");
            };
            let kind = Self::configured_feature_kind(&feature.data.feature);
            if all_warm_for_freeze && matches!(*kind, ConfiguredFeatureKind::FreezeTopLayer) {
                continue;
            }
            if let Some(cutoff_y) = cutoff_y
                && last_root_system_idx.is_none_or(|last_rs| feature_index > last_rs)
                && Self::placed_feature_provably_below_cutoff(
                    &feature.data,
                    biome_upper_y,
                    gen_min_y,
                    gen_height,
                    sea_level,
                    cutoff_y,
                )
            {
                continue;
            }
            let Ok(feature_index_i32) = i32::try_from(feature_index) else {
                panic!("decoration feature index {feature_index} exceeds i32 range");
            };
            random.set_feature_seed(decoration_seed, feature_index_i32, step_i32);
            Self::place_placed_feature_entry(
                region,
                registry,
                random,
                origin,
                feature,
                biome_zoom_seed,
            );
        }
    }

    fn placed_feature_provably_below_cutoff(
        feature: &PlacedFeatureData,
        biome_upper_y: i32,
        gen_min_y: i32,
        gen_height: i32,
        sea_level: i32,
        cutoff_y: i32,
    ) -> bool {
        let Some(reach) = Self::configured_feature_max_upward_reach(Self::configured_feature_kind(&feature.feature)) else {
            return false;
        };
        let mut max_y = gen_min_y;
        for modifier in &feature.placement {
            match modifier {
                PlacementModifier::Count { .. }
                | PlacementModifier::RarityFilter { .. }
                | PlacementModifier::RandomChance { .. }
                | PlacementModifier::InSquare
                | PlacementModifier::NoiseBasedCount { .. }
                | PlacementModifier::NoiseThresholdCount { .. }
                | PlacementModifier::BlockPredicateFilter { .. }
                | PlacementModifier::SurfaceWaterDepthFilter { .. }
                | PlacementModifier::SurfaceRelativeThresholdFilter { .. } => {}
                PlacementModifier::Biome => {
                    max_y = max_y.min(biome_upper_y);
                }
                PlacementModifier::HeightRange { height } => {
                    max_y = Self::max_height_provider_y(*height, gen_min_y, gen_height, sea_level);
                }
                PlacementModifier::Offset { y, .. }
                | PlacementModifier::RandomOffset { y_spread: y, .. } => {
                    max_y = max_y.saturating_add(y.max().max(0));
                }
                PlacementModifier::EnvironmentScan {
                    direction_of_search,
                    max_steps,
                    ..
                } => {
                    if *direction_of_search != Direction::Down {
                        max_y = max_y.saturating_add(*max_steps);
                    }
                }
                PlacementModifier::Heightmap { .. }
                | PlacementModifier::CountOnEveryLayer { .. }
                | PlacementModifier::FixedPlacement { .. }
                | PlacementModifier::Cuboid { .. }
                | PlacementModifier::RandomlySelected { .. } => {
                    return false;
                }
            }
        }
        max_y.saturating_add(reach) < cutoff_y
    }

    fn configured_feature_max_upward_reach(kind: &ConfiguredFeatureKind) -> Option<i32> {
        match kind {
            ConfiguredFeatureKind::Ore(c) | ConfiguredFeatureKind::ScatteredOre(c) => {
                Some(4 + ((c.size as f32 / 8.0 + 1.0).ceil() as i32))
            }
            ConfiguredFeatureKind::Geode(c) => Some(c.max_gen_offset.saturating_add(34)),
            ConfiguredFeatureKind::SculkPatch(_) => Some(46),
            ConfiguredFeatureKind::MultifaceGrowth(c) => Some(c.search_range.saturating_add(34)),
            ConfiguredFeatureKind::MonsterRoom => Some(38),
            ConfiguredFeatureKind::SpringFeature(_) => Some(34),
            ConfiguredFeatureKind::Disk(c) => Some(c.half_height.saturating_add(34)),
            ConfiguredFeatureKind::VegetationPatch(c)
            | ConfiguredFeatureKind::WaterloggedVegetationPatch(c) => {
                Some(c.vertical_range.saturating_add(38))
            }
            ConfiguredFeatureKind::BlockColumn(c) => {
                (c.direction == Direction::Down).then_some(36)
            }
            ConfiguredFeatureKind::Vines | ConfiguredFeatureKind::WeepingVines => Some(34),
            ConfiguredFeatureKind::PointedDripstone(_) => Some(36),
            ConfiguredFeatureKind::DripstoneCluster(c) => Some(
                c.floor_to_ceiling_search_range
                    .saturating_add(c.dripstone_block_layer_thickness.max())
                    .saturating_add(c.height.max())
                    .saturating_add(36),
            ),
            _ => None,
        }
    }

    fn max_height_provider_y(
        provider: HeightProvider,
        min_y: i32,
        height: i32,
        sea_level: i32,
    ) -> i32 {
        match provider {
            HeightProvider::Constant(anchor) => anchor.resolve_y(min_y, height, sea_level),
            HeightProvider::Uniform {
                min_inclusive,
                max_inclusive,
            }
            | HeightProvider::Trapezoid {
                min_inclusive,
                max_inclusive,
                ..
            }
            | HeightProvider::BiasedToBottom {
                min_inclusive,
                max_inclusive,
                ..
            }
            | HeightProvider::VeryBiasedToBottom {
                min_inclusive,
                max_inclusive,
                ..
            } => {
                let min = min_inclusive.resolve_y(min_y, height, sea_level);
                let max = max_inclusive.resolve_y(min_y, height, sea_level);
                min.max(max)
            }
        }
    }

    #[cfg(test)]
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors vanilla's decoration loop state without hiding generation inputs"
    )]
    pub(super) fn place_features_for_step(
        region: &mut WorldGenRegion<'_>,
        registry: &Registry,
        decoration_seed: i64,
        random: &mut WorldgenRandom,
        origin: BlockPos,
        step: usize,
        step_features: &FeatureStepData,
        possible_biomes: &[usize],
        biome_zoom_seed: i64,
    ) {
        let mut feature_indices = SmallVec::<[usize; 64]>::new();

        for &biome_id in possible_biomes {
            if let Some(indices) = step_features.feature_indices_for_biome(biome_id) {
                feature_indices.extend_from_slice(indices);
            }
        }

        feature_indices.sort_unstable();
        feature_indices.dedup();

        for feature_index in feature_indices {
            let Ok(feature_index_i32) = i32::try_from(feature_index) else {
                panic!("decoration feature index {feature_index} exceeds i32 range");
            };
            let Ok(step_i32) = i32::try_from(step) else {
                panic!("decoration step {step} exceeds i32 range");
            };
            let Some(feature) = step_features.feature(feature_index) else {
                panic!("decoration step {step} references missing feature index {feature_index}");
            };
            random.set_feature_seed(decoration_seed, feature_index_i32, step_i32);
            Self::place_placed_feature_entry(
                region,
                registry,
                random,
                origin,
                feature,
                biome_zoom_seed,
            );
        }
    }
}
