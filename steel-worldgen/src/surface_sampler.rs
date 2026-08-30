//! Browser-safe terrain surface sampling.
//!
//! This adapter intentionally depends only on deterministic worldgen state. It
//! does not own transport, persistence, DOM APIs, or scheduling.

use std::collections::HashMap;

use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::{REGISTRY, RegistryEntry, init_vanilla_registry, vanilla_blocks};
use steel_utils::BlockStateId;
use steel_utils::random::{
    Random, RandomSplitter, legacy_random::LegacyRandom, xoroshiro::Xoroshiro,
};

use crate::biomes::{BiomeSourceKind, obfuscate_biome_seed};
use crate::carver::{CarverBlockAccess, CarverStage};
use crate::density::{ColumnCache, DimensionNoises, NoiseSettings};
use crate::density_functions::{end::EndNoises, nether::NetherNoises, overworld::OverworldNoises};
use crate::noise::NoiseChunk;
use crate::noise::{Aquifer, AquiferResult, OreVeinifier};
use crate::noise_parameters::get_noise_parameters;
use crate::surface::{
    PreliminarySurfaceCorners, SurfaceBiomeAccess, SurfaceBlockAccess, SurfaceExtensions,
    SurfaceStage, SurfaceSystem,
};
use crate::vegetation::{VegetationBlockAccess, VegetationStage};
use steel_registry::feature::FeatureHeightmap;
use steel_utils::BlockPos;

/// Dimension supported by the static sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceDimension {
    /// Vanilla Overworld.
    Overworld,
    /// Vanilla Nether.
    Nether,
    /// Vanilla End.
    End,
}

/// Surface samples for an exact square, including the final shared edge.
#[derive(Debug, Clone)]
pub struct SurfaceTile {
    /// Number of samples on one edge.
    pub samples_per_side: u32,
    /// Y coordinate of the final highest non-fluid solid for each sample.
    pub heights: Vec<i16>,
    /// RGB bytes derived from the sampled biome's configured grass colour.
    pub colors: Vec<u8>,
    /// Canonical registry identifiers for the sampled biomes, in palette order.
    pub biomes: Vec<String>,
    /// Palette index for each sample, parallel to `heights` and `present`.
    pub biome_indices: Vec<u16>,
    /// Whether the sampled column contains solid terrain.
    pub present: Vec<u8>,
    /// Canonical final top-block key for every sample.
    ///
    /// Air samples use minecraft:air. This data ends after the Surface stage:
    /// carvers, structures and feature decoration have not run.
    pub surface_blocks: Vec<String>,
    /// Sparse final states written by the portable vegetation Features slice.
    ///
    /// Positions are absolute world coordinates.  The list is sorted by X/Y/Z
    /// and contains only the final state at each coordinate after all source
    /// chunks in this tile have been processed.
    pub vegetation_blocks: Vec<SurfaceVegetationBlock>,
    /// Dimension minimum build height.
    pub min_y: i16,
}

/// Canonical sparse vegetation placement returned with a surface tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceVegetationBlock {
    /// Absolute world X coordinate.
    pub x: i32,
    /// Absolute world Y coordinate.
    pub y: i32,
    /// Absolute world Z coordinate.
    pub z: i32,
    /// Canonical registry block identifier (without state properties).
    pub block: String,
    /// Canonical state identifier including every explicit property.
    pub state: String,
}

/// Maximum number of reusable terrain chunks retained by the browser sampler.
///
/// Each entry retains pre- and post-Carvers copies. A capacity sweep over the
/// viewer's 4×4 tile traversal found that 160 is the smallest measured capacity
/// that avoids regenerating chunks (400 misses versus 640 at capacity 96);
/// 256 and 400 were slightly slower in the same sweep. The compact pre-carver
/// summary keeps retained block, heightmap, and summary payload near 30 MiB.
pub const DEFAULT_SURFACE_CHUNK_CACHE_CAPACITY: usize = 160;

/// Diagnostic counters for a surface chunk cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SurfaceChunkCacheStats {
    /// Requests served by an already-retained chunk.
    pub hits: u64,
    /// Requests which generated a chunk.
    pub misses: u64,
    /// Retained chunks removed to honor the capacity.
    pub evictions: u64,
    /// Largest number of chunks retained simultaneously.
    pub peak_retained_chunks: usize,
}

struct CachedSurfaceChunk {
    pre_carver_surface: Box<[SurfaceColumn; 256]>,
    post_carver: InMemorySurfaceChunk,
    last_used: u64,
}

#[derive(Clone, Copy)]
struct SurfaceColumn {
    height: i16,
    state: BlockStateId,
    exists: bool,
}

/// Bounded least-recently-used cache for reusable surface-generation chunks.
pub struct SurfaceChunkCache {
    capacity: usize,
    clock: u64,
    chunks: HashMap<(i32, i32), CachedSurfaceChunk>,
    stats: SurfaceChunkCacheStats,
}

impl Default for SurfaceChunkCache {
    fn default() -> Self {
        Self::new(DEFAULT_SURFACE_CHUNK_CACHE_CAPACITY)
    }
}

impl SurfaceChunkCache {
    /// Creates a cache retaining at most `capacity` chunks.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            clock: 0,
            chunks: HashMap::new(),
            stats: SurfaceChunkCacheStats::default(),
        }
    }

    /// Returns diagnostic counters accumulated since construction.
    #[must_use]
    pub fn stats(&self) -> SurfaceChunkCacheStats {
        self.stats
    }

    /// Returns bytes used by retained block-state and heightmap payloads.
    #[must_use]
    pub fn retained_payload_bytes(&self) -> usize {
        self.chunks
            .values()
            .map(|entry| {
                std::mem::size_of_val(entry.pre_carver_surface.as_ref())
                    + entry.post_carver.payload_bytes()
            })
            .sum()
    }

    fn touch(&mut self, key: (i32, i32)) -> Option<&CachedSurfaceChunk> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.chunks.get_mut(&key)?;
        entry.last_used = self.clock;
        Some(entry)
    }

    fn insert(&mut self, key: (i32, i32), entry: CachedSurfaceChunk) {
        if self.capacity == 0 {
            return;
        }
        if self.chunks.len() == self.capacity {
            let oldest = self
                .chunks
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(&key, _)| key);
            if let Some(oldest) = oldest {
                self.chunks.remove(&oldest);
                self.stats.evictions += 1;
            }
        }
        self.chunks.insert(key, entry);
        self.stats.peak_retained_chunks = self.stats.peak_retained_chunks.max(self.chunks.len());
    }
}

