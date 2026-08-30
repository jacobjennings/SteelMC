//! Vanilla `BiomeManager` coordinate fuzzing shared by worldgen stages.

use glam::{DVec3, IVec3};
use steel_utils::BlockPos;

/// Resolves the biome at a block through Vanilla's fuzzed quart-cell lookup.
///
/// This is the `BiomeManager.getBiome` selection used by Surface, Carvers,
/// and feature placement.  `quart_biome` supplies the unfuzzed noise biome at
/// a quart coordinate.
pub fn fuzzed_biome_at_block<F: FnMut(IVec3) -> u16>(
    biome_zoom_seed: i64,
    pos: BlockPos,
    mut quart_biome: F,
) -> u16 {
    let abs = pos.0 - IVec3::splat(2);
    let parent = IVec3::new(abs.x >> 2, abs.y >> 2, abs.z >> 2);
    let fract = DVec3::new(
        f64::from(abs.x & 3),
        f64::from(abs.y & 3),
        f64::from(abs.z & 3),
    ) / 4.0;
    let mut min_i = 0usize;
    let mut min_dist = f64::INFINITY;
    for i in 0..8usize {
        let x_even = (i & 4) == 0;
        let y_even = (i & 2) == 0;
        let z_even = (i & 1) == 0;
        let cx = if x_even { parent.x } else { parent.x + 1 };
        let cy = if y_even { parent.y } else { parent.y + 1 };
        let cz = if z_even { parent.z } else { parent.z + 1 };
        let dx = if x_even { fract.x } else { fract.x - 1.0 };
        let dy = if y_even { fract.y } else { fract.y - 1.0 };
        let dz = if z_even { fract.z } else { fract.z - 1.0 };
        let mut value = lcg_next(biome_zoom_seed, i64::from(cx));
        value = lcg_next(value, i64::from(cy));
        value = lcg_next(value, i64::from(cz));
        value = lcg_next(value, i64::from(cx));
        value = lcg_next(value, i64::from(cy));
        value = lcg_next(value, i64::from(cz));
        let fx = get_fiddle(value);
        value = lcg_next(value, biome_zoom_seed);
        let fy = get_fiddle(value);
        value = lcg_next(value, biome_zoom_seed);
        let fz = get_fiddle(value);
        let distance = (dx + fx).powi(2) + (dy + fy).powi(2) + (dz + fz).powi(2);
        if min_dist > distance {
            min_i = i;
            min_dist = distance;
        }
    }
    quart_biome(IVec3::new(
        if (min_i & 4) == 0 {
            parent.x
        } else {
            parent.x + 1
        },
        if (min_i & 2) == 0 {
            parent.y
        } else {
            parent.y + 1
        },
        if (min_i & 1) == 0 {
            parent.z
        } else {
            parent.z + 1
        },
    ))
}

#[inline]
const fn lcg_next(mut value: i64, addend: i64) -> i64 {
    value = value.wrapping_mul(
        value
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407),
    );
    value.wrapping_add(addend)
}

#[inline]
fn get_fiddle(value: i64) -> f64 {
    ((((value >> 24).rem_euclid(1024)) as f64 / 1024.0) - 0.5) * 0.9
}
