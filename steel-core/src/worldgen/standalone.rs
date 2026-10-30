//! World generation without a live world.
//!
//! Browser builds and native comparison tools generate terrain with the same
//! stage code as the server. Retained chunks stop after Terrain. A Full tile
//! replays Features over copies of them, in x-then-z chunk order. Its result
//! therefore does not depend on which tiles were requested before it.

use std::cell::OnceCell;
use std::sync::{Arc, Weak};

use glam::IVec3;
use rustc_hash::{FxBuildHasher, FxHashMap};
use steel_registry::biome::BiomeRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::dimension_type::DimensionTypeRef;
use steel_registry::{REGISTRY, vanilla_blocks, vanilla_dimension_types};
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Identifier};
use steel_worldgen::biomes::BiomeSourceKind;
use steel_worldgen::density::DimensionNoises;
use steel_worldgen::density_functions::{
    end::{EndNoiseSettings, EndNoises},
    nether::NetherNoiseSettings,
    overworld::{OverworldNoiseSettings, OverworldNoises},
};
use steel_worldgen::noise::preliminary_surface_level;
use steel_worldgen::structure::{StructureReferenceMap, StructureStart, StructureStartMap};

use crate::bootstrap::init_globals_once;
use crate::chunk::Chunk;
use crate::chunk::chunk_generation_task::StaticCache2D;
use crate::chunk::chunk_holder::ChunkHolder;
use crate::chunk::chunk_pyramid::GENERATION_PYRAMID;
use crate::chunk::chunk_ticket_manager::ChunkTicketLevel;
use crate::chunk::paletted_container::BiomePalette;
use crate::chunk::section::{ChunkSection, Sections};
use crate::chunk::status::ChunkStatus;
use crate::worldgen::generator::{ChunkGenerator, GenerationChunk, TerrainPhase};
use crate::worldgen::stages::structures::collect_source_references;
use crate::worldgen::stages::terrain::beardifier_from_starts;
use crate::worldgen::{
    ChunkGeneratorType, EndGenerator, NetherGenerator, OverworldGenerator, WorldGenContext,
    WorldGenRegion,
};

/// Structure starts can reach terrain this many chunks away.
const STRUCTURE_RADIUS: i32 = 8;
/// Features write into this many surrounding chunks.
const FEATURE_WRITE_RADIUS: i32 = 1;
/// Default number of retained Terrain chunks.
pub const DEFAULT_TERRAIN_CACHE_CAPACITY: usize = 256;
/// Retained structure-start and biome entries per retained Terrain chunk.
const STARTS_PER_TERRAIN_ENTRY: usize = 8;

/// A dimension the standalone generator supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandaloneDimension {
    /// The Overworld.
    Overworld,
    /// The Nether.
    Nether,
    /// The End.
    End,
}

impl StandaloneDimension {
    /// Returns the vanilla dimension type.
    #[must_use]
    pub fn dimension_type(self) -> DimensionTypeRef {
        match self {
            Self::Overworld => &vanilla_dimension_types::OVERWORLD,
            Self::Nether => &vanilla_dimension_types::THE_NETHER,
            Self::End => &vanilla_dimension_types::THE_END,
        }
    }

    const fn sea_level(self) -> i32 {
        match self {
            Self::Overworld => OverworldNoiseSettings::SEA_LEVEL,
            Self::Nether => NetherNoiseSettings::SEA_LEVEL,
            Self::End => EndNoiseSettings::SEA_LEVEL,
        }
    }

    /// The Nether surface is the first solid block below its ceiling.
    const fn surface_below_ceiling(self) -> bool {
        matches!(self, Self::Nether)
    }
}

/// Which stages a tile reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileDetail {
    /// Pre-carver surface plus the blocks Features and structures write.
    Full,
    /// Pre-carver surface only. Carvers and Features do not run.
    Coarse,
    /// Biomes over estimated heights where an estimate exists.
    ///
    /// The Overworld uses its preliminary surface level and the End an island
    /// estimate. The Nether has no useful estimate, so it reads the exact
    /// pre-carver surface.
    Biome,
}

/// Which changed blocks a Full tile reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedBlockFilter {
    /// Every block whose final state differs from the carved Terrain state.
    All,
    /// Changed blocks at or above the column's pre-carver surface.
    ///
    /// Blocks below it, such as ores, are hidden by the surface a viewer draws.
    AtOrAboveSurface,
}

/// One block whose state Features or structure placement changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeneratedBlock {
    /// Absolute world X.
    pub x: i32,
    /// Absolute world Y.
    pub y: i32,
    /// Absolute world Z.
    pub z: i32,
    /// Final state after every source chunk of the tile decorated.
    pub state: BlockStateId,
}

/// Sampled terrain for one square tile, including its far edge.
pub struct StandaloneTile {
    /// Number of samples on one edge.
    pub samples_per_side: u32,
    /// Surface Y for each sample, in Z-then-X order.
    pub heights: Vec<i16>,
    /// Whether each sample found terrain.
    pub present: Vec<bool>,
    /// Biome at each sample's surface.
    pub biomes: Vec<BiomeRef>,
    /// Pre-carver surface state at each sample. Empty for biome tiles.
    pub surface_states: Vec<BlockStateId>,
    /// Changed blocks inside the tile, sorted by X, Y, then Z.
    pub generated_blocks: Vec<GeneratedBlock>,
    /// Whether `heights` are estimates rather than generated surfaces.
    pub heights_estimated: bool,
    /// Dimension minimum build height.
    pub min_y: i16,
}

