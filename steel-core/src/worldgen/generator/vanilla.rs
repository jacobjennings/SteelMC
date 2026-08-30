use std::marker::PhantomData;
use std::path::Path;

use glam::{DVec3, IVec3};
use rustc_hash::FxHashSet;
use smallvec::SmallVec;
use steel_registry::biome::BiomeRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::carver::ConfiguredCarverKind;
use steel_registry::{REGISTRY, RegistryEntry, RegistryExt};
use steel_utils::random::{
    Random, RandomSource, RandomSplitter, legacy_random::LegacyRandom, xoroshiro::Xoroshiro,
};
use steel_utils::{BlockPos, BlockStateId, ChunkPos, DowncastType, DowncastTypeKey, Identifier};
use steel_worldgen::density::{ColumnCache, DimensionNoises, NoiseSettings};
use steel_worldgen::density_functions::{
    end::EndNoises, nether::NetherNoises, overworld::OverworldNoises,
};
use steel_worldgen::noise_parameters::get_noise_parameters;
use steel_worldgen::surface::{
    PreliminarySurfaceCorners, SurfaceBiomeAccess, SurfaceExtensions, SurfaceStage, SurfaceSystem,
};

use crate::chunk::Chunk;
use crate::chunk::heightmap::{Heightmap, HeightmapType};
use crate::worldgen::carver::{CarveRun, CarverBlockIds, CarvingContext, SourceChunk, cave};
use crate::worldgen::feature::FeatureDecorationRunner;
use crate::worldgen::generator::{
    CarversPhase, ChunkGenerator, GenerationChunk, NoisePhase, SurfacePhase,
    worldgen_region_random_from_splitter,
};
use crate::worldgen::region::WorldGenRegion;
use crate::worldgen::structure::{StructureGenerator, create_structures};
use steel_worldgen::biomes::BiomeSourceKind;
use steel_worldgen::biomes::obfuscate_biome_seed;
use steel_worldgen::noise::Beardifier;
use steel_worldgen::noise::NoiseChunk;
use steel_worldgen::noise::OreVeinifier;
use steel_worldgen::noise::{Aquifer, AquiferResult, LazyAquifer, preliminary_surface_level};
use steel_worldgen::structure::GenerationContext;

const CARVER_SOURCE_CHUNK_COUNT: usize = 17 * 17;

/// Associates a dimension's statically typed Aquifer with its transient erased state.
///
/// Custom dimension noise implementations own their state type and downcast key. Steel
/// only erases the value between stages; every hot-path Aquifer call remains monomorphized.
pub trait VanillaPostNoiseStateType: DimensionNoises + 'static {
    /// Concrete state stored transiently on the generating chunk.
    type State: DowncastType + Send + Sync;

    /// Wraps the Aquifer after the Noise stage.
    fn wrap_post_noise_aquifer(aquifer: Aquifer<Self>) -> Self::State;

    /// Recovers the concrete Aquifer at a later generation stage.
    fn post_noise_aquifer(state: &mut Self::State) -> &mut Aquifer<Self>;
}

/// Steel-owned post-Noise state for the built-in dimensions.
#[doc(hidden)]
pub struct SteelPostNoiseState<N: DimensionNoises> {
    aquifer: Aquifer<N>,
}

// SAFETY: Each key uniquely identifies this exact Steel-owned specialization.
unsafe impl DowncastType for SteelPostNoiseState<OverworldNoises> {
    const TYPE_KEY: DowncastTypeKey =
        DowncastTypeKey::new("steel:worldgen_state/post_noise_overworld");
}

// SAFETY: Each key uniquely identifies this exact Steel-owned specialization.
unsafe impl DowncastType for SteelPostNoiseState<NetherNoises> {
    const TYPE_KEY: DowncastTypeKey =
        DowncastTypeKey::new("steel:worldgen_state/post_noise_nether");
}

// SAFETY: Each key uniquely identifies this exact Steel-owned specialization.
unsafe impl DowncastType for SteelPostNoiseState<EndNoises> {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:worldgen_state/post_noise_end");
}

macro_rules! impl_post_noise_state_type {
    ($noises:ty) => {
        impl VanillaPostNoiseStateType for $noises {
            type State = SteelPostNoiseState<Self>;

            fn wrap_post_noise_aquifer(aquifer: Aquifer<Self>) -> Self::State {
                SteelPostNoiseState { aquifer }
            }

            fn post_noise_aquifer(state: &mut Self::State) -> &mut Aquifer<Self> {
                &mut state.aquifer
            }
        }
    };
}

impl_post_noise_state_type!(OverworldNoises);
impl_post_noise_state_type!(NetherNoises);
impl_post_noise_state_type!(EndNoises);

