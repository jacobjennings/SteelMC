//! Source-bounded dispatch over generated Java 26.3 provider data.
//!
//! Unsupported provider families fail explicitly. A native optional refusal is
//! `Ok(None)`, distinct from unavailable world access or an unsupported provider.

use steel_registry::Registry;
use steel_registry::TaggedRegistryExt as _;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{BlockStateProperties, Property};
use steel_registry::feature::BlockHolderSet;
use steel_registry::feature::{BlockPredicate, BlockStateProviderKind};
use steel_utils::Direction;
use steel_utils::random::{Random as _, worldgen_random::WorldgenRandom};
use steel_utils::{BlockPos, BlockStateId};

use crate::state_resolver::WorldgenStateResolver;

/// Live world operations demanded by rule-based state providers.
pub trait ProviderAccess {
    /// Evaluate a predicate against current retained world state.
    fn test(&mut self, predicate: &BlockPredicate, pos: BlockPos) -> Result<bool, &'static str>;
    /// Read the current block, refusing unavailable positions.
    fn block(&mut self, pos: BlockPos) -> Result<BlockStateId, &'static str>;
}

/// Native required sampling. Only a rule provider's ordinary optional refusal
/// reads the existing block. Errors never turn into a state or an absent world.
pub fn state(
    world: &mut impl ProviderAccess,
    registry: &Registry,
    provider: &BlockStateProviderKind,
    random: &mut WorldgenRandom,
    pos: BlockPos,
) -> Result<BlockStateId, &'static str> {
    match optional_state(world, registry, provider, random, pos)? {
        Some(state) => Ok(state),
        None => world.block(pos),
    }
}