/// A generated structure start at a vanilla locate position.
pub struct StructureMarker {
    /// Position vanilla's locate command reports.
    pub locate_pos: BlockPos,
    /// The generated start.
    pub start: StructureStart,
}

/// Complete block states and fluid marks of one chunk.
pub struct ChunkSnapshot {
    /// Chunk X.
    pub chunk_x: i32,
    /// Chunk Z.
    pub chunk_z: i32,
    /// States in Y-then-Z-then-X order from the dimension minimum.
    pub states: Vec<BlockStateId>,
    /// Postprocessing marks as world positions, sorted, duplicates kept.
    pub postprocessing: Vec<(i32, i32, i32)>,
}

impl ChunkSnapshot {
    fn of(chunk: &Chunk, min_y: i32) -> Self {
        let chunk_x = chunk.pos.0.x;
        let chunk_z = chunk.pos.0.y;
        let mut states = Vec::with_capacity(chunk.sections.sections.len() * 4096);
        for section in &chunk.sections.sections {
            let section = section.read();
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        states.push(section.states.get(x, y, z));
                    }
                }
            }
        }
        let mut postprocessing = Vec::new();
        for (section_index, marks) in chunk.postprocessing.lock().iter().enumerate() {
            let section_min_y = min_y + section_index as i32 * 16;
            for packed in marks {
                postprocessing.push((
                    chunk_x * 16 + i32::from(packed & 15),
                    section_min_y + i32::from((packed >> 4) & 15),
                    chunk_z * 16 + i32::from((packed >> 8) & 15),
                ));
            }
        }
        postprocessing.sort_unstable();
        Self {
            chunk_x,
            chunk_z,
            states,
            postprocessing,
        }
    }
}

/// Base-noise occupancy sampled in Y-then-Z-then-X order.
pub struct NoiseVolume {
    /// Clipped lower sampling bound.
    pub min_y: i32,
    /// Horizontal samples in a chunk footprint.
    pub cells_xz: u32,
    /// Vertical samples, with an exclusive upper bound.
    pub cells_y: u32,
    /// Air, default solid, water and lava palette indices.
    pub voxels: Vec<u8>,
}

/// Retained-work counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StandaloneStats {
    /// Chunks whose structure starts were generated.
    pub starts_generated: u64,
    /// Chunks whose biomes were generated.
    pub biomes_generated: u64,
    /// Chunks whose noise and surface were generated.
    pub terrain_generated: u64,
    /// Feature regions decorated.
    pub feature_regions: u64,
}

#[derive(Clone, Copy)]
struct SurfaceColumn {
    height: i16,
    state: BlockStateId,
    exists: bool,
}

struct StartsEntry {
    starts: StructureStartMap,
    biomes: Option<Box<[BiomePalette]>>,
    last_used: u64,
}

struct TerrainEntry {
    surface: Box<[SurfaceColumn]>,
    carved: Option<Arc<Chunk>>,
    last_used: u64,
}

thread_local! {
    /// One generation pool per thread. Browser workers cannot spawn threads,
    /// so the pool runs on the calling thread, which rayon then keeps.
    static GENERATION_POOL: OnceCell<Arc<rayon::ThreadPool>> = const { OnceCell::new() };
}

fn generation_pool() -> Arc<rayon::ThreadPool> {
    GENERATION_POOL.with(|pool| {
        Arc::clone(pool.get_or_init(|| {
            Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .use_current_thread()
                    .build()
                    .unwrap_or_else(|error| {
                        panic!("standalone generation pool failed to start: {error}")
                    }),
            )
        }))
    })
}

/// Seed-bound generator that keeps chunks between tile requests.
pub struct StandaloneWorldgen {
    dimension: StandaloneDimension,
    seed: i64,
    pool: Arc<rayon::ThreadPool>,
    generator: Arc<ChunkGeneratorType>,
    context: Arc<WorldGenContext>,
    min_y: i32,
    height: i32,
    clock: u64,
    starts: FxHashMap<(i32, i32), StartsEntry>,
    terrain: FxHashMap<(i32, i32), TerrainEntry>,
    terrain_capacity: usize,
    stats: StandaloneStats,
}

impl StandaloneWorldgen {
    /// Creates a generator for one seed and dimension.
    ///
    /// `terrain_cache_capacity` bounds retained Terrain chunks. A request that
    /// needs more keeps all of them until it finishes.
    ///
    /// # Panics
    /// Panics if the capacity is zero.
    #[must_use]
    pub fn new(seed: i64, dimension: StandaloneDimension, terrain_cache_capacity: usize) -> Self {
        assert!(
            terrain_cache_capacity > 0,
            "terrain cache capacity must be positive"
        );
        let pool = generation_pool();
        let unsigned_seed = seed as u64;
        // Generator construction uses rayon. Running it inside the thread's own
        // pool keeps rayon from starting a global pool on this thread.
        let generator = Arc::new(pool.install(|| {
            init_globals_once();
            match dimension {
                StandaloneDimension::Overworld => {
                    ChunkGeneratorType::Overworld(OverworldGenerator::new(
                        None,
                        BiomeSourceKind::overworld(unsigned_seed),
                        unsigned_seed,
                        &pool,
                    ))
                }
                StandaloneDimension::Nether => ChunkGeneratorType::Nether(NetherGenerator::new(
                    None,
                    BiomeSourceKind::nether(unsigned_seed),
                    unsigned_seed,
                    &pool,
                )),
                StandaloneDimension::End => ChunkGeneratorType::End(EndGenerator::new(
                    None,
                    BiomeSourceKind::end(unsigned_seed),
                    unsigned_seed,
                    &pool,
                )),
            }
        }));
        let dimension_type = dimension.dimension_type();
        let context = Arc::new(WorldGenContext::new_standalone(
            Arc::clone(&generator),
            seed,
            dimension_type,
            dimension.sea_level(),
        ));
        Self {
            dimension,
            seed,
            pool,
            generator,
            context,
            min_y: dimension_type.min_y,
            height: dimension_type.height,
            clock: 0,
            starts: FxHashMap::default(),
            terrain: FxHashMap::default(),
            terrain_capacity: terrain_cache_capacity,
            stats: StandaloneStats::default(),
        }
    }

