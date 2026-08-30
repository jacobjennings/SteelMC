//! Portable, sparse vegetation placement.
//!
//! This module intentionally owns the small subset of the vanilla Features
//! stage needed by static terrain consumers.  It keeps the real registry
//! placed-feature order and modifier stream, but delegates mutable world state
//! to [`VegetationBlockAccess`].  Native chunk generation can therefore keep
//! its richer chunk/status host while WASM uses an in-memory terrain halo.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use rustc_hash::{FxHashMap, FxHashSet};
use steel_registry::biome::BiomeRef;
use steel_registry::blocks::{
    block_state_ext::BlockStateExt as _,
    properties::{BlockStateProperties, DoubleBlockHalf},
};
use steel_registry::feature::{
    BlockPredicate, CherryFoliagePlacer, CherryTrunkPlacer, ConfiguredFeatureKind,
    ConfiguredFeatureRef, FeatureHeightmap, FeatureSize, FoliagePlacer, PlacedFeatureData,
    PlacedFeatureEntryRef, PlacementModifier, TreeConfiguration, TreeDecorator, TrunkPlacer,
};
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{Registry, RegistryEntry as _, RegistryExt as _, vanilla_blocks};
use steel_utils::axis::Axis;
use steel_utils::random::{
    Random as _, RandomSource, legacy_random::LegacyRandom, worldgen_random::WorldgenRandom,
};
use steel_utils::{BlockPos, BlockStateId, Direction};

use crate::biome_zoom::fuzzed_biome_at_block;
use crate::noise::PerlinSimplexNoise;
use crate::state_resolver::WorldgenStateResolver;

/// A minimal mutable terrain view for the portable Features slice.
///
/// Implementors must expose the same post-Carvers block state and heightmap
/// inputs that native feature placement receives.  The stage writes directly
/// through this interface so later placements observe earlier sparse blocks.
pub trait VegetationBlockAccess {
    /// First writable block Y.
    fn min_y(&self) -> i32;
    /// First block Y outside the writable range.
    fn max_y_exclusive(&self) -> i32;
    /// Current state at a world position. Outside the supplied halo is air.
    fn block_state(&self, pos: BlockPos) -> BlockStateId;
    /// Writes a generated state at a world position inside the supplied halo.
    fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId);
    /// Exact feature heightmap lookup.
    fn height_at(&self, kind: FeatureHeightmap, x: i32, z: i32) -> i32;
    /// Unfuzzed noise-biome id at a quart position.
    fn biome_id_at_quart(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> u16;
}

/// One final sparse state write from the vegetation stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VegetationBlock {
    /// World X coordinate.
    pub x: i32,
    /// World Y coordinate.
    pub y: i32,
    /// World Z coordinate.
    pub z: i32,
    /// Final registry state id.
    pub state: BlockStateId,
}

impl VegetationBlock {
    /// Creates one final sparse block write at `pos`.
    #[must_use]
    pub const fn at(pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            x: pos.x(),
            y: pos.y(),
            z: pos.z(),
            state,
        }
    }

    /// Returns this write's absolute world position.
    #[must_use]
    pub const fn pos(self) -> BlockPos {
        BlockPos::new(self.x, self.y, self.z)
    }
}

/// Cached vanilla ordering for all placed features reachable from a biome source.
///
/// This is shared with the native runner so the `feature_index` used by
/// `WorldgenRandom.set_feature_seed` remains identical in portable hosts.
#[derive(Debug)]
pub struct FeatureSorter {
    steps: Box<[FeatureStepData]>,
}

/// One decoration step's globally sorted features and biome membership map.
#[derive(Debug)]
pub struct FeatureStepData {
    features: Box<[PlacedFeatureEntryRef]>,
    index_by_placed_feature_id: FxHashMap<usize, usize>,
    feature_indices_by_biome_id: FxHashMap<usize, Box<[usize]>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FeatureVertex {
    step: usize,
    order: usize,
    placed_feature_id: usize,
}

impl Ord for FeatureVertex {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.step, self.order, self.placed_feature_id).cmp(&(
            other.step,
            other.order,
            other.placed_feature_id,
        ))
    }
}

impl PartialOrd for FeatureVertex {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FeatureSorter {
    /// Builds the source-global placed-feature ordering.
    #[must_use]
    pub fn build(possible_biomes: &[BiomeRef], registry: &Registry) -> Self {
        let mut feature_order_by_id = FxHashMap::default();
        let mut next_feature_order = 0usize;
        let mut edges = BTreeMap::<FeatureVertex, BTreeSet<FeatureVertex>>::new();

        for biome in possible_biomes {
            let mut biome_features = Vec::new();
            for (step, feature_stage) in biome.features.iter().enumerate() {
                for feature_key in feature_stage {
                    let Some(placed_feature_id) = registry.placed_features.id_from_key(feature_key)
                    else {
                        panic!(
                            "biome {} references unknown placed feature {}",
                            biome.key, feature_key
                        );
                    };
                    let feature_order =
                        if let Some(&order) = feature_order_by_id.get(&placed_feature_id) {
                            order
                        } else {
                            let order = next_feature_order;
                            next_feature_order += 1;
                            feature_order_by_id.insert(placed_feature_id, order);
                            order
                        };
                    let vertex = FeatureVertex {
                        step,
                        order: feature_order,
                        placed_feature_id,
                    };
                    edges.entry(vertex).or_default();
                    biome_features.push(vertex);
                }
            }
            for pair in biome_features.windows(2) {
                edges.entry(pair[0]).or_default().insert(pair[1]);
            }
        }

        let sorted_features = Self::topological_sort(&edges);
        Self::from_sorted_features(&sorted_features, possible_biomes, registry)
    }

    #[must_use]
    /// Number of decoration steps represented by this ordering.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    #[must_use]
    /// Returns sorted placed-feature data for one decoration step.
    pub fn step(&self, step: usize) -> Option<&FeatureStepData> {
        self.steps.get(step)
    }

    fn topological_sort(
        edges: &BTreeMap<FeatureVertex, BTreeSet<FeatureVertex>>,
    ) -> Vec<FeatureVertex> {
        let mut sorted = Vec::with_capacity(edges.len());
        let mut discovered = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        let vertices = edges.keys().copied().collect::<Vec<_>>();
        for vertex in vertices {
            assert!(
                !Self::visit(vertex, edges, &mut discovered, &mut visiting, &mut sorted),
                "biome decoration placed-feature order contains a cycle"
            );
        }
        sorted.reverse();
        sorted
    }