/// Compact base-noise volume for one 16×16 chunk footprint.
///
/// `voxels` is X-major inside Z-major inside Y-major order. Its values are a
/// deliberately small transport palette: `0` air, `1` default noise solid,
/// `2` water, and `3` lava.  Value `1` is *not* a final block state: surface
/// rules, ore veins, carvers, structures and features have not run here.
#[derive(Debug, Clone)]
pub struct NoiseVolume {
    /// Number of cells on each horizontal axis.
    pub cells_xz: u32,
    /// Number of cells on the vertical axis.
    pub cells_y: u32,
    /// First represented block Y.
    pub min_y: i16,
    /// Number of source blocks represented by one cell on every axis.
    pub lod: u16,
    /// Compact material-class data in X/Z/Y order.
    pub voxels: Vec<u8>,
}

/// Raw post-Carvers chunk data exposed only for cross-host regression tests.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct CarvedChunkSnapshot {
    /// Block-state IDs in native section Y/Z/X order.
    pub states: Vec<BlockStateId>,
    /// `WORLD_SURFACE_WG` first-available height in local Z/X order.
    pub world_surface_wg: [i32; 256],
}

/// Seeded sampler reusable across many tile requests.
pub enum SurfaceSampler {
    /// Overworld sampler.
    Overworld(DimensionSurfaceSampler<OverworldNoises>),
    /// Nether sampler.
    Nether(DimensionSurfaceSampler<NetherNoises>),
    /// End sampler.
    End(DimensionSurfaceSampler<EndNoises>),
}

impl SurfaceSampler {
    /// Creates a deterministic sampler for one seed and dimension.
    #[must_use]
    pub fn new(seed: u64, dimension: SurfaceDimension) -> Self {
        init_vanilla_registry();
        match dimension {
            SurfaceDimension::Overworld => Self::Overworld(DimensionSurfaceSampler::new(
                seed,
                BiomeSourceKind::overworld(seed),
            )),
            SurfaceDimension::Nether => Self::Nether(DimensionSurfaceSampler::new(
                seed,
                BiomeSourceKind::nether(seed),
            )),
            SurfaceDimension::End => Self::End(DimensionSurfaceSampler::new(
                seed,
                BiomeSourceKind::end(seed),
            )),
        }
    }

    /// Samples an exact square at the requested resolution.
    ///
    /// # Panics
    /// Panics when size or resolution do not describe a positive exact grid.
    #[must_use]
    pub fn tile(&self, origin_x: i32, origin_z: i32, size: u32, resolution: u32) -> SurfaceTile {
        self.tile_with_cache(
            &mut SurfaceChunkCache::new(usize::MAX),
            origin_x,
            origin_z,
            size,
            resolution,
        )
    }

    /// Samples an exact square while reusing generated chunks from `cache`.
    #[must_use]
    pub fn tile_with_cache(
        &self,
        cache: &mut SurfaceChunkCache,
        origin_x: i32,
        origin_z: i32,
        size: u32,
        resolution: u32,
    ) -> SurfaceTile {
        match self {
            Self::Overworld(sampler) => {
                sampler.tile_with_cache(cache, origin_x, origin_z, size, resolution)
            }
            Self::Nether(sampler) => {
                sampler.tile_with_cache(cache, origin_x, origin_z, size, resolution)
            }
            Self::End(sampler) => {
                sampler.tile_with_cache(cache, origin_x, origin_z, size, resolution)
            }
        }
    }

    /// Samples one 16×16 chunk footprint as base-noise volume cells.
    ///
    /// The LOD is an anchored sample grid, not a claim that the skipped source
    /// blocks were homogeneous. This is important for caves: callers can use
    /// LOD 1 for exact base-noise occupancy and request 4/16/64/256 only when
    /// they explicitly accept representative cells in exchange for a smaller
    /// payload and mesh. Final generated block states require Steel's later
    /// surface/feature/structure pipeline and are intentionally not fabricated.
    #[must_use]
    pub fn noise_volume_chunk(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        max_y_exclusive: i32,
        lod: u32,
    ) -> NoiseVolume {
        match self {
            Self::Overworld(sampler) => {
                sampler.noise_volume_chunk(chunk_x, chunk_z, min_y, max_y_exclusive, lod)
            }
            Self::Nether(sampler) => {
                sampler.noise_volume_chunk(chunk_x, chunk_z, min_y, max_y_exclusive, lod)
            }
            Self::End(sampler) => {
                sampler.noise_volume_chunk(chunk_x, chunk_z, min_y, max_y_exclusive, lod)
            }
        }
    }

    /// Returns a post-Carvers chunk snapshot for native/portable parity tests.
    #[doc(hidden)]
    #[must_use]
    pub fn carved_chunk_snapshot(&self, chunk_x: i32, chunk_z: i32) -> CarvedChunkSnapshot {
        match self {
            Self::Overworld(sampler) => sampler.carved_chunk_snapshot(chunk_x, chunk_z),
            Self::Nether(sampler) => sampler.carved_chunk_snapshot(chunk_x, chunk_z),
            Self::End(sampler) => sampler.carved_chunk_snapshot(chunk_x, chunk_z),
        }
    }