    /// Returns the dimension.
    #[must_use]
    pub const fn dimension(&self) -> StandaloneDimension {
        self.dimension
    }

    /// Returns retained-work counters.
    #[must_use]
    pub const fn stats(&self) -> StandaloneStats {
        self.stats
    }

    /// Samples Java 26.3 noise before surface, carvers or decoration.
    ///
    /// Structure adaptation is excluded, matching the base-noise volume contract.
    /// The fresh chunk is dropped after sampling and never enters the Terrain cache.
    ///
    /// # Errors
    /// Returns an error for invalid coordinates, vertical bounds or LOD.
    pub fn noise_volume_chunk(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        requested_min_y: i32,
        requested_max_y: i32,
        lod: u32,
    ) -> Result<NoiseVolume, String> {
        if !matches!(lod, 1 | 4 | 16 | 64 | 256) {
            return Err("LOD must be one of 1, 4, 16, 64, 256".to_owned());
        }
        if chunk_x
            .checked_mul(16)
            .and_then(|x| x.checked_add(15))
            .is_none()
            || chunk_z
                .checked_mul(16)
                .and_then(|z| z.checked_add(15))
                .is_none()
        {
            return Err("chunk footprint must fit signed 32-bit block coordinates".to_owned());
        }
        let noise_min = self.generator.min_y();
        let min_y = requested_min_y.max(noise_min);
        let max_y = requested_max_y.min(noise_min + self.generator.gen_depth());
        if min_y >= max_y {
            return Err("volume range is empty or outside this dimension".to_owned());
        }
        self.pool.install(|| {
            let chunk = self.empty_chunk(ChunkPos::new(chunk_x, chunk_z));
            let generation = GenerationChunk::<TerrainPhase>::for_standalone(&chunk);
            self.generator.fill_from_noise(generation, None);
            generation.clear_post_noise_state();
            let cells_xz = 16_u32.div_ceil(lod);
            let cells_y = (max_y - min_y) as u32;
            let cells_y = cells_y.div_ceil(lod);
            let mut voxels = vec![0; (cells_xz * cells_xz * cells_y) as usize];
            let water = vanilla_blocks::WATER.default_state();
            let lava = vanilla_blocks::LAVA.default_state();
            let mut column = Vec::new();
            for z in 0..cells_xz {
                for x in 0..cells_xz {
                    chunk.sections.read_column_into(
                        (x * lod) as usize,
                        (z * lod) as usize,
                        &mut column,
                    );
                    for y in 0..cells_y {
                        let state = column[(min_y - self.min_y) as usize + (y * lod) as usize];
                        voxels[(y * cells_xz * cells_xz + z * cells_xz + x) as usize] =
                            if state.is_air() {
                                0
                            } else if state == water {
                                2
                            } else if state == lava {
                                3
                            } else {
                                1
                            };
                    }
                }
            }
            Ok(NoiseVolume {
                min_y,
                cells_xz,
                cells_y,
                voxels,
            })
        })
    }

    /// Generates one tile.
    ///
    /// Samples lie at `origin + i * resolution` for `i` in `0..=size / resolution`.
    /// Full tiles report changed blocks with `origin <= x, z < origin + size`.
    ///
    /// # Panics
    /// Panics if `resolution` is zero or does not divide `size`.
    pub fn tile(
        &mut self,
        origin_x: i32,
        origin_z: i32,
        size: u32,
        resolution: u32,
        detail: TileDetail,
        filter: GeneratedBlockFilter,
    ) -> StandaloneTile {
        assert!(
            size > 0 && resolution > 0 && size.is_multiple_of(resolution),
            "tile size must be a positive multiple of its resolution"
        );
        let pool = Arc::clone(&self.pool);
        let tile = pool.install(|| match detail {
            TileDetail::Biome if self.dimension != StandaloneDimension::Nether => {
                self.estimated_biome_tile(origin_x, origin_z, size, resolution)
            }
            TileDetail::Biome => {
                let mut tile =
                    self.surface_tile(origin_x, origin_z, size, resolution, TileDetail::Coarse);
                tile.surface_states.clear();
                tile
            }
            TileDetail::Coarse | TileDetail::Full => {
                let mut tile = self.surface_tile(origin_x, origin_z, size, resolution, detail);
                if detail == TileDetail::Full {
                    tile.generated_blocks = self.generated_blocks(origin_x, origin_z, size, filter);
                }
                tile
            }
        });
        self.trim_caches();
        tile
    }

