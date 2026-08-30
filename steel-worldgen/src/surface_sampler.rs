//! Browser-safe terrain surface sampling.
//!
//! This adapter intentionally depends only on deterministic worldgen state. It
//! does not own transport, persistence, DOM APIs, or scheduling.

use std::collections::HashMap;

use steel_registry::init_vanilla_registry;
use steel_utils::random::{
    Random, RandomSplitter, legacy_random::LegacyRandom, xoroshiro::Xoroshiro,
};

use crate::biomes::BiomeSourceKind;
use crate::density::{ColumnCache, DimensionNoises, NoiseSettings};
use crate::density_functions::{end::EndNoises, nether::NetherNoises, overworld::OverworldNoises};
use crate::noise::NoiseChunk;
use crate::noise_parameters::get_noise_parameters;

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
    /// Highest solid density for each sample.
    pub heights: Vec<i16>,
    /// RGB bytes derived from the sampled biome's configured grass colour.
    pub colors: Vec<u8>,
    /// Whether the sampled column contains solid terrain.
    pub present: Vec<u8>,
    /// Dimension minimum build height.
    pub min_y: i16,
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
}

/// Generic dimension implementation kept public so native adapters can reuse it.
pub struct DimensionSurfaceSampler<N: DimensionNoises> {
    noises: Box<N>,
    biome_source: BiomeSourceKind,
}

impl<N: DimensionNoises> DimensionSurfaceSampler<N> {
    fn new(seed: u64, biome_source: BiomeSourceKind) -> Self {
        let splitter: RandomSplitter = if N::Settings::LEGACY_RANDOM_SOURCE {
            LegacyRandom::from_seed(seed).next_positional()
        } else {
            Xoroshiro::from_seed(seed).next_positional()
        };
        Self {
            noises: Box::new(N::create(seed, &splitter, &get_noise_parameters())),
            biome_source,
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

        for sample_z in 0..samples_per_side {
            for sample_x in 0..samples_per_side {
                let x = origin_x.saturating_add((sample_x * resolution) as i32);
                let z = origin_z.saturating_add((sample_z * resolution) as i32);
                let chunk_x = x.div_euclid(16);
                let chunk_z = z.div_euclid(16);
                let chunk = chunks
                    .entry((chunk_x, chunk_z))
                    .or_insert_with(|| self.sample_chunk(chunk_x, chunk_z));
                let index = (z.rem_euclid(16) * 16 + x.rem_euclid(16)) as usize;
                let height = chunk[index];
                let exists = height != i16::MIN;
                let display_height = if exists {
                    height
                } else {
                    N::Settings::MIN_Y as i16
                };
                heights.push(display_height);
                present.push(u8::from(exists));

                let mut biome_sampler = self.biome_source.chunk_sampler();
                let biome = biome_sampler.sample(x >> 2, i32::from(display_height) >> 2, z >> 2);
                let color = biome.effects.grass_color.unwrap_or(0x6a_a8_4f) as u32;
                colors.extend_from_slice(&[(color >> 16) as u8, (color >> 8) as u8, color as u8]);
            }
        }

        SurfaceTile {
            samples_per_side,
            heights,
            colors,
            present,
            min_y: N::Settings::MIN_Y as i16,
        }
    }

    fn sample_chunk(&self, chunk_x: i32, chunk_z: i32) -> [i16; 256] {
        let chunk_min_x = chunk_x * 16;
        let chunk_min_z = chunk_z * 16;
        let mut result = [i16::MIN; 256];
        let mut noise_chunk = NoiseChunk::<N>::new(chunk_min_x, chunk_min_z);
        let mut cache = N::ColumnCache::default();
        cache.init_grid(chunk_min_x, chunk_min_z, &self.noises);
        noise_chunk.fill(
            &self.noises,
            &mut cache,
            None,
            |local_x, y, local_z, density, _, _| {
                let index = local_z * 16 + local_x;
                if density > 0.0 && result[index] == i16::MIN {
                    result[index] = y as i16;
                }
            },
        );
        result
    }
}
