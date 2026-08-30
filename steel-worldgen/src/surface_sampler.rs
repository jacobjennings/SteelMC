//! Browser-safe terrain surface sampling.
//!
//! This adapter intentionally depends only on deterministic worldgen state. It
//! does not own transport, persistence, DOM APIs, or scheduling.

use std::collections::HashMap;

use steel_registry::{REGISTRY, init_vanilla_registry, vanilla_blocks};
use steel_utils::random::{
    Random, RandomSplitter, legacy_random::LegacyRandom, xoroshiro::Xoroshiro,
};

use crate::biomes::BiomeSourceKind;
use crate::density::{ColumnCache, DimensionNoises, NoiseSettings};
use crate::density_functions::{end::EndNoises, nether::NetherNoises, overworld::OverworldNoises};
use crate::noise::NoiseChunk;
use crate::noise::{Aquifer, AquiferResult};
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
    /// Canonical registry identifiers for the sampled biomes, in palette order.
    pub biomes: Vec<String>,
    /// Palette index for each sample, parallel to `heights` and `present`.
    pub biome_indices: Vec<u16>,
    /// Whether the sampled column contains solid terrain.
    pub present: Vec<u8>,
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
            splitter,
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
}
