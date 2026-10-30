//! In-memory Noise and Material stages for Java 26.3.
//!
//! This is an intermediate source port, not the viewer's Full terrain API.
//! Structure density, carvers and Features are not executed. Chunk positions
//! are bounded before arithmetic, and all block storage is owned by the caller.

use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::{RegistryEntry, init_vanilla_registry, vanilla_blocks};
use steel_utils::BlockStateId;
use steel_utils::random::{Random, RandomSplitter, xoroshiro::Xoroshiro};

use crate::biomes::{BiomeSourceKind, ChunkBiomeSampler, obfuscate_biome_seed};
use crate::density::DimensionNoises;
use crate::density_functions::overworld::OverworldNoises;
use crate::noise::{Aquifer, AquiferResult, NoiseChunk, OreVeinifier};
use crate::noise_parameters::get_noise_parameters;
use crate::surface::{
    MaterialSurfaceGrid, SurfaceBiomeAccess, SurfaceBlockAccess, SurfaceExtensions, SurfaceStage,
    SurfaceSystem,
};

type Settings = <OverworldNoises as DimensionNoises>::Settings;

/// Java release implemented by this source port, independent of packaging.
pub const MINECRAFT_VERSION: &str = "26.3";

/// Reusable, seed-bound Overworld noise and material state.
pub struct PortableOverworld {
    noises: OverworldNoises,
    splitter: RandomSplitter,
    biomes: BiomeSourceKind,
    surface: SurfaceSystem,
    ore: Option<OreVeinifier>,
    biome_zoom_seed: i64,
    extensions: SurfaceExtensions,
}

impl PortableOverworld {
    /// Initializes generated 26.3 registries and the source-defined random streams.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        init_vanilla_registry();
        let splitter = Xoroshiro::from_seed(seed).next_positional();
        let params = get_noise_parameters();
        let noises = OverworldNoises::create(seed, &splitter, &params);
        let biomes = BiomeSourceKind::overworld(seed);
        let extensions = SurfaceExtensions::from_possible_biomes(&biomes.possible_biomes());
        let surface = SurfaceSystem::new(
            &splitter,
            &params,
            OverworldNoises::surface_noise_ids(),
            OverworldNoises::surface_gradient_ids(),
            Settings::default_block_id(),
            Settings::SEA_LEVEL,
        );
        let ore = Settings::MATERIAL_ORE_VEINS_ENABLED.then(|| OreVeinifier::new(&splitter));
        Self {
            noises,
            splitter,
            biomes,
            surface,
            ore,
            biome_zoom_seed: obfuscate_biome_seed(seed as i64),
            extensions,
        }
    }

    /// Generates a 16 by 16 Noise-stage chunk without structure density.
    ///
    /// The retained material density buffers belong to this chunk and must be
    /// used by `build_material` before it can be described as Material output.
    /// Coordinates are chunk coordinates within Minecraft's horizontal border.
    pub fn noise_chunk(&self, x: i32, z: i32) -> Result<PortableChunk<'_>, &'static str> {
        const MAX_CHUNK: i32 = 1_875_000;
        if !(-MAX_CHUNK..MAX_CHUNK).contains(&x) || !(-MAX_CHUNK..MAX_CHUNK).contains(&z) {
            return Err("chunk outside Minecraft horizontal border");
        }
        let min_x = x * 16;
        let min_z = z * 16;
        let mut chunk = PortableChunk {
            min_x,
            min_z,
            states: vec![vanilla_blocks::AIR.default_state(); 256 * Settings::HEIGHT as usize],
            heights: [Settings::MIN_Y; 256],
            material_values: Vec::new(),
            // Keep the binding private so another seed cannot supply material values.
            stage: ChunkStage::Noise,
            seed_binding: self,
        };
        let mut cache = <OverworldNoises as DimensionNoises>::ColumnCache::default();
        cache.init_grid(min_x, min_z, &self.noises);
        let mut aquifer = Aquifer::new(
            min_x,
            min_z,
            Settings::MIN_Y,
            Settings::HEIGHT,
            &self.splitter,
            &self.noises,
            <OverworldNoises as DimensionNoises>::ColumnCache::default(),
        );
        let value_count = OverworldNoises::material_ore_vein_value_count();
        let mut values = vec![0.0; chunk.states.len() * value_count];
        let mut material_cache = <OverworldNoises as DimensionNoises>::ColumnCache::default();
        NoiseChunk::<OverworldNoises>::new(min_x, min_z).fill(
            &self.noises,
            &mut cache,
            None,
            |lx, y, lz, density, interpolated, _| {
                let index = ((y - Settings::MIN_Y) as usize * 16 + lz) * 16 + lx;
                if value_count != 0 {
                    self.noises.fill_material_ore_vein_values(
                        &mut material_cache,
                        interpolated,
                        min_x + lx as i32,
                        y,
                        min_z + lz as i32,
                        &mut values[index * value_count..(index + 1) * value_count],
                    );
                }
                chunk.states[index] = match aquifer.compute_substance(
                    &self.noises,
                    min_x + lx as i32,
                    y,
                    min_z + lz as i32,
                    f64::from(density),
                ) {
                    AquiferResult::Solid => Settings::default_block_id(),
                    AquiferResult::Fluid(state) => state,
                    AquiferResult::Air => vanilla_blocks::AIR.default_state(),
                };
            },
        );
        chunk.material_values = values;
        chunk.prime_world_surface_heightmap();
        Ok(chunk)
    }

    /// Runs the 26.3 Material stage on the Noise output from this generator.
    ///
    /// Material ore rules consume densities sampled before the surface scan.
    /// They run for every solid block, rather than only for default stone.
    pub fn build_material(&self, chunk: &mut PortableChunk<'_>) -> Result<(), &'static str> {
        if !std::ptr::eq(self, chunk.seed_binding) || chunk.stage != ChunkStage::Noise {
            return Err("material stage requires this generator's Noise output");
        }
        let mut cache = <OverworldNoises as DimensionNoises>::ColumnCache::default();
        let mut corner = |x, z| {
            cache.ensure(x, z, &self.noises);
            self.noises
                .router_preliminary_surface_level(&mut cache, x, 0, z)
        };
        let material_surface = MaterialSurfaceGrid::from_corners(
            corner(chunk.min_x, chunk.min_z),
            corner(chunk.min_x + 16, chunk.min_z),
            corner(chunk.min_x, chunk.min_z + 16),
            corner(chunk.min_x + 16, chunk.min_z + 16),
        );
        let values = std::mem::take(&mut chunk.material_values);
        let mut biomes = PortableBiomes(self.biomes.chunk_sampler());
        SurfaceStage::<OverworldNoises>::new(&self.surface, self.biome_zoom_seed, self.extensions)
            .build_surface(
                chunk,
                &mut biomes,
                &self.noises,
                &values,
                &material_surface,
                self.ore.as_ref(),
            );
        chunk.stage = ChunkStage::Material;
        chunk.prime_world_surface_heightmap();
        Ok(())
    }
}

