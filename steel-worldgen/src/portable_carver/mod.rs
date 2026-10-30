//! World-carving: runtime types for running configured carvers during the
//! `CARVERS` chunk stage.
//!
//! Mirrors vanilla's `net.minecraft.world.level.levelgen.carver` package. The
//! [`CarvingContext`] bundles the dimension-level state; a [`CarveRun`]
//! bundles the per-chunk references that every carver method threads
//! through.

use std::{cell::Cell, sync::LazyLock};

use crate::density::DimensionNoises;
use crate::surface::{SurfaceConditionNoiseCache, SurfaceRuleContext};
use glam::IVec3;
use steel_math::trig;
use steel_registry::biome::BiomeRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::ChunkPos;
use steel_utils::{BlockPos, BlockStateId, Identifier};

use crate::noise::OreVeinifier;
use crate::noise::{Aquifer, AquiferResult};
use crate::surface::{MaterialSurfaceGrid, SurfaceBlockAccess, SurfaceSystem};
use steel_registry::carver::ConfiguredCarverKind;
use steel_utils::random::{Random, legacy_random::LegacyRandom};

pub mod canyon;
pub mod cave;
mod mask;

pub use mask::CarvingMask;

/// Portable block host retaining fluid postprocessing positions for later stages.
pub trait CarverBlockAccess: SurfaceBlockAccess {
    /// Records a fluid update that native chunk generation would schedule.
    fn mark_pos_for_postprocessing(&mut self, pos: BlockPos);

    /// Reads a world position within this chunk's generation bounds.
    fn read_carver_block(&mut self, pos: BlockPos) -> BlockStateId {
        match self.get_relative_block(
            (pos.0.x - self.chunk_min_x()) as usize,
            (pos.0.y - self.min_y()) as usize,
            (pos.0.z - self.chunk_min_z()) as usize,
        ) {
            Some(state) => state,
            None => panic!("carver position outside generation bounds"),
        }
    }

    /// Writes a world position within this chunk's generation bounds.
    fn write_carver_block(&mut self, pos: BlockPos, state: BlockStateId) {
        self.set_relative_block(
            (pos.0.x - self.chunk_min_x()) as usize,
            (pos.0.y - self.min_y()) as usize,
            (pos.0.z - self.chunk_min_z()) as usize,
            state,
        );
    }
}

/// A source chunk's position and carver-list biome — the unit of work in the
/// 17×17 `apply_carvers` loop. Each entry feeds one or more carver
/// invocations from the biome's `carvers` list.
#[derive(Debug, Clone, Copy)]
pub struct SourceChunk {
    /// Chunk position of the carver's origin.
    pub pos: ChunkPos,
    /// Biome providing the source chunk's carver list.
    pub biome: BiomeRef,
}

/// Runtime context for a single `apply_carvers` invocation on one chunk.
///
/// Supplies the 26.3 material-rule and aquifer inputs used by applyCarvingMask.
pub struct CarvingContext<'a, N: DimensionNoises> {
    /// Density functions for material-rule evaluation.
    pub noises: &'a N,
    /// Dimension minimum Y (inclusive).
    pub min_y: i32,
    /// Dimension vertical extent in blocks (`max_y = min_y + gen_depth - 1`).
    pub gen_depth: i32,
    /// Dimension sea level used by relative vertical anchors.
    pub sea_level: i32,
    /// Surface system (biome-specific surface noise + clay bands).
    pub surface_system: &'a SurfaceSystem,
    /// Aquifer for this chunk, retained from Noise or reconstructed after a disk reload.
    pub aquifer: &'a mut Aquifer<N>,
    /// Float chunkSurfaceLevel samples, shared with the Material stage.
    pub material_surface: &'a MaterialSurfaceGrid,
    /// Chunk NW block X for retained ore buffers.
    pub chunk_min_x: i32,
    /// Chunk NW block Z for retained ore buffers.
    pub chunk_min_z: i32,
    /// Density/richness values prefilled for material ore rules during noise fill.
    pub material_ore_vein_values: &'a [f32],
    /// Positional random source and block states for material ore rules.
    pub ore_veinifier: Option<&'a OreVeinifier>,
    /// Material-rule column cache for direct filler-gap sampling.
    pub material_cache: N::ColumnCache,
}

