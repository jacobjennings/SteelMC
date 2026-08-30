//! Byte-oriented WebAssembly adapter for Steel world generation.

use std::cell::RefCell;

use serde::Serialize;
use steel_registry::REGISTRY;
use steel_utils::random::{
    Random, RandomSplitter, legacy_random::LegacyRandom, xoroshiro::Xoroshiro,
};
use steel_utils::{BlockPos, ChunkPos};
use steel_worldgen::{
    biomes::BiomeSourceKind,
    density::{DimensionNoises, NoiseSettings},
    density_functions::{end::EndNoises, nether::NetherNoises, overworld::OverworldNoises},
    noise::LazyAquifer,
    noise_parameters::get_noise_parameters,
    structure::{GenerationContext, StructureGenerator, StructureStart},
    surface_sampler::{
        NoiseVolume, SurfaceChunkCache, SurfaceDimension, SurfaceSampler, SurfaceTile,
    },
};
use wasm_bindgen::prelude::*;

const MAX_STRUCTURE_MARKER_RADIUS: i32 = 4096;

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
    pub fn new(seed: &str, dimension: &str) -> Result<Self, JsValue> {
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
        Ok(Self {
            sampler,
            surface_chunk_cache: RefCell::new(SurfaceChunkCache::default()),
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
    ) -> Result<String, JsValue> {
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

    #[test]
    fn generated_marker_response_has_unique_complete_bounds() {
        let generator = SteelWorldgen::new("0", "overworld").unwrap_or_else(|error| {
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
        let generator = SteelWorldgen::new("0", "overworld")
            .unwrap_or_else(|error| panic!("surface generator should initialize: {error:?}"));
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile(0, 0, 16, 16)
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
    fn terrain_tile_serializes_canonical_cherry_vegetation_states() {
        let generator = SteelWorldgen::new("1", "overworld").unwrap_or_else(|error| {
            panic!("cherry fixture generator should initialize: {error:?}")
        });
        let value: serde_json::Value = serde_json::from_str(
            &generator
                .terrain_tile(-108 * 16, -36 * 16, 16, 1)
                .unwrap_or_else(|error| panic!("cherry fixture should serialize: {error:?}")),
        )
        .unwrap_or_else(|error| panic!("cherry fixture response must be JSON: {error}"));
        let vegetation = value["vegetationBlocks"]
            .as_array()
            .unwrap_or_else(|| panic!("cherry fixture must include vegetationBlocks"));
        assert!(
            vegetation
                .iter()
                .any(|block| block["block"] == "minecraft:cherry_log")
        );
        assert!(
            vegetation
                .iter()
                .any(|block| block["block"] == "minecraft:cherry_leaves")
        );
        assert!(
            vegetation
                .iter()
                .any(|block| block["block"] == "minecraft:pink_petals")
        );
        assert!(vegetation.iter().all(|block| {
            block["x"].is_i64()
                && block["y"].is_i64()
                && block["z"].is_i64()
                && block["state"]
                    .as_str()
                    .is_some_and(|state| state.starts_with("minecraft:"))
        }));
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
    colors: Vec<u8>,
    biomes: Vec<String>,
    biome_indices: Vec<u16>,
    surface_blocks: Vec<String>,
    vegetation_blocks: Vec<TerrainVegetationBlock>,
    min_height: i16,
    max_height: i16,
    min_y: i16,
    present: Vec<u8>,
    decorations: [u8; 0],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TerrainVegetationBlock {
    x: i32,
    y: i32,
    z: i32,
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
            colors: tile.colors,
            biomes: tile.biomes,
            biome_indices: tile.biome_indices,
            surface_blocks: tile.surface_blocks,
            vegetation_blocks: tile
                .vegetation_blocks
                .into_iter()
                .map(|block| TerrainVegetationBlock {
                    x: block.x,
                    y: block.y,
                    z: block.z,
                    block: block.block,
                    state: block.state,
                })
                .collect(),
            min_height,
            max_height,
            min_y: tile.min_y,
            present: tile.present,
            decorations: [],
        }
    }
}
