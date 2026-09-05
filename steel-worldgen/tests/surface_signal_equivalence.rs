//! Compares the prototype bounded surface signal against the generated path.
//!
//! The bounded producer exists to be checked, not trusted. Depth reasoning
//! from the generated Surface rules says a two-block window should answer
//! every `stone_depth_below` comparison the Overworld uses, but the Surface
//! stage also runs the eroded badlands and frozen ocean extensions against the
//! same block access, and those read and write outside the window a bounded
//! producer would otherwise choose. This test measures the disagreement
//! instead of assuming it away, and it measures what the bound costs.
//!
//! Fixtures are declared here rather than discovered at random: seed 1,
//! Overworld, and twelve chunks chosen from a fixed search over a declared
//! region so that ordinary land, badlands, frozen ocean and steep borders are
//! each represented. The search has a declared probe budget and reports a
//! category it could not fill rather than widening until it succeeds.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use steel_worldgen::biomes::BiomeSourceKind;
use steel_worldgen::surface_sampler::{SurfaceDimension, SurfaceSampler};
use steel_worldgen::surface_signal::{
    DEFAULT_SURFACE_SIGNAL_HEADROOM, DEFAULT_SURFACE_SIGNAL_LOOKAHEAD, SurfaceSignalWindow,
};

/// Declared fixture seed.
const SEED: u64 = 1;
/// Declared search step, in chunks.
const SEARCH_STEP_CHUNKS: i32 = 8;
/// Declared probe budget: chunk centres whose biome the search may sample.
///
/// Badlands is rare, and the first pass over a 320-chunk radius found none on
/// this seed. The budget is what makes widening the search a declared cost
/// rather than an open-ended hunt.
const PROBE_BUDGET: usize = 400_000;
/// Chunks kept per terrain category.
const CHUNKS_PER_CATEGORY: usize = 3;
/// Height span, in blocks, above which a chunk counts as a steep border.
const STEEP_SPAN_BLOCKS: i32 = 24;

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

/// The category a chunk's centre biome alone decides.
///
/// Steep borders are not decided here: they need the generated relief, which
/// costs far more than a biome sample, so they are resolved only for chunks
/// this function leaves open.
fn category_from_biome(biome_path: &str) -> Option<Category> {
    if biome_path.contains("badlands") {
        return Some(Category::Badlands);
    }
    if biome_path.contains("frozen_ocean") {
        return Some(Category::FrozenOcean);
    }
    if biome_path.contains("plains") || biome_path.contains("forest") {
        return Some(Category::OrdinaryLand);
    }
    None
}

/// Chunk centres in an outward square spiral, so a wider search reuses the
/// order of a narrower one rather than sampling a different region.
fn spiral_chunks(step: i32) -> impl Iterator<Item = (i32, i32)> {
    (0..).flat_map(move |ring: i32| {
        let extent = ring * step;
        let edge: Vec<(i32, i32)> = if ring == 0 {
            vec![(0, 0)]
        } else {
            let mut edge = Vec::new();
            let mut value = -extent;
            while value <= extent {
                edge.push((value, -extent));
                edge.push((value, extent));
                if value != -extent && value != extent {
                    edge.push((-extent, value));
                    edge.push((extent, value));
                }
                value += step;
            }
            edge
        };
        edge
    })
}