/// A chunk generator for vanilla (normal) world generation.
///
/// Matches vanilla's `NoiseBasedChunkGenerator`. The biome source is pluggable
/// per-dimension — overworld, nether, and end each provide a different
/// [`BiomeSourceKind`] variant.
///
/// Generic over `N: DimensionNoises` to support different dimensions with
/// their own transpiled density functions and noise settings.
pub struct VanillaGenerator<N: DimensionNoises> {
    /// Biome source for this dimension. Determines biomes at each quart position.
    biome_source: BiomeSourceKind,
    /// Representative biome for source-carver lookup when every possible
    /// biome from `biome_source` has the same carver list.
    ///
    /// Vanilla still samples each source biome in `apply_carvers`; Steel skips
    /// that sampling only when the source's full possible-biome set proves the
    /// carver list is uniform. If future biome sources can produce mixed
    /// carver lists this remains `None` and the vanilla per-source lookup is
    /// used.
    uniform_carver_biome: Option<BiomeRef>,
    /// Noise generators for this dimension's density functions.
    /// Boxed because noise structs can be large.
    noises: Box<N>,
    /// Seed positional splitter for per-chunk construction of aquifers.
    splitter: RandomSplitter,
    /// Ore vein generator for replacing stone with ore blocks.
    ore_veinifier: Option<OreVeinifier>,
    /// Surface system for biome-specific block replacement.
    surface_system: SurfaceSystem,
    /// Which vanilla surface extension biomes this source can produce.
    surface_extension_biomes: SurfaceExtensions,
    /// Block state ID for the default block, cached at construction time.
    default_block_id: BlockStateId,
    /// Obfuscated seed for `BiomeManager` biome zoom fuzzing.
    biome_zoom_seed: i64,
    /// World seed as i64 (matching Java's long), used for structures and carver seeding.
    seed: i64,
    /// Shared structure placement/selection engine.
    structure_generator: StructureGenerator,
    /// Cached placed-feature order for biome decoration.
    feature_runner: FeatureDecorationRunner,
    _phantom: PhantomData<N>,
}

/// Native palette-backed implementation of the portable Surface biome host.
///
/// Current-chunk values are read from the snapshot taken before Surface, while
/// one-quart spillover uses the scheduler's published neighbor palette. This
/// retains the native stage's no-lock inner column scan and its exact
/// BiomeManager input values.
struct NativeSurfaceBiomeAccess<'a> {
    biome_data: &'a [u16],
    section_count: usize,
    min_y: i32,
    chunk_quart_x: i32,
    chunk_quart_z: i32,
    neighbor_biomes: &'a dyn Fn(IVec3) -> u16,
}

impl NativeSurfaceBiomeAccess<'_> {
    fn new<'a>(
        biome_data: &'a [u16],
        section_count: usize,
        min_y: i32,
        chunk_quart_x: i32,
        chunk_quart_z: i32,
        neighbor_biomes: &'a dyn Fn(IVec3) -> u16,
    ) -> NativeSurfaceBiomeAccess<'a> {
        NativeSurfaceBiomeAccess {
            biome_data,
            section_count,
            min_y,
            chunk_quart_x,
            chunk_quart_z,
            neighbor_biomes,
        }
    }
}

impl SurfaceBiomeAccess for NativeSurfaceBiomeAccess<'_> {
    fn biome_id_at_quart(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> u16 {
        let in_chunk = quart_x >= self.chunk_quart_x
            && quart_x < self.chunk_quart_x + 4
            && quart_z >= self.chunk_quart_z
            && quart_z < self.chunk_quart_z + 4;
        if !in_chunk {
            return (self.neighbor_biomes)(IVec3::new(quart_x, quart_y, quart_z));
        }

        let min_quart_y = self.min_y >> 2;
        let total_quarts_y = self.section_count * 4;
        let local_quart_x = (quart_x - self.chunk_quart_x) as usize;
        let local_quart_z = (quart_z - self.chunk_quart_z) as usize;
        let quart_y_in_chunk = (quart_y - min_quart_y).clamp(0, total_quarts_y as i32 - 1) as usize;
        let section_index = quart_y_in_chunk / 4;
        let local_quart_y = quart_y_in_chunk % 4;
        self.biome_data[section_index * 64 + local_quart_y * 16 + local_quart_z * 4 + local_quart_x]
    }
}