    fn visit(
        vertex: FeatureVertex,
        edges: &BTreeMap<FeatureVertex, BTreeSet<FeatureVertex>>,
        discovered: &mut BTreeSet<FeatureVertex>,
        visiting: &mut BTreeSet<FeatureVertex>,
        sorted: &mut Vec<FeatureVertex>,
    ) -> bool {
        if discovered.contains(&vertex) {
            return false;
        }
        if !visiting.insert(vertex) {
            return true;
        }
        if let Some(neighbors) = edges.get(&vertex) {
            for &neighbor in neighbors {
                if Self::visit(neighbor, edges, discovered, visiting, sorted) {
                    return true;
                }
            }
        }
        visiting.remove(&vertex);
        discovered.insert(vertex);
        sorted.push(vertex);
        false
    }

    fn from_sorted_features(
        sorted_features: &[FeatureVertex],
        possible_biomes: &[BiomeRef],
        registry: &Registry,
    ) -> Self {
        let Some(max_step) = sorted_features.iter().map(|feature| feature.step).max() else {
            return Self {
                steps: Box::new([]),
            };
        };
        let mut steps = Vec::with_capacity(max_step + 1);
        for step in 0..=max_step {
            let mut features = Vec::new();
            let mut index_by_placed_feature_id = FxHashMap::default();
            for feature in sorted_features
                .iter()
                .filter(|feature| feature.step == step)
            {
                let Some(placed_feature) =
                    registry.placed_features.by_id(feature.placed_feature_id)
                else {
                    panic!(
                        "feature sorter references unknown placed feature id {}",
                        feature.placed_feature_id
                    );
                };
                let index = features.len();
                features.push(placed_feature);
                index_by_placed_feature_id.insert(feature.placed_feature_id, index);
            }
            steps.push(FeatureStepData {
                features: features.into_boxed_slice(),
                index_by_placed_feature_id,
                feature_indices_by_biome_id: FxHashMap::default(),
            });
        }
        for biome in possible_biomes {
            let Some(biome_id) = biome.try_id() else {
                panic!("possible biome {} is not registered", biome.key);
            };
            for (step, feature_stage) in biome.features.iter().enumerate() {
                let Some(step_data) = steps.get_mut(step) else {
                    continue;
                };
                let mut indices = Vec::with_capacity(feature_stage.len());
                for feature_key in feature_stage {
                    let Some(placed_feature_id) = registry.placed_features.id_from_key(feature_key)
                    else {
                        panic!(
                            "biome {} references unknown placed feature {}",
                            biome.key, feature_key
                        );
                    };
                    let Some(feature_index) = step_data.feature_index(placed_feature_id) else {
                        panic!(
                            "placed feature {} from biome {} was not included in decoration step {}",
                            feature_key, biome.key, step
                        );
                    };
                    indices.push(feature_index);
                }
                if indices.is_empty() {
                    continue;
                }
                indices.sort_unstable();
                indices.dedup();
                step_data
                    .feature_indices_by_biome_id
                    .insert(biome_id, indices.into_boxed_slice());
            }
        }
        Self {
            steps: steps.into_boxed_slice(),
        }
    }
}

impl FeatureStepData {
    fn feature_index(&self, placed_feature_id: usize) -> Option<usize> {
        self.index_by_placed_feature_id
            .get(&placed_feature_id)
            .copied()
    }

    #[must_use]
    /// Returns the placed feature at its native decoration index.
    pub fn feature(&self, index: usize) -> Option<PlacedFeatureEntryRef> {
        self.features.get(index).copied()
    }

    #[must_use]
    /// Returns native decoration indices reachable from one biome id.
    pub fn feature_indices_for_biome(&self, biome_id: usize) -> Option<&[usize]> {
        self.feature_indices_by_biome_id
            .get(&biome_id)
            .map(Box::as_ref)
    }
}

/// Portable Features-stage state for sparse vegetation output.
#[derive(Debug)]
pub struct VegetationStage {
    sorter: FeatureSorter,
    source_biome_lookup: Box<[bool]>,
    seed: i64,
    biome_zoom_seed: i64,
}

impl VegetationStage {
    /// Creates a portable stage with native source-global feature ordering.
    #[must_use]
    pub fn new(
        seed: i64,
        biome_zoom_seed: i64,
        possible_biomes: &[BiomeRef],
        registry: &Registry,
    ) -> Self {
        let mut source_biome_ids = rustc_hash::FxHashSet::default();
        let mut unique_biomes = Vec::new();
        let mut max_biome_id = 0usize;
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
        Self {
            sorter: FeatureSorter::build(&unique_biomes, registry),
            source_biome_lookup,
            seed,
            biome_zoom_seed,
        }
    }

    /// World seed used for native decoration and feature reseeding.
    #[must_use]
    pub(crate) const fn seed(&self) -> i64 {
        self.seed
    }

