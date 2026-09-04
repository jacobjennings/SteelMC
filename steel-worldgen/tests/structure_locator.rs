//! Finds seeds and chunks that contain a named structure.
//!
//! A parity fixture that asserts against a chunk with no structure in it passes
//! for the wrong reason. This is how the fixture's sample was chosen, kept so
//! the next family can choose its own instead of guessing at coordinates.
//!
//! Run it with, for example,
//! `LOCATE=igloo LOCATE_RADIUS=64 LOCATE_SEEDS=4,8,10 cargo test --release
//! -p steel-worldgen --test structure_locator -- --ignored --nocapture`.

use steel_worldgen::surface_sampler::{SurfaceDimension, SurfaceSampler};

#[test]
#[ignore = "locator, not an assertion"]
fn locate_structures() {
    let wanted = std::env::var("LOCATE").unwrap_or_else(|_| "swamp_hut".to_string());
    let radius: i32 = std::env::var("LOCATE_RADIUS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24);
    let seeds: Vec<u64> = std::env::var("LOCATE_SEEDS")
        .map(|value| {
            value
                .split(',')
                .filter_map(|seed| seed.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|_| vec![1, 7, 12345, 13579, 42, 2024]);
    for seed in seeds {
        let sampler = SurfaceSampler::new(seed, SurfaceDimension::Overworld);
        let starts = sampler.structure_starts_in_chunk_range(-radius, -radius, radius, radius);
        for (chunk_x, chunk_z, start) in starts {
            let name = start.structure.to_string();
            if !name.contains(&wanted) || start.pieces.is_empty() {
                continue;
            }
            println!(
                "FOUND seed={seed} chunk=({chunk_x},{chunk_z}) structure={name} pieces={} bb={:?}",
                start.pieces.len(),
                start.bounding_box,
            );
        }
    }
}