    /// Generates vanilla structure starts near a block position.
    ///
    /// Returned positions are vanilla locate positions within `radius_blocks`
    /// horizontally. Results are sorted by structure, then X, then Z.
    pub fn structure_markers(
        &mut self,
        center_x: i32,
        center_z: i32,
        radius_blocks: i32,
    ) -> Vec<StructureMarker> {
        let pool = Arc::clone(&self.pool);
        let markers =
            pool.install(|| self.structure_markers_inner(center_x, center_z, radius_blocks));
        self.trim_caches();
        markers
    }

    fn structure_markers_inner(
        &mut self,
        center_x: i32,
        center_z: i32,
        radius_blocks: i32,
    ) -> Vec<StructureMarker> {
        let origin = BlockPos::new(center_x, 0, center_z);
        let Some(structure_generator) = self.generator.structure_generator() else {
            return Vec::new();
        };
        let all_structures = REGISTRY
            .structures
            .iter()
            .map(|(_, structure)| structure.key.clone())
            .collect::<Vec<_>>();
        let Some(plan) = structure_generator.locate_plan_for_structures(&all_structures) else {
            return Vec::new();
        };
        let radius_sqr = i64::from(radius_blocks) * i64::from(radius_blocks);
        let mut candidates = plan.ring_candidates(origin);
        candidates.extend(plan.random_spread_candidates_in_block_radius(origin, radius_blocks));
        candidates.retain(|candidate| {
            horizontal_distance_sqr(candidate.locate_pos, origin) <= radius_sqr
        });
        candidates.sort_by_key(|candidate| {
            (
                candidate.chunk_pos.0.x,
                candidate.chunk_pos.0.y,
                candidate.scan_id(),
            )
        });
        candidates.dedup();

        let mut seen = rustc_hash::FxHashSet::default();
        let mut markers = Vec::new();
        for candidate in candidates {
            let Some(requested) = plan.structures_for_candidate(candidate) else {
                continue;
            };
            let key = (candidate.chunk_pos.0.x, candidate.chunk_pos.0.y);
            let starts = self.starts_for_marker(key);
            for start in starts.values() {
                if !requested.contains(&start.structure) {
                    continue;
                }
                if seen.insert((
                    start.structure.clone(),
                    candidate.locate_pos.0.x,
                    candidate.locate_pos.0.z,
                )) {
                    markers.push(StructureMarker {
                        locate_pos: candidate.locate_pos,
                        start: start.clone(),
                    });
                }
            }
        }
        markers.sort_by(|left, right| {
            left.start
                .structure
                .cmp(&right.start.structure)
                .then(left.locate_pos.0.x.cmp(&right.locate_pos.0.x))
                .then(left.locate_pos.0.z.cmp(&right.locate_pos.0.z))
        });
        markers
    }

    fn surface_tile(
        &mut self,
        origin_x: i32,
        origin_z: i32,
        size: u32,
        resolution: u32,
        detail: TileDetail,
    ) -> StandaloneTile {
        let samples_per_side = size / resolution + 1;
        let capacity = (samples_per_side * samples_per_side) as usize;
        let mut tile = StandaloneTile::with_capacity(samples_per_side, capacity, self.min_y);
        let carve = detail == TileDetail::Full;
        for sample_z in 0..samples_per_side {
            for sample_x in 0..samples_per_side {
                let x = origin_x.saturating_add((sample_x * resolution) as i32);
                let z = origin_z.saturating_add((sample_z * resolution) as i32);
                let key = (x.div_euclid(16), z.div_euclid(16));
                self.ensure_terrain(key, carve);
                let Some(entry) = self.terrain.get(&key) else {
                    unreachable!("terrain entry was just ensured");
                };
                let column = entry.surface[column_index(x, z)];
                tile.push_sample(
                    column.height,
                    column.exists,
                    self.generator
                        .noise_biome(x >> 2, i32::from(column.height) >> 2, z >> 2),
                    Some(column.state),
                );
            }
        }
        tile
    }

    fn estimated_biome_tile(
        &self,
        origin_x: i32,
        origin_z: i32,
        size: u32,
        resolution: u32,
    ) -> StandaloneTile {
        let samples_per_side = size / resolution + 1;
        let capacity = (samples_per_side * samples_per_side) as usize;
        let mut tile = StandaloneTile::with_capacity(samples_per_side, capacity, self.min_y);
        tile.heights_estimated = true;
        let mut estimator = HeightEstimator::new(&self.generator);
        for sample_z in 0..samples_per_side {
            for sample_x in 0..samples_per_side {
                let x = origin_x.saturating_add((sample_x * resolution) as i32);
                let z = origin_z.saturating_add((sample_z * resolution) as i32);
                let height = estimator.height(x, z);
                let exists = height.is_some();
                let height = height.unwrap_or(self.min_y as i16);
                tile.push_sample(
                    height,
                    exists,
                    self.generator
                        .noise_biome(x >> 2, i32::from(height) >> 2, z >> 2),
                    None,
                );
            }
        }
        tile
    }