    /// Decorates one source chunk and returns final sparse vegetation writes.
    ///
    /// The caller supplies a 3×3 post-Carvers terrain halo.  Writes outside the
    /// central chunk are deliberate: native Features has a write radius of one
    /// chunk and tree crowns may cross chunk boundaries.
    #[must_use]
    pub fn decorate_chunk<H: VegetationBlockAccess>(
        &self,
        host: &mut H,
        registry: &Registry,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Vec<VegetationBlock> {
        let origin = BlockPos::new(chunk_x * 16, host.min_y(), chunk_z * 16);
        let possible_biomes = self.collect_possible_biome_ids(host, chunk_x, chunk_z);
        let mut random = WorldgenRandom::from_seed(0);
        let decoration_seed = random.set_decoration_seed(self.seed, origin.x(), origin.z());
        let mut writes = Vec::new();
        for step in 0..self.sorter.step_count() {
            let Some(step_features) = self.sorter.step(step) else {
                continue;
            };
            let mut indices = Vec::new();
            for &biome_id in &possible_biomes {
                if let Some(feature_indices) = step_features.feature_indices_for_biome(biome_id) {
                    indices.extend_from_slice(feature_indices);
                }
            }
            indices.sort_unstable();
            indices.dedup();
            for feature_index in indices {
                let Some(feature) = step_features.feature(feature_index) else {
                    panic!(
                        "vegetation step {step} references missing feature index {feature_index}"
                    );
                };
                if !is_sparse_vegetation_feature(feature) {
                    continue;
                }
                random.set_feature_seed(decoration_seed, feature_index as i32, step as i32);
                let _ = self.place_placed_feature(
                    host,
                    registry,
                    &mut random,
                    origin,
                    feature,
                    &mut writes,
                    0,
                );
            }
        }
        writes.sort_unstable_by_key(|block| (block.x, block.y, block.z));
        writes.dedup_by_key(|block| (block.x, block.y, block.z));
        writes
    }

    fn collect_possible_biome_ids<H: VegetationBlockAccess>(
        &self,
        host: &mut H,
        center_chunk_x: i32,
        center_chunk_z: i32,
    ) -> Vec<usize> {
        let mut seen = vec![false; self.source_biome_lookup.len()];
        let mut biomes = Vec::new();
        let min_quart_y = host.min_y() >> 2;
        let max_quart_y = (host.max_y_exclusive() - 1) >> 2;
        for chunk_z in center_chunk_z - 1..=center_chunk_z + 1 {
            for chunk_x in center_chunk_x - 1..=center_chunk_x + 1 {
                for quart_y in min_quart_y..=max_quart_y {
                    for local_quart_z in 0..4 {
                        for local_quart_x in 0..4 {
                            let biome_id = usize::from(host.biome_id_at_quart(
                                chunk_x * 4 + local_quart_x,
                                quart_y,
                                chunk_z * 4 + local_quart_z,
                            ));
                            if self
                                .source_biome_lookup
                                .get(biome_id)
                                .copied()
                                .unwrap_or(false)
                                && !seen[biome_id]
                            {
                                seen[biome_id] = true;
                                biomes.push(biome_id);
                            }
                        }
                    }
                }
            }
        }
        biomes.sort_unstable();
        biomes
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors vanilla modifier stream state"
    )]
    fn place_placed_feature<H: VegetationBlockAccess>(
        &self,
        host: &mut H,
        registry: &Registry,
        random: &mut WorldgenRandom,
        origin: BlockPos,
        feature: PlacedFeatureEntryRef,
        writes: &mut Vec<VegetationBlock>,
        modifier_index: usize,
    ) -> bool {
        let Some(modifier) = feature.data.placement.get(modifier_index) else {
            return self.place_configured_feature(
                host,
                registry,
                random,
                origin,
                &feature.data,
                writes,
            );
        };
        match modifier {
            PlacementModifier::Biome => {
                self.biome_allows_feature(host, registry, origin, &feature.key)
                    && self.place_placed_feature(
                        host,
                        registry,
                        random,
                        origin,
                        feature,
                        writes,
                        modifier_index + 1,
                    )
            }
            PlacementModifier::BlockPredicateFilter { predicate } => {
                self.test_block_predicate(host, registry, predicate, origin)
                    && self.place_placed_feature(
                        host,
                        registry,
                        random,
                        origin,
                        feature,
                        writes,
                        modifier_index + 1,
                    )
            }
            PlacementModifier::Count { count } => {
                let mut placed = false;
                if let Ok(count) = usize::try_from(count.sample(random)) {
                    for _ in 0..count {
                        placed |= self.place_placed_feature(
                            host,
                            registry,
                            random,
                            origin,
                            feature,
                            writes,
                            modifier_index + 1,
                        );
                    }
                }
                placed
            }
            PlacementModifier::InSquare => {
                let x = origin.x() + random.next_i32_bounded(16);
                let z = origin.z() + random.next_i32_bounded(16);
                self.place_placed_feature(
                    host,
                    registry,
                    random,
                    BlockPos::new(x, origin.y(), z),
                    feature,
                    writes,
                    modifier_index + 1,
                )
            }
            PlacementModifier::Heightmap { heightmap } => {
                let height = host.height_at(*heightmap, origin.x(), origin.z());
                height > host.min_y()
                    && self.place_placed_feature(
                        host,
                        registry,
                        random,
                        BlockPos::new(origin.x(), height, origin.z()),
                        feature,
                        writes,
                        modifier_index + 1,
                    )
            }
            PlacementModifier::NoiseThresholdCount {
                noise_level,
                below_noise,
                above_noise,
            } => {
                let noise = biome_info_noise_value(
                    f64::from(origin.x()) / 200.0,
                    f64::from(origin.z()) / 200.0,
                );
                let count = if noise < *noise_level {
                    *below_noise
                } else {
                    *above_noise
                };
                let mut placed = false;
                if let Ok(count) = usize::try_from(count) {
                    for _ in 0..count {
                        placed |= self.place_placed_feature(
                            host,
                            registry,
                            random,
                            origin,
                            feature,
                            writes,
                            modifier_index + 1,
                        );
                    }
                }
                placed
            }
            PlacementModifier::RandomOffset {
                xz_spread,
                y_spread,
            } => {
                let x = origin.x() + xz_spread.sample(random);
                let y = origin.y() + y_spread.sample(random);
                let z = origin.z() + xz_spread.sample(random);
                self.place_placed_feature(
                    host,
                    registry,
                    random,
                    BlockPos::new(x, y, z),
                    feature,
                    writes,
                    modifier_index + 1,
                )
            }
            PlacementModifier::RarityFilter { chance } => {
                assert!(
                    *chance > 0,
                    "rarity filter chance must be positive, got {chance}"
                );
                random.next_f32() < 1.0 / (*chance as f32)
                    && self.place_placed_feature(
                        host,
                        registry,
                        random,
                        origin,
                        feature,
                        writes,
                        modifier_index + 1,
                    )
            }
            PlacementModifier::SurfaceWaterDepthFilter { max_water_depth } => {
                let ocean_floor =
                    host.height_at(FeatureHeightmap::OceanFloor, origin.x(), origin.z());
                let surface =
                    host.height_at(FeatureHeightmap::WorldSurface, origin.x(), origin.z());
                surface - ocean_floor <= *max_water_depth
                    && self.place_placed_feature(
                        host,
                        registry,
                        random,
                        origin,
                        feature,
                        writes,
                        modifier_index + 1,
                    )
            }
            unsupported => panic!(
                "sparse vegetation feature {} uses unsupported modifier {unsupported:?}",
                feature.key
            ),
        }
    }

