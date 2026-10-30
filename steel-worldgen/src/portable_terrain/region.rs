//! Shared Carvers-stage storage for portable feature hosts.
//!
//! This is block storage, not a replacement for `WorldGenRegion`: providers,
//! survival, POI, block entities and postprocessing still belong to the host.
//! Missing chunks and unsupported vertical reads are errors, never air.

use std::collections::BTreeMap;

use steel_utils::{BlockPos, BlockStateId};

use super::{ChunkStage, PortableChunk, PortableOverworld, Settings};
use crate::surface::SurfaceBlockAccess;

/// A complete rectangle of chunks from one generator at the Carvers stage.
/// Writes remain visible across feature calls and chunk boundaries.
pub struct PortableRegion<'a> {
    chunks: BTreeMap<(i32, i32), PortableChunk<'a>>,
    min: (i32, i32),
    max: (i32, i32),
}

impl<'a> PortableRegion<'a> {
    /// Takes existing chunks without generating missing neighbors or rerunning stages.
    /// All inputs must belong to `generator` and form a nonempty rectangle.
    pub fn new(
        generator: &'a PortableOverworld,
        chunks: Vec<PortableChunk<'a>>,
    ) -> Result<Self, &'static str> {
        if chunks.is_empty() {
            return Err("portable region requires retained chunks");
        }
        let mut retained = BTreeMap::new();
        let mut min = (i32::MAX, i32::MAX);
        let mut max = (i32::MIN, i32::MIN);
        for chunk in chunks {
            if !std::ptr::eq(generator, chunk.seed_binding) || chunk.stage != ChunkStage::Carvers {
                return Err("portable region requires this generator's Carvers output");
            }
            let pos = (chunk.min_x / 16, chunk.min_z / 16);
            min = (min.0.min(pos.0), min.1.min(pos.1));
            max = (max.0.max(pos.0), max.1.max(pos.1));
            if retained.insert(pos, chunk).is_some() {
                return Err("duplicate portable region chunk");
            }
        }
        let width = (max.0 - min.0 + 1) as u64;
        let depth = (max.1 - min.1 + 1) as u64;
        if width * depth != retained.len() as u64 {
            return Err("portable region has missing chunks");
        }
        Ok(Self {
            chunks: retained,
            min,
            max,
        })
    }

    /// Retained chunk snapshots, ordered by chunk X then Z.
    pub fn chunks(&self) -> impl Iterator<Item = ((i32, i32), &PortableChunk<'a>)> {
        self.chunks.iter().map(|(&pos, chunk)| (pos, chunk))
    }

    /// Opens a native-style horizontal write zone for one generating chunk.
    /// The caller supplies the source generation step's radius. The full zone
    /// must already be loaded. This does not select or order biome features.
    pub fn write_window(
        &mut self,
        center: (i32, i32),
        radius: i32,
    ) -> Result<RegionWindow<'_, 'a>, &'static str> {
        if radius < 0
            || i64::from(center.0) - i64::from(radius) < i64::from(self.min.0)
            || i64::from(center.1) - i64::from(radius) < i64::from(self.min.1)
            || i64::from(center.0) + i64::from(radius) > i64::from(self.max.0)
            || i64::from(center.1) + i64::from(radius) > i64::from(self.max.1)
        {
            return Err("portable write zone is not fully retained");
        }
        Ok(RegionWindow {
            region: self,
            center,
            radius,
        })
    }
}

/// A mutable block-storage view. Feature hosts must perform their native side
/// effects explicitly. This API does not claim complete `WorldGenRegion.setBlock`.
pub struct RegionWindow<'region, 'generator> {
    region: &'region mut PortableRegion<'generator>,
    center: (i32, i32),
    radius: i32,
}

impl RegionWindow<'_, '_> {
    /// Reads loaded block storage, including neighbors outside the write zone.
    /// Vertical out-of-build-height semantics are not implemented by this view.
    pub fn block(&mut self, pos: BlockPos) -> Result<BlockStateId, &'static str> {
        let chunk = self
            .region
            .chunks
            .get_mut(&(pos.x().div_euclid(16), pos.z().div_euclid(16)))
            .ok_or("read outside retained portable region")?;
        let y = pos
            .y()
            .checked_sub(chunk.min_y())
            .ok_or("portable region Y overflow")?;
        if !(0..Settings::HEIGHT).contains(&y) {
            return Err("portable region read outside build height");
        }
        chunk
            .get_relative_block(
                pos.x().rem_euclid(16) as usize,
                y as usize,
                pos.z().rem_euclid(16) as usize,
            )
            .ok_or("portable region block unavailable")
    }

    /// Applies the `WorldGenRegion.isWithinWriteZone` chessboard-distance rule.
    /// Rejected writes do not access a chunk or modify storage.
    #[must_use]
    pub fn can_write(&self, pos: BlockPos) -> bool {
        (i64::from(pos.x().div_euclid(16)) - i64::from(self.center.0)).abs()
            <= i64::from(self.radius)
            && (i64::from(pos.z().div_euclid(16)) - i64::from(self.center.1)).abs()
                <= i64::from(self.radius)
    }

    /// Writes only block storage and its world-surface height. Returns false for
    /// horizontal write-zone rejection. No provider sampling or RNG is undone.
    /// The feature host owns block entities, POI and postprocessing operations.
    pub fn write_state(
        &mut self,
        pos: BlockPos,
        state: BlockStateId,
    ) -> Result<bool, &'static str> {
        if !self.can_write(pos) {
            return Ok(false);
        }
        let chunk = self
            .region
            .chunks
            .get_mut(&(pos.x().div_euclid(16), pos.z().div_euclid(16)))
            .ok_or("write outside retained portable region")?;
        let y = pos
            .y()
            .checked_sub(chunk.min_y())
            .ok_or("portable region Y overflow")?;
        if !(0..Settings::HEIGHT).contains(&y) {
            return Err("portable region write outside build height");
        }
        chunk.set_relative_block(
            pos.x().rem_euclid(16) as usize,
            y as usize,
            pos.z().rem_euclid(16) as usize,
            state,
        );
        Ok(true)
    }
}