    fn generated_blocks(
        &mut self,
        origin_x: i32,
        origin_z: i32,
        size: u32,
        filter: GeneratedBlockFilter,
    ) -> Vec<GeneratedBlock> {
        let max_x = origin_x.saturating_add(size as i32 - 1);
        let max_z = origin_z.saturating_add(size as i32 - 1);
        let tile_min = (origin_x.div_euclid(16), origin_z.div_euclid(16));
        let tile_max = (max_x.div_euclid(16), max_z.div_euclid(16));
        let holders = self.decorate(tile_min, tile_max);
        let mut blocks = Vec::new();
        for chunk_x in tile_min.0..=tile_max.0 {
            for chunk_z in tile_min.1..=tile_max.1 {
                let Some(holder) = holders.get(&(chunk_x, chunk_z)) else {
                    unreachable!("tile chunk holder was just created");
                };
                let Some(decorated) = holder.try_chunk(ChunkStatus::Terrain) else {
                    unreachable!("tile chunk holder was published at Terrain");
                };
                let Some(entry) = self.terrain.get(&(chunk_x, chunk_z)) else {
                    unreachable!("tile terrain entry was just ensured");
                };
                let Some(carved) = entry.carved.as_deref() else {
                    unreachable!("tile terrain entry was just carved");
                };
                self.append_changed_blocks(
                    &mut blocks,
                    carved,
                    decorated,
                    &entry.surface,
                    (origin_x, origin_z, max_x, max_z),
                    filter,
                );
            }
        }
        blocks.sort_unstable_by_key(|block| (block.x, block.y, block.z));
        blocks
    }

    /// Decorates every chunk within `source_radius` of `center`, in X-then-Z
    /// order, then returns every chunk within `source_radius + 1`.
    ///
    /// A native forceload of one chunk decorates the same source set. Native
    /// comparison tools read these. Viewer tiles read only changed blocks
    /// through [`Self::tile`].
    ///
    /// # Panics
    /// Panics if `source_radius` is below one.
    pub fn decorated_chunk_snapshots(
        &mut self,
        center: (i32, i32),
        source_radius: i32,
    ) -> Vec<ChunkSnapshot> {
        assert!(source_radius >= 1, "source radius must be at least one");
        let inner = source_radius - FEATURE_WRITE_RADIUS;
        let tile_min = (center.0 - inner, center.1 - inner);
        let tile_max = (center.0 + inner, center.1 + inner);
        let ring = source_radius + 1;
        let pool = Arc::clone(&self.pool);
        let snapshots = pool.install(|| {
            let holders = self.decorate(tile_min, tile_max);
            let mut snapshots = Vec::new();
            for chunk_x in center.0 - ring..=center.0 + ring {
                for chunk_z in center.1 - ring..=center.1 + ring {
                    let Some(chunk) = holders
                        .get(&(chunk_x, chunk_z))
                        .and_then(|holder| holder.try_chunk(ChunkStatus::Terrain))
                    else {
                        unreachable!("snapshot chunk holder was just created");
                    };
                    snapshots.push(ChunkSnapshot::of(chunk, self.min_y));
                }
            }
            snapshots
        });
        self.trim_caches();
        snapshots
    }

    /// One chunk after Terrain, including its carvers and fluid marks.
    pub fn terrain_chunk_snapshot(&mut self, chunk_x: i32, chunk_z: i32) -> ChunkSnapshot {
        let pool = Arc::clone(&self.pool);
        let snapshot = pool.install(|| {
            self.ensure_terrain((chunk_x, chunk_z), true);
            ChunkSnapshot::of(&self.carved_chunk((chunk_x, chunk_z)), self.min_y)
        });
        self.trim_caches();
        snapshot
    }