    /// Returns one isolated sparse vegetation source transaction after Surface
    /// and Carvers. This exists for native/portable transaction-parity tests;
    /// normal callers should use [`Self::tile`], which includes the source halo.
    #[doc(hidden)]
    #[must_use]
    pub fn selected_vegetation_transaction_snapshot(
        &self,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Vec<SurfaceVegetationBlock> {
        match self {
            Self::Overworld(sampler) => {
                sampler.selected_vegetation_transaction_snapshot(chunk_x, chunk_z)
            }
            Self::Nether(sampler) => {
                sampler.selected_vegetation_transaction_snapshot(chunk_x, chunk_z)
            }
            Self::End(sampler) => {
                sampler.selected_vegetation_transaction_snapshot(chunk_x, chunk_z)
            }
        }
    }
}

/// Generic dimension implementation kept public so native adapters can reuse it.
pub struct DimensionSurfaceSampler<N: DimensionNoises> {
    noises: Box<N>,
    biome_source: BiomeSourceKind,
    splitter: RandomSplitter,
    ore_veinifier: Option<OreVeinifier>,
    surface_system: SurfaceSystem,
    surface_extensions: SurfaceExtensions,
    biome_zoom_seed: i64,
    vegetation_stage: VegetationStage,
}

impl<N: DimensionNoises> DimensionSurfaceSampler<N> {
    fn new(seed: u64, biome_source: BiomeSourceKind) -> Self {
        let splitter: RandomSplitter = if N::Settings::LEGACY_RANDOM_SOURCE {
            LegacyRandom::from_seed(seed).next_positional()
        } else {
            Xoroshiro::from_seed(seed).next_positional()
        };
        let noise_parameters = get_noise_parameters();
        let default_block_id = N::Settings::default_block_id();
        let surface_extensions =
            SurfaceExtensions::from_possible_biomes(&biome_source.possible_biomes());
        let surface_system = SurfaceSystem::new(
            &splitter,
            &noise_parameters,
            N::surface_noise_ids(),
            N::surface_gradient_ids(),
            default_block_id,
            N::Settings::SEA_LEVEL,
        );
        let ore_veinifier = N::Settings::ORE_VEINS_ENABLED.then(|| OreVeinifier::new(&splitter));
        let biome_zoom_seed = obfuscate_biome_seed(seed as i64);
        let vegetation_stage = VegetationStage::new(
            seed as i64,
            biome_zoom_seed,
            &biome_source.possible_biome_refs(),
            &REGISTRY,
        );
        Self {
            noises: Box::new(N::create(seed, &splitter, &noise_parameters)),
            biome_source,
            splitter,
            ore_veinifier,
            surface_system,
            surface_extensions,
            biome_zoom_seed,
            vegetation_stage,
        }
    }

    fn tile_with_cache(
        &self,
        cache: &mut SurfaceChunkCache,
        origin_x: i32,
        origin_z: i32,
        size: u32,
        resolution: u32,
    ) -> SurfaceTile {
        assert!(size > 0 && resolution > 0 && size.is_multiple_of(resolution));
        let samples_per_side = size / resolution + 1;
        let capacity = (samples_per_side * samples_per_side) as usize;
        let mut heights = Vec::with_capacity(capacity);
        let mut colors = Vec::with_capacity(capacity * 3);
        let mut present = Vec::with_capacity(capacity);
        let mut surface_blocks = Vec::with_capacity(capacity);
        let mut biomes = Vec::new();
        let mut biome_lookup = HashMap::new();
        let mut biome_indices = Vec::with_capacity(capacity);

        for sample_z in 0..samples_per_side {
            for sample_x in 0..samples_per_side {
                let x = origin_x.saturating_add((sample_x * resolution) as i32);
                let z = origin_z.saturating_add((sample_z * resolution) as i32);
                let chunk_x = x.div_euclid(16);
                let chunk_z = z.div_euclid(16);
                self.ensure_cached_chunk(cache, chunk_x, chunk_z);
                let chunk = cache
                    .touch((chunk_x, chunk_z))
                    .expect("surface chunk must have been cached");
                let index = (z.rem_euclid(16) * 16 + x.rem_euclid(16)) as usize;
                let SurfaceColumn {
                    height,
                    state,
                    exists,
                } = chunk.pre_carver_surface[index];
                heights.push(height);
                present.push(u8::from(exists));
                surface_blocks.push(canonical_block_key(state));

                let mut biome_sampler = self.biome_source.chunk_sampler();
                let biome = biome_sampler.sample(x >> 2, i32::from(height) >> 2, z >> 2);
                let biome_key = format!("{}:{}", biome.key.namespace, biome.key.path);
                let palette_index = if let Some(index) = biome_lookup.get(&biome_key) {
                    *index
                } else {
                    assert!(
                        biomes.len() < usize::from(u16::MAX),
                        "biome palette exceeds u16"
                    );
                    let index = biomes.len() as u16;
                    biome_lookup.insert(biome_key.clone(), index);
                    biomes.push(biome_key);
                    index
                };
                biome_indices.push(palette_index);
                let color = biome.effects.grass_color.unwrap_or(0x6a_a8_4f) as u32;
                colors.extend_from_slice(&[(color >> 16) as u8, (color >> 8) as u8, color as u8]);
            }
        }

        let vegetation_blocks = self.vegetation_tile(origin_x, origin_z, size, cache);

        SurfaceTile {
            samples_per_side,
            heights,
            colors,
            biomes,
            biome_indices,
            present,
            surface_blocks,
            vegetation_blocks,
            min_y: N::Settings::MIN_Y as i16,
        }
    }

    fn vegetation_tile(
        &self,
        origin_x: i32,
        origin_z: i32,
        size: u32,
        cache: &mut SurfaceChunkCache,
    ) -> Vec<SurfaceVegetationBlock> {
        let min_chunk_x = origin_x.div_euclid(16);
        let min_chunk_z = origin_z.div_euclid(16);
        let max_x = origin_x + size as i32 - 1;
        let max_z = origin_z + size as i32 - 1;
        let max_chunk_x = max_x.div_euclid(16);
        let max_chunk_z = max_z.div_euclid(16);

        // Features writes can cross one chunk boundary.  Sources in the first
        // surrounding ring may therefore contribute a tree crown or flower to
        // this tile; each such source itself requires a 3×3 read halo.
        let source_min_chunk_x = min_chunk_x - 1;
        let source_min_chunk_z = min_chunk_z - 1;
        let source_max_chunk_x = max_chunk_x + 1;
        let source_max_chunk_z = max_chunk_z + 1;
        let mut chunks = HashMap::new();
        for chunk_z in source_min_chunk_z - 1..=source_max_chunk_z + 1 {
            for chunk_x in source_min_chunk_x - 1..=source_max_chunk_x + 1 {
                self.ensure_cached_chunk(cache, chunk_x, chunk_z);
                let chunk = cache
                    .touch((chunk_x, chunk_z))
                    .expect("vegetation halo chunk must have been cached")
                    .post_carver
                    .clone();
                chunks.insert((chunk_x, chunk_z), chunk);
            }
        }

        let mut region = InMemoryVegetationRegion::new(
            &mut chunks,
            &self.biome_source,
            N::Settings::MIN_Y,
            N::Settings::HEIGHT,
        );
        let mut changed_positions = std::collections::HashSet::<(i32, i32, i32)>::new();
        // Native feature tasks are submitted in canonical X/Z ascending
        // order. Preserve that write order for overlapping crowns from
        // neighboring source chunks.
        for chunk_x in source_min_chunk_x..=source_max_chunk_x {
            for chunk_z in source_min_chunk_z..=source_max_chunk_z {
                for block in
                    self.vegetation_stage
                        .decorate_chunk(&mut region, &REGISTRY, chunk_x, chunk_z)
                {
                    if block.x >= origin_x
                        && block.x <= max_x
                        && block.z >= origin_z
                        && block.z <= max_z
                    {
                        changed_positions.insert((block.x, block.y, block.z));
                    }
                }
            }
        }
        let mut final_blocks = changed_positions
            .into_iter()
            .map(|(x, y, z)| {
                let state = region.block_state(BlockPos::new(x, y, z));
                SurfaceVegetationBlock {
                    x,
                    y,
                    z,
                    block: canonical_block_key(state),
                    state: canonical_block_state_key(state),
                }
            })
            .collect::<Vec<_>>();
        final_blocks.sort_by_key(|block| (block.x, block.y, block.z));
        final_blocks
    }

