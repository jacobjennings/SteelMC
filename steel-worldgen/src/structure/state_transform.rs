//! Portable block-state mirror and rotation used by structure placement.
//!
//! This is the vanilla `StructureTemplate` state transform, moved out of the
//! native template engine so a browser host can place structure pieces without
//! linking `steel-core`. The native template engine calls straight into these
//! functions, so there is one implementation and it cannot drift.

use steel_registry::Registry;
use steel_utils::{BlockStateId, Direction, Rotation};

use crate::structure::StructureMirror;

/// Applies a structure mirror and rotation to one block state.
///
/// Vanilla mutates the state's string properties and then looks the result back
/// up in the block's own state table, which is what this does.
///
/// # Panics
/// Panics if the rotated property set is not a valid state of the same block.
#[must_use]
pub fn transform_state(
    registry: &Registry,
    state: BlockStateId,
    mirror: StructureMirror,
    rotation: Rotation,
) -> BlockStateId {
    if mirror == StructureMirror::None && rotation == Rotation::None {
        return state;
    }

    let Some(block) = registry.blocks.by_state_id(state) else {
        return state;
    };
    let mut properties = registry
        .blocks
        .get_properties(state)
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<Vec<_>>();

    mirror_string_properties(&mut properties, mirror);
    rotate_string_properties(&mut properties, rotation);
    let property_refs = properties
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let Some(rotated) = registry
        .blocks
        .state_id_from_properties(&block.key, &property_refs)
    else {
        panic!(
            "rotating block state {} produced invalid properties",
            block.key
        );
    };
    rotated
}

/// Rotates the string properties of one block state in place.
pub fn rotate_string_properties(properties: &mut [(String, String)], rotation: Rotation) {
    let original = properties.to_vec();
    for (name, value) in properties.iter_mut() {
        match name.as_str() {
            "axis"
                if matches!(
                    rotation,
                    Rotation::Clockwise90 | Rotation::CounterClockwise90
                ) =>
            {
                match value.as_str() {
                    "x" => "z".clone_into(value),
                    "z" => "x".clone_into(value),
                    _ => {}
                }
            }
            "facing" => {
                if let Some(direction) = parse_direction(value) {
                    rotation.rotate(direction).as_str().clone_into(value);
                }
            }
            "rotation" => {
                if let Ok(segment) = value.parse::<i32>() {
                    let rotated = match rotation {
                        Rotation::None => segment,
                        Rotation::Clockwise90 => segment + 4,
                        Rotation::Clockwise180 => segment + 8,
                        Rotation::CounterClockwise90 => segment + 12,
                    };
                    *value = (rotated & 15).to_string();
                }
            }
            "shape" => {
                if let Some(rotated) = rotate_rail_shape(value, rotation) {
                    rotated.clone_into(value);
                }
            }
            "north" | "east" | "south" | "west" => {
                let from = direction_from_property_name(name);
                let source = inverse_rotate_direction(rotation, from);
                if let Some(source_name) = property_name_from_direction(source)
                    && let Some((_, source_value)) = original
                        .iter()
                        .find(|(original_name, _)| original_name == source_name)
                {
                    value.clone_from(source_value);
                }
            }
            _ => {}
        }
    }
}

