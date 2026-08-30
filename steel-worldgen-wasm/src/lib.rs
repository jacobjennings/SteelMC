//! Byte-oriented WebAssembly adapter for Steel world generation.

use serde::Serialize;
use steel_worldgen::surface_sampler::{SurfaceDimension, SurfaceSampler, SurfaceTile};
use wasm_bindgen::prelude::*;

/// Reusable single-seed generator intended to live inside a Web Worker.
#[wasm_bindgen]
pub struct SteelWorldgen {
    sampler: SurfaceSampler,
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
        Ok(Self {
            sampler: SurfaceSampler::new(signed_seed as u64, dimension),
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
        let tile = self.sampler.tile(x, z, size, resolution);
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
    min_height: i16,
    max_height: i16,
    min_y: i16,
    present: Vec<u8>,
    decorations: [u8; 0],
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
            min_height,
            max_height,
            min_y: tile.min_y,
            present: tile.present,
            decorations: [],
        }
    }
}