#[derive(PartialEq, Eq)]
enum ChunkStage {
    Noise,
    Material,
}

/// Owned pre-Carvers chunk. State order is increasing relative Y, Z, then X.
///
/// Material buffers and generator ownership cannot be changed by consumers.
/// This prevents cross-seed material evaluation and applying the stage twice.
pub struct PortableChunk<'a> {
    min_x: i32,
    min_z: i32,
    states: Vec<BlockStateId>,
    heights: [i32; 256],
    material_values: Vec<f32>,
    stage: ChunkStage,
    seed_binding: &'a PortableOverworld,
}

impl PortableChunk<'_> {
    /// All final states at the currently completed stage.
    #[must_use]
    pub fn states(&self) -> &[BlockStateId] {
        &self.states
    }

    /// First available Y above the highest non-air block, in local Z/X order.
    #[must_use]
    pub const fn heights(&self) -> &[i32; 256] {
        &self.heights
    }
}

struct PortableBiomes<'a>(ChunkBiomeSampler<'a>);

impl SurfaceBiomeAccess for PortableBiomes<'_> {
    fn biome_id_at_quart(&mut self, x: i32, y: i32, z: i32) -> u16 {
        self.0.sample(x, y, z).id() as u16
    }
}

impl SurfaceBlockAccess for PortableChunk<'_> {
    fn min_y(&self) -> i32 {
        Settings::MIN_Y
    }
    fn chunk_min_x(&self) -> i32 {
        self.min_x
    }
    fn chunk_min_z(&self) -> i32 {
        self.min_z
    }
    fn prime_world_surface_heightmap(&mut self) {
        for z in 0..16 {
            for x in 0..16 {
                self.heights[z * 16 + x] = (0..Settings::HEIGHT as usize)
                    .rev()
                    .find(|&y| !self.states[(y * 16 + z) * 16 + x].is_air())
                    .map_or(Settings::MIN_Y, |y| Settings::MIN_Y + y as i32 + 1);
            }
        }
    }
    fn world_surface_height_at(&mut self, x: usize, z: usize) -> i32 {
        self.heights[z * 16 + x]
    }
    fn read_column_into(&mut self, x: usize, z: usize, output: &mut Vec<BlockStateId>) {
        output.clear();
        output.extend((0..Settings::HEIGHT as usize).map(|y| self.states[(y * 16 + z) * 16 + x]));
    }
    fn write_column(&mut self, x: usize, z: usize, blocks: &[(usize, BlockStateId)]) {
        for &(y, state) in blocks {
            self.set_relative_block(x, y, z, state);
        }
    }
    fn get_relative_block(&mut self, x: usize, y: usize, z: usize) -> Option<BlockStateId> {
        if x >= 16 || z >= 16 || y >= Settings::HEIGHT as usize {
            return None;
        }
        Some(self.states[(y * 16 + z) * 16 + x])
    }
    fn set_relative_block(&mut self, x: usize, y: usize, z: usize, state: BlockStateId) {
        if x >= 16 || z >= 16 || y >= Settings::HEIGHT as usize {
            return;
        }
        self.states[(y * 16 + z) * 16 + x] = state;
        // Extension writes must update the height read by later columns.
        let column = z * 16 + x;
        let world_y = Settings::MIN_Y + y as i32;
        if !state.is_air() {
            self.heights[column] = self.heights[column].max(world_y + 1);
        } else if self.heights[column] == world_y + 1 {
            self.heights[column] = (0..y)
                .rev()
                .find(|&below| !self.states[(below * 16 + z) * 16 + x].is_air())
                .map_or(Settings::MIN_Y, |below| Settings::MIN_Y + below as i32 + 1);
        }
    }
}
