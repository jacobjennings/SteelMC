//! Byte-oriented WebAssembly adapter for Steel world generation.

use std::cell::RefCell;
use std::collections::HashMap;

use serde::Serialize;
use steel_registry::REGISTRY;
use steel_utils::random::{
    Random, RandomSplitter, legacy_random::LegacyRandom, xoroshiro::Xoroshiro,
};
use steel_utils::{BlockPos, BlockStateId, ChunkPos};
use steel_worldgen::{
    biomes::BiomeSourceKind,
    density::{DimensionNoises, NoiseSettings},
    density_functions::{end::EndNoises, nether::NetherNoises, overworld::OverworldNoises},
    noise::LazyAquifer,
    noise_parameters::get_noise_parameters,
    structure::{GenerationContext, StructureGenerator, StructureStart},
    surface_sampler::{
        DEFAULT_SURFACE_CHUNK_CACHE_CAPACITY, NoiseVolume, SurfaceChunkCache, SurfaceDimension,
        SurfaceSampler, SurfaceTile, canonical_block_state_key,
    },
    surface_signal::{DEFAULT_SURFACE_SIGNAL_LOOKAHEAD, SurfaceSignalStats, SurfaceSignalWindow},
};
use wasm_bindgen::prelude::*;

const MAX_STRUCTURE_MARKER_RADIUS: i32 = 4096;

fn validate_terrain_grid(size: u32, resolution: u32) -> Result<(), JsValue> {
    if size == 0
        || size > 256
        || resolution == 0
        || resolution > size
        || !size.is_multiple_of(resolution)
    {
        return Err(JsValue::from_str(
            "size must be <= 256 and evenly divisible by resolution",
        ));
    }
    Ok(())
}

/// Reusable single-seed generator intended to live inside a Web Worker.
#[wasm_bindgen]
pub struct SteelWorldgen {
    sampler: SurfaceSampler,
    surface_chunk_cache: RefCell<SurfaceChunkCache>,
    markers: StructureMarkerSampler,
    seed: String,
    dimension: &'static str,
}