    fn place_configured_feature<H: VegetationBlockAccess>(
        &self,
        host: &mut H,
        registry: &Registry,
        random: &mut WorldgenRandom,
        origin: BlockPos,
        feature: &PlacedFeatureData,
        writes: &mut Vec<VegetationBlock>,
    ) -> bool {
        let configured = match &feature.feature {
            ConfiguredFeatureRef::Reference(configured) => &configured.kind,
            ConfiguredFeatureRef::Inline(configured) => configured,
        };
        match configured {
            ConfiguredFeatureKind::SimpleBlock(config) => {
                let Some(state) = sample_provider(host, registry, random, &config.to_place, origin)
                else {
                    return false;
                };
                if !can_survive(host, state, origin) {
                    return false;
                }
                if state.get_block() == &vanilla_blocks::TALL_GRASS {
                    if !host.block_state(origin.above()).is_air() {
                        return false;
                    }
                    write_block(
                        host,
                        writes,
                        origin,
                        state.set_value(
                            &BlockStateProperties::DOUBLE_BLOCK_HALF,
                            DoubleBlockHalf::Lower,
                        ),
                    );
                    write_block(
                        host,
                        writes,
                        origin.above(),
                        state.set_value(
                            &BlockStateProperties::DOUBLE_BLOCK_HALF,
                            DoubleBlockHalf::Upper,
                        ),
                    );
                } else {
                    write_block(host, writes, origin, state);
                }
                true
            }
            ConfiguredFeatureKind::Tree(config) => {
                place_cherry_tree(host, registry, random, config, origin, writes)
            }
            _ => false,
        }
    }

    fn biome_allows_feature<H: VegetationBlockAccess>(
        &self,
        host: &mut H,
        registry: &Registry,
        origin: BlockPos,
        feature_key: &steel_utils::Identifier,
    ) -> bool {
        let biome_id = fuzzed_biome_at_block(self.biome_zoom_seed, origin, |quart| {
            host.biome_id_at_quart(quart.x, quart.y, quart.z)
        });
        let Some(biome) = registry.biomes.by_id(usize::from(biome_id)) else {
            panic!("biome filter resolved unknown biome id {biome_id}");
        };
        biome
            .features
            .iter()
            .flatten()
            .any(|key| key == feature_key)
    }

    fn test_block_predicate<H: VegetationBlockAccess>(
        &self,
        host: &mut H,
        registry: &Registry,
        predicate: &BlockPredicate,
        origin: BlockPos,
    ) -> bool {
        match predicate {
            BlockPredicate::True => true,
            BlockPredicate::AllOf { predicates } => predicates
                .iter()
                .all(|predicate| self.test_block_predicate(host, registry, predicate, origin)),
            BlockPredicate::AnyOf { predicates } => predicates
                .iter()
                .any(|predicate| self.test_block_predicate(host, registry, predicate, origin)),
            BlockPredicate::Not { predicate } => {
                !self.test_block_predicate(host, registry, predicate, origin)
            }
            BlockPredicate::MatchingBlockTag { tag, offset } => host
                .block_state(BlockPos(origin.0 + *offset))
                .get_block()
                .has_tag(tag),
            BlockPredicate::WouldSurvive { state, offset } => {
                let state = WorldgenStateResolver::feature_block_state_from_data(
                    registry,
                    state,
                    "vegetation predicate",
                );
                can_survive(host, state, BlockPos(origin.0 + *offset))
            }
            BlockPredicate::Replaceable { offset } => host
                .block_state(BlockPos(origin.0 + *offset))
                .is_replaceable(),
            BlockPredicate::InsideWorldBounds { offset } => {
                let y = origin.y() + offset.y;
                y >= host.min_y() && y < host.max_y_exclusive()
            }
            unsupported => {
                panic!("sparse vegetation predicate uses unsupported variant {unsupported:?}")
            }
        }
    }
}

fn is_sparse_vegetation_feature(feature: PlacedFeatureEntryRef) -> bool {
    feature.key.path == "trees_cherry"
        || feature.key.path == "flower_cherry"
        || feature.key.path == "patch_tall_grass_2"
        || feature.key.path == "patch_grass_plain"
}

fn write_block<H: VegetationBlockAccess>(
    host: &mut H,
    writes: &mut Vec<VegetationBlock>,
    pos: BlockPos,
    state: BlockStateId,
) {
    host.set_block_state(pos, state);
    writes.push(VegetationBlock::at(pos, state));
}

fn can_survive<H: VegetationBlockAccess>(host: &H, state: BlockStateId, pos: BlockPos) -> bool {
    let block = state.get_block();
    if block == &vanilla_blocks::CHERRY_SAPLING
        || block == &vanilla_blocks::SHORT_GRASS
        || block == &vanilla_blocks::TALL_GRASS
        || block == &vanilla_blocks::PINK_PETALS
    {
        return host
            .block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::SUPPORTS_VEGETATION);
    }
    false
}

fn sample_provider<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    provider: &steel_registry::feature::BlockStateProvider,
    pos: BlockPos,
) -> Option<BlockStateId> {
    use steel_registry::feature::BlockStateProvider;
    match provider {
        BlockStateProvider::Simple { state } | BlockStateProvider::RotatedBlock { state } => {
            Some(WorldgenStateResolver::feature_block_state_from_data(
                registry,
                state,
                "vegetation provider",
            ))
        }
        BlockStateProvider::Weighted { entries } => {
            let total_weight: i32 = entries.iter().map(|entry| entry.weight.max(0)).sum();
            if total_weight <= 0 {
                return None;
            }
            let mut pick = random.next_i32_bounded(total_weight);
            for entry in entries {
                pick -= entry.weight.max(0);
                if pick < 0 {
                    return Some(WorldgenStateResolver::feature_block_state_from_data(
                        registry,
                        &entry.data,
                        "vegetation weighted provider",
                    ));
                }
            }
            None
        }
        BlockStateProvider::RuleBased { fallback, rules } => {
            for rule in rules {
                // The only target rule is the cherry below-trunk provider.
                if test_provider_predicate(host, registry, &rule.if_true, pos) {
                    return sample_provider(host, registry, random, &rule.then, pos);
                }
            }
            fallback
                .as_deref()
                .and_then(|fallback| sample_provider(host, registry, random, fallback, pos))
        }
        unsupported => panic!("sparse vegetation uses unsupported block provider {unsupported:?}"),
    }
}

