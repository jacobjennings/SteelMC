//! Portable Vanilla canyon carver.

use std::f32::consts::{PI, TAU};

use steel_math::trig;
use steel_registry::carver::CanyonCarverConfiguration;
use steel_utils::ChunkPos;
use steel_utils::random::{Random as _, legacy_random::LegacyRandom};

use super::{
    CarveParams, CarveRun, CarverBlockAccess, CarverStyle, cached_replaceable_states, can_reach,
    horizontal_tunnel_radius,
};
use crate::density::DimensionNoises;

const MAX_TUNNEL_DISTANCE: i32 = 112;

#[derive(Debug, Clone, Copy)]
struct CanyonState {
    x: f64,
    y: f64,
    z: f64,
    horizontal_rotation: f32,
    vertical_rotation: f32,
}

#[derive(Debug, Clone, Copy)]
struct CanyonTunnel {
    seed: i64,
    thickness: f32,
    distance: i32,
    y_scale: f64,
}

impl<N, H, F> CarveRun<'_, '_, N, H, F>
where
    N: DimensionNoises,
    H: CarverBlockAccess,
    F: FnMut(steel_utils::BlockPos) -> u16,
{
    pub(crate) fn carve_canyon(
        &mut self,
        config: &CanyonCarverConfiguration,
        source_pos: ChunkPos,
        random: &mut LegacyRandom,
    ) {
        let source_min_x = source_pos.0.x * 16;
        let source_min_z = source_pos.0.y * 16;
        let params = CarveParams {
            replaceable_tag: &config.base.replaceable_tag,
            replaceable_states: cached_replaceable_states(&config.base.replaceable_tag),
            lava_level_y: config
                .base
                .lava_level
                .resolve_y(self.context.min_y, self.context.gen_depth),
            style: CarverStyle::Overworld,
        };
        let state = CanyonState {
            x: f64::from(source_min_x + random.next_i32_bounded(16)),
            y: f64::from(
                config
                    .base
                    .y
                    .sample(random, self.context.min_y, self.context.gen_depth),
            ),
            z: f64::from(source_min_z + random.next_i32_bounded(16)),
            horizontal_rotation: random.next_f32() * TAU,
            vertical_rotation: config.vertical_rotation.sample(random),
        };
        let tunnel = CanyonTunnel {
            y_scale: f64::from(config.base.y_scale.sample(random)),
            thickness: config.shape.thickness.sample(random),
            distance: (MAX_TUNNEL_DISTANCE as f32 * config.shape.distance_factor.sample(random))
                as i32,
            seed: random.next_i64(),
        };
        self.do_carve_canyon(&params, config, state, tunnel);
    }

    fn do_carve_canyon(
        &mut self,
        params: &CarveParams<'_>,
        config: &CanyonCarverConfiguration,
        mut state: CanyonState,
        tunnel: CanyonTunnel,
    ) {
        let mut random = LegacyRandom::from_seed(tunnel.seed as u64);
        let width_factors = config
            .shape
            .init_width_factors(self.context.gen_depth, &mut random);
        let mut y_rotation = 0.0_f32;
        let mut x_rotation = 0.0_f32;
        for step in 0..tunnel.distance {
            let progress = PI * step as f32 / tunnel.distance as f32;
            let mut horizontal_radius = horizontal_tunnel_radius(progress, tunnel.thickness);
            let mut vertical_radius = horizontal_radius * tunnel.y_scale;
            horizontal_radius *=
                f64::from(config.shape.horizontal_radius_factor.sample(&mut random));
            vertical_radius = config.shape.update_vertical_radius(
                &mut random,
                vertical_radius,
                tunnel.distance as f32,
                step as f32,
            );
            let horizontal_cos = trig::cos(f64::from(state.vertical_rotation));
            state.x += f64::from(trig::cos(f64::from(state.horizontal_rotation)) * horizontal_cos);
            state.y += f64::from(trig::sin(f64::from(state.vertical_rotation)));
            state.z += f64::from(trig::sin(f64::from(state.horizontal_rotation)) * horizontal_cos);
            state.vertical_rotation *= 0.7;
            state.vertical_rotation += x_rotation * 0.05;
            state.horizontal_rotation += y_rotation * 0.05;
            x_rotation *= 0.8;
            y_rotation *= 0.5;
            x_rotation += (random.next_f32() - random.next_f32()) * random.next_f32() * 2.0;
            y_rotation += (random.next_f32() - random.next_f32()) * random.next_f32() * 4.0;
            if random.next_i32_bounded(4) == 0 {
                continue;
            }
            if !can_reach(
                self.chunk_min_x,
                self.chunk_min_z,
                state.x,
                state.z,
                step,
                tunnel.distance,
                tunnel.thickness,
            ) {
                return;
            }
            let min_y = self.context.min_y;
            let skip_checker = |xd: f64, yd: f64, zd: f64, world_y: i32| {
                should_skip_canyon(&width_factors, min_y, xd, yd, zd, world_y)
            };
            self.carve_ellipsoid(
                params,
                state.x,
                state.y,
                state.z,
                horizontal_radius,
                vertical_radius,
                skip_checker,
            );
        }
    }
}

fn should_skip_canyon(
    width_factors: &[f32],
    min_y: i32,
    xd: f64,
    yd: f64,
    zd: f64,
    world_y: i32,
) -> bool {
    let factor = width_factors[(world_y - min_y - 1) as usize];
    (xd * xd + zd * zd) * f64::from(factor) + yd * yd / 6.0 >= 1.0
}