impl<N: DimensionNoises> VanillaGenerator<N> {
    /// Creates a new `VanillaGenerator` with the given biome source and seed.
    ///
    /// # Panics
    /// Panics if SHA-256 hash output is shorter than 8 bytes (cannot happen).
    #[must_use]
    pub fn new(
        world_path: Option<&Path>,
        biome_source: BiomeSourceKind,
        seed: u64,
        thread_pool: &rayon::ThreadPool,
    ) -> Self {
        // Nether uses Java's LCG; overworld/end use Xoroshiro.
        let splitter = if N::Settings::LEGACY_RANDOM_SOURCE {
            LegacyRandom::from_seed(seed).next_positional()
        } else {
            Xoroshiro::from_seed(seed).next_positional()
        };
        let noise_params = get_noise_parameters();
        let noises = N::create(seed, &splitter, &noise_params);

        let ore_veinifier = if N::Settings::ORE_VEINS_ENABLED {
            Some(OreVeinifier::new(&splitter))
        } else {
            None
        };

        let default_block_id = N::Settings::default_block_id();
        let surface_system = SurfaceSystem::new(
            &splitter,
            &noise_params,
            N::surface_noise_ids(),
            N::surface_gradient_ids(),
            default_block_id,
            N::Settings::SEA_LEVEL,
        );

        let biome_zoom_seed = obfuscate_biome_seed(seed as i64);

        // Force the lazy parameter-list R-tree inside the configured generation
        // pool so its parallel construction does not initialize Rayon's global pool.
        let possible_biome_refs = thread_pool.install(|| biome_source.possible_biome_refs());
        let possible_biomes = biome_source.possible_biomes();
        let surface_extension_biomes = SurfaceExtensions::from_possible_biomes(&possible_biomes);
        let structure_generator =
            StructureGenerator::vanilla(seed as i64, world_path, &biome_source, thread_pool);
        let uniform_carver_biome = Self::uniform_carver_biome(&possible_biomes);
        let feature_runner = FeatureDecorationRunner::new(&possible_biome_refs, &REGISTRY);

        Self {
            biome_source,
            uniform_carver_biome,
            noises: Box::new(noises),
            splitter,
            ore_veinifier,
            surface_system,
            surface_extension_biomes,
            default_block_id,
            biome_zoom_seed,
            seed: seed as i64,
            structure_generator,
            feature_runner,
            _phantom: PhantomData,
        }
    }

    fn uniform_carver_biome(possible_biomes: &FxHashSet<Identifier>) -> Option<BiomeRef> {
        let mut possible_biomes = possible_biomes.iter();
        let first_key = possible_biomes.next()?;
        let first = REGISTRY.biomes.by_key(first_key)?;

        possible_biomes
            .all(|key| {
                REGISTRY
                    .biomes
                    .by_key(key)
                    .is_some_and(|biome| biome.carvers == first.carvers)
            })
            .then_some(first)
    }
}

impl<N: VanillaPostNoiseStateType> VanillaGenerator<N> {
    #[cfg(test)]
    pub(crate) fn apply_selected_biome_decorations_for_test(
        &self,
        region: &mut WorldGenRegion<'_>,
        selected: &FxHashSet<Identifier>,
    ) {
        self.feature_runner.decorate_selected_features_for_test(
            region,
            &REGISTRY,
            self.seed,
            self.biome_zoom_seed,
            selected,
        );
    }

    fn preliminary_surface_corners(
        &self,
        chunk: GenerationChunk<'_, SurfacePhase>,
        chunk_min_x: i32,
        chunk_min_z: i32,
    ) -> PreliminarySurfaceCorners {
        let noises = &*self.noises;
        if let Some(corners) = chunk.with_post_noise_state_mut::<N::State, _>(|state| {
            let aquifer = N::post_noise_aquifer(state);
            PreliminarySurfaceCorners {
                nw: aquifer.preliminary_surface_level(noises, chunk_min_x, chunk_min_z),
                ne: aquifer.preliminary_surface_level(noises, chunk_min_x + 16, chunk_min_z),
                sw: aquifer.preliminary_surface_level(noises, chunk_min_x, chunk_min_z + 16),
                se: aquifer.preliminary_surface_level(noises, chunk_min_x + 16, chunk_min_z + 16),
            }
        }) {
            return corners;
        }

        let mut cache = N::ColumnCache::default();
        PreliminarySurfaceCorners {
            nw: preliminary_surface_level::<N>(noises, &mut cache, chunk_min_x, chunk_min_z),
            ne: preliminary_surface_level::<N>(noises, &mut cache, chunk_min_x + 16, chunk_min_z),
            sw: preliminary_surface_level::<N>(noises, &mut cache, chunk_min_x, chunk_min_z + 16),
            se: preliminary_surface_level::<N>(
                noises,
                &mut cache,
                chunk_min_x + 16,
                chunk_min_z + 16,
            ),
        }
    }
}

