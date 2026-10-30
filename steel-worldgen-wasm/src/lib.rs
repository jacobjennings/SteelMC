//! WebAssembly adapter for Steel's Java 26.3 world generation.
//!
//! Tiles run the same stage code as the server through
//! [`steel_core::worldgen::standalone`]. Responses keep the JSON shape the
//! viewer already reads.

mod generation;

use generation::{GenerationCapabilities, OperationCapability};
use rustc_hash::FxHashMap;
use serde::Serialize;
use steel_core::worldgen::standalone::{
    DEFAULT_TERRAIN_CACHE_CAPACITY, GeneratedBlockFilter, StandaloneDimension, StandaloneTile,
    StandaloneWorldgen, StructureMarker, TileDetail,
};
use steel_registry::REGISTRY;
use steel_utils::BlockStateId;
use steel_worldgen::structure::StructureStart;
use wasm_bindgen::prelude::*;

/// Release reported in every response.
pub const RELEASE: &str = "26.3";
/// Generator name reported in every response.
pub const GENERATOR: &str = "steelmc-wasm";
/// Largest structure-marker search radius in blocks.
pub const MAX_STRUCTURE_MARKER_RADIUS: i32 = 4096;
/// Largest tile edge in blocks.
pub const MAX_TILE_SIZE: u32 = 256;
/// Grass colour for biomes that define none.
const DEFAULT_GRASS_COLOR: i32 = 0x6a_a8_4f;

/// Returns the generator release.
#[wasm_bindgen]
#[must_use]
pub fn release() -> String {
    RELEASE.to_owned()
}

/// Returns stage qualification and named refusals. This does not generate terrain.
///
/// # Errors
/// Returns a serialization error.
#[wasm_bindgen]
pub fn generation_capabilities() -> Result<String, JsValue> {
    serde_json::to_string(&GenerationCapabilities::current())
        .map_err(|error| JsValue::from_str(&error.to_string()))
}

/// Checks admission without executing the requested operation.
///
/// # Errors
/// Refuses unsupported or unknown operations by their stable reason code.
#[wasm_bindgen]
pub fn check_generation_operation(operation: &str) -> Result<(), JsValue> {
    match OperationCapability::for_operation(operation).refusal() {
        Some(reason) => Err(JsValue::from_str(reason)),
        None => Ok(()),
    }
}

/// Checks a tile grid request.
///
/// # Errors
/// Returns a message when the size is zero or above 256, or the resolution
/// does not divide it.
pub fn validate_terrain_grid(size: u32, resolution: u32) -> Result<(), String> {
    if size == 0
        || size > MAX_TILE_SIZE
        || resolution == 0
        || resolution > size
        || !size.is_multiple_of(resolution)
    {
        return Err("size must be <= 256 and evenly divisible by resolution".to_owned());
    }
    Ok(())
}

/// Parses a dimension name.
///
/// # Errors
/// Returns a message for a name other than `overworld`, `nether` or `end`.
pub fn parse_dimension(name: &str) -> Result<(StandaloneDimension, &'static str), String> {
    match name {
        "overworld" => Ok((StandaloneDimension::Overworld, "overworld")),
        "nether" => Ok((StandaloneDimension::Nether, "nether")),
        "end" => Ok((StandaloneDimension::End, "end")),
        _ => Err("unknown dimension".to_owned()),
    }
}

/// Reusable single-seed generator intended to live inside a Web Worker.
#[wasm_bindgen]
pub struct SteelWorldgen {
    inner: Generator,
}

