use std::cell::RefCell;

use steel_registry::vanilla_block_tags::BlockTag;

use super::super::super::prelude::*;
use super::super::super::runner::FeatureDecorationRunner;
use super::super::super::vanilla_collections::JavaBlockPosSet;
use super::{TreeBounds, TreePlacement};

const LEAF_DISTANCE_LIMIT: usize = 7;
const UNFETCHED_STATE: u32 = u32::MAX;

#[derive(Default)]
struct TreeLeafScratch {
    shape: Vec<u8>,
    states: Vec<u32>,
    frontiers: [JavaBlockPosSet; LEAF_DISTANCE_LIMIT],
}

#[derive(Clone, Copy)]
struct LocalGridSpec {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    sx: usize,
    sxy: usize,
}

impl LocalGridSpec {
    const fn from_bounds(bounds: TreeBounds) -> Self {
        let min_x = bounds.min_x - 1;
        let min_y = bounds.min_y - 1;
        let min_z = bounds.min_z - 1;
        let sx = (bounds.max_x - bounds.min_x + 3) as usize;
        let sy = (bounds.max_y - bounds.min_y + 3) as usize;
        Self {
            min_x,
            min_y,
            min_z,
            sx,
            sxy: sx * sy,
        }
    }

    const fn total_len(self, bounds: TreeBounds) -> usize {
        let sz = (bounds.max_z - bounds.min_z + 3) as usize;
        self.sxy * sz
    }

    #[inline]
    const fn index_of(self, pos: BlockPos) -> usize {
        let dx = (pos.x() - self.min_x) as usize;
        let dy = (pos.y() - self.min_y) as usize;
        let dz = (pos.z() - self.min_z) as usize;
        dz * self.sxy + dy * self.sx + dx
    }
}

thread_local! {
    static LEAF_SCRATCH: RefCell<TreeLeafScratch> = RefCell::new(TreeLeafScratch::default());
}

#[inline]
fn cached_block_state(
    region: &impl LevelReader,
    states: &mut [u32],
    index: usize,
    pos: BlockPos,
) -> BlockStateId {
    let raw = states[index];
    if raw != UNFETCHED_STATE {
        BlockStateId(raw as u16)
    } else {
        let state = region.get_block_state(pos);
        states[index] = u32::from(state.0);
        state
    }
}