impl<N: DimensionNoises> CarvingContext<'_, N> {
    /// Floors the source-defined float chunkSurfaceLevel at the column.
    #[must_use]
    pub fn min_surface_level(&self, block_x: i32, block_z: i32) -> i32 {
        self.material_surface
            .at((block_x & 15) as usize, (block_z & 15) as usize)
            .floor() as i32
    }

    /// Runs surface rules at a single position to pick the "top material"
    /// block (grass / podzol / mycelium / sand / ...). Called by the carver
    /// when it uncovers dirt beneath a grass block so the exposed surface gets
    /// rewritten to the biome-appropriate surface block.
    ///
    /// Mirrors 26.3 MaterialSystem.topMaterial. Vanilla hardcodes
    /// `stone_depth_above = stone_depth_below = 1` here, and the water height
    /// depends on whether the carved block was replaced with a fluid.
    #[must_use]
    pub fn top_material(
        &mut self,
        biome_id: u16,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        steep: bool,
        under_fluid: bool,
    ) -> Option<BlockStateId> {
        let value_count = N::material_ore_vein_value_count();
        let mut ore_vein_results = Vec::new();
        ore_vein_results.resize(value_count / 2, None);
        if let Some(ore_veinifier) = self.ore_veinifier {
            let local_x = (block_x - self.chunk_min_x) as usize;
            let local_z = (block_z - self.chunk_min_z) as usize;
            let relative_y = (block_y - self.min_y) as usize;
            let offset = (relative_y * 16 * 16 + local_z * 16 + local_x) * value_count;
            if let Some(values) = self
                .material_ore_vein_values
                .get(offset..offset + value_count)
            {
                N::fill_prefilled_material_ore_vein_results(
                    self.noises,
                    &mut self.material_cache,
                    ore_veinifier,
                    values,
                    block_x,
                    block_y,
                    block_z,
                    &mut ore_vein_results,
                );
            }
        }

        // Surface noise inputs (same helpers build_surface uses per column).
        let surface_depth = self.surface_system.get_surface_depth(block_x, block_z);
        let surface_secondary = self.surface_system.get_surface_secondary(block_x, block_z);
        let min_surface_level = self.min_surface_level(block_x, block_z) + surface_depth - 8;

        let water_height = if under_fluid { block_y + 1 } else { i32::MIN };
        let condition_noise_values: Vec<Cell<f64>> = N::surface_noise_ids()
            .iter()
            .map(|_| Cell::new(0.0))
            .collect();
        let condition_noise_initialized: Vec<Cell<bool>> = N::surface_noise_ids()
            .iter()
            .map(|_| Cell::new(false))
            .collect();
        let condition_noise_cache =
            SurfaceConditionNoiseCache::new(&condition_noise_values, &condition_noise_initialized);

        let mut ctx = SurfaceRuleContext::new(
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
            &condition_noise_cache,
            N::surface_rule_block_states(),
        );

        ctx = ctx.with_ore_vein_results(&ore_vein_results);
        N::try_apply_surface_rule(&mut ctx)
    }
}

/// Vanilla's global `#minecraft:uncarvable` block tag
/// (`applyCarvingMask`'s `!blockState.is(BlockTags.UNCARVABLE)` check).
/// Every carver shares this one tag now, not a per-carver one.
pub const UNCARVABLE_TAG: Identifier = Identifier::vanilla_static("uncarvable");

/// Per-state membership cache for [`UNCARVABLE_TAG`], resolved once into a
/// state-id table to avoid repeated tag lookups in the carve loop.
#[derive(Debug)]
pub struct CarverReplaceableStates {
    states: Box<[bool]>,
}

