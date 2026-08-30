//! Portable execution of Vanilla's chunk Surface stage.
//!
//! The stage operates only through block and biome host traits so native chunks,
//! an in-memory WebAssembly chunk, and future deterministic adapters share the
//! exact same column scan and generated surface-rule evaluation.

use std::{cell::Cell, marker::PhantomData};

use rustc_hash::FxHashSet;
use steel_math::lerp2;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::{RegistryEntry, vanilla_biomes};
use steel_utils::{BlockStateId, Identifier};

use crate::density::DimensionNoises;
use crate::surface::{
    SurfaceBiomeProvider, SurfaceConditionNoiseCache, SurfaceRuleContext, SurfaceSystem,
};

/// Four preliminary-surface samples at a chunk's block corners.
///
/// The samples correspond to local block coordinates (0, 0), (16, 0),
/// (0, 16), and (16, 16). Surface-rule preliminary-surface conditions
/// bilinearly interpolate these values exactly as Vanilla does.
#[derive(Debug, Clone, Copy)]
pub struct PreliminarySurfaceCorners {
    /// Corner at chunk minimum X/Z.
    pub nw: i32,
    /// Corner at chunk minimum X plus 16.
    pub ne: i32,
    /// Corner at chunk minimum Z plus 16.
    pub sw: i32,
    /// Corner at chunk minimum X/Z plus 16.
    pub se: i32,
}

/// The Vanilla surface extensions a biome source can require.
#[derive(Debug, Clone, Copy, Default)]
pub struct SurfaceExtensions {
    /// Whether the source can produce eroded badlands.
    pub eroded_badlands: bool,
    /// Whether the source can produce frozen-ocean biomes.
    pub frozen_ocean: bool,
}

impl SurfaceExtensions {
    /// Resolves extension requirements from a source's possible biome keys.
    #[must_use]
    pub fn from_possible_biomes(possible_biomes: &FxHashSet<Identifier>) -> Self {
        Self {
            eroded_badlands: possible_biomes.contains(&vanilla_biomes::ERODED_BADLANDS.key),
            frozen_ocean: possible_biomes.contains(&vanilla_biomes::FROZEN_OCEAN.key)
                || possible_biomes.contains(&vanilla_biomes::DEEP_FROZEN_OCEAN.key),
        }
    }

    /// Whether an extension needs the biome at the column's current surface.
    #[must_use]
    pub const fn needs_surface_biome(self) -> bool {
        self.eroded_badlands || self.frozen_ocean
    }
}

/// Mutable 16 by 16 block-column host for the Surface stage.
///
/// Coordinates supplied to column methods are local to the host chunk;
/// relative Y is offset from min_y. Hosts preserve direct write semantics for
/// the world-surface heightmap because the stage reads it to find a column
/// start and to evaluate the asymmetric steep condition.
pub trait SurfaceBlockAccess {
    /// Dimension minimum block Y.
    fn min_y(&self) -> i32;

    /// World X coordinate of local column zero.
    fn chunk_min_x(&self) -> i32;

    /// World Z coordinate of local column zero.
    fn chunk_min_z(&self) -> i32;

    /// Ensures world-surface height information is ready for reads.
    fn prime_world_surface_heightmap(&mut self);

    /// Returns the world-surface first-available Y at a local X/Z column.
    fn world_surface_height_at(&mut self, local_x: usize, local_z: usize) -> i32;

    /// Copies a complete local block column in increasing relative-Y order.
    fn read_column_into(&mut self, local_x: usize, local_z: usize, output: &mut Vec<BlockStateId>);

    /// Writes several states in one local column.
    fn write_column(&mut self, local_x: usize, local_z: usize, blocks: &[(usize, BlockStateId)]);

    /// Reads one block using local X/Z and relative Y coordinates.
    fn get_relative_block(
        &mut self,
        local_x: usize,
        relative_y: usize,
        local_z: usize,
    ) -> Option<BlockStateId>;

