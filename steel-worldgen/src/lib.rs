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
/// Portable Java 26.3 cave and canyon stage.
pub mod portable_carver;
/// Server-independent Java 26.3 ground-cover feature kernels.
pub mod portable_feature;
/// In-memory Java 26.3 Noise and Material stages.
pub mod portable_terrain;
/// `state_resolver`
pub mod state_resolver;
/// structure
pub mod structure;
/// Surface rule context types for generated code.
pub mod surface;
mod surface_stage;
mod surface_system;
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