fn test_provider_predicate<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    predicate: &BlockPredicate,
    origin: BlockPos,
) -> bool {
    match predicate {
        BlockPredicate::True => true,
        BlockPredicate::Not { predicate } => {
            !test_provider_predicate(host, registry, predicate, origin)
        }
        BlockPredicate::MatchingBlockTag { tag, offset } => host
            .block_state(BlockPos(origin.0 + *offset))
            .get_block()
            .has_tag(tag),
        BlockPredicate::AllOf { predicates } => predicates
            .iter()
            .all(|predicate| test_provider_predicate(host, registry, predicate, origin)),
        BlockPredicate::AnyOf { predicates } => predicates
            .iter()
            .any(|predicate| test_provider_predicate(host, registry, predicate, origin)),
        BlockPredicate::WouldSurvive { state, offset } => {
            let state = WorldgenStateResolver::feature_block_state_from_data(
                registry,
                state,
                "vegetation provider predicate",
            );
            can_survive(host, state, BlockPos(origin.0 + *offset))
        }
        unsupported => panic!("sparse vegetation provider predicate unsupported: {unsupported:?}"),
    }
}

// ── Cherry tree -------------------------------------------------------------
//
// This is the native Cherry trunk/foliage algorithm expressed only in terms of
// `VegetationBlockAccess`.  It is deliberately not a simplified canopy model:
// branch interpolation, foliage holes and hanging leaves consume the same
// random stream as the native `TreeFeature` implementation.

#[derive(Clone, Copy)]
struct FoliageAttachment {
    pos: BlockPos,
    radius_offset: i32,
}

#[derive(Default)]
struct PositionSet {
    entries: Vec<BlockPos>,
    present: FxHashSet<BlockPos>,
}

impl PositionSet {
    fn insert(&mut self, pos: BlockPos) {
        if self.present.insert(pos) {
            self.entries.push(pos);
        }
    }

    fn contains(&self, pos: BlockPos) -> bool {
        self.present.contains(&pos)
    }

    fn pop_insertion_order(&mut self) -> Option<BlockPos> {
        let pos = self
            .entries
            .iter()
            .copied()
            .find(|pos| self.present.remove(pos))?;
        Some(pos)
    }

    fn y_sorted(&self) -> Vec<BlockPos> {
        let mut positions = self.entries.clone();
        positions.sort_by_key(BlockPos::y);
        positions
    }
}

#[derive(Default)]
struct TreePlacement {
    trunks: PositionSet,
    foliage: PositionSet,
    decorations: PositionSet,
}

#[derive(Clone, Copy)]
struct TreeBounds {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    max_x: i32,
    max_y: i32,
    max_z: i32,
}

impl TreeBounds {
    fn from_placement(placement: &TreePlacement) -> Option<Self> {
        let mut positions = placement
            .trunks
            .entries
            .iter()
            .chain(&placement.foliage.entries)
            .chain(&placement.decorations.entries)
            .copied();
        let first = positions.next()?;
        let mut bounds = Self {
            min_x: first.x(),
            min_y: first.y(),
            min_z: first.z(),
            max_x: first.x(),
            max_y: first.y(),
            max_z: first.z(),
        };
        for pos in positions {
            bounds.min_x = bounds.min_x.min(pos.x());
            bounds.min_y = bounds.min_y.min(pos.y());
            bounds.min_z = bounds.min_z.min(pos.z());
            bounds.max_x = bounds.max_x.max(pos.x());
            bounds.max_y = bounds.max_y.max(pos.y());
            bounds.max_z = bounds.max_z.max(pos.z());
        }
        Some(bounds)
    }

    const fn contains(self, pos: BlockPos) -> bool {
        pos.x() >= self.min_x
            && pos.x() <= self.max_x
            && pos.y() >= self.min_y
            && pos.y() <= self.max_y
            && pos.z() >= self.min_z
            && pos.z() <= self.max_z
    }
}

impl TreePlacement {
    fn set_trunk<H: VegetationBlockAccess>(
        &mut self,
        host: &mut H,
        writes: &mut Vec<VegetationBlock>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        self.trunks.insert(pos);
        write_block(host, writes, pos, state);
    }

    fn set_foliage<H: VegetationBlockAccess>(
        &mut self,
        host: &mut H,
        writes: &mut Vec<VegetationBlock>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        self.foliage.insert(pos);
        write_block(host, writes, pos, state);
    }

    fn set_decoration<H: VegetationBlockAccess>(
        &mut self,
        host: &mut H,
        writes: &mut Vec<VegetationBlock>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        self.decorations.insert(pos);
        write_block(host, writes, pos, state);
    }
}