    fn ensure_cached_chunk(&self, cache: &mut SurfaceChunkCache, chunk_x: i32, chunk_z: i32) {
        let key = (chunk_x, chunk_z);
        if cache.chunks.contains_key(&key) {
            cache.stats.hits += 1;
            return;
        }
        cache.stats.misses += 1;
        let mut post_carver = self.sample_surface_chunk_data(chunk_x, chunk_z);
        let pre_carver_surface = Box::new(std::array::from_fn(|index| {
            let (height, state, exists) = post_carver.top_surface(index);
            SurfaceColumn {
                height,
                state,
                exists,
            }
        }));
        self.apply_carvers_to_chunk(&mut post_carver);
        cache.clock = cache.clock.wrapping_add(1);
        cache.insert(
            key,
            CachedSurfaceChunk {
                pre_carver_surface,
                post_carver,
                last_used: cache.clock,
            },
        );
    }

    fn sample_surface_chunk_data(&self, chunk_x: i32, chunk_z: i32) -> InMemorySurfaceChunk {
        let chunk_min_x = chunk_x * 16;
        let chunk_min_z = chunk_z * 16;
        let min_y = N::Settings::MIN_Y;
        let height = N::Settings::HEIGHT;
        let default_block_id = N::Settings::default_block_id();
        let mut blocks = InMemorySurfaceChunk::new(chunk_min_x, chunk_min_z, min_y, height);
        let mut noise_chunk = NoiseChunk::<N>::new(chunk_min_x, chunk_min_z);
        let mut column_cache = N::ColumnCache::default();
        column_cache.init_grid(chunk_min_x, chunk_min_z, &self.noises);
        let mut aquifer = Aquifer::<N>::new(
            chunk_min_x,
            chunk_min_z,
            min_y,
            height,
            &self.splitter,
            &self.noises,
            column_cache.clone(),
        );

        noise_chunk.fill(
            &self.noises,
            &mut column_cache,
            None,
            |local_x, world_y, local_z, density, interpolated, cache| {
                let state = match aquifer.compute_substance(
                    &self.noises,
                    chunk_min_x + local_x as i32,
                    world_y,
                    chunk_min_z + local_z as i32,
                    density,
                ) {
                    AquiferResult::Solid => self
                        .ore_veinifier
                        .as_ref()
                        .and_then(|ore_veinifier| {
                            ore_veinifier.compute_interpolated(
                                &*self.noises,
                                cache,
                                interpolated,
                                chunk_min_x + local_x as i32,
                                world_y,
                                chunk_min_z + local_z as i32,
                            )
                        })
                        .unwrap_or(default_block_id),
                    AquiferResult::Fluid(state) => state,
                    AquiferResult::Air => return,
                };
                blocks.set_initial(local_x, world_y, local_z, state);
            },
        );

        let stage = SurfaceStage::<N>::new(
            &self.surface_system,
            default_block_id,
            self.biome_zoom_seed,
            self.surface_extensions,
        );
        let preliminary_surface_corners =
            stage
                .needs_preliminary_surface()
                .then(|| PreliminarySurfaceCorners {
                    nw: aquifer.preliminary_surface_level(&self.noises, chunk_min_x, chunk_min_z),
                    ne: aquifer.preliminary_surface_level(
                        &self.noises,
                        chunk_min_x + 16,
                        chunk_min_z,
                    ),
                    sw: aquifer.preliminary_surface_level(
                        &self.noises,
                        chunk_min_x,
                        chunk_min_z + 16,
                    ),
                    se: aquifer.preliminary_surface_level(
                        &self.noises,
                        chunk_min_x + 16,
                        chunk_min_z + 16,
                    ),
                });
        let mut biomes =
            InMemorySurfaceBiomeAccess::new(&self.biome_source, chunk_x, chunk_z, min_y, height);
        stage.build_surface(&mut blocks, &mut biomes, preliminary_surface_corners);
        blocks
    }

    fn apply_carvers_to_chunk(&self, chunk: &mut InMemorySurfaceChunk) {
        let carvers = CarverStage::<N>::new(
            &self.noises,
            &self.splitter,
            &self.surface_system,
            self.vegetation_stage.seed(),
            self.biome_zoom_seed,
        );
        let mut biome_sampler = self.biome_source.chunk_sampler();
        carvers.apply_chunk(chunk, |quart_x, quart_y, quart_z| {
            biome_sampler.sample(quart_x, quart_y, quart_z).id() as u16
        });
    }

    fn carved_chunk_snapshot(&self, chunk_x: i32, chunk_z: i32) -> CarvedChunkSnapshot {
        let mut chunk = self.sample_surface_chunk_data(chunk_x, chunk_z);
        self.apply_carvers_to_chunk(&mut chunk);
        chunk.snapshot()
    }

