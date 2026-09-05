//! Prototype bounded producer for the top-surface signal of one chunk.
//!
//! A surface tile consumes one height, one block state and one presence flag
//! per column. The generated path reaches those through a complete
//! `InMemorySurfaceChunk` of `16 * 16 * HEIGHT` block slots and a Surface
//! stage that descends every column to the dimension minimum.
//!
//! This producer keeps the same noise, aquifer, ore-vein and Surface-rule
//! evaluation, and changes only how much of each column is materialized: the
//! fill stops `lookahead` blocks below a column's topmost solid block, and the
//! Surface stage then runs against a chunk whose vertical extent is the window
//! those stops describe.
//!
//! This is prototype scaffolding for a measurement, not a replacement. The
//! generated path is untouched and remains authoritative.

use crate::density::{DimensionNoises, NoiseSettings};
use crate::noise::ColumnFlow;
use crate::surface_stage::{PreliminarySurfaceCorners, SurfaceExtensions};
use crate::surface_system::SurfaceSystem;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_utils::BlockStateId;

/// Blocks below a column's topmost solid block that the fill still visits.
///
/// The Surface rules read `stone_depth_below` only through comparisons of the
/// form `stone_depth_below <= k`. A window that extends `k + 1` blocks below
/// the top block answers every such comparison the same way the full column
/// does, because the truncated scan reports a value of at least `k + 1`
/// exactly when the full scan would also report more than `k`. The generated
/// Overworld rules use `k = 1`.
///
/// `steel-worldgen/tests/surface_signal_equivalence.rs` measures this rather
/// than taking it from the rules: on the declared Overworld fixtures a
/// lookahead of 1 reproduces every generated height and block state, and
/// raising it to 2, 8 or 32 changes nothing.
pub const DEFAULT_SURFACE_SIGNAL_LOOKAHEAD: i32 = 1;

/// Blocks above a column's topmost noise block that a fixed window covers.
///
/// This is the value the first prototype shipped, and it is kept only so the
/// equivalence test can still measure a fixed window against the derived one.
/// It is not a bound. The Surface stage writes above the noise column through
/// two extensions whose reach is decided by noise, so no constant is exact
/// everywhere, and this one is wrong in eroded badlands. Use
/// [`SurfaceSignalWindow::Derived`] instead.
pub const DEFAULT_SURFACE_SIGNAL_HEADROOM: i32 = 16;

/// How a bounded producer decides the vertical extent of its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceSignalWindow {
    /// A constant number of blocks above the topmost noise block.
    ///
    /// Measurement only. Nothing establishes that any constant is enough.
    Fixed(i32),
    /// The extent the Surface stage's own extensions ask for.
    ///
    /// The Surface rules descend from the world-surface heightmap and never
    /// write above it, so the noise column alone needs no headroom. Only the
    /// eroded badlands and frozen ocean extensions write higher, and each
    /// decides how high from 2D noise at the column's own X and Z before any
    /// block exists. Asking them directly replaces a guess with the value
    /// they will use.
    Derived,
}

/// The vertical extent one chunk's surface extensions require.
///
/// `ceiling` is exclusive. Both fields are already the maximum and minimum
/// over the chunk's 256 columns, because the windowed chunk is one box rather
/// than 256 independent columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionWindowDemand {
    /// Lowest block Y an extension reads or writes.
    pub floor: Option<i32>,
    /// Exclusive upper bound of the blocks an extension reads or writes.
    pub ceiling: Option<i32>,
}

impl ExtensionWindowDemand {
    /// The demand of a chunk whose biome source needs no extension.
    pub const NONE: Self = Self {
        floor: None,
        ceiling: None,
    };

    /// Records one column's required top block Y.
    fn require_top(&mut self, top_block_y: i32) {
        // The badlands extension reports a new start height one block above
        // its topmost write, and the Surface rules then read that slot. The
        // frozen ocean extension starts one block above its iceberg top.
        // Both therefore need one slot above `top_block_y`, and the ceiling
        // is exclusive, so it sits two above.
        let ceiling = top_block_y + 2;
        self.ceiling = Some(self.ceiling.map_or(ceiling, |current: i32| current.max(ceiling)));
    }

    /// Records one column's required bottom block Y.
    fn require_floor(&mut self, floor_block_y: i32) {
        self.floor = Some(self.floor.map_or(floor_block_y, |current: i32| current.min(floor_block_y)));
    }
}