impl FeatureDecorationRunner {
    pub(super) fn update_tree_leaves(
        region: &mut impl LevelAccessor,
        bounds: TreeBounds,
        placement: &TreePlacement,
    ) {
        let grid = LocalGridSpec::from_bounds(bounds);
        let total = grid.total_len(bounds);
        let cache_writes_directly = !region.requires_live_write_preflight();

        LEAF_SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            let TreeLeafScratch {
                shape,
                states,
                frontiers,
            } = &mut *scratch;

            shape.clear();
            shape.resize(total, 0);
            states.clear();
            states.resize(total, UNFETCHED_STATE);
            for frontier in frontiers.iter_mut() {
                frontier.reset_to_initial_capacity();
            }

            for pos in placement
                .decorations
                .java_order()
                .copied()
                .chain(placement.roots.java_order().copied())
            {
                if bounds.contains(pos) {
                    shape[grid.index_of(pos)] = 1;
                }
            }

            for pos in placement.trunks.java_order().copied() {
                frontiers[0].insert(pos);
            }

            let mut smallest_distance = 0;

            loop {
                while smallest_distance < LEAF_DISTANCE_LIMIT
                    && frontiers[smallest_distance].is_empty()
                {
                    smallest_distance += 1;
                }
                if smallest_distance >= LEAF_DISTANCE_LIMIT {
                    break;
                }

                let Some(pos) = frontiers[smallest_distance].pop_java_ordered_position() else {
                    continue;
                };
                if !bounds.contains(pos) {
                    continue;
                }

                let index = grid.index_of(pos);
                if smallest_distance != 0 {
                    let state = cached_block_state(region, states, index, pos);
                    if state
                        .try_get_value(&BlockStateProperties::DISTANCE)
                        .is_some()
                    {
                        let distance = smallest_distance as u8;
                        let next_state =
                            state.set_value(&BlockStateProperties::DISTANCE, distance);
                        let flags = UpdateFlags::UPDATE_NEIGHBORS
                            | UpdateFlags::UPDATE_CLIENTS
                            | UpdateFlags::UPDATE_KNOWN_SHAPE;
                        let accepted = region.set_block_state(pos, next_state, flags);
                        states[index] = if accepted && cache_writes_directly {
                            u32::from(next_state.0)
                        } else {
                            UNFETCHED_STATE
                        };
                    }
                }

                shape[index] = 1;

                for direction in Self::VANILLA_DIRECTION_VALUES {
                    let neighbor_pos = pos.relative(direction);
                    if !bounds.contains(neighbor_pos) {
                        continue;
                    }
                    let neighbor_index = grid.index_of(neighbor_pos);
                    if shape[neighbor_index] != 0 {
                        continue;
                    }

                    let state = cached_block_state(region, states, neighbor_index, neighbor_pos);
                    let Some(distance) = Self::tree_optional_leaf_distance_at(state) else {
                        continue;
                    };
                    let new_distance = distance.min((smallest_distance + 1) as u8);
                    if new_distance < LEAF_DISTANCE_LIMIT as u8 {
                        frontiers[usize::from(new_distance)].insert(neighbor_pos);
                        smallest_distance = smallest_distance.min(usize::from(new_distance));
                    }
                }
            }

            Self::update_tree_shape_at_edge(
                region,
                bounds,
                grid,
                shape,
                states,
                cache_writes_directly,
            );
        });
    }

    fn update_tree_shape_at_edge(
        region: &mut impl LevelAccessor,
        bounds: TreeBounds,
        grid: LocalGridSpec,
        shape: &[u8],
        states: &mut [u32],
        cache_writes_directly: bool,
    ) {
        for x in bounds.min_x..=bounds.max_x {
            for y in bounds.min_y..=bounds.max_y {
                Self::scan_tree_shape_line(
                    region,
                    grid,
                    shape,
                    states,
                    cache_writes_directly,
                    bounds.min_z,
                    bounds.max_z,
                    |z| BlockPos::new(x, y, z),
                    Direction::North,
                    Direction::South,
                );
            }
        }

        for z in bounds.min_z..=bounds.max_z {
            for x in bounds.min_x..=bounds.max_x {
                Self::scan_tree_shape_line(
                    region,
                    grid,
                    shape,
                    states,
                    cache_writes_directly,
                    bounds.min_y,
                    bounds.max_y,
                    |y| BlockPos::new(x, y, z),
                    Direction::Down,
                    Direction::Up,
                );
            }
        }

        for y in bounds.min_y..=bounds.max_y {
            for z in bounds.min_z..=bounds.max_z {
                Self::scan_tree_shape_line(
                    region,
                    grid,
                    shape,
                    states,
                    cache_writes_directly,
                    bounds.min_x,
                    bounds.max_x,
                    |x| BlockPos::new(x, y, z),
                    Direction::West,
                    Direction::East,
                );
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "threads local grid cache buffers without allocating a wrapper per axis"
    )]
    fn scan_tree_shape_line(
        region: &mut impl LevelAccessor,
        grid: LocalGridSpec,
        shape: &[u8],
        states: &mut [u32],
        cache_writes_directly: bool,
        start: i32,
        end: i32,
        mut pos_at: impl FnMut(i32) -> BlockPos,
        negative: Direction,
        positive: Direction,
    ) {
        let mut last_full = false;
        for cursor in start..=end + 1 {
            let full = cursor != end + 1 && shape[grid.index_of(pos_at(cursor))] != 0;
            if !last_full && full {
                Self::update_tree_shape_face(
                    region,
                    grid,
                    states,
                    cache_writes_directly,
                    pos_at(cursor),
                    negative,
                );
            }

            if last_full && !full {
                Self::update_tree_shape_face(
                    region,
                    grid,
                    states,
                    cache_writes_directly,
                    pos_at(cursor - 1),
                    positive,
                );
            }

            last_full = full;
        }
    }

    fn update_tree_shape_face(
        region: &mut impl LevelAccessor,
        grid: LocalGridSpec,
        states: &mut [u32],
        cache_writes_directly: bool,
        pos: BlockPos,
        direction: Direction,
    ) {
        let neighbor_pos = pos.relative(direction);
        let pos_index = grid.index_of(pos);
        let neighbor_index = grid.index_of(neighbor_pos);
        let state = cached_block_state(region, states, pos_index, pos);
        let neighbor_state = cached_block_state(region, states, neighbor_index, neighbor_pos);

        if !state.is_air() {
            Self::update_leaf_shape_at_edge(region, pos, state, neighbor_state);
        }
        if !neighbor_state.is_air() {
            Self::update_leaf_shape_at_edge(region, neighbor_pos, neighbor_state, state);
        }

        let new_state = if state.is_air() {
            state
        } else {
            let updated = BLOCK_BEHAVIORS
                .get_behavior(state.get_block())
                .update_shape(state, region, pos, direction, neighbor_pos, neighbor_state);
            if state != updated {
                let accepted = region.set_block_state(pos, updated, UpdateFlags::UPDATE_CLIENTS);
                states[pos_index] = if accepted && cache_writes_directly {
                    u32::from(updated.0)
                } else {
                    UNFETCHED_STATE
                };
            }
            updated
        };

        if !neighbor_state.is_air() {
            let new_neighbor_state = BLOCK_BEHAVIORS
                .get_behavior(neighbor_state.get_block())
                .update_shape(
                    neighbor_state,
                    region,
                    neighbor_pos,
                    direction.opposite(),
                    pos,
                    new_state,
                );
            if neighbor_state != new_neighbor_state {
                let accepted = region.set_block_state(
                    neighbor_pos,
                    new_neighbor_state,
                    UpdateFlags::UPDATE_CLIENTS,
                );
                states[neighbor_index] = if accepted && cache_writes_directly {
                    u32::from(new_neighbor_state.0)
                } else {
                    UNFETCHED_STATE
                };
            }
        }
    }

    fn update_leaf_shape_at_edge(
        region: &mut impl LevelAccessor,
        pos: BlockPos,
        state: BlockStateId,
        neighbor_state: BlockStateId,
    ) {
        if !state.get_block().has_tag(&BlockTag::LEAVES) {
            return;
        }

        let Some(distance) = state.try_get_value(&BlockStateProperties::DISTANCE) else {
            return;
        };

        if !Self::tree_can_schedule_tick_at(region, pos) {
            return;
        }

        if state.try_get_value(&BlockStateProperties::WATERLOGGED) == Some(true) {
            let _ = region.schedule_fluid_tick_default(pos, &vanilla_fluids::WATER, 5);
        }

        let distance_from_neighbor = Self::tree_leaf_distance_at(neighbor_state) + 1;
        if distance_from_neighbor != 1 || distance != distance_from_neighbor {
            let _ = region.schedule_block_tick_default(pos, state.get_block(), 1);
        }
    }

    fn tree_optional_leaf_distance_at(state: BlockStateId) -> Option<u8> {
        if state
            .get_block()
            .has_tag(&BlockTag::PREVENTS_NEARBY_LEAF_DECAY)
        {
            return Some(0);
        }

        state.try_get_value(&BlockStateProperties::DISTANCE)
    }

    fn tree_leaf_distance_at(state: BlockStateId) -> u8 {
        Self::tree_optional_leaf_distance_at(state).unwrap_or(7)
    }

    fn tree_can_schedule_tick_at(region: &mut impl LevelAccessor, pos: BlockPos) -> bool {
        region.can_write_to_chunk(
            SectionPos::block_to_section_coord(pos.x()),
            SectionPos::block_to_section_coord(pos.z()),
        )
    }
}