/// Picks the declared fixtures by scanning outward in a fixed order.
///
/// Returns the fixtures and the categories the probe budget did not fill.
fn select_fixtures(sampler: &SurfaceSampler) -> (Vec<(Category, i32, i32)>, Vec<Category>) {
    let biome_source = BiomeSourceKind::overworld(SEED);
    let mut chunk_biomes = biome_source.chunk_sampler();
    let mut chosen: Vec<(Category, i32, i32)> = Vec::new();
    let mut probes = 0_usize;

    let filled = |chosen: &[(Category, i32, i32)], category: Category| {
        chosen.iter().filter(|(kind, ..)| *kind == category).count() >= CHUNKS_PER_CATEGORY
    };

    for (chunk_x, chunk_z) in spiral_chunks(SEARCH_STEP_CHUNKS) {
        if probes >= PROBE_BUDGET || CATEGORIES.iter().all(|&kind| filled(&chosen, kind)) {
            break;
        }
        probes += 1;
        let biome = chunk_biomes.sample(chunk_x * 4 + 2, 16, chunk_z * 4 + 2);
        let path = biome.key.path.to_string();

        if let Some(category) = category_from_biome(&path) {
            if !filled(&chosen, category) {
                chosen.push((category, chunk_x, chunk_z));
            }
            continue;
        }

        // Everything else is a steep-border candidate, and only those pay for
        // the relief probe.
        if filled(&chosen, Category::SteepBorder) {
            continue;
        }
        let tile = sampler.biome_tile(chunk_x * 16, chunk_z * 16, 16, 4);
        let span = i32::from(tile.heights.iter().copied().max().unwrap_or(0))
            - i32::from(tile.heights.iter().copied().min().unwrap_or(0));
        if span >= STEEP_SPAN_BLOCKS {
            chosen.push((Category::SteepBorder, chunk_x, chunk_z));
        }
    }

    let missing = CATEGORIES
        .into_iter()
        .filter(|&kind| !filled(&chosen, kind))
        .collect();
    println!("fixture search probed {probes} chunk centres of a {PROBE_BUDGET} budget");
    (chosen, missing)
}

/// One sweep entry: the window under test and what it produced.
struct SweepRow {
    label: String,
    columns: u64,
    height_mismatches: u64,
    state_mismatches: u64,
    skipped: u64,
    full: u64,
    windowed_slots: usize,
    full_slots: usize,
    first_mismatch: Option<String>,
}

fn sweep(
    sampler: &SurfaceSampler,
    fixtures: &[(Category, i32, i32)],
    label: String,
    lookahead: i32,
    window: SurfaceSignalWindow,
) -> SweepRow {
    let mut row = SweepRow {
        label,
        columns: 0,
        height_mismatches: 0,
        state_mismatches: 0,
        skipped: 0,
        full: 0,
        windowed_slots: 0,
        full_slots: 0,
        first_mismatch: None,
    };
    for &(category, chunk_x, chunk_z) in fixtures {
        let generated = sampler.generated_surface_columns(chunk_x, chunk_z);
        let (bounded, stats) = sampler.surface_signal_chunk(chunk_x, chunk_z, lookahead, window);
        assert_eq!(generated.len(), bounded.len());
        row.skipped += stats.skipped_density_evaluations();
        row.full += stats.full_density_evaluations;
        row.windowed_slots += stats.windowed_block_slots;
        row.full_slots += stats.full_block_slots;
        for (index, (expected, actual)) in generated.iter().zip(bounded.iter()).enumerate() {
            row.columns += 1;
            let height_differs = expected.0 != actual.0;
            let state_differs = expected.1 != actual.1 || expected.2 != actual.2;
            if height_differs {
                row.height_mismatches += 1;
            }
            if state_differs {
                row.state_mismatches += 1;
            }
            if (height_differs || state_differs) && row.first_mismatch.is_none() {
                row.first_mismatch = Some(format!(
                    "{},{} ({category:?}): generated height {} state {:?}, bounded height {} state {:?}",
                    chunk_x * 16 + (index % 16) as i32,
                    chunk_z * 16 + (index / 16) as i32,
                    expected.0,
                    expected.1,
                    actual.0,
                    actual.1,
                ));
            }
        }
    }
    row
}