impl CarverReplaceableStates {
    fn build() -> Self {
        let states = REGISTRY
            .blocks
            .state_to_block_lookup
            .iter()
            .map(|&block| block.has_tag(&UNCARVABLE_TAG))
            .collect();
        Self { states }
    }

    /// Returns whether `state` belongs to `#minecraft:uncarvable`.
    #[inline]
    #[must_use]
    pub fn contains(&self, state: BlockStateId) -> bool {
        self.states.get(state.0 as usize).copied().unwrap_or(false)
    }
}

static UNCARVABLE_STATES: LazyLock<CarverReplaceableStates> =
    LazyLock::new(CarverReplaceableStates::build);

/// A block may be carved unless it's in `#minecraft:uncarvable`. No separate
/// air check — air isn't in the tag, so it's replaced like anything else.
#[must_use]
pub fn can_replace_block(state: BlockStateId) -> bool {
    !UNCARVABLE_STATES.contains(state)
}

/// Well-known block state IDs a carver needs. Cached once per `apply_carvers`
/// call so the carver loop doesn't hit the registry in its hot path.
#[derive(Debug, Clone, Copy)]
pub struct CarverBlockIds {
    /// `minecraft:air`.
    pub air: BlockStateId,
    /// `minecraft:cave_air`.
    pub cave_air: BlockStateId,
    /// `minecraft:lava` (fluid block state).
    pub lava: BlockStateId,
    /// `minecraft:grass_block` default state.
    pub grass_block: BlockStateId,
    /// `minecraft:mycelium` default state.
    pub mycelium: BlockStateId,
    /// `minecraft:dirt` default state.
    pub dirt: BlockStateId,
}

impl CarverBlockIds {
    /// Looks up the well-known block state IDs once from the registry.
    #[must_use]
    pub fn load() -> Self {
        static IDS: LazyLock<CarverBlockIds> = LazyLock::new(CarverBlockIds::load_uncached);
        *IDS
    }

    fn load_uncached() -> Self {
        use steel_registry::{REGISTRY, vanilla_blocks};
        Self {
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
        }
    }

    /// Returns whether the given state is one of the air variants this
    /// carver uses (i.e. not a fluid). Used by the top-material flow to
    /// decide `under_fluid`.
    #[must_use]
    pub const fn is_air_like(&self, state: BlockStateId) -> bool {
        // SAFETY: BlockStateId is a `#[repr(transparent)]` wrapper around u16.
        // Hand-written equality keeps this function `const`.
        state.0 == self.air.0 || state.0 == self.cave_air.0
    }
}

/// Predicate called inside the carver's Y scan to decide whether a block is
/// outside the carved shape for a given ellipsoid (cave floor cutoff, canyon
/// width-by-height, etc). Matches vanilla's `WorldCarver.CarveSkipChecker`.
pub trait CarveSkipChecker {
    /// `xd`, `yd`, `zd` are the ellipsoid-normalized offsets from the carver
    /// origin to this block's center (see `CarveRun::carve_ellipsoid`);
    /// `world_y` is the absolute Y coordinate of the current block.
    fn should_skip(&mut self, xd: f64, yd: f64, zd: f64, world_y: i32) -> bool;
}

impl<F: FnMut(f64, f64, f64, i32) -> bool> CarveSkipChecker for F {
    fn should_skip(&mut self, xd: f64, yd: f64, zd: f64, world_y: i32) -> bool {
        self(xd, yd, zd, world_y)
    }
}

/// Vanilla cave/canyon tunnel radius calculation.
#[inline]
#[must_use]
pub(super) fn horizontal_tunnel_radius(progress_arg: f32, thickness: f32) -> f64 {
    let radius_offset = trig::sin(f64::from(progress_arg)) * thickness;
    1.5 + f64::from(radius_offset)
}

