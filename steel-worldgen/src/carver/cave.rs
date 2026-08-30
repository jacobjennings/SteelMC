//! Portable Cave and Nether Cave carvers.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use steel_math::trig;
use steel_registry::carver::CaveCarverConfiguration;
use steel_utils::ChunkPos;
use steel_utils::random::{Random, legacy_random::LegacyRandom};

use super::{
    CarveParams, CarveRun, CarveSkipChecker, CarverBlockAccess, CarverStyle,
    cached_replaceable_states, can_reach, horizontal_tunnel_radius,
};
use crate::density::DimensionNoises;

/// Vanilla cave-carver flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaveKind {
    /// Overworld cave behavior.
    Overworld,
    /// Nether cave behavior.
    Nether,
}

impl CaveKind {
    const fn cave_bound(self) -> i32 {
        match self {
            Self::Overworld => 15,
            Self::Nether => 10,
        }
    }

    const fn y_scale(self) -> f64 {
        match self {
            Self::Overworld => 1.0,
            Self::Nether => 5.0,
        }
    }

    const fn style(self) -> CarverStyle {
        match self {
            Self::Overworld => CarverStyle::Overworld,
            Self::Nether => CarverStyle::Nether,
        }
    }

    fn thickness(self, random: &mut impl Random) -> f32 {
        match self {
            Self::Overworld => {
                let mut thickness = random.next_f32() * 2.0 + random.next_f32();
                if random.next_i32_bounded(10) == 0 {
                    thickness *= random.next_f32() * random.next_f32() * 3.0 + 1.0;
                }
                thickness
            }
            Self::Nether => (random.next_f32() * 2.0 + random.next_f32()) * 2.0,
        }
    }
}

const MAX_TUNNEL_DISTANCE: i32 = 112;

#[derive(Debug, Clone, Copy)]
struct TunnelState {
    x: f64,
    y: f64,
    z: f64,
    horizontal_rotation: f32,
    vertical_rotation: f32,
}

#[derive(Debug, Clone, Copy)]
struct TunnelParams {
    tunnel_seed: i64,
    horizontal_radius_multiplier: f64,
    vertical_radius_multiplier: f64,
    thickness: f32,
    step: i32,
    distance: i32,
    y_scale: f64,
}

