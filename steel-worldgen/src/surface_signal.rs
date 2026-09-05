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

/// Blocks above a column's topmost noise block that the window still covers.
///
/// The Surface stage does not only rewrite blocks the noise produced. It also
/// writes above them, so a window that stops at the top of the noise column
/// silently drops those writes and reports a lower surface. The measured
/// example is frozen ocean, where a generated column surfaces at Y 72 while a
/// window with no headroom reports 62.
///
/// This value is a working figure, not a proven bound. The equivalence test
/// sweeps it, and the smallest exact headroom depends on which columns are
/// sampled: one declared fixture set needed 16 and another needed 4. Nothing
/// here establishes a maximum for the whole Overworld, so a production version
/// needs either a proof from the extension code or a much wider sample.
pub const DEFAULT_SURFACE_SIGNAL_HEADROOM: i32 = 16;

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
