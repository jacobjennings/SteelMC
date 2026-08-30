//! Portable Vanilla carver stage.
//!
//! The native chunk host and the static WASM host share this algorithm through
//! [`CarverBlockAccess`].  It deliberately carries no chunk-status, entity,
//! or scheduling dependency: those remain boundary concerns of the host.

use std::{cell::Cell, sync::LazyLock};

use rustc_hash::FxHashMap;
use steel_math::{lerp2, trig};
use steel_registry::biome::BiomeRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::carver::ConfiguredCarverKind;
use steel_registry::{REGISTRY, RegistryExt as _, vanilla_blocks};
use steel_utils::random::{Random as _, RandomSplitter, legacy_random::LegacyRandom};
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Identifier};

use crate::biome_zoom::fuzzed_biome_at_block;
use crate::density::{ColumnCache as _, DimensionNoises, NoiseSettings as _};
use crate::noise::{Aquifer, AquiferResult};
use crate::surface::{
    PreliminarySurfaceCorners, SurfaceConditionNoiseCache, SurfaceRuleContext, SurfaceSystem,
};

pub mod canyon;
pub mod cave;
mod mask;

pub use mask::CarvingMask;

/// The mutable per-chunk data that the portable Carvers stage needs.
///
/// Native callers may forward fluid-postprocessing bookkeeping through the
/// default hook.  The static sampler takes a final block-state snapshot, so
/// its hook is intentionally a no-op.
pub trait CarverBlockAccess {
    /// First block Y in the chunk.
    fn min_y(&self) -> i32;
    /// Vertical block extent of the chunk.
    fn height(&self) -> i32;
    /// World X of the chunk's west edge.
    fn chunk_min_x(&self) -> i32;
    /// World Z of the chunk's north edge.
    fn chunk_min_z(&self) -> i32;
    /// Reads one current local-chunk block state.
    fn block_state(&self, pos: BlockPos) -> BlockStateId;
    /// Writes one current local-chunk block state.
    fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId);
    /// Gets the `WORLD_SURFACE_WG` first-available height for one local column.
    fn world_surface_wg_first_available(&self, local_x: usize, local_z: usize) -> i32;
    /// Records a fluid update requested by a native host after carving.
    fn mark_pos_for_postprocessing(&mut self, _pos: BlockPos) {}
}

/// Portable driver for one Vanilla Carvers invocation.
pub struct CarverStage<'a, N: DimensionNoises> {
    noises: &'a N,
    splitter: &'a RandomSplitter,
    surface_system: &'a SurfaceSystem,
    seed: i64,
    biome_zoom_seed: i64,
}

impl<'a, N: DimensionNoises> CarverStage<'a, N> {
    /// Creates a stage using the same dimension state as Noise and Surface.
    #[must_use]
    pub const fn new(
        noises: &'a N,
        splitter: &'a RandomSplitter,
        surface_system: &'a SurfaceSystem,
        seed: i64,
        biome_zoom_seed: i64,
    ) -> Self {
        Self {
            noises,
            splitter,
            surface_system,
            seed,
            biome_zoom_seed,
        }
    }