    /// Writes one block using local X/Z and relative Y coordinates.
    fn set_relative_block(
        &mut self,
        local_x: usize,
        relative_y: usize,
        local_z: usize,
        state: BlockStateId,
    );
}

/// Supplies un-fuzzed biome IDs at quart coordinates for Surface lookups.
///
/// The reusable stage owns Vanilla's BiomeManager fuzzing. Implementations
/// provide the same underlying palette values a normal chunk generator would
/// expose through its noise-biome lookup.
pub trait SurfaceBiomeAccess {
    /// Returns the biome registry ID at one un-fuzzed quart position.
    fn biome_id_at_quart(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> u16;
}

/// Dimension-specialized Surface-stage runner.
///
/// Generated surface rules are associated with N; all mutable world state is
/// supplied by the portable host traits rather than the native server crate.
pub struct SurfaceStage<'a, N: DimensionNoises> {
    system: &'a SurfaceSystem,
    default_block_id: BlockStateId,
    biome_zoom_seed: i64,
    extensions: SurfaceExtensions,
    _noises: PhantomData<fn() -> N>,
}

impl<'a, N: DimensionNoises> SurfaceStage<'a, N> {
    /// Creates one reusable stage runner for a dimension generator.
    #[must_use]
    pub const fn new(
        system: &'a SurfaceSystem,
        default_block_id: BlockStateId,
        biome_zoom_seed: i64,
        extensions: SurfaceExtensions,
    ) -> Self {
        Self {
            system,
            default_block_id,
            biome_zoom_seed,
            extensions,
            _noises: PhantomData,
        }
    }

    /// Whether this Surface execution needs preliminary-surface corner inputs.
    #[must_use]
    pub fn needs_preliminary_surface(&self) -> bool {
        N::surface_rule_uses_preliminary_surface() || self.extensions.frozen_ocean
    }

    /// Whether this Surface execution reads a fuzzed biome value.
    #[must_use]
    pub fn needs_biomes(&self) -> bool {
        N::surface_rule_uses_biome() || self.extensions.needs_surface_biome()
    }

