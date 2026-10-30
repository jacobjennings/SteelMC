//! Source-bounded dispatch over generated Java 26.3 provider data.
//!
//! Unsupported provider families fail explicitly. A native optional refusal is
//! `Ok(None)`, distinct from unavailable world access or an unsupported provider.

use steel_registry::Registry;
use steel_registry::feature::{BlockPredicate, BlockStateProviderKind};
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

/// Native reference, simple, weighted and rule-based optional sampling.
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
        BlockStateProviderKind::RotatedBlock { .. } => {
            return Err("portable rotated provider unavailable");
        }
        BlockStateProviderKind::RandomizedInt { .. } => {
            return Err("portable randomized-int provider unavailable");
        }
        BlockStateProviderKind::Noise(_) => return Err("portable noise provider unavailable"),
        BlockStateProviderKind::NoiseThreshold(_) => {
            return Err("portable noise-threshold provider unavailable");
        }
        BlockStateProviderKind::DualNoise(_) => {
            return Err("portable dual-noise provider unavailable");
        }
        BlockStateProviderKind::CopyProperties { .. } => {
            return Err("portable copy-properties provider unavailable");
        }
        BlockStateProviderKind::RandomBlock { .. } => {
            return Err("portable random-block provider unavailable");
        }
    };
    Ok(Some(WorldgenStateResolver::feature_block_state_from_data(
        registry,
        data,
        "portable state provider",
    )))
}