#[wasm_bindgen]
impl SteelWorldgen {
    /// Creates a generator for one seed and dimension.
    ///
    /// # Errors
    /// Returns an error for an invalid signed 64-bit seed or dimension.
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &str, dimension: &str, cache_capacity: Option<u32>) -> Result<Self, JsValue> {
        let signed_seed = seed
            .parse::<i64>()
            .map_err(|_| JsValue::from_str("seed must be a signed 64-bit integer"))?;
        let (dimension, name) = match dimension {
            "overworld" => (SurfaceDimension::Overworld, "overworld"),
            "nether" => (SurfaceDimension::Nether, "nether"),
            "end" => (SurfaceDimension::End, "end"),
            _ => return Err(JsValue::from_str("unknown dimension")),
        };
        let sampler = SurfaceSampler::new(signed_seed as u64, dimension);
        let markers = StructureMarkerSampler::new(signed_seed, dimension);
        let cache_capacity = cache_capacity
            .map(|capacity| {
                usize::try_from(capacity)
                    .map_err(|_| JsValue::from_str("surface chunk cache capacity is too large"))
            })
            .transpose()?
            .unwrap_or(DEFAULT_SURFACE_CHUNK_CACHE_CAPACITY);
        if cache_capacity == 0 {
            return Err(JsValue::from_str(
                "surface chunk cache capacity must be greater than zero",
            ));
        }
        Ok(Self {
            sampler,
            surface_chunk_cache: RefCell::new(SurfaceChunkCache::new(cache_capacity)),
            markers,
            seed: seed.to_owned(),
            dimension: name,
        })
    }

    /// Generates a JSON terrain tile compatible with the native terrain API.
    ///
    /// # Errors
    /// Returns an error when the requested grid is invalid or serialization fails.
    pub fn terrain_tile(
        &self,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        compact_surface_blocks: Option<bool>,
    ) -> Result<String, JsValue> {
        validate_terrain_grid(size, resolution)?;
        let tile = self.sampler.tile_with_cache(
            &mut self.surface_chunk_cache.borrow_mut(),
            x,
            z,
            size,
            resolution,
        );
        serde_json::to_string(&TerrainResponse::new(
            &self.seed,
            self.dimension,
            x,
            z,
            size,
            resolution,
            tile,
            None,
            compact_surface_blocks.unwrap_or(false),
        ))
        .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    /// Runs one chunk through the bounded surface signal or the generated path.
    ///
    /// Both arms return the same per-column shape, so a caller can time them
    /// against each other and compare their results on the same chunk. The
    /// generated path stays authoritative: nothing here changes what a tile
    /// contains, and no tile entry point calls this.
    ///
    /// `include_columns` controls whether the per-column arrays are
    /// serialized. A timing caller leaves it off and reads `digest`, which is
    /// computed from every column either way so the work cannot be skipped.
    ///
    /// # Errors
    /// Returns an error when serialization fails.
    pub fn surface_signal_columns(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        bounded: bool,
        include_columns: Option<bool>,
    ) -> Result<String, JsValue> {
        let (columns, stats) = if bounded {
            let (columns, stats) = self.sampler.surface_signal_chunk(
                chunk_x,
                chunk_z,
                DEFAULT_SURFACE_SIGNAL_LOOKAHEAD,
                SurfaceSignalWindow::Derived,
            );
            (columns, Some(stats))
        } else {
            (
                self.sampler.generated_surface_columns(chunk_x, chunk_z),
                None,
            )
        };
        serde_json::to_string(&SurfaceSignalColumnsResponse::new(
            &self.seed,
            self.dimension,
            chunk_x,
            chunk_z,
            bounded,
            &columns,
            stats,
            include_columns.unwrap_or(false),
        ))
        .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    /// Largest simultaneous flat vegetation payload generated by this worker.
    #[wasm_bindgen(getter)]
    pub fn peak_live_flat_chunk_bytes(&self) -> usize {
        self.surface_chunk_cache
            .borrow()
            .stats()
            .peak_live_flat_chunk_bytes
    }

    /// Generates a terrain tile without vegetation generation or carving.
    ///
    /// # Errors
    /// Returns an error when the requested grid is invalid or serialization fails.
    pub fn terrain_tile_coarse(
        &self,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        compact_surface_blocks: Option<bool>,
    ) -> Result<String, JsValue> {
        validate_terrain_grid(size, resolution)?;
        let tile = self.sampler.coarse_tile_with_cache(
            &mut self.surface_chunk_cache.borrow_mut(),
            x,
            z,
            size,
            resolution,
        );
        serde_json::to_string(&TerrainResponse::new(
            &self.seed,
            self.dimension,
            x,
            z,
            size,
            resolution,
            tile,
            None,
            compact_surface_blocks.unwrap_or(false),
        ))
        .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    /// Generates a biome-only overview tile with approximate heights.
    ///
    /// Heights are preliminary density-router estimates. Surface blocks and
    /// vegetation are empty because no terrain stages are generated.
    ///
    /// # Errors
    /// Returns an error when the requested grid is invalid or serialization fails.
    pub fn terrain_tile_biome(
        &self,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        compact_surface_blocks: Option<bool>,
    ) -> Result<String, JsValue> {
        validate_terrain_grid(size, resolution)?;
        let tile = self.sampler.biome_tile(x, z, size, resolution);
        serde_json::to_string(&TerrainResponse::new(
            &self.seed,
            self.dimension,
            x,
            z,
            size,
            resolution,
            tile,
            Some("preliminary_surface_level"),
            compact_surface_blocks.unwrap_or(false),
        ))
        .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    /// Generates base-noise occupancy for one chunk footprint.
    ///
    /// `max_y` is exclusive, so it is directly suitable for a "max layer"
    /// control. The result deliberately contains no surface-rule, ore, carver,
    /// feature or structure blocks; see `material_keys` for its compact
    /// classification palette.
    ///
    /// # Errors
    /// Returns an error for an invalid vertical range or unsupported LOD.
    pub fn noise_volume_chunk(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        max_y: i32,
        lod: u32,
    ) -> Result<String, JsValue> {
        if min_y >= max_y {
            return Err(JsValue::from_str("min_y must be below max_y"));
        }
        if !matches!(lod, 1 | 4 | 16 | 64 | 256) {
            return Err(JsValue::from_str("LOD must be one of 1, 4, 16, 64, 256"));
        }
        serde_json::to_string(&VolumeResponse::new(
            &self.seed,
            self.dimension,
            chunk_x,
            chunk_z,
            self.sampler
                .noise_volume_chunk(chunk_x, chunk_z, min_y, max_y, lod),
        ))
        .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    /// Generates exact vanilla structure starts near a map centre.
    ///
    /// The radius is deliberately bounded because this completes the same
    /// biome, terrain, and piece-generation checks as a native structure-start
    /// chunk. Returned positions are vanilla locate positions; bounds are the
    /// generated `StructureStart` bounding boxes when present.
    ///
    /// # Errors
    /// Returns an error for a negative or too-large radius, or serialization
    /// failure.
    pub fn structure_markers(
        &self,
        center_x: i32,
        center_z: i32,
        radius_blocks: i32,
    ) -> Result<String, JsValue> {
        if !(0..=MAX_STRUCTURE_MARKER_RADIUS).contains(&radius_blocks) {
            return Err(JsValue::from_str(
                "structure marker radius must be between 0 and 4096 blocks",
            ));
        }
        serde_json::to_string(&StructureMarkerResponse {
            generator: "steelmc-wasm",
            version: "26.2",
            seed: &self.seed,
            dimension: self.dimension,
            center_x,
            center_z,
            radius_blocks,
            markers: self.markers.markers(center_x, center_z, radius_blocks),
        })
        .map_err(|error| JsValue::from_str(&error.to_string()))
    }
}

enum StructureMarkerSampler {
    Overworld(DimensionStructureMarkerSampler<OverworldNoises>),
    Nether(DimensionStructureMarkerSampler<NetherNoises>),
    End(DimensionStructureMarkerSampler<EndNoises>),
}

impl StructureMarkerSampler {
    fn new(seed: i64, dimension: SurfaceDimension) -> Self {
        let biome_source = match dimension {
            SurfaceDimension::Overworld => BiomeSourceKind::overworld(seed as u64),
            SurfaceDimension::Nether => BiomeSourceKind::nether(seed as u64),
            SurfaceDimension::End => BiomeSourceKind::end(seed as u64),
        };
        match dimension {
            SurfaceDimension::Overworld => {
                Self::Overworld(DimensionStructureMarkerSampler::new(seed, biome_source))
            }
            SurfaceDimension::Nether => {
                Self::Nether(DimensionStructureMarkerSampler::new(seed, biome_source))
            }
            SurfaceDimension::End => {
                Self::End(DimensionStructureMarkerSampler::new(seed, biome_source))
            }
        }
    }

    fn markers(&self, center_x: i32, center_z: i32, radius_blocks: i32) -> Vec<StructureMarker> {
        match self {
            Self::Overworld(generator) => generator.markers(center_x, center_z, radius_blocks),
            Self::Nether(generator) => generator.markers(center_x, center_z, radius_blocks),
            Self::End(generator) => generator.markers(center_x, center_z, radius_blocks),
        }
    }
}

struct DimensionStructureMarkerSampler<N: DimensionNoises> {
    seed: i64,
    biome_source: BiomeSourceKind,
    noises: Box<N>,
    splitter: RandomSplitter,
    structure_generator: StructureGenerator,
}

impl<N: DimensionNoises> DimensionStructureMarkerSampler<N> {
    fn new(seed: i64, biome_source: BiomeSourceKind) -> Self {
        let splitter = if N::Settings::LEGACY_RANDOM_SOURCE {
            LegacyRandom::from_seed(seed as u64).next_positional()
        } else {
            Xoroshiro::from_seed(seed as u64).next_positional()
        };
        let noises = Box::new(N::create(seed as u64, &splitter, &get_noise_parameters()));
        let structure_generator = StructureGenerator::vanilla_single_threaded(seed, &biome_source);
        Self {
            seed,
            biome_source,
            noises,
            splitter,
            structure_generator,
        }
    }

    fn markers(&self, center_x: i32, center_z: i32, radius_blocks: i32) -> Vec<StructureMarker> {
        let origin = BlockPos::new(center_x, 0, center_z);
        let all_structures = REGISTRY
            .structures
            .iter()
            .map(|(_, structure)| structure.key.clone())
            .collect::<Vec<_>>();
        let Some(plan) = self
            .structure_generator
            .locate_plan_for_structures(&all_structures)
        else {
            return Vec::new();
        };
        let radius_sqr = i64::from(radius_blocks) * i64::from(radius_blocks);
        let mut candidates = plan
            .ring_candidates(origin)
            .into_iter()
            .chain(plan.random_spread_candidates_in_block_radius(origin, radius_blocks))
            .filter(|candidate| horizontal_distance_sqr(candidate.locate_pos, origin) <= radius_sqr)
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| {
            (
                candidate.chunk_pos.0.x,
                candidate.chunk_pos.0.y,
                candidate.scan_id(),
            )
        });

        let mut starts_by_chunk = std::collections::HashMap::new();
        let mut seen = std::collections::HashSet::new();
        let mut markers = Vec::new();
        for candidate in candidates {
            let starts = starts_by_chunk
                .entry(candidate.chunk_pos)
                .or_insert_with(|| self.generate_starts(candidate.chunk_pos));
            let Some(requested) = plan.structures_for_candidate(candidate) else {
                continue;
            };
            for start in starts {
                if requested
                    .iter()
                    .all(|structure| *structure != start.structure)
                {
                    continue;
                }
                let key = (
                    start.structure.clone(),
                    candidate.locate_pos.0.x,
                    candidate.locate_pos.0.z,
                );
                if seen.insert(key) {
                    markers.push(StructureMarker::from_start(
                        start.clone(),
                        candidate.locate_pos,
                    ));
                }
            }
        }
        markers.sort_by(|left, right| {
            left.structure
                .cmp(&right.structure)
                .then(left.x.cmp(&right.x))
                .then(left.z.cmp(&right.z))
        });
        markers
    }

    fn generate_starts(&self, chunk_pos: ChunkPos) -> Vec<StructureStart> {
        let mut biome_sampler = self.biome_source.chunk_sampler();
        let mut height_cache = N::ColumnCache::default();
        let mut aquifer = LazyAquifer::new(
            chunk_pos.0.x * 16,
            chunk_pos.0.y * 16,
            &self.splitter,
            &*self.noises,
        );
        let mut surface_y_cache = None;
        let mut height_cache_grid_ready = false;
        let mut context = GenerationContext::new(
            self.seed,
            chunk_pos.0.x,
            chunk_pos.0.y,
            N::Settings::SEA_LEVEL,
            &*self.noises,
            &self.splitter,
            self.structure_generator.template_pools(),
            self.structure_generator.templates(),
            &mut biome_sampler,
            &mut height_cache,
            &mut aquifer,
            &mut surface_y_cache,
            &mut height_cache_grid_ready,
        );
        self.structure_generator
            .generate_starts_for_chunk(&mut context, |_| false)
    }
}

fn horizontal_distance_sqr(a: BlockPos, b: BlockPos) -> i64 {
    let dx = i64::from(a.0.x) - i64::from(b.0.x);
    let dz = i64::from(a.0.z) - i64::from(b.0.z);
    dx * dx + dz * dz
}

/// One chunk's per-column surface result from one of the two producers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SurfaceSignalColumnsResponse<'a> {
    seed: &'a str,
    dimension: &'a str,
    chunk_x: i32,
    chunk_z: i32,
    /// True for the bounded producer, false for the generated path.
    bounded: bool,
    /// Every column folded into one number, so a timing-only call still pays
    /// for the whole result and cannot have it optimized away.
    digest: i64,
    /// Columns that produced a block at all.
    present_columns: u32,
    /// Distinct canonical block state keys the chunk produced.
    distinct_states: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    heights: Option<Vec<i16>>,
    /// Index into `states`, parallel to `heights`. A column with no block
    /// carries the index of its state anyway, and `exists` says which.
    #[serde(skip_serializing_if = "Option::is_none")]
    state_indices: Option<Vec<u16>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    states: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exists: Option<Vec<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<SurfaceSignalStatsResponse>,
}

impl<'a> SurfaceSignalColumnsResponse<'a> {
    fn new(
        seed: &'a str,
        dimension: &'a str,
        chunk_x: i32,
        chunk_z: i32,
        bounded: bool,
        columns: &[(i16, BlockStateId, bool)],
        stats: Option<SurfaceSignalStats>,
        include_columns: bool,
    ) -> Self {
        let mut digest: i64 = 0;
        let mut present_columns = 0;
        let mut state_lookup: HashMap<BlockStateId, u16> = HashMap::new();
        let mut states: Vec<String> = Vec::new();
        let mut state_indices: Vec<u16> = Vec::with_capacity(columns.len());
        let mut heights: Vec<i16> = Vec::with_capacity(columns.len());
        let mut exists: Vec<bool> = Vec::with_capacity(columns.len());
        for (index, (height, state, column_exists)) in columns.iter().enumerate() {
            let index = index as i64;
            digest = digest
                .wrapping_add(i64::from(*height).wrapping_mul(index + 1))
                .wrapping_add(i64::from(state.0).wrapping_mul(index + 3))
                .wrapping_add(i64::from(*column_exists));
            if *column_exists {
                present_columns += 1;
            }
            let state_index = match state_lookup.get(state) {
                Some(state_index) => *state_index,
                None => {
                    let state_index = u16::try_from(states.len()).unwrap_or(u16::MAX);
                    state_lookup.insert(*state, state_index);
                    states.push(canonical_block_state_key(*state));
                    state_index
                }
            };
            state_indices.push(state_index);
            heights.push(*height);
            exists.push(*column_exists);
        }
        let distinct_states = states.len();
        Self {
            seed,
            dimension,
            chunk_x,
            chunk_z,
            bounded,
            digest,
            present_columns,
            distinct_states,
            heights: include_columns.then_some(heights),
            state_indices: include_columns.then_some(state_indices),
            states: include_columns.then_some(states),
            exists: include_columns.then_some(exists),
            stats: stats.map(SurfaceSignalStatsResponse::from),
        }
    }
}

/// What the bounded producer skipped, as the browser reads it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SurfaceSignalStatsResponse {
    density_evaluations: u64,
    full_density_evaluations: u64,
    windowed_block_slots: usize,
    full_block_slots: usize,
    window_min_y: i32,
    window_max_y: i32,
    unbounded_columns: u32,
}

impl From<SurfaceSignalStats> for SurfaceSignalStatsResponse {
    fn from(stats: SurfaceSignalStats) -> Self {
        Self {
            density_evaluations: stats.density_evaluations,
            full_density_evaluations: stats.full_density_evaluations,
            windowed_block_slots: stats.windowed_block_slots,
            full_block_slots: stats.full_block_slots,
            window_min_y: stats.window_min_y,
            window_max_y: stats.window_max_y,
            unbounded_columns: stats.unbounded_columns,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StructureMarkerResponse<'a> {
    generator: &'static str,
    version: &'static str,
    seed: &'a str,
    dimension: &'a str,
    center_x: i32,
    center_z: i32,
    radius_blocks: i32,
    markers: Vec<StructureMarker>,
}

#[derive(Serialize)]
struct StructureMarker {
    structure: String,
    x: i32,
    z: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bounds: Option<StructureBounds>,
}

impl StructureMarker {
    fn from_start(start: StructureStart, locate_pos: BlockPos) -> Self {
        let bounds = start.bounding_box.map(|bounds| StructureBounds {
            min_x: bounds.min_x(),
            min_y: bounds.min_y(),
            min_z: bounds.min_z(),
            max_x: bounds.max_x(),
            max_y: bounds.max_y(),
            max_z: bounds.max_z(),
        });
        Self {
            structure: start.structure.to_string(),
            x: locate_pos.0.x,
            z: locate_pos.0.z,
            y: start.placement_reference_pos().map(|position| position.0.y),
            bounds,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StructureBounds {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    max_x: i32,
    max_y: i32,
    max_z: i32,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// Rebuilds the flat placement list from the palette transport.
    fn generated_blocks(value: &serde_json::Value) -> Vec<(i32, i32, i32, String, String)> {
        let palette = value["generatedBlockPalette"]
            .as_array()
            .unwrap_or_else(|| panic!("response must include generatedBlockPalette"));
        let positions = value["generatedBlockPositions"]
            .as_array()
            .unwrap_or_else(|| panic!("response must include generatedBlockPositions"));
        let indices = value["generatedBlockIndices"]
            .as_array()
            .unwrap_or_else(|| panic!("response must include generatedBlockIndices"));
        assert_eq!(
            positions.len(),
            indices.len() * 3,
            "each placement must carry exactly one x, y, z triple"
        );
        indices
            .iter()
            .enumerate()
            .map(|(placement, index)| {
                let index = usize::try_from(
                    index
                        .as_u64()
                        .unwrap_or_else(|| panic!("palette index must be an integer")),
                )
                .unwrap_or_else(|error| panic!("palette index must fit a usize: {error}"));
                let entry = palette
                    .get(index)
                    .unwrap_or_else(|| panic!("palette index {index} is out of range"));
                let coordinate = |offset: usize| {
                    i32::try_from(
                        positions[placement * 3 + offset]
                            .as_i64()
                            .unwrap_or_else(|| panic!("position must be an integer")),
                    )
                    .unwrap_or_else(|error| panic!("position must fit an i32: {error}"))
                };
                (
                    coordinate(0),
                    coordinate(1),
                    coordinate(2),
                    entry["block"].as_str().unwrap_or_default().to_owned(),
                    entry["state"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn generated_marker_response_has_unique_complete_bounds() {
        let generator = SteelWorldgen::new("0", "overworld", None).unwrap_or_else(|error| {
            panic!("native structure generator should initialize: {error:?}")
        });
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .structure_markers(0, 0, 512)
                .unwrap_or_else(|error| panic!("marker generation should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("marker response should be JSON: {error}"));
        let markers = value["markers"]
            .as_array()
            .unwrap_or_else(|| panic!("marker response must contain an array"));
        let mut seen = HashSet::new();
        for marker in markers {
            let structure = marker["structure"]
                .as_str()
                .unwrap_or_else(|| panic!("marker must have a canonical structure id"));
            let x = marker["x"]
                .as_i64()
                .unwrap_or_else(|| panic!("marker must have an x coordinate"));
            let z = marker["z"]
                .as_i64()
                .unwrap_or_else(|| panic!("marker must have a z coordinate"));
            assert!(
                seen.insert((structure, x, z)),
                "markers must be deduplicated"
            );
            if let Some(bounds) = marker.get("bounds") {
                for key in ["minX", "minY", "minZ", "maxX", "maxY", "maxZ"] {
                    assert!(bounds[key].is_i64(), "bounds must include {key}");
                }
            }
        }
    }

    #[test]
    fn terrain_tile_serializes_final_surface_blocks_parallel_to_samples() {
        let generator = SteelWorldgen::new("0", "overworld", None)
            .unwrap_or_else(|error| panic!("surface generator should initialize: {error:?}"));
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile(0, 0, 16, 16, None)
                .unwrap_or_else(|error| panic!("surface tile should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("surface tile response must be JSON: {error}"));
        let blocks = value["surfaceBlocks"]
            .as_array()
            .unwrap_or_else(|| panic!("surface tile response must include surfaceBlocks"));
        let heights = value["heights"]
            .as_array()
            .unwrap_or_else(|| panic!("surface tile response must include heights"));
        let present = value["present"]
            .as_array()
            .unwrap_or_else(|| panic!("surface tile response must include present"));

        assert_eq!(blocks.len(), heights.len());
        assert_eq!(blocks.len(), present.len());
        assert!(
            blocks
                .iter()
                .all(|block| matches!(block.as_str(), Some(key) if key.starts_with("minecraft:")))
        );
    }

    #[test]
    fn compact_terrain_tile_serializes_surface_block_palette() {
        let generator = SteelWorldgen::new("0", "overworld", None)
            .unwrap_or_else(|error| panic!("surface generator should initialize: {error:?}"));
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile(0, 0, 16, 1, Some(true))
                .unwrap_or_else(|error| panic!("surface tile should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("surface tile response must be JSON: {error}"));
        let blocks = value["surfaceBlocks"]
            .as_array()
            .unwrap_or_else(|| panic!("surface tile response must include surfaceBlocks"));
        let indices = value["surfaceBlockIndices"]
            .as_array()
            .unwrap_or_else(|| panic!("surface tile response must include surfaceBlockIndices"));
        let heights = value["heights"]
            .as_array()
            .unwrap_or_else(|| panic!("surface tile response must include heights"));

        assert!(blocks.len() < heights.len());
        assert_eq!(indices.len(), heights.len());
        assert!(indices.iter().all(|index| {
            index
                .as_u64()
                .is_some_and(|index| index < blocks.len() as u64)
        }));
    }

    #[test]
    fn compact_biome_tile_serializes_empty_surface_palette() {
        let generator = SteelWorldgen::new("0", "overworld", None)
            .unwrap_or_else(|error| panic!("surface generator should initialize: {error:?}"));
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile_biome(0, 0, 16, 1, Some(true))
                .unwrap_or_else(|error| panic!("biome tile should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("biome tile response must be JSON: {error}"));

        assert_eq!(value["surfaceBlocks"], serde_json::json!([]));
        assert_eq!(value["surfaceBlockIndices"], serde_json::json!([]));
    }

    #[test]
    fn terrain_tile_serializes_canonical_cherry_generated_states() {
        let generator = SteelWorldgen::new("1", "overworld", None).unwrap_or_else(|error| {
            panic!("cherry fixture generator should initialize: {error:?}")
        });
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile(-108 * 16, -36 * 16, 16, 1, None)
                .unwrap_or_else(|error| panic!("cherry fixture should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("cherry fixture response must be JSON: {error}"));
        let generated = generated_blocks(&value);
        for expected in [
            "minecraft:cherry_log",
            "minecraft:cherry_leaves",
            "minecraft:pink_petals",
        ] {
            assert!(
                generated
                    .iter()
                    .any(|(_, _, _, block, _)| block == expected),
                "cherry fixture must place {expected}"
            );
        }
        assert!(
            generated
                .iter()
                .all(|(_, _, _, block, state)| state.starts_with(block.as_str()))
        );
    }

    /// The generated-block field must carry more than vegetation.
    ///
    /// This fixture is an ice spikes biome, where the vanilla `ice_spike` and
    /// `ice_patch` placed features build packed ice spires. Before the portable
    /// Features slice implemented them the payload here was empty, and the
    /// viewer drew flat snow where Minecraft draws spires.
    #[test]
    fn terrain_tile_serializes_packed_ice_spikes() {
        let generator = SteelWorldgen::new("12345", "overworld", None)
            .unwrap_or_else(|error| panic!("ice spike generator should initialize: {error:?}"));
        let mut packed_ice = 0usize;
        let mut min_y = i64::MAX;
        let mut max_y = i64::MIN;
        for chunk_z in -58..-54 {
            for chunk_x in 48..52 {
                let value: serde_json::Value = serde_json::from_str(
                    &generator
                        .terrain_tile(chunk_x * 16, chunk_z * 16, 16, 1, None)
                        .unwrap_or_else(|error| {
                            panic!("ice spike fixture should serialize: {error:?}")
                        }),
                )
                .unwrap_or_else(|error| panic!("ice spike response must be JSON: {error}"));
                for (_, y, _, block, _) in generated_blocks(&value) {
                    if block != "minecraft:packed_ice" {
                        continue;
                    }
                    packed_ice += 1;
                    min_y = min_y.min(i64::from(y));
                    max_y = max_y.max(i64::from(y));
                }
            }
        }
        assert!(
            packed_ice > 0,
            "ice spikes biome must generate packed ice blocks"
        );
        assert!(
            max_y - min_y >= 4,
            "packed ice must span a vertical range, got {min_y} to {max_y}"
        );
    }

    /// Swamp huts already generate, but they never reached the tile payload.
    ///
    /// Seed 1 chunk (-1834, -76) is a swamp-hut start. The hut is the only
    /// portable producer of a potted red mushroom, so that block proves the
    /// structure piece stream joined the generated-block palette.
    #[test]
    fn terrain_tile_serializes_swamp_hut_blocks() {
        let generator = SteelWorldgen::new("1", "overworld", None)
            .unwrap_or_else(|error| panic!("swamp hut generator should initialize: {error:?}"));
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile(-1834 * 16, -76 * 16, 16, 1, None)
                .unwrap_or_else(|error| panic!("swamp hut fixture should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("swamp hut response must be JSON: {error}"));
        let generated = generated_blocks(&value);
        assert!(
            generated
                .iter()
                .any(|(_, _, _, block, _)| block == "minecraft:potted_red_mushroom"),
            "swamp hut tile must place a potted red mushroom, got {} generated blocks",
            generated.len()
        );
        assert!(
            generated
                .iter()
                .any(|(_, _, _, block, _)| block == "minecraft:cauldron"),
            "swamp hut tile must place a cauldron"
        );
    }

    /// Igloos already generate, but they never reached the tile payload.
    ///
    /// Seed 1 chunk (-2026, 268) is an igloo start. White carpet is not a
    /// snowy-biome surface block, so its presence proves the template piece
    /// stream joined the generated-block palette.
    #[test]
    fn terrain_tile_serializes_igloo_blocks() {
        let generator = SteelWorldgen::new("1", "overworld", None)
            .unwrap_or_else(|error| panic!("igloo generator should initialize: {error:?}"));
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile(-2026 * 16, 268 * 16, 16, 1, None)
                .unwrap_or_else(|error| panic!("igloo fixture should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("igloo response must be JSON: {error}"));
        let generated = generated_blocks(&value);
        assert!(
            generated
                .iter()
                .any(|(_, _, _, block, _)| block == "minecraft:white_carpet"),
            "igloo tile must place white carpet, got {} generated blocks",
            generated.len()
        );
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TerrainResponse<'a> {
    generator: &'static str,
    version: &'static str,
    seed: &'a str,
    dimension: &'a str,
    origin_x: i32,
    origin_z: i32,
    size: u32,
    resolution: u32,
    samples_per_side: u32,
    heights: Vec<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height_approximation: Option<&'static str>,
    colors: Vec<u8>,
    biomes: Vec<String>,
    biome_indices: Vec<u16>,
    surface_blocks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_block_indices: Option<Vec<u16>>,
    /// Distinct block states placed by every stage that runs after the surface.
    ///
    /// This is deliberately general and carries no per-feature or per-structure
    /// discrimination. Consumers classify by block, not by which producer wrote
    /// it, so a stage that becomes generatable later flows through unchanged.
    /// An entry may be `minecraft:air` where a generated feature or structure
    /// clears terrain that the surface stage had filled.
    ///
    /// This is a palette, read together with `generated_block_positions` and
    /// `generated_block_indices`. A tile in an ice spikes biome holds tens of
    /// thousands of placements drawn from a handful of distinct states, and
    /// repeating the two identifier strings per placement cost more than every
    /// other field in the response combined.
    generated_block_palette: Vec<TerrainGeneratedBlockState>,
    /// Absolute world coordinates as flat `x, y, z` triples, one per placement.
    generated_block_positions: Vec<i32>,
    /// Palette index for each placement, parallel to the position triples.
    generated_block_indices: Vec<u16>,
    min_height: i16,
    max_height: i16,
    min_y: i16,
    present: Vec<u8>,
    decorations: [u8; 0],
}

/// One distinct block state in the generated-block palette.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TerrainGeneratedBlockState {
    block: String,
    state: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeResponse<'a> {
    generator: &'static str,
    version: &'static str,
    seed: &'a str,
    dimension: &'a str,
    chunk_x: i32,
    chunk_z: i32,
    min_y: i16,
    cells_xz: u32,
    cells_y: u32,
    lod: u16,
    /// `noise_default_solid` has no canonical block key: it is only the
    /// dimension's base noise block before later worldgen stages.
    material_keys: [&'static str; 4],
    voxels: Vec<u8>,
}

impl<'a> VolumeResponse<'a> {
    fn new(
        seed: &'a str,
        dimension: &'a str,
        chunk_x: i32,
        chunk_z: i32,
        volume: NoiseVolume,
    ) -> Self {
        Self {
            generator: "steelmc-wasm",
            version: "26.2",
            seed,
            dimension,
            chunk_x,
            chunk_z,
            min_y: volume.min_y,
            cells_xz: volume.cells_xz,
            cells_y: volume.cells_y,
            lod: volume.lod,
            material_keys: [
                "minecraft:air",
                "steel:noise_default_solid",
                "minecraft:water",
                "minecraft:lava",
            ],
            voxels: volume.voxels,
        }
    }
}

impl<'a> TerrainResponse<'a> {
    fn new(
        seed: &'a str,
        dimension: &'a str,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        tile: SurfaceTile,
        height_approximation: Option<&'static str>,
        compact_surface_blocks: bool,
    ) -> Self {
        let mut terrain_heights = tile
            .heights
            .iter()
            .zip(&tile.present)
            .filter_map(|(height, present)| (*present != 0).then_some(*height));
        let first = terrain_heights.next().unwrap_or(tile.min_y);
        let (min_height, max_height) = terrain_heights
            .fold((first, first), |(minimum, maximum), height| {
                (minimum.min(height), maximum.max(height))
            });
        let generated_count = tile.vegetation_blocks.len() + tile.structure_blocks.len();
        let mut generated_block_palette: Vec<TerrainGeneratedBlockState> = Vec::new();
        let mut palette_lookup: std::collections::HashMap<(String, String), u16> =
            std::collections::HashMap::new();
        let mut generated_block_positions = Vec::with_capacity(generated_count * 3);
        let mut generated_block_indices = Vec::with_capacity(generated_count);
        for block in tile
            .vegetation_blocks
            .into_iter()
            .chain(tile.structure_blocks)
        {
            let key = (block.block, block.state);
            let index = match palette_lookup.get(&key) {
                Some(&index) => index,
                None => {
                    let index = u16::try_from(generated_block_palette.len())
                        .expect("generated block palette exceeds u16");
                    generated_block_palette.push(TerrainGeneratedBlockState {
                        block: key.0.clone(),
                        state: key.1.clone(),
                    });
                    palette_lookup.insert(key, index);
                    index
                }
            };
            generated_block_positions.extend_from_slice(&[block.x, block.y, block.z]);
            generated_block_indices.push(index);
        }

        let (surface_blocks, surface_block_indices) = if compact_surface_blocks {
            (tile.surface_blocks, Some(tile.surface_block_indices))
        } else {
            let expanded = tile
                .surface_block_indices
                .iter()
                .map(|index| tile.surface_blocks[usize::from(*index)].clone())
                .collect();
            (expanded, None)
        };
        Self {
            generator: "steelmc-wasm",
            version: "26.2",
            seed,
            dimension,
            origin_x: x,
            origin_z: z,
            size,
            resolution,
            samples_per_side: tile.samples_per_side,
            heights: tile.heights,
            height_approximation,
            colors: tile.colors,
            biomes: tile.biomes,
            biome_indices: tile.biome_indices,
            surface_blocks,
            surface_block_indices,
            generated_block_palette,
            generated_block_positions,
            generated_block_indices,
            min_height,
            max_height,
            min_y: tile.min_y,
            present: tile.present,
            decorations: [],
        }
    }
}