    /// Runs the exact Surface stage for one 16 by 16 chunk host.
    #[expect(
        clippy::too_many_lines,
        reason = "the single column scan mirrors Vanilla SurfaceSystem behavior"
    )]
    pub fn build_surface<A: SurfaceBlockAccess, B: SurfaceBiomeAccess>(
        &self,
        blocks: &mut A,
        biomes: &mut B,
        preliminary_surface_corners: Option<PreliminarySurfaceCorners>,
    ) {
        let min_y = blocks.min_y();
        let chunk_min_x = blocks.chunk_min_x();
        let chunk_min_z = blocks.chunk_min_z();
        let surface_rule_block_states = N::surface_rule_block_states();
        let surface_rule_uses_biome = N::surface_rule_uses_biome();
        let surface_rule_uses_preliminary_surface = N::surface_rule_uses_preliminary_surface();
        let surface_rule_uses_surface_secondary = N::surface_rule_uses_surface_secondary();
        let surface_rule_uses_steep = N::surface_rule_uses_steep();
        let lazy_surface_rule_biome =
            surface_rule_uses_biome && surface_rule_uses_preliminary_surface;
        let surface_needs_min_surface_level = self.needs_preliminary_surface();
        let surface_needs_biomes = self.needs_biomes();

        if surface_needs_min_surface_level && preliminary_surface_corners.is_none() {
            panic!("surface stage requires preliminary-surface corner values");
        }
        let preliminary_surface_corners = surface_needs_min_surface_level
            .then_some(preliminary_surface_corners)
            .flatten();

        blocks.prime_world_surface_heightmap();

        let eroded_badlands_id = (*vanilla_biomes::ERODED_BADLANDS).id() as u16;
        let frozen_ocean_id = (*vanilla_biomes::FROZEN_OCEAN).id() as u16;
        let deep_frozen_ocean_id = (*vanilla_biomes::DEEP_FROZEN_OCEAN).id() as u16;

        let mut pending_writes: Vec<(usize, BlockStateId)> = Vec::new();
        let mut column_buf: Vec<BlockStateId> = Vec::new();
        let condition_noise_values = N::surface_noise_ids()
            .iter()
            .map(|_| Cell::new(0.0))
            .collect::<Vec<_>>();
        let condition_noise_initialized = N::surface_noise_ids()
            .iter()
            .map(|_| Cell::new(false))
            .collect::<Vec<_>>();
        let condition_noise_cache =
            SurfaceConditionNoiseCache::new(&condition_noise_values, &condition_noise_initialized);

        for local_x in 0..16usize {
            for local_z in 0..16usize {
                let block_x = chunk_min_x + local_x as i32;
                let block_z = chunk_min_z + local_z as i32;
                let mut start_height = blocks.world_surface_height_at(local_x, local_z);

                let mut biome_col = surface_needs_biomes.then(|| {
                    FuzzedBiomeColumn::new(self.biome_zoom_seed, block_x, block_z, biomes)
                });

                let surface_biome_id = if self.extensions.needs_surface_biome() {
                    biome_col
                        .as_mut()
                        .map(|biome_col| biome_col.get(start_height))
                } else {
                    None
                };
                if self.extensions.eroded_badlands && surface_biome_id == Some(eroded_badlands_id) {
                    start_height = self.system.eroded_badlands_extension(
                        blocks,
                        local_x,
                        local_z,
                        block_x,
                        block_z,
                        start_height,
                        min_y,
                    );
                }

                blocks.read_column_into(local_x, local_z, &mut column_buf);

                let surface_depth = self.system.get_surface_depth(block_x, block_z);
                let surface_secondary = if surface_rule_uses_surface_secondary {
                    self.system.get_surface_secondary(block_x, block_z)
                } else {
                    0.0
                };
                condition_noise_cache.reset();

                let min_surface_level = if let Some(corners) = preliminary_surface_corners {
                    let t_x = f64::from(local_x as u8) / 16.0;
                    let t_z = f64::from(local_z as u8) / 16.0;
                    let interp = lerp2(
                        t_x,
                        t_z,
                        f64::from(corners.nw),
                        f64::from(corners.ne),
                        f64::from(corners.sw),
                        f64::from(corners.se),
                    );
                    interp.floor() as i32 + surface_depth - 8
                } else {
                    0
                };

                // Vanilla's steep predicate is asymmetric: south >= north + 4
                // or west >= east + 4. Do not replace this with an absolute
                // difference.
                let steep = surface_rule_uses_steep && {
                    let z_north = local_z.saturating_sub(1);
                    let z_south = (local_z + 1).min(15);
                    let h_north = blocks.world_surface_height_at(local_x, z_north) - 1;
                    let h_south = blocks.world_surface_height_at(local_x, z_south) - 1;
                    if h_south >= h_north + 4 {
                        true
                    } else {
                        let x_west = local_x.saturating_sub(1);
                        let x_east = (local_x + 1).min(15);
                        let h_west = blocks.world_surface_height_at(x_west, local_z) - 1;
                        let h_east = blocks.world_surface_height_at(x_east, local_z) - 1;
                        h_west >= h_east + 4
                    }
                };

                let mut stone_depth_above: i32 = 0;
                let mut water_height: i32 = i32::MIN;
                let mut next_ceiling_stone_y: i32 = i32::MAX;
                pending_writes.clear();

                for y in (min_y..=start_height).rev() {
                    let relative_y = (y - min_y) as usize;
                    let state = column_buf[relative_y];

                    if state.is_air() {
                        stone_depth_above = 0;
                        water_height = i32::MIN;
                        continue;
                    }

                    if state.get_block().config.liquid {
                        if water_height == i32::MIN {
                            water_height = y + 1;
                        }
                        continue;
                    }

                    if next_ceiling_stone_y >= y {
                        next_ceiling_stone_y = i32::MIN;
                        for la_y in (min_y - 1..y).rev() {
                            if la_y < min_y {
                                next_ceiling_stone_y = la_y + 1;
                                break;
                            }
                            let la_rel = (la_y - min_y) as usize;
                            let la_state = column_buf[la_rel];
                            if la_state.is_air() || la_state.get_block().config.liquid {
                                next_ceiling_stone_y = la_y + 1;
                                break;
                            }
                        }
                    }

                    stone_depth_above += 1;
                    let stone_depth_below = y - next_ceiling_stone_y + 1;

                    if state == self.default_block_id {
                        let eager_biome_id = if surface_rule_uses_biome && !lazy_surface_rule_biome
                        {
                            biome_col.as_mut().map(|biome_col| biome_col.get(y))
                        } else {
                            None
                        };
                        let biome_provider = if lazy_surface_rule_biome {
                            biome_col
                                .as_mut()
                                .map(|biome_col| biome_col as &mut dyn SurfaceBiomeProvider)
                        } else {
                            None
                        };

                        let mut ctx = SurfaceRuleContext::new(
                            block_x,
                            block_z,
                            surface_depth,
                            surface_secondary,
                            min_surface_level,
                            steep,
                            y,
                            stone_depth_above,
                            stone_depth_below,
                            water_height,
                            eager_biome_id,
                            biome_provider,
                            self.system,
                            &condition_noise_cache,
                            surface_rule_block_states,
                        );
                        if let Some(new_block) = N::try_apply_surface_rule(&mut ctx) {
                            pending_writes.push((relative_y, new_block));
                        }
                    }
                }

                if !pending_writes.is_empty() {
                    blocks.write_column(local_x, local_z, &pending_writes);
                    for &(relative_y, state) in &pending_writes {
                        column_buf[relative_y] = state;
                    }
                }

                if self.extensions.frozen_ocean
                    && let Some(surface_biome_id) = surface_biome_id
                        .filter(|id| *id == frozen_ocean_id || *id == deep_frozen_ocean_id)
                {
                    pending_writes.clear();
                    self.system.collect_frozen_ocean_extension_writes(
                        surface_biome_id,
                        block_x,
                        block_z,
                        start_height,
                        min_surface_level,
                        min_y,
                        &column_buf,
                        &mut pending_writes,
                    );
                    if !pending_writes.is_empty() {
                        blocks.write_column(local_x, local_z, &pending_writes);
                    }
                }
            }
        }
    }
}

