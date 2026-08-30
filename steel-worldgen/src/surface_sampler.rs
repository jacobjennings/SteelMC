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
use crate::density::{ColumnCache, DimensionNoises, NoiseSettings};
use crate::density_functions::{end::EndNoises, nether::NetherNoises, overworld::OverworldNoises};
use crate::noise::NoiseChunk;
use crate::noise::{Aquifer, AquiferResult, OreVeinifier};
use crate::noise_parameters::get_noise_parameters;
use crate::surface::{
    PreliminarySurfaceCorners, SurfaceBiomeAccess, SurfaceBlockAccess, SurfaceExtensions,
    SurfaceStage, SurfaceSystem,
};

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
    /// Dimension minimum build height.
    pub min_y: i16,
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
        match self {
            Self::Overworld(sampler) => sampler.tile(origin_x, origin_z, size, resolution),
            Self::Nether(sampler) => sampler.tile(origin_x, origin_z, size, resolution),
            Self::End(sampler) => sampler.tile(origin_x, origin_z, size, resolution),
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
        Self {
            noises: Box::new(N::create(seed, &splitter, &noise_parameters)),
            biome_source,
            splitter,
            ore_veinifier,
            surface_system,
            surface_extensions,
            biome_zoom_seed: obfuscate_biome_seed(seed as i64),
        }
    }

    fn tile(&self, origin_x: i32, origin_z: i32, size: u32, resolution: u32) -> SurfaceTile {
        assert!(size > 0 && resolution > 0 && size.is_multiple_of(resolution));
        let samples_per_side = size / resolution + 1;
        let capacity = (samples_per_side * samples_per_side) as usize;
        let mut chunks = HashMap::new();
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
                let chunk = chunks
                    .entry((chunk_x, chunk_z))
                    .or_insert_with(|| self.sample_surface_chunk(chunk_x, chunk_z));
                let index = (z.rem_euclid(16) * 16 + x.rem_euclid(16)) as usize;
                let (height, state, exists) = chunk.top_surface(index);
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

        SurfaceTile {
            samples_per_side,
            heights,
            colors,
            biomes,
            biome_indices,
            present,
            surface_blocks,
            min_y: N::Settings::MIN_Y as i16,
        }
    }

    fn sample_surface_chunk(&self, chunk_x: i32, chunk_z: i32) -> SurfaceChunkTop {
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
        blocks.into_top_surface()
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

/// Final non-fluid top state for every local column of a generated chunk.
struct SurfaceChunkTop {
    heights: [i16; 256],
    states: Vec<BlockStateId>,
}

impl SurfaceChunkTop {
    fn top_surface(&self, index: usize) -> (i16, BlockStateId, bool) {
        let height = self.heights[index];
        let state = self.states[index];
        (height, state, !state.is_air())
    }
}

/// Minimal mutable chunk representation used by the browser-safe Surface host.
///
/// It intentionally contains only pre-Features block data and the world-surface
/// heights read by the shared Surface stage. Native chunks remain responsible
/// for persistence, postprocessing and status publication.
struct InMemorySurfaceChunk {
    chunk_min_x: i32,
    chunk_min_z: i32,
    min_y: i32,
    height: usize,
    blocks: Vec<BlockStateId>,
    world_surface: [i32; 256],
}

impl InMemorySurfaceChunk {
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

    fn into_top_surface(self) -> SurfaceChunkTop {
        let air = vanilla_blocks::AIR.default_state();
        let mut heights = [self.min_y as i16; 256];
        let mut states = vec![air; 256];
        for local_x in 0..16usize {
            for local_z in 0..16usize {
                let column = Self::column_index(local_x, local_z);
                for relative_y in (0..self.height).rev() {
                    let state = self.blocks[self.block_index(local_x, relative_y, local_z)];
                    if !state.is_air() && !state.get_block().config.liquid {
                        heights[column] = (self.min_y + relative_y as i32) as i16;
                        states[column] = state;
                        break;
                    }
                }
            }
        }
        SurfaceChunkTop { heights, states }
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

fn canonical_block_key(state: BlockStateId) -> String {
    let Some(block) = REGISTRY.blocks.by_state_id(state) else {
        panic!("surface host produced an unknown block state {}", state.0);
    };
    block.key.to_string()
}

#[cfg(test)]
mod tests {
    use super::{SurfaceDimension, SurfaceSampler};

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
}