/// Mirrors the string properties of one block state in place.
pub fn mirror_string_properties(properties: &mut [(String, String)], mirror: StructureMirror) {
    if mirror == StructureMirror::None {
        return;
    }

    let original = properties.to_vec();
    let facing = original
        .iter()
        .find(|(name, _)| name == "facing")
        .and_then(|(_, value)| parse_direction(value));
    let stair_shape = original
        .iter()
        .find(|(name, _)| name == "shape")
        .and_then(|(_, value)| parse_stair_shape(value));

    let mirrored_stairs = facing
        .zip(stair_shape)
        .and_then(|(direction, shape)| mirror_stair_shape(direction, shape, mirror));

    for (name, value) in properties.iter_mut() {
        match name.as_str() {
            "facing" => {
                if let Some((mirrored_facing, _)) = mirrored_stairs {
                    mirrored_facing.as_str().clone_into(value);
                } else if let Some(direction) = parse_direction(value) {
                    mirror_direction(direction, mirror).as_str().clone_into(value);
                }
            }
            "rotation" => {
                if let Ok(segment) = value.parse::<i32>() {
                    *value = mirror_rotation_segment(segment, 16, mirror).to_string();
                }
            }
            "hinge" => match value.as_str() {
                "left" => "right".clone_into(value),
                "right" => "left".clone_into(value),
                _ => {}
            },
            "shape" => {
                if let Some((_, mirrored_shape)) = mirrored_stairs {
                    mirrored_shape.clone_into(value);
                } else if let Some(mirrored_shape) = mirror_rail_shape(value, mirror) {
                    mirrored_shape.clone_into(value);
                }
            }
            "north" | "east" | "south" | "west" => {
                let from = direction_from_property_name(name);
                let source = mirror_direction(from, mirror);
                if let Some(source_name) = property_name_from_direction(source)
                    && let Some((_, source_value)) = original
                        .iter()
                        .find(|(original_name, _)| original_name == source_name)
                {
                    value.clone_from(source_value);
                }
            }
            _ => {}
        }
    }
}

/// Parses a `facing` property value.
#[must_use]
pub fn parse_direction(value: &str) -> Option<Direction> {
    match value {
        "down" => Some(Direction::Down),
        "up" => Some(Direction::Up),
        "north" => Some(Direction::North),
        "south" => Some(Direction::South),
        "west" => Some(Direction::West),
        "east" => Some(Direction::East),
        _ => None,
    }
}

/// Maps a connection property name onto its direction.
#[must_use]
pub fn direction_from_property_name(name: &str) -> Direction {
    match name {
        "east" => Direction::East,
        "south" => Direction::South,
        "west" => Direction::West,
        _ => Direction::North,
    }
}

/// Mirrors one direction.
#[must_use]
pub const fn mirror_direction(direction: Direction, mirror: StructureMirror) -> Direction {
    match mirror {
        StructureMirror::FrontBack => match direction {
            Direction::West => Direction::East,
            Direction::East => Direction::West,
            other => other,
        },
        StructureMirror::LeftRight => match direction {
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            other => other,
        },
        StructureMirror::None => direction,
    }
}

const fn mirror_rotation_segment(rotation: i32, steps: i32, mirror: StructureMirror) -> i32 {
    let half_steps = steps / 2;
    let corrected = if rotation > half_steps {
        rotation - steps
    } else {
        rotation
    };
    match mirror {
        StructureMirror::LeftRight => (half_steps - corrected + steps) % steps,
        StructureMirror::FrontBack => (steps - corrected) % steps,
        StructureMirror::None => rotation,
    }
}

const fn inverse_rotate_direction(rotation: Rotation, direction: Direction) -> Direction {
    match rotation {
        Rotation::None => direction,
        Rotation::Clockwise90 => Rotation::CounterClockwise90.rotate(direction),
        Rotation::Clockwise180 => Rotation::Clockwise180.rotate(direction),
        Rotation::CounterClockwise90 => Rotation::Clockwise90.rotate(direction),
    }
}

const fn property_name_from_direction(direction: Direction) -> Option<&'static str> {
    match direction {
        Direction::North => Some("north"),
        Direction::East => Some("east"),
        Direction::South => Some("south"),
        Direction::West => Some("west"),
        Direction::Down | Direction::Up => None,
    }
}