/// Column-local cache for fuzzed biome lookups.
///
/// This is Vanilla's BiomeManager choice between the eight neighboring quart
/// cells. X/Z work is immutable within a block column; only the two Y
/// candidate groups change while the Surface scan descends.
struct FuzzedBiomeColumn<'a, B: SurfaceBiomeAccess + ?Sized> {
    biome_access: &'a mut B,
    biome_zoom_seed: i64,
    parent_x: i32,
    parent_z: i32,
    fract_x: f64,
    fract_z: f64,
    cached_parent_y: i32,
    /// Per-candidate cached values: Y fiddle and X/Z partial distance.
    candidates: [(f64, f64); 8],
    /// LCG state after adding each X candidate.
    rval_after_cx: [i64; 2],
}

impl<'a, B: SurfaceBiomeAccess + ?Sized> FuzzedBiomeColumn<'a, B> {
    fn new(biome_zoom_seed: i64, block_x: i32, block_z: i32, biome_access: &'a mut B) -> Self {
        let abs_x = block_x - 2;
        let abs_z = block_z - 2;
        let parent_x = abs_x >> 2;
        let parent_z = abs_z >> 2;
        Self {
            biome_access,
            biome_zoom_seed,
            parent_x,
            parent_z,
            fract_x: f64::from(abs_x & 3) / 4.0,
            fract_z: f64::from(abs_z & 3) / 4.0,
            cached_parent_y: i32::MIN,
            candidates: [(0.0, 0.0); 8],
            rval_after_cx: [
                lcg_next(biome_zoom_seed, i64::from(parent_x)),
                lcg_next(biome_zoom_seed, i64::from(parent_x + 1)),
            ],
        }
    }

