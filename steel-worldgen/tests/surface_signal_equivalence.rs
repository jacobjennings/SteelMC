//! Compares the prototype bounded surface signal against the generated path.
//!
//! The bounded producer exists to be checked, not trusted. Depth reasoning
//! from the generated Surface rules says a two-block window should answer
//! every `stone_depth_below` comparison the Overworld uses, but the Surface
//! stage also runs the eroded badlands and frozen ocean extensions against the
//! same block access, and those read a column the bounded window truncates.
//! This test measures the disagreement instead of assuming it away.
//!
//! Fixtures are declared here rather than discovered at random: seed 1,
//! Overworld, and twelve chunks chosen from a fixed search over a declared
//! region so that ordinary land, badlands, frozen ocean and steep borders are
//! each represented.

use std::fmt::Write as _;

use steel_worldgen::biomes::BiomeSourceKind;
use steel_worldgen::surface_sampler::{SurfaceDimension, SurfaceSampler};
use steel_worldgen::surface_signal::{
    DEFAULT_SURFACE_SIGNAL_HEADROOM, DEFAULT_SURFACE_SIGNAL_LOOKAHEAD,
};

/// Declared fixture seed.
const SEED: u64 = 1;
/// Declared search region, in chunks, scanned in a fixed order.
const SEARCH_RADIUS_CHUNKS: i32 = 320;
/// Declared search step, in chunks.
const SEARCH_STEP_CHUNKS: i32 = 5;
/// Chunks kept per terrain category.
const CHUNKS_PER_CATEGORY: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Category {
    OrdinaryLand,
    Badlands,
    FrozenOcean,
    SteepBorder,
}

const CATEGORIES: [Category; 4] = [
    Category::OrdinaryLand,
    Category::Badlands,
    Category::FrozenOcean,
    Category::SteepBorder,
];

/// Classifies a chunk from its centre biome and its generated height relief.
fn classify(biome_path: &str, height_span: i32) -> Option<Category> {
    if biome_path.contains("badlands") {
        return Some(Category::Badlands);
    }
    if biome_path.contains("frozen_ocean") {
        return Some(Category::FrozenOcean);
    }
    if height_span >= 24 {
        return Some(Category::SteepBorder);
    }
    if biome_path.contains("plains") || biome_path.contains("forest") {
        return Some(Category::OrdinaryLand);
    }
    None
}

/// Picks the declared fixtures by scanning the declared region in a fixed order.
fn select_fixtures(sampler: &SurfaceSampler) -> Vec<(Category, i32, i32)> {
    let biome_source = BiomeSourceKind::overworld(SEED);
    let mut chosen: Vec<(Category, i32, i32)> = Vec::new();
    for chunk_z in
        (-SEARCH_RADIUS_CHUNKS..=SEARCH_RADIUS_CHUNKS).step_by(SEARCH_STEP_CHUNKS as usize)
    {
        for chunk_x in
            (-SEARCH_RADIUS_CHUNKS..=SEARCH_RADIUS_CHUNKS).step_by(SEARCH_STEP_CHUNKS as usize)
        {
            if CATEGORIES.iter().all(|category| {
                chosen.iter().filter(|(kind, ..)| kind == category).count() >= CHUNKS_PER_CATEGORY
            }) {
                return chosen;
            }
            let mut chunk_biomes = biome_source.chunk_sampler();
            let biome = chunk_biomes.sample(chunk_x * 4 + 2, 16, chunk_z * 4 + 2);
            let path = biome.key.path.to_string();
            // Cheap relief probe: the biome tile is the approximate-height path.
            let tile = sampler.biome_tile(chunk_x * 16, chunk_z * 16, 16, 4);
            let span = i32::from(tile.heights.iter().copied().max().unwrap_or(0))
                - i32::from(tile.heights.iter().copied().min().unwrap_or(0));
            let Some(category) = classify(&path, span) else {
                continue;
            };
            if chosen.iter().filter(|(kind, ..)| *kind == category).count() < CHUNKS_PER_CATEGORY {
                chosen.push((category, chunk_x, chunk_z));
            }
        }
    }
    chosen
}

#[test]
fn bounded_surface_signal_matches_generated_columns() {
    let sampler = SurfaceSampler::new(SEED, SurfaceDimension::Overworld);
    let fixtures = select_fixtures(&sampler);
    assert!(
        !fixtures.is_empty(),
        "the declared search region produced no fixtures"
    );

    // Sweep the lookahead so the depth the rules actually need is measured
    // rather than asserted from reading them.
    let mut report = String::new();
    let mut exact_at: Option<(i32, i32)> = None;
    let mut default_exact = false;
    for (lookahead, headroom) in [
        (1, 0),
        (1, 1),
        (1, 2),
        (1, 4),
        (1, 8),
        (1, 16),
        (2, 8),
        (8, 8),
    ] {
        let mut columns = 0_u64;
        let mut height_mismatches = 0_u64;
        let mut state_mismatches = 0_u64;
        let mut skipped = 0_u64;
        let mut full = 0_u64;
        let mut worst: Option<(Category, i32, i32, i16, i16)> = None;
        for &(category, chunk_x, chunk_z) in &fixtures {
            let generated = sampler.generated_surface_columns(chunk_x, chunk_z);
            let (bounded, stats) =
                sampler.surface_signal_chunk(chunk_x, chunk_z, lookahead, headroom);
            assert_eq!(generated.len(), bounded.len());
            skipped += stats.skipped_density_evaluations();
            full += stats.full_density_evaluations;
            for (index, (expected, actual)) in generated.iter().zip(bounded.iter()).enumerate() {
                columns += 1;
                if expected.0 != actual.0 {
                    height_mismatches += 1;
                    if worst.is_none() {
                        worst = Some((
                            category,
                            chunk_x * 16 + (index % 16) as i32,
                            chunk_z * 16 + (index / 16) as i32,
                            expected.0,
                            actual.0,
                        ));
                    }
                }
                if expected.1 != actual.1 || expected.2 != actual.2 {
                    state_mismatches += 1;
                }
            }
        }
        let _ = writeln!(
            report,
            "lookahead {lookahead} headroom {headroom}: {columns} columns, \
             {height_mismatches} height mismatches, {state_mismatches} state mismatches, \
             skipped {skipped} of {full} density evaluations"
        );
        if let Some((category, x, z, expected, actual)) = worst {
            let _ = writeln!(
                report,
                "  first height mismatch at {x},{z} ({category:?}): generated {expected}, bounded {actual}"
            );
        }
        if height_mismatches == 0 && state_mismatches == 0 {
            if exact_at.is_none() {
                exact_at = Some((lookahead, headroom));
            }
            if lookahead == DEFAULT_SURFACE_SIGNAL_LOOKAHEAD
                && headroom == DEFAULT_SURFACE_SIGNAL_HEADROOM
            {
                default_exact = true;
            }
        }
    }

    println!("fixtures: {fixtures:?}");
    print!("{report}");
    // The smallest exact headroom depends on which columns are sampled, so
    // this asserts that the documented defaults are exact rather than that
    // they are minimal. The sweep above is printed so a change in the smallest
    // exact value is visible without failing the run.
    assert!(
        default_exact,
        "the documented defaults no longer reproduce the generated columns:\n{report}"
    );
    assert!(
        exact_at.is_some(),
        "no swept window reproduced the generated columns exactly:\n{report}"
    );
}