/// Rotates a rail `shape` property value.
#[must_use]
pub fn rotate_rail_shape(shape: &str, rotation: Rotation) -> Option<&'static str> {
    match rotation {
        Rotation::Clockwise180 => match shape {
            "ascending_east" => Some("ascending_west"),
            "ascending_west" => Some("ascending_east"),
            "ascending_north" => Some("ascending_south"),
            "ascending_south" => Some("ascending_north"),
            "north_south" => Some("north_south"),
            "east_west" => Some("east_west"),
            "south_east" => Some("north_west"),
            "south_west" => Some("north_east"),
            "north_west" => Some("south_east"),
            "north_east" => Some("south_west"),
            _ => None,
        },
        Rotation::CounterClockwise90 => match shape {
            "ascending_east" => Some("ascending_north"),
            "ascending_west" => Some("ascending_south"),
            "ascending_north" => Some("ascending_west"),
            "ascending_south" => Some("ascending_east"),
            "north_south" => Some("east_west"),
            "east_west" => Some("north_south"),
            "south_east" => Some("north_east"),
            "south_west" => Some("south_east"),
            "north_west" => Some("south_west"),
            "north_east" => Some("north_west"),
            _ => None,
        },
        Rotation::Clockwise90 => match shape {
            "ascending_east" => Some("ascending_south"),
            "ascending_west" => Some("ascending_north"),
            "ascending_north" => Some("ascending_east"),
            "ascending_south" => Some("ascending_west"),
            "north_south" => Some("east_west"),
            "east_west" => Some("north_south"),
            "south_east" => Some("south_west"),
            "south_west" => Some("north_west"),
            "north_west" => Some("north_east"),
            "north_east" => Some("south_east"),
            _ => None,
        },
        Rotation::None => None,
    }
}

/// Mirrors a rail `shape` property value.
#[must_use]
pub fn mirror_rail_shape(shape: &str, mirror: StructureMirror) -> Option<&'static str> {
    match mirror {
        StructureMirror::LeftRight => match shape {
            "ascending_north" => Some("ascending_south"),
            "ascending_south" => Some("ascending_north"),
            "north_south" => Some("north_south"),
            "east_west" => Some("east_west"),
            "south_east" => Some("north_east"),
            "south_west" => Some("north_west"),
            "north_west" => Some("south_west"),
            "north_east" => Some("south_east"),
            _ => None,
        },
        StructureMirror::FrontBack => match shape {
            "ascending_east" => Some("ascending_west"),
            "ascending_west" => Some("ascending_east"),
            "ascending_north" => Some("ascending_north"),
            "ascending_south" => Some("ascending_south"),
            "north_south" => Some("north_south"),
            "east_west" => Some("east_west"),
            "south_east" => Some("south_west"),
            "south_west" => Some("south_east"),
            "north_west" => Some("north_east"),
            "north_east" => Some("north_west"),
            _ => None,
        },
        StructureMirror::None => None,
    }
}

/// Normalises a stair `shape` property value.
#[must_use]
pub fn parse_stair_shape(shape: &str) -> Option<&'static str> {
    match shape {
        "straight" => Some("straight"),
        "inner_left" => Some("inner_left"),
        "inner_right" => Some("inner_right"),
        "outer_left" => Some("outer_left"),
        "outer_right" => Some("outer_right"),
        _ => None,
    }
}

/// Mirrors a stair's facing and shape together.
#[must_use]
pub fn mirror_stair_shape(
    direction: Direction,
    shape: &str,
    mirror: StructureMirror,
) -> Option<(Direction, &'static str)> {
    match mirror {
        StructureMirror::LeftRight
            if matches!(direction, Direction::North | Direction::South) =>
        {
            Some((
                direction.opposite(),
                match shape {
                    "outer_left" => "outer_right",
                    "inner_right" => "inner_left",
                    "inner_left" => "inner_right",
                    "outer_right" => "outer_left",
                    "straight" => "straight",
                    _ => return None,
                },
            ))
        }
        StructureMirror::FrontBack if matches!(direction, Direction::West | Direction::East) => {
            Some((
                direction.opposite(),
                match shape {
                    "outer_left" => "outer_right",
                    "outer_right" => "outer_left",
                    "inner_left" => "inner_left",
                    "inner_right" => "inner_right",
                    "straight" => "straight",
                    _ => return None,
                },
            ))
        }
        StructureMirror::None | StructureMirror::LeftRight | StructureMirror::FrontBack => None,
    }
}