    fn selected_vegetation_transaction_snapshot(
        &self,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Vec<SurfaceVegetationBlock> {
        let mut chunks = HashMap::new();
        for source_z in chunk_z - 1..=chunk_z + 1 {
            for source_x in chunk_x - 1..=chunk_x + 1 {
                let mut chunk = self.sample_surface_chunk_data(source_x, source_z);
                self.apply_carvers_to_chunk(&mut chunk);
                chunks.insert((source_x, source_z), chunk);
            }
        }
        let mut region = InMemoryVegetationRegion::new(
            &mut chunks,
            &self.biome_source,
            N::Settings::MIN_Y,
            N::Settings::HEIGHT,
        );
        let mut changed_positions = std::collections::HashSet::new();
        for block in self
            .vegetation_stage
            .decorate_chunk(&mut region, &REGISTRY, chunk_x, chunk_z)
        {
            changed_positions.insert((block.x, block.y, block.z));
        }
        let mut blocks = changed_positions
            .into_iter()
            .map(|(x, y, z)| {
                let state = region.block_state(BlockPos::new(x, y, z));
                SurfaceVegetationBlock {
                    x,
                    y,
                    z,
                    block: canonical_block_key(state),
                    state: canonical_block_state_key(state),
                }
            })
            .collect::<Vec<_>>();
        blocks.sort_by_key(|block| (block.x, block.y, block.z));
        blocks
    }

    fn noise_volume_chunk(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        requested_min_y: i32,
        requested_max_y: i32,
        lod: u32,
    ) -> NoiseVolume {
        assert!(
            matches!(lod, 1 | 4 | 16 | 64 | 256),
            "unsupported volume LOD"
        );
        let min_y = requested_min_y.max(N::Settings::MIN_Y);
        let max_y = requested_max_y.min(N::Settings::MIN_Y + N::Settings::HEIGHT);
        assert!(min_y < max_y, "volume range is outside this dimension");
        let cells_xz = 16_u32.div_ceil(lod);
        let cells_y = (max_y - min_y).unsigned_abs().div_ceil(lod);
        let mut voxels = vec![0; (cells_xz * cells_xz * cells_y) as usize];
        let chunk_min_x = chunk_x * 16;
        let chunk_min_z = chunk_z * 16;
        let mut noise_chunk = NoiseChunk::<N>::new(chunk_min_x, chunk_min_z);
        let mut cache = N::ColumnCache::default();
        cache.init_grid(chunk_min_x, chunk_min_z, &self.noises);
        let mut aquifer = Aquifer::<N>::new(
            chunk_min_x,
            chunk_min_z,
            N::Settings::MIN_Y,
            N::Settings::HEIGHT,
            &self.splitter,
            &self.noises,
            cache.clone(),
        );
        let water_id = REGISTRY.blocks.get_default_state_id(&vanilla_blocks::WATER);
        noise_chunk.fill(
            &self.noises,
            &mut cache,
            None,
            |local_x, y, local_z, density, _, _| {
                if y < min_y
                    || y >= max_y
                    || (local_x as u32) % lod != 0
                    || (local_z as u32) % lod != 0
                    || (y - min_y).unsigned_abs() % lod != 0
                {
                    return;
                }
                let x_cell = local_x as u32 / lod;
                let z_cell = local_z as u32 / lod;
                let y_cell = (y - min_y).unsigned_abs() / lod;
                let index = (y_cell * cells_xz * cells_xz + z_cell * cells_xz + x_cell) as usize;
                let material = match aquifer.compute_substance(
                    &self.noises,
                    chunk_min_x + local_x as i32,
                    y,
                    chunk_min_z + local_z as i32,
                    density,
                ) {
                    AquiferResult::Air => 0,
                    AquiferResult::Solid => 1,
                    AquiferResult::Fluid(id) if id == water_id => 2,
                    AquiferResult::Fluid(_) => 3,
                };
                voxels[index] = material;
            },
        );
        NoiseVolume {
            cells_xz,
            cells_y,
            min_y: min_y as i16,
            lod: lod as u16,
            voxels,
        }
    }
}

/// Minimal mutable chunk representation used by the browser-safe Surface host.
///
/// It intentionally contains only pre-Features block data and the world-surface
/// heights read by the shared Surface stage. Native chunks remain responsible
/// for persistence, postprocessing and status publication.
#[derive(Clone)]
struct InMemorySurfaceChunk {
    chunk_min_x: i32,
    chunk_min_z: i32,
    min_y: i32,
    height: usize,
    blocks: Vec<BlockStateId>,
    world_surface: [i32; 256],
}

impl InMemorySurfaceChunk {
    fn payload_bytes(&self) -> usize {
        self.blocks.capacity() * std::mem::size_of::<BlockStateId>()
            + std::mem::size_of_val(&self.world_surface)
    }

    fn new(chunk_min_x: i32, chunk_min_z: i32, min_y: i32, height: i32) -> Self {
        let air = vanilla_blocks::AIR.default_state();
        Self {
            chunk_min_x,
            chunk_min_z,
            min_y,
            height: height as usize,
            blocks: vec![air; 16 * 16 * height as usize],
            world_surface: [min_y; 256],
        }
    }

    fn column_index(local_x: usize, local_z: usize) -> usize {
        local_z * 16 + local_x
    }

    fn block_index(&self, local_x: usize, relative_y: usize, local_z: usize) -> usize {
        (Self::column_index(local_x, local_z) * self.height) + relative_y
    }

    fn set_initial(&mut self, local_x: usize, world_y: i32, local_z: usize, state: BlockStateId) {
        let relative_y = (world_y - self.min_y) as usize;
        let index = self.block_index(local_x, relative_y, local_z);
        self.blocks[index] = state;
        if !state.is_air() {
            let column = Self::column_index(local_x, local_z);
            self.world_surface[column] = self.world_surface[column].max(world_y + 1);
        }
    }

    fn top_surface(&self, index: usize) -> (i16, BlockStateId, bool) {
        let local_x = index % 16;
        let local_z = index / 16;
        let air = vanilla_blocks::AIR.default_state();
        for relative_y in (0..self.height).rev() {
            let state = self.blocks[self.block_index(local_x, relative_y, local_z)];
            if !state.is_air() && !state.get_block().config.liquid {
                return (self.min_y as i16 + relative_y as i16, state, true);
            }
        }
        (self.min_y as i16, air, false)
    }

    fn block_at(&self, local_x: usize, world_y: i32, local_z: usize) -> Option<BlockStateId> {
        if local_x >= 16
            || local_z >= 16
            || !(self.min_y..self.min_y + self.height as i32).contains(&world_y)
        {
            return None;
        }
        let relative_y = (world_y - self.min_y) as usize;
        Some(self.blocks[self.block_index(local_x, relative_y, local_z)])
    }

    fn feature_height_at(&self, kind: FeatureHeightmap, local_x: usize, local_z: usize) -> i32 {
        for relative_y in (0..self.height).rev() {
            let state = self.blocks[self.block_index(local_x, relative_y, local_z)];
            let block = state.get_block();
            let opaque = match kind {
                FeatureHeightmap::WorldSurface | FeatureHeightmap::WorldSurfaceWg => {
                    !state.is_air()
                }
                FeatureHeightmap::OceanFloor | FeatureHeightmap::OceanFloorWg => {
                    state.blocks_motion()
                }
                FeatureHeightmap::MotionBlocking => state.blocks_motion() || state.has_fluid(),
                FeatureHeightmap::MotionBlockingNoLeaves => {
                    (state.blocks_motion() || state.has_fluid())
                        && !block.has_tag(&steel_registry::vanilla_block_tags::BlockTag::LEAVES)
                }
            };
            if opaque {
                return self.min_y + relative_y as i32 + 1;
            }
        }
        self.min_y
    }