#[test]
fn bounded_surface_signal_matches_generated_columns() {
    let sampler = SurfaceSampler::new(SEED, SurfaceDimension::Overworld);
    let (fixtures, missing) = select_fixtures(&sampler);
    assert!(
        !fixtures.is_empty(),
        "the declared search region produced no fixtures"
    );
    println!("fixtures: {fixtures:?}");
    if !missing.is_empty() {
        println!("categories the probe budget did not fill: {missing:?}");
    }

    let mut rows = Vec::new();
    for headroom in [0, 4, 8, 16, 32] {
        rows.push(sweep(
            &sampler,
            &fixtures,
            format!("fixed headroom {headroom}"),
            DEFAULT_SURFACE_SIGNAL_LOOKAHEAD,
            SurfaceSignalWindow::Fixed(headroom),
        ));
    }
    for lookahead in [2, 8] {
        rows.push(sweep(
            &sampler,
            &fixtures,
            format!("lookahead {lookahead}, fixed headroom {DEFAULT_SURFACE_SIGNAL_HEADROOM}"),
            lookahead,
            SurfaceSignalWindow::Fixed(DEFAULT_SURFACE_SIGNAL_HEADROOM),
        ));
    }
    rows.push(sweep(
        &sampler,
        &fixtures,
        "derived".to_string(),
        DEFAULT_SURFACE_SIGNAL_LOOKAHEAD,
        SurfaceSignalWindow::Derived,
    ));

    let mut report = String::new();
    for row in &rows {
        let _ = writeln!(
            report,
            "{}: {} columns, {} height mismatches, {} state mismatches, \
             skipped {} of {} density evaluations, {} of {} block slots kept",
            row.label,
            row.columns,
            row.height_mismatches,
            row.state_mismatches,
            row.skipped,
            row.full,
            row.windowed_slots,
            row.full_slots
        );
        if let Some(first) = &row.first_mismatch {
            let _ = writeln!(report, "  first mismatch at {first}");
        }
    }
    print!("{report}");

    let derived = rows
        .last()
        .expect("the derived window is the last swept row");
    assert_eq!(derived.label, "derived");
    assert_eq!(
        (derived.height_mismatches, derived.state_mismatches),
        (0, 0),
        "the derived window no longer reproduces the generated columns:\n{report}"
    );
}

/// Paired cold and warm cost of the bounded producer against the generated one.
///
/// The arms are interleaved within each repetition rather than run in blocks,
/// so a drift in machine load moves both by about the same amount instead of
/// landing entirely on whichever ran second. Cold builds a fresh sampler for
/// every chunk, so it includes sampler construction and an empty cache. Warm
/// repeats a chunk on the sampler that has just produced it.
#[test]
fn bounded_surface_signal_cost() {
    const REPETITIONS: usize = 3;
    let selector = SurfaceSampler::new(SEED, SurfaceDimension::Overworld);
    let (fixtures, _missing) = select_fixtures(&selector);
    let measured: Vec<(Category, i32, i32)> = CATEGORIES
        .into_iter()
        .filter_map(|kind| fixtures.iter().find(|(found, ..)| *found == kind).copied())
        .collect();
    drop(selector);

    let mut bounded_cold = Duration::ZERO;
    let mut full_cold = Duration::ZERO;
    let mut bounded_warm = Duration::ZERO;
    let mut full_warm = Duration::ZERO;
    let mut windowed_slots = 0_usize;
    let mut full_slots = 0_usize;

    for _ in 0..REPETITIONS {
        for &(_, chunk_x, chunk_z) in &measured {
            // Cold: a fresh sampler each time, so construction and an empty
            // cache are inside the measurement for both arms.
            let sampler = SurfaceSampler::new(SEED, SurfaceDimension::Overworld);
            let start = Instant::now();
            let (_, stats) = sampler.surface_signal_chunk(
                chunk_x,
                chunk_z,
                DEFAULT_SURFACE_SIGNAL_LOOKAHEAD,
                SurfaceSignalWindow::Derived,
            );
            bounded_cold += start.elapsed();
            windowed_slots += stats.windowed_block_slots;
            full_slots += stats.full_block_slots;

            let start = Instant::now();
            let _ = sampler.surface_signal_chunk(
                chunk_x,
                chunk_z,
                DEFAULT_SURFACE_SIGNAL_LOOKAHEAD,
                SurfaceSignalWindow::Derived,
            );
            bounded_warm += start.elapsed();
            drop(sampler);

            let sampler = SurfaceSampler::new(SEED, SurfaceDimension::Overworld);
            let start = Instant::now();
            let _ = sampler.generated_surface_columns(chunk_x, chunk_z);
            full_cold += start.elapsed();

            let start = Instant::now();
            let _ = sampler.generated_surface_columns(chunk_x, chunk_z);
            full_warm += start.elapsed();
        }
    }

    let samples = (REPETITIONS * measured.len()) as u32;
    println!("cost over {samples} paired samples on {} chunks", measured.len());
    println!(
        "  cold: bounded {:?} per chunk, generated {:?} per chunk",
        bounded_cold / samples,
        full_cold / samples
    );
    println!(
        "  warm: bounded {:?} per chunk, generated {:?} per chunk",
        bounded_warm / samples,
        full_warm / samples
    );
    println!(
        "  block slots: bounded {} of generated {} ({:.1} percent kept)",
        windowed_slots,
        full_slots,
        100.0 * windowed_slots as f64 / full_slots as f64
    );
}
