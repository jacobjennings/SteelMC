//! World generation noise, density functions, and surface rule runtime support.

#![feature(portable_simd)]

extern crate self as steel_worldgen;

pub use steel_utils::{BlockStateId, random};

/// Biome sources and climate samplers.
pub mod biomes;
/// Density function system for world generation.
pub mod density;
/// Noise generation utilities for world generation.
pub mod noise;
/// `state_resolver`
pub mod state_resolver;
/// structure
pub mod structure;
/// Surface rule context types for generated code.
pub mod surface;
/// Browser-safe sampling of the deterministic terrain density surface.
pub mod surface_sampler;
/// Portable Surface-stage runner and host traits.
pub mod surface_stage;
/// Dependency-light Vanilla SurfaceSystem implementation.
pub mod surface_system;
/// utils
pub mod utils;

#[expect(warnings)]
#[rustfmt::skip]
#[path = "generated/vanilla_multi_noise.rs"]
pub mod multi_noise;

#[expect(warnings)]
#[rustfmt::skip]
#[path = "generated/vanilla_noise_parameters.rs"]
pub mod noise_parameters;

#[expect(warnings)]
#[rustfmt::skip]
#[path = "generated/vanilla_density_functions/mod.rs"]
pub mod density_functions;