fn place_cherry_tree<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    config: &TreeConfiguration,
    origin: BlockPos,
    writes: &mut Vec<VegetationBlock>,
) -> bool {
    let (TrunkPlacer::Cherry(trunk_placer), FoliagePlacer::Cherry(foliage_placer)) =
        (&config.trunk_placer, &config.foliage_placer)
    else {
        return false;
    };
    if config.root_placer.is_some() {
        return false;
    }

    let tree_height = trunk_placer.base_height
        + random.next_i32_bounded(trunk_placer.height_rand_a + 1)
        + random.next_i32_bounded(trunk_placer.height_rand_b + 1);
    let foliage_height = foliage_placer.height.sample(random);
    let leaf_radius = foliage_placer.radius.sample(random);
    let max_y = origin.y() + tree_height + 1;
    if origin.y() < host.min_y() + 1 || max_y > host.max_y_exclusive() {
        return false;
    }

    let clipped_height = max_free_cherry_tree_height(host, tree_height, origin, config);
    let min_clipped_height = match &config.minimum_size {
        FeatureSize::TwoLayers(size) => size.min_clipped_height,
        FeatureSize::ThreeLayers(size) => size.min_clipped_height,
    };
    if clipped_height < tree_height
        && min_clipped_height.is_none_or(|minimum| clipped_height < minimum)
    {
        return false;
    }

    let mut placement = TreePlacement::default();
    let attachments = place_cherry_tree_trunk(
        host,
        registry,
        random,
        clipped_height,
        origin,
        config,
        trunk_placer,
        writes,
        &mut placement,
    );
    for attachment in attachments {
        create_cherry_tree_foliage(
            host,
            registry,
            random,
            config,
            foliage_placer,
            attachment,
            foliage_height,
            leaf_radius,
            writes,
            &mut placement,
        );
    }
    if placement.trunks.entries.is_empty() && placement.foliage.entries.is_empty() {
        return false;
    }
    for decorator in &config.decorators {
        match decorator {
            TreeDecorator::Beehive { probability } => place_beehive_decorator(
                host,
                registry,
                random,
                *probability,
                writes,
                &mut placement,
            ),
            unsupported => panic!(
                "sparse cherry vegetation does not yet support tree decorator {unsupported:?}"
            ),
        }
    }
    if let Some(bounds) = TreeBounds::from_placement(&placement) {
        update_cherry_leaf_distances(host, writes, bounds, &placement);
    }
    true
}

/// Native tree post-pass: propagate `distance` from placed logs across the
/// generated foliage volume.  Shape-edge notifications in native only schedule
/// future ticks for this cherry configuration; the deterministic final state
/// relevant to the sparse response is the distance propagation below.
fn update_cherry_leaf_distances<H: VegetationBlockAccess>(
    host: &mut H,
    writes: &mut Vec<VegetationBlock>,
    bounds: TreeBounds,
    placement: &TreePlacement,
) {
    const LEAF_DISTANCE_LIMIT: usize = 7;
    let mut shape = FxHashSet::default();
    for pos in &placement.decorations.entries {
        if bounds.contains(*pos) {
            shape.insert(*pos);
        }
    }
    let mut frontiers = (0..LEAF_DISTANCE_LIMIT)
        .map(|_| PositionSet::default())
        .collect::<Vec<_>>();
    for pos in &placement.trunks.entries {
        frontiers[0].insert(*pos);
    }
    let mut smallest_distance = 0usize;
    loop {
        while smallest_distance < LEAF_DISTANCE_LIMIT
            && frontiers[smallest_distance].present.is_empty()
        {
            smallest_distance += 1;
        }
        if smallest_distance >= LEAF_DISTANCE_LIMIT {
            break;
        }
        let Some(pos) = frontiers[smallest_distance].pop_insertion_order() else {
            continue;
        };
        if !bounds.contains(pos) {
            continue;
        }
        if smallest_distance != 0 {
            let state = host.block_state(pos);
            if state
                .try_get_value(&BlockStateProperties::DISTANCE)
                .is_some()
            {
                write_block(
                    host,
                    writes,
                    pos,
                    state.set_value(&BlockStateProperties::DISTANCE, smallest_distance as u8),
                );
            }
        }
        shape.insert(pos);
        for direction in Direction::ALL {
            let neighbor = pos.relative(direction);
            if !bounds.contains(neighbor) || shape.contains(&neighbor) {
                continue;
            }
            let state = host.block_state(neighbor);
            let Some(distance) = tree_optional_leaf_distance(state) else {
                continue;
            };
            let new_distance = distance.min((smallest_distance + 1) as u8);
            if new_distance < LEAF_DISTANCE_LIMIT as u8 {
                frontiers[usize::from(new_distance)].insert(neighbor);
                smallest_distance = smallest_distance.min(usize::from(new_distance));
            }
        }
    }
}

fn tree_optional_leaf_distance(state: BlockStateId) -> Option<u8> {
    if state
        .get_block()
        .has_tag(&BlockTag::PREVENTS_NEARBY_LEAF_DECAY)
    {
        return Some(0);
    }
    state.try_get_value(&BlockStateProperties::DISTANCE)
}

fn tree_size_at_height(size: &FeatureSize, tree_height: i32, y: i32) -> i32 {
    match size {
        FeatureSize::TwoLayers(size) => {
            if y < size.limit {
                size.lower_size
            } else {
                size.upper_size
            }
        }
        FeatureSize::ThreeLayers(size) => {
            if y < size.limit {
                size.lower_size
            } else if y >= tree_height - size.upper_limit {
                size.upper_size
            } else {
                size.middle_size
            }
        }
    }
}