impl<N, H, F> CarveRun<'_, '_, N, H, F>
where
    N: DimensionNoises,
    H: CarverBlockAccess,
    F: FnMut(steel_utils::BlockPos) -> u16,
{
    pub(crate) fn carve_cave(
        &mut self,
        config: &CaveCarverConfiguration,
        kind: CaveKind,
        source_pos: ChunkPos,
        random: &mut LegacyRandom,
    ) {
        let inner = random.next_i32_bounded(kind.cave_bound());
        let middle = random.next_i32_bounded(inner + 1);
        let cave_count = random.next_i32_bounded(middle + 1);
        let source_min_x = source_pos.0.x * 16;
        let source_min_z = source_pos.0.y * 16;
        let params = CarveParams {
            replaceable_tag: &config.base.replaceable_tag,
            replaceable_states: cached_replaceable_states(&config.base.replaceable_tag),
            lava_level_y: config
                .base
                .lava_level
                .resolve_y(self.context.min_y, self.context.gen_depth),
            style: kind.style(),
        };

        for _ in 0..cave_count {
            let x = f64::from(source_min_x + random.next_i32_bounded(16));
            let y = f64::from(config.base.y.sample(
                random,
                self.context.min_y,
                self.context.gen_depth,
            ));
            let z = f64::from(source_min_z + random.next_i32_bounded(16));
            let horizontal_radius_multiplier =
                f64::from(config.horizontal_radius_multiplier.sample(random));
            let vertical_radius_multiplier =
                f64::from(config.vertical_radius_multiplier.sample(random));
            let floor_level = f64::from(config.floor_level.sample(random));
            let skip_checker = move |xd: f64, yd: f64, zd: f64, _world_y: i32| {
                yd <= floor_level || xd * xd + yd * yd + zd * zd >= 1.0
            };
            let mut tunnels = 1_i32;
            if random.next_i32_bounded(4) == 0 {
                self.create_room(
                    &params,
                    x,
                    y,
                    z,
                    1.0 + random.next_f32() * 6.0,
                    f64::from(config.base.y_scale.sample(random)),
                    &skip_checker,
                );
                tunnels += random.next_i32_bounded(4);
            }
            for _ in 0..tunnels {
                let state = TunnelState {
                    x,
                    y,
                    z,
                    horizontal_rotation: random.next_f32() * TAU,
                    vertical_rotation: (random.next_f32() - 0.5) / 4.0,
                };
                let tunnel = TunnelParams {
                    tunnel_seed: 0,
                    horizontal_radius_multiplier,
                    vertical_radius_multiplier,
                    thickness: kind.thickness(random),
                    step: 0,
                    distance: MAX_TUNNEL_DISTANCE
                        - random.next_i32_bounded(MAX_TUNNEL_DISTANCE / 4),
                    y_scale: kind.y_scale(),
                };
                self.create_tunnel(
                    &params,
                    state,
                    TunnelParams {
                        tunnel_seed: random.next_i64(),
                        ..tunnel
                    },
                    skip_checker,
                );
            }
        }
    }

    #[expect(clippy::too_many_arguments, reason = "mirrors Vanilla CaveWorldCarver")]
    fn create_room<S: CarveSkipChecker>(
        &mut self,
        params: &CarveParams<'_>,
        x: f64,
        y: f64,
        z: f64,
        thickness: f32,
        y_scale: f64,
        skip_checker: S,
    ) {
        let horizontal_radius =
            1.5 + f64::from(trig::sin(f64::from(FRAC_PI_2))) * f64::from(thickness);
        self.carve_ellipsoid(
            params,
            x + 1.0,
            y,
            z,
            horizontal_radius,
            horizontal_radius * y_scale,
            skip_checker,
        );
    }

    fn create_tunnel<S>(
        &mut self,
        params: &CarveParams<'_>,
        mut state: TunnelState,
        tunnel: TunnelParams,
        skip_checker: S,
    ) where
        S: CarveSkipChecker + Copy,
    {
        let mut random = LegacyRandom::from_seed(tunnel.tunnel_seed as u64);
        let split_point = random.next_i32_bounded(tunnel.distance / 2) + tunnel.distance / 4;
        let steep = random.next_i32_bounded(6) == 0;
        let mut y_rotation = 0.0_f32;
        let mut x_rotation = 0.0_f32;
        for step in tunnel.step..tunnel.distance {
            let progress = PI * step as f32 / tunnel.distance as f32;
            let horizontal_radius = horizontal_tunnel_radius(progress, tunnel.thickness);
            let vertical_radius = horizontal_radius * tunnel.y_scale;
            let horizontal_cos = trig::cos(f64::from(state.vertical_rotation));
            state.x += f64::from(trig::cos(f64::from(state.horizontal_rotation)) * horizontal_cos);
            state.y += f64::from(trig::sin(f64::from(state.vertical_rotation)));
            state.z += f64::from(trig::sin(f64::from(state.horizontal_rotation)) * horizontal_cos);
            state.vertical_rotation *= if steep { 0.92 } else { 0.7 };
            state.vertical_rotation += x_rotation * 0.1;
            state.horizontal_rotation += y_rotation * 0.1;
            x_rotation *= 0.9;
            y_rotation *= 0.75;
            x_rotation += (random.next_f32() - random.next_f32()) * random.next_f32() * 2.0;
            y_rotation += (random.next_f32() - random.next_f32()) * random.next_f32() * 4.0;
            if step == split_point && tunnel.thickness > 1.0 {
                let seed_a = random.next_i64();
                let thickness_a = random.next_f32() * 0.5 + 0.5;
                let state_a = TunnelState {
                    horizontal_rotation: state.horizontal_rotation - FRAC_PI_2,
                    vertical_rotation: state.vertical_rotation / 3.0,
                    ..state
                };
                let seed_b = random.next_i64();
                let thickness_b = random.next_f32() * 0.5 + 0.5;
                let state_b = TunnelState {
                    horizontal_rotation: state.horizontal_rotation + FRAC_PI_2,
                    vertical_rotation: state.vertical_rotation / 3.0,
                    ..state
                };
                self.create_tunnel(
                    params,
                    state_a,
                    TunnelParams {
                        tunnel_seed: seed_a,
                        thickness: thickness_a,
                        step,
                        y_scale: 1.0,
                        ..tunnel
                    },
                    skip_checker,
                );
                self.create_tunnel(
                    params,
                    state_b,
                    TunnelParams {
                        tunnel_seed: seed_b,
                        thickness: thickness_b,
                        step,
                        y_scale: 1.0,
                        ..tunnel
                    },
                    skip_checker,
                );
                return;
            }
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
            self.carve_ellipsoid(
                params,
                state.x,
                state.y,
                state.z,
                horizontal_radius * tunnel.horizontal_radius_multiplier,
                vertical_radius * tunnel.vertical_radius_multiplier,
                skip_checker,
            );
        }
    }
}