    /// Decorates every chunk that can write into `tile_min..=tile_max`.
    ///
    /// Returns holders whose Terrain-status chunks are decorated copies. The
    /// retained Terrain chunks stay undecorated.
    fn decorate(
        &mut self,
        tile_min: (i32, i32),
        tile_max: (i32, i32),
    ) -> Arc<FxHashMap<(i32, i32), Arc<ChunkHolder>>> {
        // Features can write one chunk away, and each decorating chunk reads
        // Terrain one chunk further out.
        let source_min = (
            tile_min.0 - FEATURE_WRITE_RADIUS,
            tile_min.1 - FEATURE_WRITE_RADIUS,
        );
        let source_max = (
            tile_max.0 + FEATURE_WRITE_RADIUS,
            tile_max.1 + FEATURE_WRITE_RADIUS,
        );
        let terrain_min = (source_min.0 - 1, source_min.1 - 1);
        let terrain_max = (source_max.0 + 1, source_max.1 + 1);
        for chunk_x in terrain_min.0..=terrain_max.0 {
            for chunk_z in terrain_min.1..=terrain_max.1 {
                self.ensure_terrain((chunk_x, chunk_z), true);
            }
        }

        let mut holders = FxHashMap::with_capacity_and_hasher(
            ((source_max.0 - source_min.0 + 1 + 2 * STRUCTURE_RADIUS)
                * (source_max.1 - source_min.1 + 1 + 2 * STRUCTURE_RADIUS)) as usize,
            FxBuildHasher,
        );
        for chunk_x in source_min.0 - STRUCTURE_RADIUS..=source_max.0 + STRUCTURE_RADIUS {
            for chunk_z in source_min.1 - STRUCTURE_RADIUS..=source_max.1 + STRUCTURE_RADIUS {
                let pos = ChunkPos::new(chunk_x, chunk_z);
                let holder = Arc::new(ChunkHolder::new(
                    pos,
                    ChunkTicketLevel::STRONGEST,
                    None,
                    self.min_y,
                    self.height,
                ));
                let in_terrain = (terrain_min.0..=terrain_max.0).contains(&chunk_x)
                    && (terrain_min.1..=terrain_max.1).contains(&chunk_z);
                if in_terrain {
                    let carved = self.carved_chunk((chunk_x, chunk_z));
                    holder.insert_chunk(carved.clone_generation_state(), ChunkStatus::Terrain);
                } else {
                    let chunk = self.empty_chunk(pos);
                    if let Some(entry) = self.starts.get(&(chunk_x, chunk_z)) {
                        chunk.structure_starts_mut().clone_from(&entry.starts);
                    }
                    holder.insert_chunk(chunk, ChunkStatus::StructureStarts);
                }
                holders.insert((chunk_x, chunk_z), holder);
            }
        }
        let holders = Arc::new(holders);

        let feature_step = GENERATION_PYRAMID.get_step_to(ChunkStatus::Features);
        let cache_radius = feature_step.direct_dependencies.get_radius() as i32;
        for chunk_x in source_min.0..=source_max.0 {
            for chunk_z in source_min.1..=source_max.1 {
                let Some(center_holder) = holders.get(&(chunk_x, chunk_z)) else {
                    unreachable!("feature source holder was just created");
                };
                let Some(center_chunk) = center_holder.try_chunk(ChunkStatus::Terrain) else {
                    unreachable!("feature source holder was published at Terrain");
                };
                center_chunk.prime_final_heightmaps();
                let cache_holders = Arc::clone(&holders);
                let cache = Arc::new(StaticCache2D::create(
                    chunk_x,
                    chunk_z,
                    cache_radius,
                    move |x, z| match cache_holders.get(&(x, z)) {
                        Some(holder) => Arc::clone(holder),
                        None => panic!("missing feature dependency chunk ({x}, {z})"),
                    },
                ));
                let center = ChunkPos::new(chunk_x, chunk_z);
                let region_random = self
                    .generator
                    .create_worldgen_region_random(self.seed, center);
                let mut region =
                    WorldGenRegion::new(&self.context, feature_step, &cache, center, region_random);
                self.generator.apply_biome_decorations(&mut region);
                self.stats.feature_regions += 1;
            }
        }

        holders
    }

    fn append_changed_blocks(
        &self,
        blocks: &mut Vec<GeneratedBlock>,
        carved: &Chunk,
        decorated: &Chunk,
        surface: &[SurfaceColumn],
        (min_x, min_z, max_x, max_z): (i32, i32, i32, i32),
        filter: GeneratedBlockFilter,
    ) {
        let chunk_min_x = carved.pos.0.x * 16;
        let chunk_min_z = carved.pos.0.y * 16;
        let carved_sections = &carved.sections.sections;
        let decorated_sections = &decorated.sections.sections;
        for (section_index, (before, after)) in carved_sections
            .iter()
            .zip(decorated_sections.iter())
            .enumerate()
        {
            let before = before.read();
            let after = after.read();
            let section_min_y = self.min_y + (section_index as i32) * 16;
            for local_y in 0..16 {
                let y = section_min_y + local_y as i32;
                for local_z in 0..16 {
                    let z = chunk_min_z + local_z as i32;
                    if z < min_z || z > max_z {
                        continue;
                    }
                    for local_x in 0..16 {
                        let x = chunk_min_x + local_x as i32;
                        if x < min_x || x > max_x {
                            continue;
                        }
                        let state = after.states.get(local_x, local_y, local_z);
                        if state == before.states.get(local_x, local_y, local_z) {
                            continue;
                        }
                        if filter == GeneratedBlockFilter::AtOrAboveSurface {
                            let column = surface[local_z * 16 + local_x];
                            if column.exists && y < i32::from(column.height) {
                                continue;
                            }
                        }
                        blocks.push(GeneratedBlock { x, y, z, state });
                    }
                }
            }
        }
    }