/// Source-ordered optional sampling over generated provider configurations.
/// Rules run in source order. A matching nested rule that declines continues
/// with the next rule. Only after all rules decline is the fallback sampled.
pub fn optional_state(
    world: &mut impl ProviderAccess,
    registry: &Registry,
    provider: &BlockStateProviderKind,
    random: &mut WorldgenRandom,
    pos: BlockPos,
) -> Result<Option<BlockStateId>, &'static str> {
    let data = match provider {
        BlockStateProviderKind::Reference(provider) => {
            return optional_state(world, registry, &provider.kind, random, pos);
        }
        BlockStateProviderKind::Simple { state } => state,
        BlockStateProviderKind::Weighted { entries } => {
            let mut total: i32 = 0;
            for entry in entries {
                if entry.weight < 0 {
                    return Err("negative portable provider weight");
                }
                total = total
                    .checked_add(entry.weight)
                    .ok_or("portable provider weight overflow")?;
            }
            if total == 0 {
                return Err("empty portable weighted provider");
            }
            let mut selection = random.next_i32_bounded(total);
            let mut selected = None;
            for entry in entries {
                selection -= entry.weight;
                if selection < 0 {
                    selected = Some(&entry.data);
                    break;
                }
            }
            selected.ok_or("portable weighted provider selection unavailable")?
        }
        BlockStateProviderKind::RuleBased { fallback, rules } => {
            for rule in rules {
                if world.test(&rule.if_true, pos)?
                    && let Some(state) = optional_state(world, registry, &rule.then, random, pos)?
                {
                    return Ok(Some(state));
                }
            }
            return match fallback {
                Some(fallback) => optional_state(world, registry, fallback, random, pos),
                None => Ok(None),
            };
        }
        BlockStateProviderKind::RotatedBlock {
            state: source,
            direction,
        } => {
            // Native selects the direction before sampling the nested provider.
            let direction =
                direction.unwrap_or_else(|| Direction::ALL[random.next_i32_bounded(6) as usize]);
            let mut sampled = state(world, registry, source, random, pos)?;
            if supports_property(registry, sampled, &BlockStateProperties::AXIS)? {
                sampled = sampled.set_value(&BlockStateProperties::AXIS, direction.axis());
            }
            if supports_property(registry, sampled, &BlockStateProperties::FACING)? {
                sampled = sampled.set_value(&BlockStateProperties::FACING, direction);
            }
            if direction.is_horizontal()
                && supports_property(registry, sampled, &BlockStateProperties::HORIZONTAL_FACING)?
            {
                sampled = sampled.set_value(&BlockStateProperties::HORIZONTAL_FACING, direction);
            }
            return Ok(Some(sampled));
        }
        BlockStateProviderKind::RandomizedInt {
            property,
            source,
            values,
        } => {
            let sampled = state(world, registry, source, random, pos)?;
            let block = registry
                .blocks
                .by_state_id(sampled)
                .ok_or("invalid portable provider state")?;
            // A missing or non-integer property must not consume the values RNG.
            if !block
                .properties
                .iter()
                .any(|p| p.get_name() == property && p.integer_range().is_some())
            {
                return Ok(Some(sampled));
            }
            let value = values.sample(random).to_string();
            let current = registry.blocks.get_properties(sampled);
            let properties: Vec<_> = current
                .iter()
                .map(|(name, old)| {
                    (
                        *name,
                        if *name == property {
                            value.as_str()
                        } else {
                            *old
                        },
                    )
                })
                .collect();
            return registry
                .blocks
                .state_id_from_block_properties(block, &properties)
                .map(Some)
                .ok_or("invalid portable randomized-int value");
        }
        BlockStateProviderKind::Noise(provider) => {
            return noise::sample(registry, provider, pos).map(Some);
        }
        BlockStateProviderKind::NoiseThreshold(provider) => {
            return noise::threshold(registry, provider, random, pos).map(Some);
        }
        BlockStateProviderKind::DualNoise(provider) => {
            return noise::dual(registry, provider, pos).map(Some);
        }
        BlockStateProviderKind::CopyProperties { source } => {
            let mut sampled = state(world, registry, source, random, pos)?;
            let existing = world.block(pos)?;
            let target = registry
                .blocks
                .by_state_id(sampled)
                .ok_or("invalid portable copy target")?;
            let origin = registry
                .blocks
                .by_state_id(existing)
                .ok_or("invalid portable copy source")?;
            for (name, value) in registry.blocks.get_properties(existing) {
                let Some(source_property) = origin.properties.iter().find(|p| p.get_name() == name)
                else {
                    continue;
                };
                // Native Property equality includes its name, value type and allowed values.
                if target.properties.iter().any(|p| {
                    p.get_name() == name
                        && p.integer_range() == source_property.integer_range()
                        && p.get_possible_value_names()
                            == source_property.get_possible_value_names()
                }) {
                    sampled = registry
                        .blocks
                        .try_set_property_by_name(sampled, name, value)
                        .ok_or("invalid portable copied property")?;
                }
            }
            return Ok(Some(sampled));
        }
        BlockStateProviderKind::RandomBlock { blocks } => {
            let candidates = match blocks {
                BlockHolderSet::Entries(entries) => entries.to_vec(),
                BlockHolderSet::Tag(tag) => registry.blocks.iter_tag(tag).collect(),
            };
            let count = i32::try_from(candidates.len())
                .map_err(|_| "portable random-block list too large")?;
            if count == 0 {
                return Ok(None);
            }
            let block = candidates[random.next_i32_bounded(count) as usize];
            return Ok(Some(registry.blocks.get_default_state_id(block)));
        }
    };
    Ok(Some(WorldgenStateResolver::feature_block_state_from_data(
        registry,
        data,
        "portable state provider",
    )))
}

// Native trySetValue requires the actual property, including its allowed values.
// Facing and horizontal facing share a name but are distinct properties.
fn supports_property(
    registry: &Registry,
    state: BlockStateId,
    property: &impl Property,
) -> Result<bool, &'static str> {
    let block = registry
        .blocks
        .by_state_id(state)
        .ok_or("invalid portable rotated state")?;
    let expected = property.get_possible_value_names();
    Ok(block.properties.iter().any(|candidate| {
        let values = candidate.get_possible_value_names();
        candidate.get_name() == property.get_name()
            && candidate.integer_range() == property.integer_range()
            && values.len() == expected.len()
            && values.iter().all(|value| expected.contains(value))
    }))
}

mod noise;