/// Asks a chunk's active surface extensions how far they reach.
///
/// The biome at each column is not known before the fill runs, so every
/// extension the biome source can produce is evaluated at every column. That
/// is conservative rather than exact: it can only widen the window, never
/// narrow it below what the generated path needs. It costs three 2D noise
/// samples per column per active extension, against the tens of thousands of
/// density evaluations the window removes.
///
/// `preliminary_surface_corners` is required for the frozen ocean floor and
/// is otherwise unused. Passing `None` while that extension is active leaves
/// the floor unconstrained, which is not safe, so the caller supplies the
/// same corners it hands the Surface stage.
#[must_use]
pub fn extension_window_demand(
    system: &SurfaceSystem,
    extensions: SurfaceExtensions,
    chunk_min_x: i32,
    chunk_min_z: i32,
    preliminary_surface_corners: Option<PreliminarySurfaceCorners>,
) -> ExtensionWindowDemand {
    let mut demand = ExtensionWindowDemand::NONE;
    if !extensions.eroded_badlands && !extensions.frozen_ocean {
        return demand;
    }

    for local_z in 0..16usize {
        for local_x in 0..16usize {
            let block_x = chunk_min_x + local_x as i32;
            let block_z = chunk_min_z + local_z as i32;

            if extensions.eroded_badlands
                && let Some(top) = system.eroded_badlands_extension_top(block_x, block_z)
            {
                demand.require_top(top);
            }

            if extensions.frozen_ocean
                && let Some(top) = system.frozen_ocean_extension_top(block_x, block_z)
            {
                demand.require_top(top);
                if let Some(corners) = preliminary_surface_corners {
                    let surface_depth = system.get_surface_depth(block_x, block_z);
                    demand.require_floor(corners.min_surface_level(
                        local_x,
                        local_z,
                        surface_depth,
                    ));
                }
            }
        }
    }

    demand
}

/// What the bounded producer skipped relative to a full column fill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceSignalStats {
    /// Density callbacks the bounded fill actually ran.
    pub density_evaluations: u64,
    /// Density callbacks a full column fill would have run for the same chunk.
    pub full_density_evaluations: u64,
    /// Block slots the windowed chunk allocated.
    pub windowed_block_slots: usize,
    /// Block slots a full chunk would have allocated.
    pub full_block_slots: usize,
    /// Lowest and highest block Y the window covered.
    pub window_min_y: i32,
    /// Exclusive upper bound of the window.
    pub window_max_y: i32,
    /// Columns whose fill reached the bottom of the dimension without a stop.
    pub unbounded_columns: u32,
}

impl SurfaceSignalStats {
    /// Density callbacks the bound removed.
    #[must_use]
    pub const fn skipped_density_evaluations(&self) -> u64 {
        self.full_density_evaluations
            .saturating_sub(self.density_evaluations)
    }

    /// Block slots the window removed.
    #[must_use]
    pub const fn skipped_block_slots(&self) -> usize {
        self.full_block_slots
            .saturating_sub(self.windowed_block_slots)
    }
}

/// One column's non-air blocks, recorded while the bounded fill descends.
///
/// Air is never stored. The fill visits a column from the sky downwards, so
/// the first solid block it reports is the column's topmost solid block.
pub(crate) struct ColumnRecorder {
    /// `(world_y, state)` for every non-air block the fill produced.
    pub(crate) blocks: Vec<(i32, BlockStateId)>,
    /// Topmost solid (non-air, non-liquid) block, once seen.
    pub(crate) top_solid_y: Option<i32>,
    /// Y at which this column stopped. `None` while it is still running.
    pub(crate) stop_y: Option<i32>,
}

impl ColumnRecorder {
    pub(crate) const fn new() -> Self {
        Self {
            blocks: Vec::new(),
            top_solid_y: None,
            stop_y: None,
        }
    }

    /// Records one visited block and reports whether the column is finished.
    ///
    /// `state` is `None` where the aquifer produced air, which the generated
    /// path also leaves unwritten. Air is still visited, because a cave
    /// immediately below the surface has to fall inside the window.
    pub(crate) fn record(
        &mut self,
        world_y: i32,
        state: Option<BlockStateId>,
        lookahead: i32,
    ) -> ColumnFlow {
        if let Some(state) = state {
            self.blocks.push((world_y, state));
            let solid = !state.is_air() && !state.get_block().config.liquid;
            if solid && self.top_solid_y.is_none() {
                self.top_solid_y = Some(world_y);
            }
        }
        let Some(top) = self.top_solid_y else {
            return ColumnFlow::Continue;
        };
        if world_y <= top - lookahead {
            self.stop_y = Some(world_y);
            return ColumnFlow::FinishColumn;
        }
        ColumnFlow::Continue
    }

    /// Highest block Y this column contributed, exclusive.
    pub(crate) fn window_ceiling<N: DimensionNoises>(&self) -> i32 {
        self.blocks
            .first()
            .map_or(N::Settings::MIN_Y, |&(world_y, _)| world_y + 1)
    }

    /// Lowest block Y this column contributed to the chunk window.
    pub(crate) fn window_floor<N: DimensionNoises>(&self) -> i32 {
        self.stop_y.unwrap_or(N::Settings::MIN_Y)
    }
}