    fn carved_chunk(&self, key: (i32, i32)) -> Arc<Chunk> {
        match self
            .terrain
            .get(&key)
            .and_then(|entry| entry.carved.as_ref())
        {
            Some(chunk) => Arc::clone(chunk),
            None => unreachable!(
                "carved terrain chunk ({}, {}) was just ensured",
                key.0, key.1
            ),
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn empty_chunk(&self, pos: ChunkPos) -> Chunk {
        let sections = (0..self.height / 16)
            .map(|_| ChunkSection::new_empty())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Chunk::new(
            Sections::from_owned(sections),
            pos,
            self.min_y,
            self.height,
            Weak::new(),
        )
    }

    fn ensure_starts(&mut self, key: (i32, i32)) {
        let now = self.tick();
        if let Some(entry) = self.starts.get_mut(&key) {
            entry.last_used = now;
            return;
        }
        let chunk = self.empty_chunk(ChunkPos::new(key.0, key.1));
        self.generator.create_structures(&chunk);
        let starts = std::mem::take(&mut *chunk.structure_starts_mut());
        self.stats.starts_generated += 1;
        self.starts.insert(
            key,
            StartsEntry {
                starts,
                biomes: None,
                last_used: now,
            },
        );
    }

    /// Returns starts for a marker candidate without retaining empty chunks.
    ///
    /// Marker scans touch many more chunks than terrain does. Retaining only
    /// chunks that hold a start keeps them from evicting terrain inputs.
    fn starts_for_marker(&mut self, key: (i32, i32)) -> StructureStartMap {
        let now = self.tick();
        if let Some(entry) = self.starts.get_mut(&key) {
            entry.last_used = now;
            return entry.starts.clone();
        }
        let chunk = self.empty_chunk(ChunkPos::new(key.0, key.1));
        self.generator.create_structures(&chunk);
        let starts = std::mem::take(&mut *chunk.structure_starts_mut());
        self.stats.starts_generated += 1;
        if !starts.is_empty() {
            self.starts.insert(
                key,
                StartsEntry {
                    starts: starts.clone(),
                    biomes: None,
                    last_used: now,
                },
            );
        }
        starts
    }

    fn ensure_biomes(&mut self, key: (i32, i32)) {
        self.ensure_starts(key);
        if self
            .starts
            .get(&key)
            .is_some_and(|entry| entry.biomes.is_some())
        {
            return;
        }
        let chunk = self.empty_chunk(ChunkPos::new(key.0, key.1));
        self.generator.create_biomes(&chunk);
        let biomes = chunk
            .sections
            .sections
            .iter()
            .map(|section| section.read().biomes.clone())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.stats.biomes_generated += 1;
        if let Some(entry) = self.starts.get_mut(&key) {
            entry.biomes = Some(biomes);
        }
    }

    fn ensure_terrain(&mut self, key: (i32, i32), carve: bool) {
        let now = self.tick();
        if let Some(entry) = self.terrain.get_mut(&key) {
            entry.last_used = now;
            if !carve || entry.carved.is_some() {
                return;
            }
        }
        for dx in -STRUCTURE_RADIUS..=STRUCTURE_RADIUS {
            for dz in -STRUCTURE_RADIUS..=STRUCTURE_RADIUS {
                self.ensure_starts((key.0 + dx, key.1 + dz));
            }
        }
        for dx in -1..=1 {
            for dz in -1..=1 {
                self.ensure_biomes((key.0 + dx, key.1 + dz));
            }
        }
        let (surface, carved) = self.build_terrain(key, carve);
        self.stats.terrain_generated += 1;
        self.terrain.insert(
            key,
            TerrainEntry {
                surface,
                carved: carved.map(Arc::new),
                last_used: now,
            },
        );
    }

    /// Runs Terrain for one chunk whose starts and neighbour biomes exist.
    fn build_terrain(&self, key: (i32, i32), carve: bool) -> (Box<[SurfaceColumn]>, Option<Chunk>) {
        let pos = ChunkPos::new(key.0, key.1);
        let chunk = self.empty_chunk(pos);
        let Some(own) = self.starts.get(&key) else {
            unreachable!("terrain chunk starts were just ensured");
        };
        chunk.structure_starts_mut().clone_from(&own.starts);
        if let Some(biomes) = &own.biomes {
            for (section, palette) in chunk.sections.sections.iter().zip(biomes.iter()) {
                section.write().biomes = palette.clone();
            }
        }

        let mut references = StructureReferenceMap::default();
        for source_x in (key.0 - STRUCTURE_RADIUS)..=(key.0 + STRUCTURE_RADIUS) {
            for source_z in (key.1 - STRUCTURE_RADIUS)..=(key.1 + STRUCTURE_RADIUS) {
                if let Some(source) = self.starts.get(&(source_x, source_z)) {
                    collect_source_references(
                        &mut references,
                        pos,
                        ChunkPos::new(source_x, source_z),
                        &source.starts,
                    );
                }
            }
        }
        let reference_list = {
            let mut target_references = chunk.structure_references_mut();
            for (structure_id, source_chunks) in references {
                target_references
                    .entry(structure_id)
                    .or_default()
                    .extend(source_chunks);
            }
            target_references
                .iter()
                .map(|(structure_id, source_chunks)| {
                    (
                        structure_id.clone(),
                        source_chunks.iter().copied().collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<(Identifier, Vec<ChunkPos>)>>()
        };
        let beardifier = beardifier_from_starts(
            &reference_list,
            |source| {
                self.starts
                    .get(&(source.0.x, source.0.y))
                    .map(|entry| &entry.starts)
            },
            key.0,
            key.1,
        );

        let min_quart_y = self.min_y >> 2;
        let quart_height = (self.height / 16) * 4;
        let neighbor_biomes = |quart_pos: IVec3| -> u16 {
            let neighbor_x = quart_pos.x >> 2;
            let neighbor_z = quart_pos.z >> 2;
            let Some(biomes) = self
                .starts
                .get(&(neighbor_x, neighbor_z))
                .and_then(|entry| entry.biomes.as_ref())
            else {
                panic!("neighbor chunk ({neighbor_x}, {neighbor_z}) has no biomes");
            };
            let local_x = (quart_pos.x - neighbor_x * 4) as usize;
            let local_z = (quart_pos.z - neighbor_z * 4) as usize;
            let local_y = (quart_pos.y - min_quart_y).clamp(0, quart_height - 1) as usize;
            biomes[local_y / 4].get(local_x, local_y % 4, local_z)
        };

        let generation = GenerationChunk::<TerrainPhase>::for_standalone(&chunk);
        self.generator
            .fill_from_noise(generation, beardifier.as_ref());
        self.generator.build_surface(generation, &neighbor_biomes);
        let surface = self.surface_columns(&chunk);
        if carve {
            self.generator.apply_carvers(generation);
        }
        generation.clear_post_noise_state();
        (surface, carve.then_some(chunk))
    }

    fn surface_columns(&self, chunk: &Chunk) -> Box<[SurfaceColumn]> {
        let mut column = Vec::new();
        let below_ceiling = self.dimension.surface_below_ceiling();
        let air = vanilla_blocks::AIR.default_state();
        (0..256)
            .map(|index| {
                chunk
                    .sections
                    .read_column_into(index % 16, index / 16, &mut column);
                let mut found_opening = !below_ceiling;
                for (relative_y, state) in column.iter().enumerate().rev() {
                    let solid = !state.is_air() && !state.get_block().config.liquid;
                    if !solid {
                        found_opening = true;
                    } else if found_opening {
                        return SurfaceColumn {
                            height: (self.min_y + relative_y as i32) as i16,
                            state: *state,
                            exists: true,
                        };
                    }
                }
                SurfaceColumn {
                    height: self.min_y as i16,
                    state: air,
                    exists: false,
                }
            })
            .collect()
    }

    fn trim_caches(&mut self) {
        trim_lru(&mut self.terrain, self.terrain_capacity, |entry| {
            entry.last_used
        });
        trim_lru(
            &mut self.starts,
            self.terrain_capacity
                .saturating_mul(STARTS_PER_TERRAIN_ENTRY)
                .max(1024),
            |entry| entry.last_used,
        );
    }
}

impl StandaloneTile {
    fn with_capacity(samples_per_side: u32, capacity: usize, min_y: i32) -> Self {
        Self {
            samples_per_side,
            heights: Vec::with_capacity(capacity),
            present: Vec::with_capacity(capacity),
            biomes: Vec::with_capacity(capacity),
            surface_states: Vec::with_capacity(capacity),
            generated_blocks: Vec::new(),
            heights_estimated: false,
            min_y: min_y as i16,
        }
    }

    fn push_sample(
        &mut self,
        height: i16,
        exists: bool,
        biome: BiomeRef,
        surface: Option<BlockStateId>,
    ) {
        self.heights.push(height);
        self.present.push(exists);
        self.biomes.push(biome);
        if let Some(surface) = surface {
            self.surface_states.push(surface);
        }
    }
}

/// Cheap height estimates for biome overview tiles.
enum HeightEstimator<'a> {
    Overworld(
        &'a OverworldGenerator,
        <OverworldNoises as DimensionNoises>::ColumnCache,
    ),
    End(
        &'a EndGenerator,
        <EndNoises as DimensionNoises>::ColumnCache,
    ),
    None,
}

impl<'a> HeightEstimator<'a> {
    fn new(generator: &'a ChunkGeneratorType) -> Self {
        match generator {
            ChunkGeneratorType::Overworld(generator) => {
                Self::Overworld(generator, Default::default())
            }
            ChunkGeneratorType::End(generator) => Self::End(generator, Default::default()),
            _ => Self::None,
        }
    }

    fn height(&mut self, x: i32, z: i32) -> Option<i16> {
        match self {
            Self::Overworld(generator, cache) => Some(
                preliminary_surface_level(generator.noises(), cache, x, z)
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
            ),
            Self::End(generator, cache) => {
                let sample_x = (x >> 2) << 2;
                let sample_z = (z >> 2) << 2;
                cache.ensure(sample_x, sample_z, generator.noises());
                approximate_end_island_height(f64::from(
                    generator
                        .noises()
                        .router_erosion(cache, sample_x, 0, sample_z),
                ))
            }
            Self::None => None,
        }
    }
}

/// Highest Y where the End's island density, without its 3D noise, is solid.
fn approximate_end_island_height(island_density: f64) -> Option<i16> {
    // A small positive allowance represents the zero-centered blended-noise
    // contribution without evaluating that three-dimensional noise column.
    let approximate_sloped_cheese = island_density + 0.031_25;
    (0_i16..128).rev().find(|height| {
        let y = f64::from(*height);
        let lower_slide = ((y - 4.0) / (32.0 - 4.0)).clamp(0.0, 1.0);
        let upper_slide = ((y - 56.0) / (312.0 - 56.0)).clamp(0.0, 1.0);
        let upper_value = 1.0 - upper_slide;
        let density = 0.64
            * (-0.234_375
                + lower_slide
                    * (0.234_375
                        + (-23.437_5 + upper_value * (23.437_5 + approximate_sloped_cheese))));
        density > 0.0
    })
}

const fn column_index(x: i32, z: i32) -> usize {
    (z.rem_euclid(16) * 16 + x.rem_euclid(16)) as usize
}

fn horizontal_distance_sqr(a: BlockPos, b: BlockPos) -> i64 {
    let dx = i64::from(a.0.x) - i64::from(b.0.x);
    let dz = i64::from(a.0.z) - i64::from(b.0.z);
    dx * dx + dz * dz
}

fn trim_lru<V>(map: &mut FxHashMap<(i32, i32), V>, capacity: usize, last_used: impl Fn(&V) -> u64) {
    if map.len() <= capacity {
        return;
    }
    let mut ages = map.values().map(&last_used).collect::<Vec<_>>();
    let excess = map.len() - capacity;
    let (_, cutoff, _) = ages.select_nth_unstable(excess - 1);
    let cutoff = *cutoff;
    map.retain(|_, value| last_used(value) > cutoff);
}
