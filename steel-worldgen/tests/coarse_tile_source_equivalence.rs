//! Whole-tile equivalence for the bounded Coarse surface producer.
//!
//! `surface_signal_equivalence.rs` compares one chunk's columns. This compares
//! what a Coarse tile actually is: its heights, its presence flags, its colours,
//! both of its palettes and both of its index arrays, produced through
//! `coarse_tile_with_cache_from` with each source in turn.
//!
//! It also exercises the two orders that share a cache. A Coarse tile followed
//! by a Full tile must produce the Full tile the generated path alone produces,
//! because a cheap Coarse entry must never decide what Full shows. A Full tile
//! followed by a Coarse tile must produce the same Coarse tile, because the
//! retained generated entry is reused rather than displaced.
//!
//! The fixtures are the twelve chunks `docs/research/steelmc-cheap-signals/surface-bounds.md`
//! recorded, hard-coded here rather than searched for again.

use steel_worldgen::surface_sampler::{
    CoarseSurfaceSource, SurfaceChunkCache, SurfaceDimension, SurfaceSampler, SurfaceTile,
};

const SEED: u64 = 1;

/// The twelve fixture chunks, three per terrain category, on seed 1.
const FIXTURES: [(&str, i32, i32); 12] = [
    ("ordinary land", 16, 8),
    ("ordinary land", 16, 16),
    ("ordinary land", 24, 0),
    ("steep border", 8, 8),
    ("steep border", 40, -40),
    ("steep border", 40, -48),
    ("frozen ocean", 24, -96),
    ("frozen ocean", 8, -128),
    ("frozen ocean", -8, -136),
    ("eroded badlands", 360, -552),
    ("eroded badlands", 368, -552),
    ("eroded badlands", 376, -552),
];

/// The two grids the viewer asks for: its fine tile and its far tile.
const GRIDS: [(u32, u32); 2] = [(64, 1), (256, 4)];

fn sampler() -> SurfaceSampler {
    SurfaceSampler::new(SEED, SurfaceDimension::Overworld)
}

fn origin(chunk_x: i32, chunk_z: i32) -> (i32, i32) {
    (chunk_x * 16, chunk_z * 16)
}

/// Fails with the first differing field rather than one opaque inequality.
fn assert_same_tile(context: &str, expected: &SurfaceTile, actual: &SurfaceTile) {
    assert_eq!(
        expected.samples_per_side, actual.samples_per_side,
        "{context}: samples_per_side"
    );
    assert_eq!(expected.min_y, actual.min_y, "{context}: min_y");
    assert_eq!(
        expected.heights.len(),
        actual.heights.len(),
        "{context}: heights length"
    );
    for (index, (left, right)) in expected.heights.iter().zip(&actual.heights).enumerate() {
        assert_eq!(left, right, "{context}: height at sample {index}");
    }
    for (index, (left, right)) in expected.present.iter().zip(&actual.present).enumerate() {
        assert_eq!(left, right, "{context}: presence at sample {index}");
    }
    for (index, (left, right)) in expected.colors.iter().zip(&actual.colors).enumerate() {
        assert_eq!(left, right, "{context}: colour byte {index}");
    }
    assert_eq!(
        expected.surface_blocks, actual.surface_blocks,
        "{context}: surface block palette"
    );
    assert_eq!(
        expected.surface_block_indices, actual.surface_block_indices,
        "{context}: surface block indices"
    );
    assert_eq!(expected.biomes, actual.biomes, "{context}: biome palette");
    assert_eq!(
        expected.biome_indices, actual.biome_indices,
        "{context}: biome indices"
    );
    assert_eq!(
        format!("{:?}", expected.vegetation_blocks),
        format!("{:?}", actual.vegetation_blocks),
        "{context}: vegetation blocks"
    );
    assert_eq!(
        format!("{:?}", expected.structure_blocks),
        format!("{:?}", actual.structure_blocks),
        "{context}: structure blocks"
    );
}

fn coarse(
    sampler: &SurfaceSampler,
    cache: &mut SurfaceChunkCache,
    origin: (i32, i32),
    grid: (u32, u32),
    source: CoarseSurfaceSource,
) -> SurfaceTile {
    sampler.coarse_tile_with_cache_from(cache, origin.0, origin.1, grid.0, grid.1, source)
}