    fn update_surface_after_write(
        &mut self,
        local_x: usize,
        relative_y: usize,
        local_z: usize,
        previous: BlockStateId,
        state: BlockStateId,
    ) {
        let column = Self::column_index(local_x, local_z);
        let world_y = self.min_y + relative_y as i32;
        if !state.is_air() && world_y >= self.world_surface[column] {
            self.world_surface[column] = world_y + 1;
        } else if !previous.is_air() && state.is_air() && self.world_surface[column] == world_y + 1
        {
            self.recompute_world_surface(local_x, local_z);
        }
    }

    fn recompute_world_surface(&mut self, local_x: usize, local_z: usize) {
        let column = Self::column_index(local_x, local_z);
        self.world_surface[column] = self.min_y;
        for relative_y in (0..self.height).rev() {
            let state = self.blocks[self.block_index(local_x, relative_y, local_z)];
            if !state.is_air() {
                self.world_surface[column] = self.min_y + relative_y as i32 + 1;
                break;
            }
        }
    }

    fn snapshot(&self) -> CarvedChunkSnapshot {
        let mut states = Vec::with_capacity(self.blocks.len());
        for relative_y in 0..self.height {
            for local_z in 0..16 {
                for local_x in 0..16 {
                    states.push(self.blocks[self.block_index(local_x, relative_y, local_z)]);
                }
            }
        }
        CarvedChunkSnapshot {
            states,
            world_surface_wg: self.world_surface,
        }
    }
}

impl SurfaceBlockAccess for InMemorySurfaceChunk {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn chunk_min_x(&self) -> i32 {
        self.chunk_min_x
    }

    fn chunk_min_z(&self) -> i32 {
        self.chunk_min_z
    }

    fn prime_world_surface_heightmap(&mut self) {}

    fn world_surface_height_at(&mut self, local_x: usize, local_z: usize) -> i32 {
        self.world_surface[Self::column_index(local_x, local_z)]
    }

    fn read_column_into(&mut self, local_x: usize, local_z: usize, output: &mut Vec<BlockStateId>) {
        output.clear();
        let start = self.block_index(local_x, 0, local_z);
        output.extend_from_slice(&self.blocks[start..start + self.height]);
    }

    fn write_column(&mut self, local_x: usize, local_z: usize, writes: &[(usize, BlockStateId)]) {
        for &(relative_y, state) in writes {
            self.set_relative_block(local_x, relative_y, local_z, state);
        }
    }

    fn get_relative_block(
        &mut self,
        local_x: usize,
        relative_y: usize,
        local_z: usize,
    ) -> Option<BlockStateId> {
        (local_x < 16 && local_z < 16 && relative_y < self.height)
            .then(|| self.blocks[self.block_index(local_x, relative_y, local_z)])
    }

    fn set_relative_block(
        &mut self,
        local_x: usize,
        relative_y: usize,
        local_z: usize,
        state: BlockStateId,
    ) {
        if local_x >= 16 || local_z >= 16 || relative_y >= self.height {
            return;
        }
        let index = self.block_index(local_x, relative_y, local_z);
        let previous = self.blocks[index];
        self.blocks[index] = state;
        self.update_surface_after_write(local_x, relative_y, local_z, previous, state);
    }
}

impl CarverBlockAccess for InMemorySurfaceChunk {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn height(&self) -> i32 {
        self.height as i32
    }

    fn chunk_min_x(&self) -> i32 {
        self.chunk_min_x
    }

    fn chunk_min_z(&self) -> i32 {
        self.chunk_min_z
    }

    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        let local_x = pos.x().rem_euclid(16) as usize;
        let local_z = pos.z().rem_euclid(16) as usize;
        self.block_at(local_x, pos.y(), local_z)
            .unwrap_or_else(|| vanilla_blocks::AIR.default_state())
    }

    fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId) {
        if !(self.min_y..self.min_y + self.height as i32).contains(&pos.y()) {
            return;
        }
        self.set_relative_block(
            pos.x().rem_euclid(16) as usize,
            (pos.y() - self.min_y) as usize,
            pos.z().rem_euclid(16) as usize,
            state,
        );
    }

    fn world_surface_wg_first_available(&self, local_x: usize, local_z: usize) -> i32 {
        self.world_surface[Self::column_index(local_x, local_z)]
    }
}

/// Precomputed local and one-quart-neighbor biome palettes for the WASM host.
///
/// Each palette is filled in the same section/X/Y/Z order as native
/// create_biomes. That matters for the multi-noise R-tree warm start at exact
/// biome-boundary ties; a sparse direct sampler would not reproduce it.
struct InMemorySurfaceBiomeAccess {
    chunk_x: i32,
    chunk_z: i32,
    min_quart_y: i32,
    total_quarts_y: usize,
    palettes: Vec<Box<[u16]>>,
}

impl InMemorySurfaceBiomeAccess {
    fn new(
        biome_source: &BiomeSourceKind,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
    ) -> Self {
        let section_count = (height / 16) as usize;
        let mut palettes = Vec::with_capacity(9);
        for chunk_z_offset in -1..=1 {
            for chunk_x_offset in -1..=1 {
                let palette_chunk_x = chunk_x + chunk_x_offset;
                let palette_chunk_z = chunk_z + chunk_z_offset;
                let mut sampler = biome_source.chunk_sampler();
                sampler.init_grid(palette_chunk_x * 16, palette_chunk_z * 16);
                let mut palette = vec![0; section_count * 64];
                for section_index in 0..section_count {
                    let section_quart_y = min_y / 4 + section_index as i32 * 4;
                    for local_quart_x in 0..4i32 {
                        let quart_x = palette_chunk_x * 4 + local_quart_x;
                        for local_quart_y in 0..4i32 {
                            let quart_y = section_quart_y + local_quart_y;
                            for local_quart_z in 0..4i32 {
                                let quart_z = palette_chunk_z * 4 + local_quart_z;
                                let index = section_index * 64
                                    + local_quart_y as usize * 16
                                    + local_quart_z as usize * 4
                                    + local_quart_x as usize;
                                palette[index] =
                                    sampler.sample(quart_x, quart_y, quart_z).id() as u16;
                            }
                        }
                    }
                }
                palettes.push(palette.into_boxed_slice());
            }
        }
        Self {
            chunk_x,
            chunk_z,
            min_quart_y: min_y >> 2,
            total_quarts_y: section_count * 4,
            palettes,
        }
    }
}