fn max_free_cherry_tree_height<H: VegetationBlockAccess>(
    host: &H,
    tree_height: i32,
    origin: BlockPos,
    config: &TreeConfiguration,
) -> i32 {
    for y in 0..=tree_height + 1 {
        let radius = tree_size_at_height(&config.minimum_size, tree_height, y);
        for x in -radius..=radius {
            for z in -radius..=radius {
                let state = host.block_state(origin.offset(x, y, z));
                let free = state.is_air()
                    || state.get_block().has_tag(&BlockTag::REPLACEABLE_BY_TREES)
                    || state.get_block().has_tag(&BlockTag::LOGS);
                if !free {
                    return y - 2;
                }
            }
        }
    }
    tree_height
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla Cherry trunk placement"
)]
fn place_cherry_tree_trunk<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    tree_height: i32,
    origin: BlockPos,
    config: &TreeConfiguration,
    placer: &CherryTrunkPlacer,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) -> Vec<FoliageAttachment> {
    place_below_trunk_block(
        host,
        registry,
        random,
        origin.below(),
        config,
        writes,
        placement,
    );
    let first_branch_offset =
        0.max(tree_height - 1 + placer.branch_start_offset_from_top.sample(random));
    let second_provider = placer
        .branch_start_offset_from_top
        .with_max_inclusive(placer.branch_start_offset_from_top.max_inclusive - 1);
    let mut second_branch_offset = 0.max(tree_height - 1 + second_provider.sample(random));
    if second_branch_offset >= first_branch_offset {
        second_branch_offset += 1;
    }
    let branch_count = placer.branch_count.sample(random);
    let has_middle_branch = branch_count == 3;
    let has_both_side_branches = branch_count >= 2;
    let trunk_height = if has_middle_branch {
        tree_height
    } else if has_both_side_branches {
        first_branch_offset.max(second_branch_offset) + 1
    } else {
        first_branch_offset + 1
    };
    for y in 0..trunk_height {
        let _ = place_tree_log(
            host,
            registry,
            random,
            origin.above_n(y),
            config,
            writes,
            placement,
        );
    }
    let mut attachments = Vec::new();
    if has_middle_branch {
        attachments.push(FoliageAttachment {
            pos: origin.above_n(trunk_height),
            radius_offset: 0,
        });
    }
    let tree_direction = Direction::HORIZONTAL[random.next_i32_bounded(4) as usize];
    let sideways_axis = tree_direction.get_axis();
    attachments.push(generate_cherry_tree_branch(
        host,
        registry,
        random,
        tree_height,
        origin,
        config,
        placer,
        tree_direction,
        first_branch_offset,
        first_branch_offset < trunk_height - 1,
        sideways_axis,
        writes,
        placement,
    ));
    if has_both_side_branches {
        attachments.push(generate_cherry_tree_branch(
            host,
            registry,
            random,
            tree_height,
            origin,
            config,
            placer,
            tree_direction.opposite(),
            second_branch_offset,
            second_branch_offset < trunk_height - 1,
            sideways_axis,
            writes,
            placement,
        ));
    }
    attachments
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla Cherry branch placement"
)]
fn generate_cherry_tree_branch<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    tree_height: i32,
    origin: BlockPos,
    config: &TreeConfiguration,
    placer: &CherryTrunkPlacer,
    branch_direction: Direction,
    offset_from_origin: i32,
    middle_continues_upwards: bool,
    sideways_axis: Axis,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) -> FoliageAttachment {
    let mut log_pos = origin.above_n(offset_from_origin);
    let branch_end_y = tree_height - 1 + placer.branch_end_offset_from_top.sample(random);
    let extend_branch = middle_continues_upwards || branch_end_y < offset_from_origin;
    let distance_to_trunk =
        placer.branch_horizontal_length.sample(random) + i32::from(extend_branch);
    let branch_end_pos = origin
        .relative_n(branch_direction, distance_to_trunk)
        .above_n(branch_end_y);
    let horizontal_steps = if extend_branch { 2 } else { 1 };
    for _ in 0..horizontal_steps {
        log_pos = log_pos.relative(branch_direction);
        let _ = place_tree_log_with_axis(
            host,
            registry,
            random,
            log_pos,
            sideways_axis,
            config,
            writes,
            placement,
        );
    }
    let vertical_direction = if branch_end_pos.y() > log_pos.y() {
        Direction::Up
    } else {
        Direction::Down
    };
    loop {
        let distance = manhattan_distance(log_pos, branch_end_pos);
        if distance == 0 {
            return FoliageAttachment {
                pos: branch_end_pos.above(),
                radius_offset: 0,
            };
        }
        let vertical_distance = (branch_end_pos.y() - log_pos.y()).abs();
        let grow_vertically = random.next_f32() < vertical_distance as f32 / distance as f32;
        log_pos = if grow_vertically {
            log_pos.relative(vertical_direction)
        } else {
            log_pos.relative(branch_direction)
        };
        if grow_vertically {
            let _ = place_tree_log(host, registry, random, log_pos, config, writes, placement);
        } else {
            let _ = place_tree_log_with_axis(
                host,
                registry,
                random,
                log_pos,
                sideways_axis,
                config,
                writes,
                placement,
            );
        }
    }
}

fn place_below_trunk_block<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    pos: BlockPos,
    config: &TreeConfiguration,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) {
    if let Some(state) = sample_provider(host, registry, random, &config.below_trunk_provider, pos)
    {
        placement.set_trunk(host, writes, pos, state);
    }
}

fn tree_valid_position<H: VegetationBlockAccess>(host: &H, pos: BlockPos) -> bool {
    let state = host.block_state(pos);
    state.is_air() || state.get_block().has_tag(&BlockTag::REPLACEABLE_BY_TREES)
}

fn place_tree_log<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    pos: BlockPos,
    config: &TreeConfiguration,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) -> bool {
    if !tree_valid_position(host, pos) {
        return false;
    }
    let Some(state) = sample_provider(host, registry, random, &config.trunk_provider, pos) else {
        return false;
    };
    placement.set_trunk(host, writes, pos, state);
    true
}