#[test]
fn bounded_coarse_tiles_match_generated_coarse_tiles() {
    let sampler = sampler();
    for (size, resolution) in GRIDS {
        for (category, chunk_x, chunk_z) in FIXTURES {
            let origin = origin(chunk_x, chunk_z);
            let context = format!("{category} {chunk_x},{chunk_z} at {size}/{resolution}, cold");

            let mut generated_cache = SurfaceChunkCache::default();
            let expected = coarse(
                &sampler,
                &mut generated_cache,
                origin,
                (size, resolution),
                CoarseSurfaceSource::Generated,
            );

            let mut bounded_cache = SurfaceChunkCache::default();
            let actual = coarse(
                &sampler,
                &mut bounded_cache,
                origin,
                (size, resolution),
                CoarseSurfaceSource::Bounded,
            );
            assert_same_tile(&context, &expected, &actual);

            // Warm: the same tile again in the cache that just produced it.
            let warm_context = format!("{context} -> warm");
            let expected_warm = coarse(
                &sampler,
                &mut generated_cache,
                origin,
                (size, resolution),
                CoarseSurfaceSource::Generated,
            );
            let actual_warm = coarse(
                &sampler,
                &mut bounded_cache,
                origin,
                (size, resolution),
                CoarseSurfaceSource::Bounded,
            );
            assert_same_tile(&warm_context, &expected_warm, &actual_warm);
            assert_same_tile(&warm_context, &expected, &expected_warm);
            assert_eq!(
                bounded_cache.stats().misses,
                generated_cache.stats().misses,
                "{warm_context}: the two sources must miss on the same chunks"
            );
        }
    }
}

#[test]
fn a_bounded_coarse_entry_does_not_change_the_full_tile_after_it() {
    let sampler = sampler();
    let (size, resolution) = (64, 1);
    for (category, chunk_x, chunk_z) in FIXTURES {
        let origin = origin(chunk_x, chunk_z);
        let context = format!("{category} {chunk_x},{chunk_z} at {size}/{resolution}");

        // The reference: a Full tile that no Coarse request preceded.
        let mut alone = SurfaceChunkCache::default();
        let expected = sampler.tile_with_cache(&mut alone, origin.0, origin.1, size, resolution);

        for source in [CoarseSurfaceSource::Generated, CoarseSurfaceSource::Bounded] {
            let mut cache = SurfaceChunkCache::default();
            let _ = coarse(&sampler, &mut cache, origin, (size, resolution), source);
            let after = sampler.tile_with_cache(&mut cache, origin.0, origin.1, size, resolution);
            assert_same_tile(
                &format!("{context}: coarse {source:?} then full"),
                &expected,
                &after,
            );

            let replacements = cache.stats().bounded_surface_replacements;
            match source {
                CoarseSurfaceSource::Bounded => assert!(
                    replacements > 0,
                    "{context}: a full request must replace the bounded columns it found"
                ),
                CoarseSurfaceSource::Generated => assert_eq!(
                    replacements, 0,
                    "{context}: a generated entry must not be replaced"
                ),
            }
        }
    }
}

#[test]
fn a_full_tile_before_a_bounded_coarse_request_still_gives_the_coarse_tile() {
    let sampler = sampler();
    let (size, resolution) = (64, 1);
    for (category, chunk_x, chunk_z) in FIXTURES {
        let origin = origin(chunk_x, chunk_z);
        let context = format!("{category} {chunk_x},{chunk_z} at {size}/{resolution}");

        let mut alone = SurfaceChunkCache::default();
        let expected = coarse(
            &sampler,
            &mut alone,
            origin,
            (size, resolution),
            CoarseSurfaceSource::Generated,
        );

        for source in [CoarseSurfaceSource::Generated, CoarseSurfaceSource::Bounded] {
            let mut cache = SurfaceChunkCache::default();
            let _ = sampler.tile_with_cache(&mut cache, origin.0, origin.1, size, resolution);
            let after = coarse(&sampler, &mut cache, origin, (size, resolution), source);
            assert_same_tile(
                &format!("{context}: full then coarse {source:?}"),
                &expected,
                &after,
            );
            assert_eq!(
                cache.stats().bounded_surface_replacements,
                0,
                "{context}: a coarse request after a full one replaces nothing"
            );
        }
    }
}