    #[inline]
    fn compute_cy_group(&mut self, cy: i32, high: bool) {
        let base_idx = if high { 2 } else { 0 };
        for cx_idx in 0..2usize {
            let cx = self.parent_x + cx_idx as i32;
            let dx = if cx_idx == 0 {
                self.fract_x
            } else {
                self.fract_x - 1.0
            };
            let rval_cy = lcg_next(self.rval_after_cx[cx_idx], i64::from(cy));
            for cz_off in 0..2usize {
                let cz = self.parent_z + cz_off as i32;
                let dz = if cz_off == 0 {
                    self.fract_z
                } else {
                    self.fract_z - 1.0
                };

                let mut rval = lcg_next(rval_cy, i64::from(cz));
                rval = lcg_next(rval, i64::from(cx));
                rval = lcg_next(rval, i64::from(cy));
                rval = lcg_next(rval, i64::from(cz));
                let fx = get_fiddle(rval);
                rval = lcg_next(rval, self.biome_zoom_seed);
                let fy = get_fiddle(rval);
                rval = lcg_next(rval, self.biome_zoom_seed);
                let fz = get_fiddle(rval);

                let xz_partial = (dx + fx) * (dx + fx) + (dz + fz) * (dz + fz);
                self.candidates[cx_idx * 4 + base_idx + cz_off] = (fy, xz_partial);
            }
        }
    }

    fn recompute_candidates(&mut self, parent_y: i32) {
        if self.cached_parent_y != i32::MIN && parent_y == self.cached_parent_y - 1 {
            self.candidates[2] = self.candidates[0];
            self.candidates[3] = self.candidates[1];
            self.candidates[6] = self.candidates[4];
            self.candidates[7] = self.candidates[5];
            self.compute_cy_group(parent_y, false);
        } else {
            self.compute_cy_group(parent_y, false);
            self.compute_cy_group(parent_y + 1, true);
        }
        self.cached_parent_y = parent_y;
    }

    #[inline]
    fn get(&mut self, block_y: i32) -> u16 {
        let abs_y = block_y - 2;
        let parent_y = abs_y >> 2;
        let fract_y = f64::from(abs_y & 3) / 4.0;

        if parent_y != self.cached_parent_y {
            self.recompute_candidates(parent_y);
        }

        let mut min_i = 0usize;
        let mut min_dist = f64::INFINITY;
        for i in 0..8usize {
            let (fy, xz_partial) = self.candidates[i];
            let dy = if (i & 2) == 0 { fract_y } else { fract_y - 1.0 };
            let dist = xz_partial + (dy + fy) * (dy + fy);
            if min_dist > dist {
                min_i = i;
                min_dist = dist;
            }
        }

        let quart_x = if (min_i & 4) == 0 {
            self.parent_x
        } else {
            self.parent_x + 1
        };
        let quart_y = if (min_i & 2) == 0 {
            parent_y
        } else {
            parent_y + 1
        };
        let quart_z = if (min_i & 1) == 0 {
            self.parent_z
        } else {
            self.parent_z + 1
        };
        self.biome_access
            .biome_id_at_quart(quart_x, quart_y, quart_z)
    }
}

impl<B: SurfaceBiomeAccess + ?Sized> SurfaceBiomeProvider for FuzzedBiomeColumn<'_, B> {
    #[inline]
    fn biome_id(&mut self, block_y: i32) -> u16 {
        self.get(block_y)
    }
}

/// Vanilla's LinearCongruentialGenerator next operation.
#[inline]
const fn lcg_next(mut rval: i64, c: i64) -> i64 {
    rval = rval.wrapping_mul(
        rval.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407),
    );
    rval.wrapping_add(c)
}

/// Vanilla's BiomeManager fiddle transformation.
#[inline]
fn get_fiddle(rval: i64) -> f64 {
    let uniform = ((rval >> 24).rem_euclid(1024)) as f64 / 1024.0;
    (uniform - 0.5) * 0.9
}
