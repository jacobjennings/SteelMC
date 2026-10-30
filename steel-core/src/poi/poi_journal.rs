//! POI manager tasks recorded by generation without a live world.
//!
//! Vanilla `WorldGenRegion.setBlock` calls `ServerLevel.updatePOIOnBlockStateChange`
//! after each accepted chunk write. That method compares the old and new states'
//! `PoiTypes.forState` holders and, when they differ, enqueues a `PoiManager.remove`
//! for an old type and then a `PoiManager.add` for a new type on the server executor.
//! A server applies these tasks to its world storage. Standalone generation has no
//! world, so each written chunk keeps the tasks in enqueue order. Every task for a
//! position lands in the chunk that contains it, so per-chunk order preserves the
//! per-position order the server executor would apply.

use rustc_hash::FxHashMap;
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::BlockPos;

/// One deferred POI manager task, in the order vanilla enqueues it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoiTask {
    /// `PoiManager.remove(pos)`, enqueued when the old state had a POI type.
    Remove(BlockPos),
    /// `PoiManager.add(pos, type)`, enqueued when the new state has a POI type.
    Add(BlockPos, usize),
}

/// A POI record left after every task ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiRecordState {
    /// Block position of the record.
    pub pos: BlockPos,
    /// POI type registry id.
    pub poi_type_id: usize,
    /// Free tickets of a record that no entity has claimed.
    pub free_tickets: u32,
}

/// Records and task outcomes after replaying tasks with vanilla `PoiSection` rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoiReplay {
    /// Records that remain, sorted by X, Y, then Z.
    pub records: Vec<PoiRecordState>,
    /// Add tasks that stored a new record.
    pub added: u32,
    /// Add tasks whose position already held the same type. Vanilla keeps the
    /// existing record and returns null.
    pub unchanged_adds: u32,
    /// Add tasks whose position held another type. Vanilla reports a data
    /// mismatch and replaces the stored record.
    pub replaced_adds: u32,
    /// Remove tasks that deleted a record.
    pub removed: u32,
    /// Remove tasks with no record at the position. Vanilla logs and continues.
    pub missing_removes: u32,
}

/// Replays tasks in order and returns the records a server would save.
#[must_use]
pub fn replay_poi_tasks(tasks: &[PoiTask]) -> PoiReplay {
    let mut records: FxHashMap<BlockPos, usize> = FxHashMap::default();
    let mut replay = PoiReplay::default();
    for task in tasks {
        match *task {
            PoiTask::Remove(pos) => {
                if records.remove(&pos).is_some() {
                    replay.removed += 1;
                } else {
                    replay.missing_removes += 1;
                }
            }
            PoiTask::Add(pos, poi_type_id) => match records.insert(pos, poi_type_id) {
                None => replay.added += 1,
                Some(previous) if previous == poi_type_id => replay.unchanged_adds += 1,
                Some(_) => replay.replaced_adds += 1,
            },
        }
    }
    replay.records = records
        .into_iter()
        .map(|(pos, poi_type_id)| PoiRecordState {
            pos,
            poi_type_id,
            free_tickets: REGISTRY
                .poi_types
                .by_id(poi_type_id)
                .map_or(0, |poi_type| poi_type.ticket_count),
        })
        .collect();
    replay
        .records
        .sort_unstable_by_key(|record| (record.pos.x(), record.pos.y(), record.pos.z()));
    replay
}
