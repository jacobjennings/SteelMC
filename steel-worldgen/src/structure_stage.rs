//! Portable structure-piece pass over a post-Carvers terrain halo.
//!
//! Vanilla writes structure piece blocks at the very start of the Features
//! stage, before any placed feature runs, clipped to the chunk being decorated.
//! This module runs that pass against the same in-memory halo the portable
//! Features slice uses, and reports the positions it wrote so a host can turn
//! them into a sparse generated block list.
//!
//! It covers only the piece families
//! [`crate::structure::piece_placer`] implements. A start whose pieces are all
//! unimplemented contributes nothing, which a host can detect rather than
//! mistake for an empty site.

use glam::IVec3;
use rustc_hash::FxHashSet;
use steel_registry::Registry;
use steel_registry::feature::FeatureHeightmap;
use steel_utils::{BlockPos, BlockStateId, BoundingBox};

use crate::structure::piece_placer::{is_portable_piece, place_piece};
use crate::structure::start::StructureStart;
use crate::vegetation::VegetationBlockAccess;

/// Vanilla's decoration writable box: the centre chunk, one block in from the
/// build limits at both ends.
#[must_use]
pub const fn writable_box(chunk_x: i32, chunk_z: i32, min_y: i32, max_y_exclusive: i32) -> BoundingBox {
    let min_x = chunk_x * 16;
    let min_z = chunk_z * 16;
    BoundingBox::new(
        IVec3::new(min_x, min_y + 1, min_z),
        IVec3::new(min_x + 15, max_y_exclusive - 1, min_z + 15),
    )
}

/// A terrain halo that remembers every position written through it.
///
/// Structure placement writes states directly into the shared halo so later
/// features see them, which means the writes cannot be read back off a return
/// value. Recording them here is how the pass reports what it did.
pub struct RecordingRegion<'a, H: VegetationBlockAccess> {
    inner: &'a mut H,
    written: FxHashSet<(i32, i32, i32)>,
}

impl<'a, H: VegetationBlockAccess> RecordingRegion<'a, H> {
    /// Wraps a halo so writes through it are recorded.
    pub fn new(inner: &'a mut H) -> Self {
        Self {
            inner,
            written: FxHashSet::default(),
        }
    }

    /// Every position written, in ascending X/Y/Z order.
    #[must_use]
    pub fn written_positions(&self) -> Vec<(i32, i32, i32)> {
        let mut positions = self.written.iter().copied().collect::<Vec<_>>();
        positions.sort_unstable();
        positions
    }
}

impl<H: VegetationBlockAccess> VegetationBlockAccess for RecordingRegion<'_, H> {
    fn min_y(&self) -> i32 {
        self.inner.min_y()
    }

    fn max_y_exclusive(&self) -> i32 {
        self.inner.max_y_exclusive()
    }

    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        self.inner.block_state(pos)
    }

    fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId) {
        self.inner.set_block_state(pos, state);
        self.written.insert((pos.x(), pos.y(), pos.z()));
    }

    fn height_at(&self, kind: FeatureHeightmap, x: i32, z: i32) -> i32 {
        self.inner.height_at(kind, x, z)
    }

    fn biome_id_at_quart(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> u16 {
        self.inner.biome_id_at_quart(quart_x, quart_y, quart_z)
    }
}

/// Places every portable piece of `starts` that reaches `clip`.
///
/// `starts` must already be in a deterministic order. Vanilla walks structures
/// in decoration-step order, and once more than one family is portable this
/// pass has to walk them in that same order, because two overlapping families
/// would otherwise disagree about which one wrote last.
pub fn place_structure_pieces<H: VegetationBlockAccess>(
    region: &mut RecordingRegion<'_, H>,
    registry: &Registry,
    starts: &mut [StructureStart],
    clip: BoundingBox,
) -> usize {
    let mut placed = 0;
    for start in starts.iter_mut() {
        if start.pieces.is_empty() {
            continue;
        }
        for piece in &mut start.pieces {
            if !is_portable_piece(piece) || !piece.bounding_box.intersects(clip) {
                continue;
            }
            if place_piece(region, registry, piece, clip) {
                placed += 1;
            }
        }
    }
    placed
}