/// The references every carver method needs. Bundled so `carve_ellipsoid`,
/// `carve_block`, `create_tunnel`, `create_room`, `carve_cave`,
/// `carve_canyon`, and `do_carve` can all be `&mut self` methods instead of
/// repeating the same 7–8 arguments.
pub struct CarveRun<'a, 'b, N, F>
where
    N: DimensionNoises,
    F: FnMut(BlockPos) -> u16,
{
    /// Dimension-level context (aquifer, surface system, bounds, psl).
    pub ctx: &'a mut CarvingContext<'b, N>,
    /// Noise generators for this dimension.
    pub noises: &'a N,
    /// Chunk being carved into.
    pub chunk: &'a mut dyn CarverBlockAccess,
    /// Chunk NW block X (cached; `ctx.chunk_min_x` mirrors this).
    pub chunk_min_x: i32,
    /// Chunk NW block Z (cached; `ctx.chunk_min_z` mirrors this).
    pub chunk_min_z: i32,
    /// Biome lookup (vanilla `BiomeManager.getBiome`-style, fuzzed).
    pub biome_getter: &'a mut F,
    /// Carving mask for this Terrain operation.
    pub mask: &'a mut CarvingMask,
    /// Block IDs cached once per carver session.
    pub ids: CarverBlockIds,
}