impl SurfaceBiomeAccess for InMemorySurfaceBiomeAccess {
    fn biome_id_at_quart(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> u16 {
        let source_chunk_x = quart_x >> 2;
        let source_chunk_z = quart_z >> 2;
        let chunk_x_offset = source_chunk_x - self.chunk_x;
        let chunk_z_offset = source_chunk_z - self.chunk_z;
        if !(-1..=1).contains(&chunk_x_offset) || !(-1..=1).contains(&chunk_z_offset) {
            panic!("surface biome lookup escaped the one-quart neighboring chunk ring");
        }
        let palette_index = ((chunk_z_offset + 1) * 3 + (chunk_x_offset + 1)) as usize;
        let local_quart_x = (quart_x - source_chunk_x * 4) as usize;
        let local_quart_z = (quart_z - source_chunk_z * 4) as usize;
        let quart_y_in_chunk =
            (quart_y - self.min_quart_y).clamp(0, self.total_quarts_y as i32 - 1) as usize;
        let section_index = quart_y_in_chunk / 4;
        let local_quart_y = quart_y_in_chunk % 4;
        self.palettes[palette_index]
            [section_index * 64 + local_quart_y * 16 + local_quart_z * 4 + local_quart_x]
    }
}

/// Mutable post-Carvers terrain halo for the portable Features slice.
struct InMemoryVegetationRegion<'a> {
    chunks: &'a mut HashMap<(i32, i32), InMemorySurfaceChunk>,
    biome_source: &'a BiomeSourceKind,
    biome_access: HashMap<(i32, i32), InMemorySurfaceBiomeAccess>,
    min_y: i32,
    height: i32,
}

impl<'a> InMemoryVegetationRegion<'a> {
    fn new(
        chunks: &'a mut HashMap<(i32, i32), InMemorySurfaceChunk>,
        biome_source: &'a BiomeSourceKind,
        min_y: i32,
        height: i32,
    ) -> Self {
        Self {
            chunks,
            biome_source,
            biome_access: HashMap::new(),
            min_y,
            height,
        }
    }

    fn chunk_and_local(value: i32) -> (i32, usize) {
        (value.div_euclid(16), value.rem_euclid(16) as usize)
    }
}

impl VegetationBlockAccess for InMemoryVegetationRegion<'_> {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn max_y_exclusive(&self) -> i32 {
        self.min_y + self.height
    }

    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        let (chunk_x, local_x) = Self::chunk_and_local(pos.x());
        let (chunk_z, local_z) = Self::chunk_and_local(pos.z());
        self.chunks
            .get(&(chunk_x, chunk_z))
            .and_then(|chunk| chunk.block_at(local_x, pos.y(), local_z))
            .unwrap_or_else(|| vanilla_blocks::AIR.default_state())
    }

    fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId) {
        let (chunk_x, local_x) = Self::chunk_and_local(pos.x());
        let (chunk_z, local_z) = Self::chunk_and_local(pos.z());
        let Some(chunk) = self.chunks.get_mut(&(chunk_x, chunk_z)) else {
            return;
        };
        if !(self.min_y..self.min_y + self.height).contains(&pos.y()) {
            return;
        }
        chunk.set_relative_block(local_x, (pos.y() - self.min_y) as usize, local_z, state);
    }

    fn height_at(&self, kind: FeatureHeightmap, x: i32, z: i32) -> i32 {
        let (chunk_x, local_x) = Self::chunk_and_local(x);
        let (chunk_z, local_z) = Self::chunk_and_local(z);
        self.chunks
            .get(&(chunk_x, chunk_z))
            .map_or(self.min_y, |chunk| {
                chunk.feature_height_at(kind, local_x, local_z)
            })
    }

    fn biome_id_at_quart(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> u16 {
        let chunk_x = quart_x.div_euclid(4);
        let chunk_z = quart_z.div_euclid(4);
        let access = self
            .biome_access
            .entry((chunk_x, chunk_z))
            .or_insert_with(|| {
                InMemorySurfaceBiomeAccess::new(
                    self.biome_source,
                    chunk_x,
                    chunk_z,
                    self.min_y,
                    self.height,
                )
            });
        access.biome_id_at_quart(quart_x, quart_y, quart_z)
    }
}

fn canonical_block_key(state: BlockStateId) -> String {
    let Some(block) = REGISTRY.blocks.by_state_id(state) else {
        panic!("surface host produced an unknown block state {}", state.0);
    };
    block.key.to_string()
}