impl<N: VanillaPostNoiseStateType> ChunkGenerator for VanillaGenerator<N> {
    fn min_y(&self) -> i32 {
        N::Settings::MIN_Y
    }

    fn gen_depth(&self) -> i32 {
        N::Settings::HEIGHT
    }

    fn noise_biome(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeRef {
        self.biome_source
            .chunk_sampler()
            .sample(quart_x, quart_y, quart_z)
    }

    fn initial_spawn_search_origin(&self) -> steel_utils::BlockPos {
        self.biome_source.initial_spawn_search_origin()
    }

    fn structure_generator(&self) -> Option<&StructureGenerator> {
        Some(&self.structure_generator)
    }

    fn create_structures(&self, chunk: &Chunk) {
        let pos = chunk.pos();
        let chunk_x = pos.0.x;
        let chunk_z = pos.0.y;

        let mut sampler = self.biome_source.chunk_sampler();
        let chunk_min_x = chunk_x * 16;
        let chunk_min_z = chunk_z * 16;

        let mut height_cache = N::ColumnCache::default();
        let sea_level = N::Settings::SEA_LEVEL;

        // No eager `init_grid`: most chunks' structures (mineshaft, village)
        // use their own caches, and the 1–4 column probes of the remainder
        // hit this cache's lazy single-entry mode cheaply. Eager 5×5 quart
        // init cost ~36µs per chunk with no payoff.
        let mut aquifer = LazyAquifer::new(chunk_min_x, chunk_min_z, &self.splitter, &*self.noises);
        let mut surface_y_cache: Option<i32> = None;
        let mut height_cache_grid_ready = false;
        let mut ctx = GenerationContext::<'_, '_, N>::new(
            self.seed,
            chunk_x,
            chunk_z,
            sea_level,
            &self.noises,
            &self.splitter,
            self.structure_generator.template_pools(),
            self.structure_generator.templates(),
            &mut sampler,
            &mut height_cache,
            &mut aquifer,
            &mut surface_y_cache,
            &mut height_cache_grid_ready,
        );

        create_structures(&self.structure_generator, chunk, &mut ctx);
    }

    fn create_biomes(&self, chunk: &Chunk) {
        let pos = chunk.pos();
        let min_y = chunk.min_y();
        let section_count = chunk.sections().sections.len();

        let chunk_x = pos.0.x;
        let chunk_z = pos.0.y;

        let mut sampler = self.biome_source.chunk_sampler();
        // Pre-compute the flat (xz-only) climate-noise grid for this chunk so the
        // per-cell sampling below does O(1) column lookups instead of recomputing
        // the flat noise for all 1536 cells (the noise stage's `fill_from_noise`
        // already does this). Values are bit-identical — same functions, same quart
        // coordinates — so biome selection is unchanged.
        sampler.init_grid(chunk_x * 16, chunk_z * 16);

        // Match vanilla's iteration order: Section(Y) → X → Y → Z.
        // This is critical because the R-tree biome cache (persistent warm-start)
        // determines tie-breaking for equal-distance entries, and the cache state
        // depends on the order of biome lookups.
        for section_index in 0..section_count {
            let section_y = (min_y / 16) + section_index as i32;
            let section = &chunk.sections().sections[section_index];
            let mut section_guard = section.write();

            for local_quart_x in 0..4i32 {
                let quart_x = chunk_x * 4 + local_quart_x;

                for local_quart_y in 0..4i32 {
                    let quart_y = section_y * 4 + local_quart_y;

                    for local_quart_z in 0..4i32 {
                        let quart_z = chunk_z * 4 + local_quart_z;

                        let biome = sampler.sample(quart_x, quart_y, quart_z);
                        let biome_id = biome.id() as u16;

                        section_guard.biomes.set(
                            local_quart_x as usize,
                            local_quart_y as usize,
                            local_quart_z as usize,
                            biome_id,
                        );
                    }
                }
            }
        }

        chunk.mark_dirty();
    }

    fn fill_from_noise(
        &self,
        chunk: GenerationChunk<'_, NoisePhase>,
        beardifier: Option<&Beardifier>,
    ) {
        let pos = chunk.pos();
        let chunk_min_x = pos.0.x * 16;
        let chunk_min_z = pos.0.y * 16;

        let min_y = N::Settings::MIN_Y;
        let height = N::Settings::HEIGHT;

        let mut noise_chunk = NoiseChunk::<N>::new(chunk_min_x, chunk_min_z);
        let noises = &*self.noises;

        let mut column_cache = N::ColumnCache::default();
        column_cache.init_grid(chunk_min_x, chunk_min_z, noises);

        let default_block_id = self.default_block_id;
        let ore_veinifier = &self.ore_veinifier;
        let mut aquifer = Aquifer::<N>::new(
            chunk_min_x,
            chunk_min_z,
            min_y,
            height,
            &self.splitter,
            noises,
            // Aquifer samples at arbitrary (x,z) outside the chunk, so it needs its own cache
            column_cache.clone(),
        );

        // Collect writes per (x,z) column and flush in batch to avoid per-block
        // write lock acquisition on sections.
        let mut pending_writes: Vec<(usize, usize, usize, BlockStateId)> = Vec::new();
        let mut prev_x: usize = usize::MAX;
        let mut prev_z: usize = usize::MAX;
        let mut ocean_floor_wg =
            Heightmap::new(HeightmapType::OceanFloorWg, min_y, N::Settings::HEIGHT);
        let mut world_surface_wg =
            Heightmap::new(HeightmapType::WorldSurfaceWg, min_y, N::Settings::HEIGHT);

        noise_chunk.fill(
            noises,
            &mut column_cache,
            beardifier,
            |local_x, world_y, local_z, density, interpolated, cache| {
                // Flush when we move to a new column
                if local_x != prev_x || local_z != prev_z {
                    if !pending_writes.is_empty() {
                        chunk.write_block_batch(&pending_writes);
                        pending_writes.clear();
                    }
                    prev_x = local_x;
                    prev_z = local_z;
                }

                let relative_y = (world_y - min_y) as usize;
                let world_x = chunk_min_x + local_x as i32;
                let world_z = chunk_min_z + local_z as i32;

                match aquifer.compute_substance(noises, world_x, world_y, world_z, density) {
                    AquiferResult::Solid => {
                        let block = ore_veinifier
                            .as_ref()
                            .and_then(|ov| {
                                ov.compute_interpolated(
                                    noises,
                                    cache,
                                    interpolated,
                                    world_x,
                                    world_y,
                                    world_z,
                                )
                            })
                            .unwrap_or(default_block_id);
                        pending_writes.push((local_x, relative_y, local_z, block));
                        ocean_floor_wg.update_for_initial_fill(local_x, world_y, local_z, block);
                        world_surface_wg.update_for_initial_fill(local_x, world_y, local_z, block);
                    }
                    AquiferResult::Fluid(id) => {
                        pending_writes.push((local_x, relative_y, local_z, id));
                        ocean_floor_wg.update_for_initial_fill(local_x, world_y, local_z, id);
                        world_surface_wg.update_for_initial_fill(local_x, world_y, local_z, id);
                        if aquifer.should_schedule_fluid_update() && id.has_fluid() {
                            chunk.mark_pos_for_postprocessing(BlockPos::new(
                                world_x, world_y, world_z,
                            ));
                        }
                    }
                    AquiferResult::Air => {}
                }
            },
        );

        // Flush remaining writes
        if !pending_writes.is_empty() {
            chunk.write_block_batch(&pending_writes);
        }

        chunk.replace_noise_heightmaps(ocean_floor_wg, world_surface_wg);

        if N::Settings::AQUIFERS_ENABLED {
            chunk.install_post_noise_state(N::wrap_post_noise_aquifer(aquifer));
        }
    }

    fn build_surface(
        &self,
        chunk: GenerationChunk<'_, SurfacePhase>,
        neighbor_biomes: &dyn Fn(IVec3) -> u16,
    ) {
        let pos = chunk.pos();
        let chunk_min_x = pos.0.x * 16;
        let chunk_min_z = pos.0.y * 16;
        let stage = SurfaceStage::<N>::new(
            &self.surface_system,
            self.default_block_id,
            self.biome_zoom_seed,
            self.surface_extension_biomes,
        );
        let preliminary_surface_corners = stage
            .needs_preliminary_surface()
            .then(|| self.preliminary_surface_corners(chunk, chunk_min_x, chunk_min_z));
        let biome_data = stage.needs_biomes().then(|| chunk.read_all_biomes());
        let mut biome_access = NativeSurfaceBiomeAccess::new(
            biome_data.as_deref().unwrap_or(&[]),
            chunk.section_count(),
            chunk.min_y(),
            pos.0.x * 4,
            pos.0.y * 4,
            neighbor_biomes,
        );
        let mut blocks = chunk;
        stage.build_surface(&mut blocks, &mut biome_access, preliminary_surface_corners);
    }

    fn apply_carvers(&self, chunk: GenerationChunk<'_, CarversPhase>) {
        if self
            .uniform_carver_biome
            .is_some_and(|biome| biome.carvers.is_empty())
        {
            chunk.clear_post_noise_state();
            return;
        }

        chunk.consume_post_noise_state::<N::State, _>(|retained_state| {
            chunk.prime_world_surface_heightmap();

            let pos = chunk.pos();
            let chunk_min_x = pos.0.x * 16;
            let chunk_min_z = pos.0.y * 16;
            let min_y = N::Settings::MIN_Y;
            let height = N::Settings::HEIGHT;
            let noises = &*self.noises;

            let mut rebuilt_aquifer = None;
            let aquifer = if let Some(state) = retained_state {
                N::post_noise_aquifer(state)
            } else {
                let mut column_cache = N::ColumnCache::default();
                if N::Settings::AQUIFERS_ENABLED {
                    column_cache.init_grid(chunk_min_x, chunk_min_z, noises);
                }
                rebuilt_aquifer.insert(Aquifer::<N>::new(
                    chunk_min_x,
                    chunk_min_z,
                    min_y,
                    height,
                    &self.splitter,
                    noises,
                    column_cache,
                ))
            };

            // Preliminary surface level at the chunk's 4 corners — used by
            // top_material min_surface_level interpolation.
            let psl_corners = PreliminarySurfaceCorners {
                nw: aquifer.preliminary_surface_level(noises, chunk_min_x, chunk_min_z),
                ne: aquifer.preliminary_surface_level(noises, chunk_min_x + 16, chunk_min_z),
                sw: aquifer.preliminary_surface_level(noises, chunk_min_x, chunk_min_z + 16),
                se: aquifer.preliminary_surface_level(noises, chunk_min_x + 16, chunk_min_z + 16),
            };

            let mut ctx = CarvingContext {
                min_y,
                gen_depth: height,
                surface_system: &self.surface_system,
                aquifer,
                default_block_id: self.default_block_id,
                psl_corners,
                chunk_min_x,
                chunk_min_z,
            };

            let ids = CarverBlockIds::load();

            // Pre-fetch the 17×17 source-chunk carver lists. Done up front so we
            // can later close over `biome_sampler` mutably inside `biome_getter`.
            // Vanilla samples every source biome here; when this generator's full
            // possible-biome set has a uniform carver list, the representative
            // biome gives the same carver keys without 289 climate lookups.
            let mut biome_sampler = self.biome_source.chunk_sampler();
            let mut source_biomes: SmallVec<[SourceChunk; CARVER_SOURCE_CHUNK_COUNT]> =
                SmallVec::new();
            for dx in -8i32..=8 {
                for dz in -8i32..=8 {
                    let sx = pos.0.x + dx;
                    let sz = pos.0.y + dz;
                    let biome = if let Some(biome) = self.uniform_carver_biome {
                        biome
                    } else {
                        let qx = (sx * 16) >> 2;
                        let qz = (sz * 16) >> 2;
                        biome_sampler.sample(qx, 0, qz)
                    };
                    source_biomes.push(SourceChunk {
                        pos: ChunkPos::new(sx, sz),
                        biome,
                    });
                }
            }

            // `WorldgenRandom(LegacyRandomSource(generateUniqueSeed()))` — initial
            // seed is irrelevant; every carver overwrites it via
            // `set_large_feature_seed` before its probability check.
            let mut random = LegacyRandom::from_seed(0);
            let seed_i64 = self.seed;

            let biome_zoom_seed = self.biome_zoom_seed;
            // BiomeManager-fuzzed lookup — matches vanilla's `BiomeManager.getBiome`
            // used by the carver's top-material path. An unfuzzed quart lookup
            // would mismatch vanilla at quart-cell boundaries.
            let mut biome_getter = |pos: BlockPos| -> u16 {
                fuzzed_biome_at_block(biome_zoom_seed, pos, |q_pos| {
                    biome_sampler.sample(q_pos.x, q_pos.y, q_pos.z).id() as u16
                })
            };

            chunk.with_carving_mask(|mask| {
                let mut run = CarveRun {
                    ctx: &mut ctx,
                    noises,
                    chunk,
                    chunk_min_x,
                    chunk_min_z,
                    biome_getter: &mut biome_getter,
                    mask,
                    ids,
                };

                run.run_all(&source_biomes, seed_i64, &mut random);
            });
        });
    }

    fn create_worldgen_region_random(&self, _world_seed: i64, center: ChunkPos) -> RandomSource {
        worldgen_region_random_from_splitter(&self.splitter, center)
    }

    fn apply_biome_decorations(&self, region: &mut WorldGenRegion<'_>) {
        self.feature_runner
            .decorate(region, &REGISTRY, self.seed, self.biome_zoom_seed);
    }
}

impl<N, F> CarveRun<'_, '_, N, F>
where
    N: DimensionNoises,
    F: FnMut(BlockPos) -> u16,
{
    /// Drive the 17×17 source-chunk carver loop. Each carver in each source
    /// biome is seeded via `set_large_feature_seed`, probability-checked,
    /// then dispatched to the appropriate `carve_*` method.
    fn run_all(&mut self, source_biomes: &[SourceChunk], seed_i64: i64, random: &mut LegacyRandom) {
        for source in source_biomes {
            for (index, carver_key) in source.biome.carvers.iter().enumerate() {
                let Some(carver) = REGISTRY.configured_carvers.by_key(carver_key) else {
                    panic!(
                        "biome {} references unknown configured carver {}",
                        source.biome.key, carver_key
                    );
                };
                let index_i64 = index as i64;
                random.set_large_feature_seed(
                    seed_i64.wrapping_add(index_i64),
                    source.pos.0.x,
                    source.pos.0.y,
                );

                let probability = carver.base().probability;
                if random.next_f32() > probability {
                    continue;
                }

                match &carver.kind {
                    ConfiguredCarverKind::Cave(cfg) => {
                        self.carve_cave(cfg, cave::CaveKind::Overworld, source.pos, random);
                    }
                    ConfiguredCarverKind::NetherCave(cfg) => {
                        self.carve_cave(cfg, cave::CaveKind::Nether, source.pos, random);
                    }
                    ConfiguredCarverKind::Canyon(cfg) => {
                        self.carve_canyon(cfg, source.pos, random);
                    }
                }
            }
        }
    }
}

// ── BiomeManager biome zoom helpers ──────────────────────────────────────────

/// Vanilla's `LinearCongruentialGenerator.next()`.
#[inline]
const fn lcg_next(mut rval: i64, c: i64) -> i64 {
    rval = rval.wrapping_mul(
        rval.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407),
    );
    rval = rval.wrapping_add(c);
    rval
}