    /// Applies the full 17×17 source-carver sweep to a post-Surface chunk.
    ///
    /// `biome_at_quart` must be the unfuzzed noise-biome lookup.  The source
    /// sweep and top-material lookup deliberately share it, matching Vanilla.
    pub fn apply_chunk<H, F>(&self, host: &mut H, mut biome_at_quart: F)
    where
        H: CarverBlockAccess,
        F: FnMut(i32, i32, i32) -> u16,
    {
        let chunk_min_x = host.chunk_min_x();
        let chunk_min_z = host.chunk_min_z();
        let chunk_x = chunk_min_x.div_euclid(16);
        let chunk_z = chunk_min_z.div_euclid(16);
        let mut source_biomes = Vec::with_capacity(17 * 17);
        for dx in -8_i32..=8 {
            for dz in -8_i32..=8 {
                let source_x = chunk_x + dx;
                let source_z = chunk_z + dz;
                source_biomes.push(SourceChunk {
                    pos: ChunkPos::new(source_x, source_z),
                    biome: REGISTRY
                        .biomes
                        .by_id(biome_at_quart(source_x * 4, 0, source_z * 4) as usize)
                        .expect("carver source biome id must be registered"),
                });
            }
        }

        let min_y = host.min_y();
        let height = host.height();
        let mut column_cache = N::ColumnCache::default();
        if N::Settings::AQUIFERS_ENABLED {
            column_cache.init_grid(chunk_min_x, chunk_min_z, self.noises);
        }
        let mut aquifer = Aquifer::<N>::new(
            chunk_min_x,
            chunk_min_z,
            min_y,
            height,
            self.splitter,
            self.noises,
            column_cache,
        );
        let corners = PreliminarySurfaceCorners {
            nw: aquifer.preliminary_surface_level(self.noises, chunk_min_x, chunk_min_z),
            ne: aquifer.preliminary_surface_level(self.noises, chunk_min_x + 16, chunk_min_z),
            sw: aquifer.preliminary_surface_level(self.noises, chunk_min_x, chunk_min_z + 16),
            se: aquifer.preliminary_surface_level(self.noises, chunk_min_x + 16, chunk_min_z + 16),
        };
        let mut context = CarvingContext {
            min_y,
            gen_depth: height,
            surface_system: self.surface_system,
            aquifer: &mut aquifer,
            corners,
            chunk_min_x,
            chunk_min_z,
        };
        let mut mask = CarvingMask::new(height, min_y);
        let mut biome_getter = |pos: BlockPos| {
            fuzzed_biome_at_block(self.biome_zoom_seed, pos, |quart| {
                biome_at_quart(quart.x, quart.y, quart.z)
            })
        };
        let mut run = CarveRun {
            context: &mut context,
            noises: self.noises,
            host,
            chunk_min_x,
            chunk_min_z,
            biome_getter: &mut biome_getter,
            mask: &mut mask,
            ids: CarverBlockIds::load(),
        };
        run.run_all(&source_biomes, self.seed);
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceChunk {
    pos: ChunkPos,
    biome: BiomeRef,
}

struct CarvingContext<'a, N: DimensionNoises> {
    min_y: i32,
    gen_depth: i32,
    surface_system: &'a SurfaceSystem,
    aquifer: &'a mut Aquifer<N>,
    corners: PreliminarySurfaceCorners,
    chunk_min_x: i32,
    chunk_min_z: i32,
}

impl<N: DimensionNoises> CarvingContext<'_, N> {
    fn min_surface_level(&self, block_x: i32, block_z: i32) -> i32 {
        let local_x = (block_x - self.chunk_min_x).clamp(0, 16);
        let local_z = (block_z - self.chunk_min_z).clamp(0, 16);
        let x_fraction = f64::from(local_x as u8) / 16.0;
        let z_fraction = f64::from(local_z as u8) / 16.0;
        let corners = self.corners;
        lerp2(
            x_fraction,
            z_fraction,
            f64::from(corners.nw),
            f64::from(corners.ne),
            f64::from(corners.sw),
            f64::from(corners.se),
        )
        .floor() as i32
    }

    fn top_material(
        &self,
        biome_id: u16,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        steep: bool,
        under_fluid: bool,
    ) -> Option<BlockStateId> {
        let surface_depth = self.surface_system.get_surface_depth(block_x, block_z);
        let surface_secondary = self.surface_system.get_surface_secondary(block_x, block_z);
        let min_surface_level = self.min_surface_level(block_x, block_z) + surface_depth - 8;
        let water_height = if under_fluid { block_y + 1 } else { i32::MIN };
        let values = N::surface_noise_ids()
            .iter()
            .map(|_| Cell::new(0.0))
            .collect::<Vec<_>>();
        let initialized = N::surface_noise_ids()
            .iter()
            .map(|_| Cell::new(false))
            .collect::<Vec<_>>();
        let cache = SurfaceConditionNoiseCache::new(&values, &initialized);
        let mut context = SurfaceRuleContext::new(
            block_x,
            block_z,
            surface_depth,
            surface_secondary,
            min_surface_level,
            steep,
            block_y,
            1,
            1,
            water_height,
            Some(biome_id),
            None,
            self.surface_system,
            &cache,
            N::surface_rule_block_states(),
        );
        N::try_apply_surface_rule(&mut context)
    }
}

