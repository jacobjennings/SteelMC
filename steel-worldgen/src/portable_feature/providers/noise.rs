//! Java 26.3 provider noise keeps double main coordinates and float slow coordinates.

use steel_registry::Registry;
use steel_registry::feature::{
    BlockStateData, DualNoiseProvider, FeatureNoiseParameters, NoiseProvider,
    NoiseThresholdProvider,
};
use steel_utils::random::{
    Random as _, RandomSource, legacy_random::LegacyRandom, worldgen_random::WorldgenRandom,
};
use steel_utils::{BlockPos, BlockStateId};

use crate::{noise::NormalNoise, state_resolver::WorldgenStateResolver};

fn normal(parameters: &FeatureNoiseParameters, seed: i64) -> NormalNoise {
    let mut random = RandomSource::Legacy(LegacyRandom::from_seed(seed as u64));
    NormalNoise::create_from_random_with_params(
        &mut random,
        parameters.base_octave,
        parameters.base_amplitude,
        parameters.octave_count,
        parameters.normalize,
        &parameters.amplitude_modifiers,
    )
}

fn value(noise: &NormalNoise, pos: BlockPos, scale: f32) -> f32 {
    // NoiseBasedStateProvider promotes scale before multiplying integer coordinates.
    let scale = f64::from(scale);
    noise.get_value(
        f64::from(pos.x()) * scale,
        f64::from(pos.y()) * scale,
        f64::from(pos.z()) * scale,
    )
}

fn slow_value(noise: &NormalNoise, pos: BlockPos, scale: f32) -> f32 {
    noise.get_value(
        f64::from(pos.x() as f32 * scale),
        f64::from(pos.y() as f32 * scale),
        f64::from(pos.z() as f32 * scale),
    )
}

fn index(count: usize, noise: f32) -> Result<usize, &'static str> {
    if count == 0 {
        return Err("empty portable noise-provider states");
    }
    // Preserve native addition then division, including the f32 rounding.
    Ok((((1.0_f32 + noise) / 2.0).clamp(0.0, 0.9999) * count as f32) as usize)
}

fn resolve(registry: &Registry, data: &BlockStateData) -> BlockStateId {
    WorldgenStateResolver::feature_block_state_from_data(registry, data, "portable noise provider")
}

fn choose(
    registry: &Registry,
    states: &[BlockStateData],
    noise: f32,
) -> Result<BlockStateId, &'static str> {
    Ok(resolve(registry, &states[index(states.len(), noise)?]))
}

fn random_state(
    registry: &Registry,
    states: &[BlockStateData],
    random: &mut WorldgenRandom,
) -> Result<BlockStateId, &'static str> {
    let count =
        i32::try_from(states.len()).map_err(|_| "portable noise-provider states too large")?;
    if count == 0 {
        return Err("empty portable noise-provider states");
    }
    Ok(resolve(
        registry,
        &states[random.next_i32_bounded(count) as usize],
    ))
}

pub(super) fn sample(
    registry: &Registry,
    provider: &NoiseProvider,
    pos: BlockPos,
) -> Result<BlockStateId, &'static str> {
    let noise = normal(&provider.noise, provider.seed);
    choose(
        registry,
        &provider.states,
        value(&noise, pos, provider.scale),
    )
}

pub(super) fn threshold(
    registry: &Registry,
    provider: &NoiseThresholdProvider,
    random: &mut WorldgenRandom,
    pos: BlockPos,
) -> Result<BlockStateId, &'static str> {
    let noise = normal(&provider.noise, provider.seed);
    if value(&noise, pos, provider.scale) < provider.threshold {
        random_state(registry, &provider.low_states, random)
    } else if random.next_f32() < provider.high_chance {
        random_state(registry, &provider.high_states, random)
    } else {
        Ok(resolve(registry, &provider.default_state))
    }
}

pub(super) fn dual(
    registry: &Registry,
    provider: &DualNoiseProvider,
    pos: BlockPos,
) -> Result<BlockStateId, &'static str> {
    if provider.variety[0] < 1
        || provider.variety[1] < provider.variety[0]
        || provider.variety[1] > 64
    {
        return Err("invalid portable dual-noise variety");
    }
    let slow = normal(&provider.slow_noise, provider.seed);
    let variety = steel_math::map_clamped(
        f64::from(slow_value(&slow, pos, provider.slow_scale)),
        -1.0,
        1.0,
        f64::from(provider.variety[0]),
        f64::from(provider.variety[1] + 1),
    ) as i32;
    let mut states = Vec::with_capacity(variety as usize);
    for i in 0..variety {
        // BlockPos.offset uses Java's wrapping int arithmetic.
        let offset = BlockPos::new(
            pos.x().wrapping_add(i * 54_545),
            pos.y(),
            pos.z().wrapping_add(i * 34_234),
        );
        states.push(choose(
            registry,
            &provider.states,
            slow_value(&slow, offset, provider.slow_scale),
        )?);
    }
    let noise = normal(&provider.noise, provider.seed);
    Ok(states[index(states.len(), value(&noise, pos, provider.scale))?])
}