fn canonical_block_state_key(state: BlockStateId) -> String {
    let block = canonical_block_key(state);
    let properties = REGISTRY.blocks.get_properties(state);
    if properties.is_empty() {
        return block;
    }
    let properties = properties
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    format!("{block}[{}]", properties.join(","))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::{SurfaceChunkCache, SurfaceDimension, SurfaceSampler, SurfaceTile};

    fn assert_tiles_equal(actual: &SurfaceTile, expected: &SurfaceTile) {
        assert_eq!(actual.samples_per_side, expected.samples_per_side);
        assert_eq!(actual.heights, expected.heights);
        assert_eq!(actual.colors, expected.colors);
        assert_eq!(actual.biomes, expected.biomes);
        assert_eq!(actual.biome_indices, expected.biome_indices);
        assert_eq!(actual.present, expected.present);
        assert_eq!(actual.surface_blocks, expected.surface_blocks);
        assert_eq!(actual.vegetation_blocks, expected.vegetation_blocks);
        assert_eq!(actual.min_y, expected.min_y);
    }

    #[test]
    fn cached_tiles_are_identical_to_uncached_tiles() {
        let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
        let mut cache = SurfaceChunkCache::default();
        let cases = [
            (0, 0, 64),
            (64, 0, 64),
            (-128, -64, 64),
            (7, 11, 64),
            (256, 256, 256),
        ];
        for (x, z, size) in cases {
            let cached = sampler.tile_with_cache(&mut cache, x, z, size, 1);
            let uncached = sampler.tile(x, z, size, 1);
            assert_tiles_equal(&cached, &uncached);
        }

        // Exercise summarized pre-carver columns across constant eviction.
        let mut tiny_cache = SurfaceChunkCache::new(1);
        let cached = sampler.tile_with_cache(&mut tiny_cache, 0, 0, 16, 1);
        let uncached = sampler.tile(0, 0, 16, 1);
        assert_tiles_equal(&cached, &uncached);
    }

    fn median_ms(mut values: Vec<f64>) -> f64 {
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    }

    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn measure_surface_chunk_cache() {
        const REPETITIONS: usize = 3;
        const CAPACITIES: [usize; 4] = [96, 160, 256, 400];
        let mut isolated_cached = Vec::new();
        let mut isolated_uncached = Vec::new();

        println!("capacity ratio hit_rate peak_chunks retained_payload_bytes evictions");
        for capacity in CAPACITIES {
            let mut grid_cached = Vec::new();
            let mut grid_uncached = Vec::new();
            let mut final_stats = None;
            let mut retained_payload_bytes = 0;
            for _ in 0..REPETITIONS {
                let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
                let mut cache = SurfaceChunkCache::new(capacity);
                let start = Instant::now();
                for tile_z in 0..4 {
                    for tile_x in 0..4 {
                        let _ =
                            sampler.tile_with_cache(&mut cache, tile_x * 64, tile_z * 64, 64, 1);
                    }
                }
                grid_cached.push(start.elapsed().as_secs_f64() * 1_000.0);
                final_stats = Some(cache.stats());
                retained_payload_bytes = cache.retained_payload_bytes();

                let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
                let start = Instant::now();
                for tile_z in 0..4 {
                    for tile_x in 0..4 {
                        let _ = sampler.tile(tile_x * 64, tile_z * 64, 64, 1);
                    }
                }
                grid_uncached.push(start.elapsed().as_secs_f64() * 1_000.0);
            }
            let cached = median_ms(grid_cached);
            let uncached = median_ms(grid_uncached);
            let stats = final_stats.expect("capacity sweep must execute");
            let requests = stats.hits + stats.misses;
            println!(
                "{capacity} {:.3}x {:.2}% {} {} {} (cached={cached:.3} ms uncached={uncached:.3} ms hits={} misses={})",
                uncached / cached,
                stats.hits as f64 * 100.0 / requests as f64,
                stats.peak_retained_chunks,
                retained_payload_bytes,
                stats.evictions,
                stats.hits,
                stats.misses,
            );
        }

        for _ in 0..REPETITIONS {
            let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
            let mut cache = SurfaceChunkCache::default();
            let start = Instant::now();
            let _ = sampler.tile_with_cache(&mut cache, 0, 0, 64, 1);
            isolated_cached.push(start.elapsed().as_secs_f64() * 1_000.0);

            let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
            let start = Instant::now();
            let _ = sampler.tile(0, 0, 64, 1);
            isolated_uncached.push(start.elapsed().as_secs_f64() * 1_000.0);
        }

        let isolated_cached = median_ms(isolated_cached);
        let isolated_uncached = median_ms(isolated_uncached);
        println!(
            "isolated cached={isolated_cached:.3} ms uncached={isolated_uncached:.3} ms ratio={:.3}x regression={:.2}%",
            isolated_uncached / isolated_cached,
            (isolated_cached / isolated_uncached - 1.0) * 100.0,
        );
    }

    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn measure_isolated_surface_chunk_cache() {
        const REPETITIONS: usize = 9;
        let mut cached_times = Vec::new();
        let mut uncached_times = Vec::new();
        for repetition in 0..REPETITIONS {
            let mut measure_cached = || {
                let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
                let mut cache = SurfaceChunkCache::default();
                let start = Instant::now();
                let _ = sampler.tile_with_cache(&mut cache, 0, 0, 64, 1);
                cached_times.push(start.elapsed().as_secs_f64() * 1_000.0);
            };
            let mut measure_uncached = || {
                let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
                let start = Instant::now();
                let _ = sampler.tile(0, 0, 64, 1);
                uncached_times.push(start.elapsed().as_secs_f64() * 1_000.0);
            };
            if repetition.is_multiple_of(2) {
                measure_cached();
                measure_uncached();
            } else {
                measure_uncached();
                measure_cached();
            }
        }
        let cached = median_ms(cached_times);
        let uncached = median_ms(uncached_times);
        println!(
            "isolated_order_balanced cached={cached:.3} ms uncached={uncached:.3} ms ratio={:.3}x regression={:.2}%",
            uncached / cached,
            (cached / uncached - 1.0) * 100.0,
        );
    }

    #[test]
    fn noise_volume_has_a_compact_palette_grid() {
        let sampler = SurfaceSampler::new(0, SurfaceDimension::End);
        let volume = sampler.noise_volume_chunk(0, 0, -64, 64, 4);
        assert_eq!(volume.cells_xz, 4);
        // End generation clamps the request to its 0..64 noise range.
        assert_eq!(volume.cells_y, 16);
        assert_eq!(volume.voxels.len(), 4 * 4 * 16);
        assert!(volume.voxels.iter().all(|material| *material <= 3));
    }

    #[test]
    fn surface_tile_reports_one_canonical_final_block_for_each_sample() {
        let sampler = SurfaceSampler::new(0, SurfaceDimension::Overworld);
        let tile = sampler.tile(0, 0, 16, 16);
        let expected_samples = (tile.samples_per_side * tile.samples_per_side) as usize;

        assert_eq!(tile.surface_blocks.len(), expected_samples);
        assert_eq!(tile.heights.len(), expected_samples);
        assert_eq!(tile.present.len(), expected_samples);
        assert!(
            tile.surface_blocks
                .iter()
                .all(|block| block.starts_with("minecraft:"))
        );
        assert!(
            tile.surface_blocks
                .iter()
                .zip(&tile.present)
                .all(|(block, present)| *present != 0 || block == "minecraft:air")
        );
    }

    #[test]
    fn cherry_grove_fixture_emits_sparse_final_vegetation_states() {
        // This chunk is a fixed cherry-grove fixture used by the native
        // Features parity harness. It deliberately exercises logs, leaf
        // distance states, petals and the tall-grass double-block path.
        let sampler = SurfaceSampler::new(1, SurfaceDimension::Overworld);
        let tile = sampler.tile(-108 * 16, -36 * 16, 16, 1);
        assert!(
            tile.vegetation_blocks
                .iter()
                .any(|block| block.block == "minecraft:cherry_log")
        );
        assert!(
            tile.vegetation_blocks
                .iter()
                .any(|block| block.block == "minecraft:cherry_leaves")
        );
        assert!(
            tile.vegetation_blocks
                .iter()
                .any(|block| block.block == "minecraft:pink_petals")
        );
    }
}