fn place_tree_log_with_axis<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    pos: BlockPos,
    axis: Axis,
    config: &TreeConfiguration,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) -> bool {
    if !tree_valid_position(host, pos) {
        return false;
    }
    let Some(mut state) = sample_provider(host, registry, random, &config.trunk_provider, pos)
    else {
        return false;
    };
    if state.try_get_value(&BlockStateProperties::AXIS).is_some() {
        state = state.set_value(&BlockStateProperties::AXIS, axis);
    }
    placement.set_trunk(host, writes, pos, state);
    true
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla Cherry foliage placement"
)]
fn create_cherry_tree_foliage<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    config: &TreeConfiguration,
    placer: &CherryFoliagePlacer,
    attachment: FoliageAttachment,
    foliage_height: i32,
    leaf_radius: i32,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) {
    let offset = placer.offset.sample(random);
    let foliage_pos = attachment.pos.above_n(offset);
    let current_radius = leaf_radius + attachment.radius_offset - 1;
    place_cherry_leaves_row(
        host,
        registry,
        random,
        config,
        placer,
        foliage_pos,
        current_radius - 2,
        foliage_height - 3,
        writes,
        placement,
    );
    place_cherry_leaves_row(
        host,
        registry,
        random,
        config,
        placer,
        foliage_pos,
        current_radius - 1,
        foliage_height - 4,
        writes,
        placement,
    );
    for y in (0..=foliage_height - 5).rev() {
        place_cherry_leaves_row(
            host,
            registry,
            random,
            config,
            placer,
            foliage_pos,
            current_radius,
            y,
            writes,
            placement,
        );
    }
    place_cherry_hanging_leaves_row(
        host,
        registry,
        random,
        config,
        placer,
        foliage_pos,
        current_radius,
        -1,
        writes,
        placement,
    );
    place_cherry_hanging_leaves_row(
        host,
        registry,
        random,
        config,
        placer,
        foliage_pos,
        current_radius - 1,
        -2,
        writes,
        placement,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla foliage row helper"
)]
fn place_cherry_leaves_row<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    config: &TreeConfiguration,
    placer: &CherryFoliagePlacer,
    origin: BlockPos,
    current_radius: i32,
    y: i32,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) {
    for dx in -current_radius..=current_radius {
        for dz in -current_radius..=current_radius {
            if cherry_foliage_should_skip_location(random, placer, dx, y, dz, current_radius) {
                continue;
            }
            let _ = try_place_tree_leaf(
                host,
                registry,
                random,
                config,
                origin.offset(dx, y, dz),
                writes,
                placement,
            );
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla hanging foliage helper"
)]
fn place_cherry_hanging_leaves_row<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    config: &TreeConfiguration,
    placer: &CherryFoliagePlacer,
    origin: BlockPos,
    current_radius: i32,
    y: i32,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) {
    place_cherry_leaves_row(
        host,
        registry,
        random,
        config,
        placer,
        origin,
        current_radius,
        y,
        writes,
        placement,
    );
    let log_pos = origin.below();
    for along_edge in Direction::HORIZONTAL {
        let to_edge = along_edge.rotate_y_clockwise();
        let mut pos = origin
            .offset(0, y - 1, 0)
            .relative_n(to_edge, current_radius)
            .relative_n(along_edge, -current_radius);
        let mut offset_along_edge = -current_radius;
        while offset_along_edge < current_radius {
            let leaves_above = placement.foliage.contains(pos.above());
            if leaves_above
                && try_place_hanging_leaf(
                    host,
                    registry,
                    random,
                    config,
                    placer.hanging_leaves_chance,
                    log_pos,
                    pos,
                    writes,
                    placement,
                )
            {
                let _ = try_place_hanging_leaf(
                    host,
                    registry,
                    random,
                    config,
                    placer.hanging_leaves_extension_chance,
                    log_pos,
                    pos.below(),
                    writes,
                    placement,
                );
            }
            offset_along_edge += 1;
            pos = pos.relative(along_edge);
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla hanging leaf extension"
)]
fn try_place_hanging_leaf<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    config: &TreeConfiguration,
    chance: f32,
    log_pos: BlockPos,
    pos: BlockPos,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) -> bool {
    if manhattan_distance(pos, log_pos) >= 7 || random.next_f32() > chance {
        return false;
    }
    try_place_tree_leaf(host, registry, random, config, pos, writes, placement)
}

fn cherry_foliage_should_skip_location(
    random: &mut WorldgenRandom,
    placer: &CherryFoliagePlacer,
    dx: i32,
    y: i32,
    dz: i32,
    current_radius: i32,
) -> bool {
    let dx = dx.abs();
    let dz = dz.abs();
    if y == -1
        && (dx == current_radius || dz == current_radius)
        && random.next_f32() < placer.wide_bottom_layer_hole_chance
    {
        return true;
    }
    let corner = dx == current_radius && dz == current_radius;
    if current_radius > 2 {
        corner
            || (dx + dz > current_radius * 2 - 2 && random.next_f32() < placer.corner_hole_chance)
    } else {
        corner && random.next_f32() < placer.corner_hole_chance
    }
}

fn try_place_tree_leaf<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    config: &TreeConfiguration,
    pos: BlockPos,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) -> bool {
    let current_state = host.block_state(pos);
    let persistent = current_state
        .try_get_value(&BlockStateProperties::PERSISTENT)
        .unwrap_or(false);
    let valid = current_state.is_air()
        || current_state
            .get_block()
            .has_tag(&BlockTag::REPLACEABLE_BY_TREES);
    if persistent || !valid {
        return false;
    }
    let Some(state) = sample_provider(host, registry, random, &config.foliage_provider, pos) else {
        return false;
    };
    placement.set_foliage(host, writes, pos, state);
    true
}

fn place_beehive_decorator<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    random: &mut WorldgenRandom,
    probability: f32,
    writes: &mut Vec<VegetationBlock>,
    placement: &mut TreePlacement,
) {
    let logs = placement.trunks.y_sorted();
    if logs.is_empty() || random.next_f32() >= probability {
        return;
    }
    let leaves = placement.foliage.y_sorted();
    let hive_y = if let Some(first_leaf) = leaves.first() {
        (first_leaf.y() - 1).max(logs[0].y() + 1)
    } else {
        (logs[0].y() + 1 + random.next_i32_bounded(3)).min(logs[logs.len() - 1].y())
    };
    let mut candidates = Vec::new();
    for log in logs.into_iter().filter(|pos| pos.y() == hive_y) {
        for direction in [Direction::East, Direction::South, Direction::West] {
            candidates.push(log.relative(direction));
        }
    }
    for index in (1..candidates.len()).rev() {
        let swap = random.next_i32_bounded((index + 1) as i32) as usize;
        candidates.swap(index, swap);
    }
    let Some(pos) = candidates.into_iter().find(|pos| {
        host.block_state(*pos).is_air() && host.block_state(pos.relative(Direction::South)).is_air()
    }) else {
        return;
    };
    let state = registry
        .blocks
        .get_default_state_id(&vanilla_blocks::BEE_NEST)
        .set_value(&BlockStateProperties::HORIZONTAL_FACING, Direction::South);
    placement.set_decoration(host, writes, pos, state);
    // Native installs a beehive block entity here. Its worldgen payload only
    // advances this feature's random stream, so reproduce that consumption in
    // a sparse host without requiring block-entity storage.
    let bees = 2 + random.next_i32_bounded(2);
    for _ in 0..bees {
        let _ = random.next_i32_bounded(599);
    }
}

const fn manhattan_distance(left: BlockPos, right: BlockPos) -> i32 {
    (left.x() - right.x()).abs() + (left.y() - right.y()).abs() + (left.z() - right.z()).abs()
}

static BIOME_INFO_NOISE: LazyLock<PerlinSimplexNoise> = LazyLock::new(|| {
    let mut random = RandomSource::Legacy(LegacyRandom::from_seed(2345));
    PerlinSimplexNoise::new(&mut random, &[0])
});

fn biome_info_noise_value(x: f64, z: f64) -> f64 {
    BIOME_INFO_NOISE.get_value(x, z)
}