impl<N, F> CarveRun<'_, '_, N, F>
where
    N: DimensionNoises,
    F: FnMut(BlockPos) -> u16,
{
    /// Carve every block inside the given ellipsoid that falls in this chunk.
    /// Mirrors vanilla's `WorldCarver.carveEllipsoid`.
    ///
    /// Returns `true` if at least one block was carved.
    #[expect(
        clippy::similar_names,
        reason = "min_x_idx / min_z_idx / max_x_idx / max_z_idx mirror vanilla"
    )]
    pub fn carve_ellipsoid<S: CarveSkipChecker>(
        &mut self,
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

        let min_x_idx = ((x - horizontal_radius).floor() as i32 - self.chunk_min_x - 1).max(0);
        let max_x_idx = ((x + horizontal_radius).floor() as i32 - self.chunk_min_x).min(15);
        let min_y = ((y - vertical_radius).floor() as i32 - 1).max(self.ctx.min_y + 1);
        // Vanilla: `chunk.isUpgrading() ? 0 : 7`. No chunk upgrade path yet,
        // so always 7 — matches extractor config.
        let protected_blocks_on_top = 7;
        let max_y = ((y + vertical_radius).floor() as i32 + 1)
            .min(self.ctx.min_y + self.ctx.gen_depth - 1 - protected_blocks_on_top);
        let min_z_idx = ((z - horizontal_radius).floor() as i32 - self.chunk_min_z - 1).max(0);
        let max_z_idx = ((z + horizontal_radius).floor() as i32 - self.chunk_min_z).min(15);

        let mut carved = false;

        for x_idx in min_x_idx..=max_x_idx {
            let world_x = self.chunk_min_x + x_idx;
            let xd = (f64::from(world_x) + 0.5 - x) / horizontal_radius;

            for z_idx in min_z_idx..=max_z_idx {
                let world_z = self.chunk_min_z + z_idx;
                let zd = (f64::from(world_z) + 0.5 - z) / horizontal_radius;
                if xd * xd + zd * zd >= 1.0 {
                    continue;
                }

                // Scan top-down; range is exclusive of min_y (matches vanilla's
                // `worldY > minY`).
                for world_y in (min_y + 1..=max_y).rev() {
                    let yd = (f64::from(world_y) - 0.5 - y) / vertical_radius;
                    if skip_checker.should_skip(xd, yd, zd, world_y) {
                        continue;
                    }
                    self.mask.set(x_idx, world_y, z_idx);
                    carved = true;
                }
            }
        }

        carved
    }

    /// Applies the shared output mask after every carver has marked its geometry.
    pub fn apply_carving_mask(&mut self) {
        self.mask.visit(|local_x, local_z, bottom_y, top_y| {
            let world_x = self.chunk_min_x + local_x;
            let world_z = self.chunk_min_z + local_z;
            let mut has_grass = false;

            for world_y in (bottom_y..=top_y).rev() {
                let pos = BlockPos::new(world_x, world_y, world_z);
                let existing = self.chunk.read_carver_block(pos);
                if !can_replace_block(existing) {
                    continue;
                }
                if existing.get_block() == self.ids.grass_block.get_block()
                    || existing.get_block() == self.ids.mycelium.get_block()
                {
                    has_grass = true;
                }

                let state = match self.ctx.aquifer.compute_substance(
                    self.noises,
                    world_x,
                    world_y,
                    world_z,
                    0.0,
                ) {
                    AquiferResult::Solid => continue,
                    AquiferResult::Fluid(state) => state,
                    AquiferResult::Air => self.ids.air,
                };
                self.chunk.write_carver_block(pos, state);
                if self.ctx.aquifer.should_schedule_fluid_update() && state.has_fluid() {
                    self.chunk.mark_pos_for_postprocessing(pos);
                }

                if !has_grass {
                    continue;
                }
                let below_pos = BlockPos::new(world_x, world_y - 1, world_z);
                if self.chunk.read_carver_block(below_pos).get_block() != self.ids.dirt.get_block()
                {
                    continue;
                }
                let steep = Self::steep_material_condition(self.chunk, world_x, world_z);
                let biome_id =
                    (self.biome_getter)(BlockPos(IVec3::new(world_x, world_y - 1, world_z)));
                if let Some(top) = self.ctx.top_material(
                    biome_id,
                    world_x,
                    world_y - 1,
                    world_z,
                    steep,
                    state.has_fluid(),
                ) {
                    self.chunk.write_carver_block(below_pos, top);
                    if top.has_fluid() {
                        self.chunk.mark_pos_for_postprocessing(below_pos);
                    }
                }
            }
        });
    }

    fn steep_material_condition(
        chunk: &mut dyn CarverBlockAccess,
        world_x: i32,
        world_z: i32,
    ) -> bool {
        let x = (world_x & 15) as usize;
        let z = (world_z & 15) as usize;
        let north = chunk.world_surface_height_at(x, z.saturating_sub(1));
        let south = chunk.world_surface_height_at(x, (z + 1).min(15));
        let west = chunk.world_surface_height_at(x.saturating_sub(1), z);
        let east = chunk.world_surface_height_at((x + 1).min(15), z);
        south >= north + 4 || west >= east + 4
    }

    /// Source order is X then Z, with one random reseed per carver index.
    pub fn run_all(&mut self, sources: &[SourceChunk], seed: i64) -> Result<(), &'static str> {
        let mut random = LegacyRandom::from_seed(0);
        for source in sources {
            for (index, key) in source.biome.carvers.iter().enumerate() {
                let carver = REGISTRY
                    .configured_carvers
                    .by_key(key)
                    .ok_or("biome references unavailable configured carver")?;
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
                        self.carve_cave(config, source.pos, &mut random)
                    }
                    ConfiguredCarverKind::Canyon(config) => {
                        self.carve_canyon(config, source.pos, &mut random)
                    }
                }
            }
        }
        Ok(())
    }
}

/// Vanilla's `WorldCarver.canReach` — prunes carver steps that can't touch
/// any block in the given chunk (used by cave/canyon tunnel loops before
/// carving an ellipsoid).
#[must_use]
pub fn can_reach(
    chunk_min_x: i32,
    chunk_min_z: i32,
    x: f64,
    z: f64,
    current_step: i32,
    total_steps: i32,
    thickness: f32,
) -> bool {
    let x_mid = f64::from(chunk_min_x) + 8.0;
    let z_mid = f64::from(chunk_min_z) + 8.0;
    let xd = x - x_mid;
    let zd = z - z_mid;
    let remaining = f64::from(total_steps - current_step);
    let rr = f64::from(thickness + 2.0_f32 + 16.0_f32);
    xd * xd + zd * zd - remaining * remaining <= rr * rr
}