/// Vanilla's `BiomeManager.getFiddle()`.
#[inline]
fn get_fiddle(rval: i64) -> f64 {
    let uniform = ((rval >> 24).rem_euclid(1024)) as f64 / 1024.0;
    (uniform - 0.5) * 0.9
}

/// Single-shot fuzzed biome lookup at a block position. Matches vanilla's
/// `BiomeManager.getBiome(BlockPos)`: the block is shifted by `-2`, snapped to
/// the enclosing quart cell, and the winning biome is chosen from the 8
/// corners of that cell by `get_fiddle`-perturbed squared distance.
///
/// `quart_biome` returns the unfuzzed biome at a quart-coordinate — typically
/// `biome_sampler.sample(qx, qy, qz).id()`.
///
/// Used by carver top-material lookups where a simple unfuzzed lookup would
/// differ from vanilla at the quart-cell boundaries.
pub(crate) fn fuzzed_biome_at_block<F: FnMut(IVec3) -> u16>(
    biome_zoom_seed: i64,
    pos: BlockPos,
    mut quart_biome: F,
) -> u16 {
    let abs = pos.0 - IVec3::splat(2);
    let parent = IVec3::new(abs.x >> 2, abs.y >> 2, abs.z >> 2);
    let fract = DVec3::new(
        f64::from(abs.x & 3),
        f64::from(abs.y & 3),
        f64::from(abs.z & 3),
    ) / 4.0;

    let mut min_i = 0usize;
    let mut min_dist = f64::INFINITY;

    for i in 0..8usize {
        let x_even = (i & 4) == 0;
        let y_even = (i & 2) == 0;
        let z_even = (i & 1) == 0;
        let cx = if x_even { parent.x } else { parent.x + 1 };
        let cy = if y_even { parent.y } else { parent.y + 1 };
        let cz = if z_even { parent.z } else { parent.z + 1 };
        let dx = if x_even { fract.x } else { fract.x - 1.0 };
        let dy = if y_even { fract.y } else { fract.y - 1.0 };
        let dz = if z_even { fract.z } else { fract.z - 1.0 };

        // BiomeManager.getFiddledDistance — identical sequence to
        // FuzzedBiomeColumn::compute_cy_group but without the column cache.
        let mut rval = lcg_next(biome_zoom_seed, i64::from(cx));
        rval = lcg_next(rval, i64::from(cy));
        rval = lcg_next(rval, i64::from(cz));
        rval = lcg_next(rval, i64::from(cx));
        rval = lcg_next(rval, i64::from(cy));
        rval = lcg_next(rval, i64::from(cz));
        let fx = get_fiddle(rval);
        rval = lcg_next(rval, biome_zoom_seed);
        let fy = get_fiddle(rval);
        rval = lcg_next(rval, biome_zoom_seed);
        let fz = get_fiddle(rval);

        let dist = (dx + fx).powi(2) + (dy + fy).powi(2) + (dz + fz).powi(2);
        if min_dist > dist {
            min_i = i;
            min_dist = dist;
        }
    }

    let b = IVec3::new(
        if (min_i & 4) == 0 {
            parent.x
        } else {
            parent.x + 1
        },
        if (min_i & 2) == 0 {
            parent.y
        } else {
            parent.y + 1
        },
        if (min_i & 1) == 0 {
            parent.z
        } else {
            parent.z + 1
        },
    );
    quart_biome(b)
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use glam::IVec3;
    use steel_registry::{init_vanilla_registry, vanilla_dimension_types};
    use steel_worldgen::biomes::BiomeSourceKind;

    use crate::behavior::init_behaviors;
    use crate::chunk::{
        Chunk,
        heightmap::HeightmapType,
        section::{ChunkSection, Sections},
    };
    use crate::worldgen::carving_mask::CarvingMask;
    use crate::worldgen::generator::{
        CarversPhase, ChunkGenerator as _, GenerationChunk, NoisePhase, SurfacePhase,
        context::OverworldGenerator,
    };

    fn make_overworld_chunk() -> Chunk {
        let dimension = &vanilla_dimension_types::OVERWORLD;
        let sections = (0..dimension.height / 16)
            .map(|_| ChunkSection::new_empty())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Chunk::new(
            Sections::from_owned(sections),
            steel_utils::ChunkPos::new(0, 0),
            dimension.min_y,
            dimension.height,
            Weak::new(),
        )
    }

    fn self_neighbor_biome(chunk: &Chunk, quart: IVec3) -> u16 {
        let sections = chunk.sections();
        let min_quart_y = chunk.min_y() >> 2;
        let quart_y =
            (quart.y - min_quart_y).clamp(0, (sections.sections.len() * 4) as i32 - 1) as usize;
        sections.sections[quart_y / 4].read().biomes.get(
            (quart.x & 3) as usize,
            quart_y % 4,
            (quart.z & 3) as usize,
        )
    }

    fn blocks(chunk: &Chunk) -> Vec<steel_utils::BlockStateId> {
        let mut blocks = Vec::with_capacity((chunk.height() * 16 * 16) as usize);
        for relative_y in 0..chunk.height() as usize {
            for z in 0..16 {
                for x in 0..16 {
                    let Some(state) = chunk.get_relative_block(x, relative_y, z) else {
                        panic!("test coordinates must stay inside the chunk");
                    };
                    blocks.push(state);
                }
            }
        }
        blocks
    }

    fn has_overworld_post_noise_state(chunk: &Chunk) -> bool {
        chunk
            .with_transient_generation_state_mut::<
                super::SteelPostNoiseState<super::OverworldNoises>,
                _,
            >(|_| ())
            .is_some()
    }

    #[test]
    fn retained_and_rebuilt_aquifers_produce_identical_carver_output() {
        init_vanilla_registry();
        init_behaviors();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("test generation pool should build");
        let generator = OverworldGenerator::new(None, BiomeSourceKind::overworld(0), 0, &pool);
        let warm = make_overworld_chunk();
        let cold = make_overworld_chunk();

        for chunk in [&warm, &cold] {
            generator.create_biomes(chunk);
            generator.fill_from_noise(GenerationChunk::<NoisePhase>::for_test(chunk), None);
        }
        assert!(has_overworld_post_noise_state(&warm));
        assert!(has_overworld_post_noise_state(&cold));

        cold.clear_transient_generation_state();
        for chunk in [&warm, &cold] {
            generator.build_surface(GenerationChunk::<SurfacePhase>::for_test(chunk), &|quart| {
                self_neighbor_biome(chunk, quart)
            });
        }
        assert!(has_overworld_post_noise_state(&warm));
        assert!(!has_overworld_post_noise_state(&cold));

        generator.apply_carvers(GenerationChunk::<CarversPhase>::for_test(&warm));
        generator.apply_carvers(GenerationChunk::<CarversPhase>::for_test(&cold));
        assert!(!has_overworld_post_noise_state(&warm));
        assert!(!has_overworld_post_noise_state(&cold));

        assert_eq!(blocks(&warm), blocks(&cold));
        assert_eq!(
            warm.carving_mask
                .read()
                .as_ref()
                .map(CarvingMask::to_packed_u64s),
            cold.carving_mask
                .read()
                .as_ref()
                .map(CarvingMask::to_packed_u64s)
        );
        assert_eq!(&*warm.postprocessing.lock(), &*cold.postprocessing.lock());
        for x in 0..16 {
            for z in 0..16 {
                assert_eq!(
                    warm.generation_height_at(HeightmapType::WorldSurfaceWg, x, z),
                    cold.generation_height_at(HeightmapType::WorldSurfaceWg, x, z)
                );
            }
        }
    }
}