#[derive(Debug)]
struct CarverReplaceableStates {
    states: Box<[bool]>,
}

impl CarverReplaceableStates {
    fn build(tag: &Identifier) -> Self {
        Self {
            states: REGISTRY
                .blocks
                .state_to_block_lookup
                .iter()
                .map(|&block| block.has_tag(tag))
                .collect(),
        }
    }

    #[inline]
    fn contains(&self, state: BlockStateId) -> bool {
        self.states.get(state.0 as usize).copied().unwrap_or(false)
    }
}

static REPLACEABLE_STATES: LazyLock<FxHashMap<Identifier, CarverReplaceableStates>> =
    LazyLock::new(|| {
        let mut by_tag = FxHashMap::default();
        for (_, carver) in REGISTRY.configured_carvers.iter() {
            let tag = &carver.base().replaceable_tag;
            by_tag
                .entry(tag.clone())
                .or_insert_with(|| CarverReplaceableStates::build(tag));
        }
        by_tag
    });

fn cached_replaceable_states(tag: &Identifier) -> Option<&'static CarverReplaceableStates> {
    REPLACEABLE_STATES.get(tag)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CarverStyle {
    Overworld,
    Nether,
}

#[derive(Debug, Clone, Copy)]
struct CarverBlockIds {
    air: BlockStateId,
    cave_air: BlockStateId,
    lava: BlockStateId,
    grass_block: BlockStateId,
    mycelium: BlockStateId,
    dirt: BlockStateId,
}

impl CarverBlockIds {
    fn load() -> Self {
        static IDS: LazyLock<CarverBlockIds> = LazyLock::new(|| CarverBlockIds {
            air: REGISTRY.blocks.get_default_state_id(&vanilla_blocks::AIR),
            cave_air: REGISTRY
                .blocks
                .get_default_state_id(&vanilla_blocks::CAVE_AIR),
            lava: REGISTRY.blocks.get_default_state_id(&vanilla_blocks::LAVA),
            grass_block: REGISTRY
                .blocks
                .get_default_state_id(&vanilla_blocks::GRASS_BLOCK),
            mycelium: REGISTRY
                .blocks
                .get_default_state_id(&vanilla_blocks::MYCELIUM),
            dirt: REGISTRY.blocks.get_default_state_id(&vanilla_blocks::DIRT),
        });
        *IDS
    }

    const fn is_air_like(self, state: BlockStateId) -> bool {
        state.0 == self.air.0 || state.0 == self.cave_air.0
    }
}

pub(crate) trait CarveSkipChecker {
    fn should_skip(&mut self, xd: f64, yd: f64, zd: f64, world_y: i32) -> bool;
}

impl<F: FnMut(f64, f64, f64, i32) -> bool> CarveSkipChecker for F {
    fn should_skip(&mut self, xd: f64, yd: f64, zd: f64, world_y: i32) -> bool {
        self(xd, yd, zd, world_y)
    }
}

pub(crate) struct CarveParams<'a> {
    pub(crate) replaceable_tag: &'a Identifier,
    replaceable_states: Option<&'static CarverReplaceableStates>,
    pub(crate) lava_level_y: i32,
    pub(crate) style: CarverStyle,
}

#[inline]
pub(crate) fn horizontal_tunnel_radius(progress_arg: f32, thickness: f32) -> f64 {
    1.5 + f64::from(trig::sin(f64::from(progress_arg)) * thickness)
}