#[wasm_bindgen]
impl SteelWorldgen {
    /// Creates a generator for one seed and dimension.
    ///
    /// `cache_capacity` bounds retained Terrain chunks.
    ///
    /// # Errors
    /// Returns an error for an invalid signed 64-bit seed, dimension or
    /// capacity.
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &str, dimension: &str, cache_capacity: Option<u32>) -> Result<Self, JsValue> {
        Generator::new(seed, dimension, cache_capacity)
            .map(|inner| Self { inner })
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Generates a Full tile: surface plus every block Features and
    /// structures change at or above it.
    ///
    /// # Errors
    /// Returns an error when the requested grid is invalid or serialization fails.
    pub fn terrain_tile(
        &mut self,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        compact_surface_blocks: Option<bool>,
    ) -> Result<String, JsValue> {
        self.inner
            .terrain_json(
                x,
                z,
                size,
                resolution,
                TileDetail::Full,
                compact_surface_blocks,
            )
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Generates a tile without carvers, Features or structure placement.
    ///
    /// # Errors
    /// Returns an error when the requested grid is invalid or serialization fails.
    pub fn terrain_tile_coarse(
        &mut self,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        compact_surface_blocks: Option<bool>,
    ) -> Result<String, JsValue> {
        self.inner
            .terrain_json(
                x,
                z,
                size,
                resolution,
                TileDetail::Coarse,
                compact_surface_blocks,
            )
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Generates a biome overview tile.
    ///
    /// Overworld and End heights are estimates and the response names the
    /// estimate. Nether heights are generated surfaces. Surface blocks and
    /// generated blocks are empty.
    ///
    /// # Errors
    /// Returns an error when the requested grid is invalid or serialization fails.
    pub fn terrain_tile_biome(
        &mut self,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        compact_surface_blocks: Option<bool>,
    ) -> Result<String, JsValue> {
        self.inner
            .terrain_json(
                x,
                z,
                size,
                resolution,
                TileDetail::Biome,
                compact_surface_blocks,
            )
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Generates vanilla structure starts near a map centre.
    ///
    /// Returned positions are vanilla locate positions. Bounds are the
    /// generated pieces' bounding box.
    ///
    /// # Errors
    /// Returns an error for a negative or too-large radius, or serialization
    /// failure.
    pub fn structure_markers(
        &mut self,
        center_x: i32,
        center_z: i32,
        radius_blocks: i32,
    ) -> Result<String, JsValue> {
        self.inner
            .structure_markers_json(center_x, center_z, radius_blocks)
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Returns base-noise occupancy with an exclusive maximum Y.
    ///
    /// # Errors
    /// Returns an error for invalid bounds, coordinates, LOD or serialization.
    pub fn noise_volume_chunk(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        max_y: i32,
        lod: u32,
    ) -> Result<String, JsValue> {
        self.inner
            .noise_volume_json(chunk_x, chunk_z, min_y, max_y, lod)
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Generator release.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn version(&self) -> String {
        RELEASE.to_owned()
    }
}

/// Platform-independent generator behind [`SteelWorldgen`].
///
/// Native tools use it directly to produce the same JSON as the browser.
pub struct Generator {
    worldgen: StandaloneWorldgen,
    seed: String,
    dimension: &'static str,
}

impl Generator {
    /// Creates a generator for one seed and dimension.
    ///
    /// # Errors
    /// Returns a message for an invalid seed, dimension or capacity.
    pub fn new(seed: &str, dimension: &str, cache_capacity: Option<u32>) -> Result<Self, String> {
        let signed_seed = seed
            .parse::<i64>()
            .map_err(|_| "seed must be a signed 64-bit integer".to_owned())?;
        let (dimension, name) = parse_dimension(dimension)?;
        let capacity = match cache_capacity {
            Some(0) => {
                return Err("terrain chunk cache capacity must be greater than zero".to_owned());
            }
            Some(capacity) => usize::try_from(capacity)
                .map_err(|_| "terrain chunk cache capacity is too large".to_owned())?,
            None => DEFAULT_TERRAIN_CACHE_CAPACITY,
        };
        Ok(Self {
            worldgen: StandaloneWorldgen::new(signed_seed, dimension, capacity),
            seed: seed.to_owned(),
            dimension: name,
        })
    }

    /// Serializes the standalone base-noise grid using the viewer contract.
    ///
    /// # Errors
    /// Returns an error for an invalid request or serialization failure.
    pub fn noise_volume_json(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        max_y: i32,
        lod: u32,
    ) -> Result<String, String> {
        let volume = self
            .worldgen
            .noise_volume_chunk(chunk_x, chunk_z, min_y, max_y, lod)?;
        serde_json::to_string(&VolumeResponse {
            generator: GENERATOR,
            version: RELEASE,
            seed: &self.seed,
            dimension: self.dimension,
            chunk_x,
            chunk_z,
            min_y: volume.min_y,
            cells_xz: volume.cells_xz,
            cells_y: volume.cells_y,
            lod,
            material_keys: [
                "minecraft:air",
                "steel:noise_default_solid",
                "minecraft:water",
                "minecraft:lava",
            ],
            voxels: volume.voxels,
        })
        .map_err(|error| error.to_string())
    }

    /// Returns the underlying generator.
    pub const fn worldgen(&mut self) -> &mut StandaloneWorldgen {
        &mut self.worldgen
    }

    /// Generates one tile as viewer JSON.
    ///
    /// # Errors
    /// Returns a message when the grid is invalid or serialization fails.
    pub fn terrain_json(
        &mut self,
        x: i32,
        z: i32,
        size: u32,
        resolution: u32,
        detail: TileDetail,
        compact_surface_blocks: Option<bool>,
    ) -> Result<String, String> {
        validate_terrain_grid(size, resolution)?;
        let tile = self.worldgen.tile(
            x,
            z,
            size,
            resolution,
            detail,
            GeneratedBlockFilter::AtOrAboveSurface,
        );
        let height_approximation = tile.heights_estimated.then(|| match self.dimension {
            "end" => "end_island_density",
            _ => "preliminary_surface_level",
        });
        serde_json::to_string(&TerrainResponse::new(
            &self.seed,
            self.dimension,
            (x, z, size, resolution),
            tile,
            height_approximation,
            compact_surface_blocks.unwrap_or(false),
            detail,
        ))
        .map_err(|error| error.to_string())
    }

    /// Generates structure markers as viewer JSON.
    ///
    /// # Errors
    /// Returns a message for a radius outside `0..=4096` or serialization failure.
    pub fn structure_markers_json(
        &mut self,
        center_x: i32,
        center_z: i32,
        radius_blocks: i32,
    ) -> Result<String, String> {
        if !(0..=MAX_STRUCTURE_MARKER_RADIUS).contains(&radius_blocks) {
            return Err("structure marker radius must be between 0 and 4096 blocks".to_owned());
        }
        let markers = self
            .worldgen
            .structure_markers(center_x, center_z, radius_blocks)
            .into_iter()
            .map(MarkerResponse::from_marker)
            .collect();
        serde_json::to_string(&StructureMarkerResponse {
            generator: GENERATOR,
            version: RELEASE,
            seed: &self.seed,
            dimension: self.dimension,
            center_x,
            center_z,
            radius_blocks,
            markers,
        })
        .map_err(|error| error.to_string())
    }
}

/// Registry block identifier without properties.
///
/// # Panics
/// Panics for an unknown state id.
#[must_use]
pub fn canonical_block_key(state: BlockStateId) -> String {
    let Some(block) = REGISTRY.blocks.by_state_id(state) else {
        panic!("generation produced an unknown block state {}", state.0);
    };
    block.key.to_string()
}

/// Block identifier with properties sorted by name, as vanilla prints states.
#[must_use]
pub fn canonical_block_state_key(state: BlockStateId) -> String {
    let block = canonical_block_key(state);
    let mut properties = REGISTRY.blocks.get_properties(state);
    if properties.is_empty() {
        return block;
    }
    properties.sort_unstable_by_key(|(key, _)| *key);
    let properties = properties
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    format!("{block}[{}]", properties.join(","))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TerrainResponse<'a> {
    generator: &'static str,
    version: &'static str,
    generation_stage: &'static str,
    native_full_qualified: bool,
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
    /// Distinct final states of every block that Features or structure
    /// placement changed. An entry may be air where generation cleared terrain.
    generated_block_palette: Vec<GeneratedBlockState>,
    /// Absolute world coordinates as flat `x, y, z` triples.
    generated_block_positions: Vec<i32>,
    /// Palette index for each placement, parallel to the position triples.
    generated_block_indices: Vec<u16>,
    min_height: i16,
    max_height: i16,
    min_y: i16,
    present: Vec<u8>,
    decorations: [u8; 0],
}

#[derive(Serialize)]
struct GeneratedBlockState {
    block: String,
    state: String,
}

impl<'a> TerrainResponse<'a> {
    fn new(
        seed: &'a str,
        dimension: &'a str,
        (origin_x, origin_z, size, resolution): (i32, i32, u32, u32),
        tile: StandaloneTile,
        height_approximation: Option<&'static str>,
        compact_surface_blocks: bool,
        detail: TileDetail,
    ) -> Self {
        let mut terrain_heights = tile
            .heights
            .iter()
            .zip(&tile.present)
            .filter_map(|(height, present)| present.then_some(*height));
        let first = terrain_heights.next().unwrap_or(tile.min_y);
        let (min_height, max_height) = terrain_heights
            .fold((first, first), |(minimum, maximum), height| {
                (minimum.min(height), maximum.max(height))
            });

        let mut biomes = Palette::default();
        let mut colors = Vec::with_capacity(tile.biomes.len() * 3);
        let mut biome_indices = Vec::with_capacity(tile.biomes.len());
        for biome in &tile.biomes {
            biome_indices.push(biomes.index(biome.key.to_string()));
            let color = biome.effects.grass_color.unwrap_or(DEFAULT_GRASS_COLOR) as u32;
            colors.extend_from_slice(&[(color >> 16) as u8, (color >> 8) as u8, color as u8]);
        }

        let mut surface_blocks = Palette::default();
        let surface_block_indices = tile
            .surface_states
            .iter()
            .map(|state| surface_blocks.index(canonical_block_key(*state)))
            .collect::<Vec<_>>();
        let (surface_blocks, surface_block_indices) = if compact_surface_blocks {
            (surface_blocks.entries, Some(surface_block_indices))
        } else {
            let expanded = surface_block_indices
                .iter()
                .map(|index| surface_blocks.entries[usize::from(*index)].clone())
                .collect();
            (expanded, None)
        };

        let mut state_lookup: FxHashMap<BlockStateId, u16> = FxHashMap::default();
        let mut generated_block_palette = Vec::new();
        let mut generated_block_positions = Vec::with_capacity(tile.generated_blocks.len() * 3);
        let mut generated_block_indices = Vec::with_capacity(tile.generated_blocks.len());
        for block in &tile.generated_blocks {
            let index = *state_lookup.entry(block.state).or_insert_with(|| {
                let index = u16::try_from(generated_block_palette.len())
                    .unwrap_or_else(|_| panic!("generated block palette exceeds u16"));
                generated_block_palette.push(GeneratedBlockState {
                    block: canonical_block_key(block.state),
                    state: canonical_block_state_key(block.state),
                });
                index
            });
            generated_block_positions.extend_from_slice(&[block.x, block.y, block.z]);
            generated_block_indices.push(index);
        }

        Self {
            generator: GENERATOR,
            version: RELEASE,
            generation_stage: match detail {
                TileDetail::Full => "features",
                TileDetail::Coarse => "surface",
                TileDetail::Biome => "biome",
            },
            native_full_qualified: false,
            seed,
            dimension,
            origin_x,
            origin_z,
            size,
            resolution,
            samples_per_side: tile.samples_per_side,
            heights: tile.heights,
            height_approximation,
            colors,
            biomes: biomes.entries,
            biome_indices,
            surface_blocks,
            surface_block_indices,
            generated_block_palette,
            generated_block_positions,
            generated_block_indices,
            min_height,
            max_height,
            min_y: tile.min_y,
            present: tile.present.into_iter().map(u8::from).collect(),
            decorations: [],
        }
    }
}

#[derive(Default)]
struct Palette {
    entries: Vec<String>,
    lookup: FxHashMap<String, u16>,
}

impl Palette {
    fn index(&mut self, key: String) -> u16 {
        if let Some(index) = self.lookup.get(&key) {
            return *index;
        }
        let index = u16::try_from(self.entries.len())
            .unwrap_or_else(|_| panic!("response palette exceeds u16"));
        self.entries.push(key.clone());
        self.lookup.insert(key, index);
        index
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
    markers: Vec<MarkerResponse>,
}

#[derive(Serialize)]
struct MarkerResponse {
    structure: String,
    x: i32,
    z: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bounds: Option<StructureBounds>,
}

impl MarkerResponse {
    fn from_marker(marker: StructureMarker) -> Self {
        let bounds = StructureStart::compute_bounding_box(&marker.start.pieces, 0).map(|bounds| {
            StructureBounds {
                min_x: bounds.min_x(),
                min_y: bounds.min_y(),
                min_z: bounds.min_z(),
                max_x: bounds.max_x(),
                max_y: bounds.max_y(),
                max_z: bounds.max_z(),
            }
        });
        Self {
            structure: marker.start.structure.to_string(),
            x: marker.locate_pos.0.x,
            z: marker.locate_pos.0.z,
            y: marker
                .start
                .placement_reference_pos()
                .map(|position| position.0.y),
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

/// Existing browser volume transport, with X varying fastest.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeResponse<'a> {
    generator: &'static str,
    version: &'static str,
    seed: &'a str,
    dimension: &'static str,
    chunk_x: i32,
    chunk_z: i32,
    min_y: i32,
    cells_xz: u32,
    cells_y: u32,
    lod: u32,
    material_keys: [&'static str; 4],
    voxels: Vec<u8>,
}