pub(crate) fn can_reach(
    chunk_min_x: i32,
    chunk_min_z: i32,
    x: f64,
    z: f64,
    current_step: i32,
    total_steps: i32,
    thickness: f32,
) -> bool {
    let x_delta = x - (f64::from(chunk_min_x) + 8.0);
    let z_delta = z - (f64::from(chunk_min_z) + 8.0);
    let remaining = f64::from(total_steps - current_step);
    let radius = f64::from(thickness + 2.0_f32 + 16.0_f32);
    x_delta * x_delta + z_delta * z_delta - remaining * remaining <= radius * radius
}

enum CarveState {
    Place(BlockStateId),
    Skip,
}

pub(crate) struct CarveRun<'a, 'b, N, H, F>
where
    N: DimensionNoises,
    H: CarverBlockAccess,
    F: FnMut(BlockPos) -> u16,
{
    context: &'a mut CarvingContext<'b, N>,
    noises: &'a N,
    host: &'a mut H,
    chunk_min_x: i32,
    chunk_min_z: i32,
    biome_getter: &'a mut F,
    mask: &'a mut CarvingMask,
    ids: CarverBlockIds,
}

impl<N, H, F> CarveRun<'_, '_, N, H, F>
where
    N: DimensionNoises,
    H: CarverBlockAccess,
    F: FnMut(BlockPos) -> u16,
{
    fn run_all(&mut self, sources: &[SourceChunk], seed: i64) {
        let mut random = LegacyRandom::from_seed(0);
        for source in sources {
            for (index, carver_key) in source.biome.carvers.iter().enumerate() {
                let carver = REGISTRY
                    .configured_carvers
                    .by_key(carver_key)
                    .unwrap_or_else(|| {
                        panic!(
                            "biome {} references unknown configured carver {carver_key}",
                            source.biome.key
                        )
                    });
                random.set_large_feature_seed(
                    seed.wrapping_add(index as i64),
                    source.pos.0.x,
                    source.pos.0.y,
                );
                if random.next_f32() > carver.base().probability {
                    continue;
                }
                match &carver.kind {
                    ConfiguredCarverKind::Cave(config) => {
                        self.carve_cave(
                            &config,
                            cave::CaveKind::Overworld,
                            source.pos,
                            &mut random,
                        );
                    }
                    ConfiguredCarverKind::NetherCave(config) => {
                        self.carve_cave(&config, cave::CaveKind::Nether, source.pos, &mut random);
                    }
                    ConfiguredCarverKind::Canyon(config) => {
                        self.carve_canyon(&config, source.pos, &mut random);
                    }
                }
            }
        }
    }

    #[expect(clippy::too_many_arguments, reason = "mirrors Vanilla WorldCarver")]
    pub(crate) fn carve_ellipsoid<S: CarveSkipChecker>(
        &mut self,
        params: &CarveParams<'_>,
        x: f64,
        y: f64,
        z: f64,
        horizontal_radius: f64,
        vertical_radius: f64,
        mut skip_checker: S,
    ) -> bool {
        let middle_x = f64::from(self.chunk_min_x) + 8.0;
        let middle_z = f64::from(self.chunk_min_z) + 8.0;
        let max_delta = 16.0 + horizontal_radius * 2.0;
        if (x - middle_x).abs() > max_delta || (z - middle_z).abs() > max_delta {
            return false;
        }
        let min_x = ((x - horizontal_radius).floor() as i32 - self.chunk_min_x - 1).max(0);
        let max_x = ((x + horizontal_radius).floor() as i32 - self.chunk_min_x).min(15);
        let min_y = ((y - vertical_radius).floor() as i32 - 1).max(self.context.min_y + 1);
        let max_y = ((y + vertical_radius).floor() as i32 + 1)
            .min(self.context.min_y + self.context.gen_depth - 8);
        let min_z = ((z - horizontal_radius).floor() as i32 - self.chunk_min_z - 1).max(0);
        let max_z = ((z + horizontal_radius).floor() as i32 - self.chunk_min_z).min(15);
        let mut carved = false;
        for local_x in min_x..=max_x {
            let world_x = self.chunk_min_x + local_x;
            let xd = (f64::from(world_x) + 0.5 - x) / horizontal_radius;
            for local_z in min_z..=max_z {
                let world_z = self.chunk_min_z + local_z;
                let zd = (f64::from(world_z) + 0.5 - z) / horizontal_radius;
                if xd * xd + zd * zd >= 1.0 {
                    continue;
                }
                let mut has_grass = false;
                for world_y in (min_y + 1..=max_y).rev() {
                    let yd = (f64::from(world_y) - 0.5 - y) / vertical_radius;
                    if skip_checker.should_skip(xd, yd, zd, world_y)
                        || !self.mask.set_if_unset(local_x, world_y, local_z)
                    {
                        continue;
                    }
                    carved |= self.carve_block(params, world_x, world_y, world_z, &mut has_grass);
                }
            }
        }
        carved
    }

    fn carve_block(
        &mut self,
        params: &CarveParams<'_>,
        world_x: i32,
        world_y: i32,
        world_z: i32,
        has_grass: &mut bool,
    ) -> bool {
        let pos = BlockPos::new(world_x, world_y, world_z);
        let existing = self.host.block_state(pos);
        if existing == self.ids.grass_block || existing == self.ids.mycelium {
            *has_grass = true;
        }
        if !self.can_replace(params, existing) {
            return false;
        }
        let state = match self.get_carve_state(params, world_x, world_y, world_z) {
            CarveState::Place(state) => state,
            CarveState::Skip => return false,
        };
        self.host.set_block_state(pos, state);
        if params.style == CarverStyle::Overworld
            && self.context.aquifer.should_schedule_fluid_update()
            && state.has_fluid()
        {
            self.host.mark_pos_for_postprocessing(pos);
        }
        if params.style == CarverStyle::Overworld && *has_grass {
            let below = BlockPos::new(world_x, world_y - 1, world_z);
            if self.host.block_state(below) == self.ids.dirt {
                let biome_id = (self.biome_getter)(below);
                if let Some(state) = self.context.top_material(
                    biome_id,
                    world_x,
                    world_y - 1,
                    world_z,
                    self.steep_material_condition(world_x, world_z),
                    !self.ids.is_air_like(state),
                ) {
                    self.host.set_block_state(below, state);
                    if state.has_fluid() {
                        self.host.mark_pos_for_postprocessing(below);
                    }
                }
            }
        }
        true
    }

    fn can_replace(&self, params: &CarveParams<'_>, state: BlockStateId) -> bool {
        if state.is_air() {
            return false;
        }
        params
            .replaceable_states
            .is_some_and(|states| states.contains(state))
            || REGISTRY
                .blocks
                .by_state_id(state)
                .is_some_and(|block| block.has_tag(params.replaceable_tag))
    }

    fn steep_material_condition(&self, world_x: i32, world_z: i32) -> bool {
        let local_x = (world_x & 15) as usize;
        let local_z = (world_z & 15) as usize;
        let north = self
            .host
            .world_surface_wg_first_available(local_x, local_z.saturating_sub(1));
        let south = self
            .host
            .world_surface_wg_first_available(local_x, (local_z + 1).min(15));
        if south >= north + 4 {
            return true;
        }
        let west = self
            .host
            .world_surface_wg_first_available(local_x.saturating_sub(1), local_z);
        let east = self
            .host
            .world_surface_wg_first_available((local_x + 1).min(15), local_z);
        west >= east + 4
    }

    fn get_carve_state(&mut self, params: &CarveParams<'_>, x: i32, y: i32, z: i32) -> CarveState {
        match params.style {
            CarverStyle::Overworld => {
                if y <= params.lava_level_y {
                    return CarveState::Place(self.ids.lava);
                }
                match self
                    .context
                    .aquifer
                    .compute_substance(self.noises, x, y, z, 0.0)
                {
                    AquiferResult::Solid => CarveState::Skip,
                    AquiferResult::Fluid(state) => CarveState::Place(state),
                    AquiferResult::Air => CarveState::Place(self.ids.air),
                }
            }
            CarverStyle::Nether => {
                if y <= self.context.min_y + 31 {
                    CarveState::Place(self.ids.lava)
                } else {
                    CarveState::Place(self.ids.cave_air)
                }
            }
        }
    }
}
