# b6-biome-tile report

## CHANGES

- `steel-worldgen/src/surface_sampler.rs`: added `SurfaceSampler::biome_tile`, which samples real biome-source data at the preliminary density-router surface estimate while skipping density-column fill, aquifers, ore veins, surface rules, carvers, and vegetation; added a palette/index regression and the committed three-path measurement harness.
- `steel-worldgen-wasm/src/lib.rs`: exported `terrain_tile_biome` with the same grid arguments as `terrain_tile`, empty surface/vegetation block arrays, and `heightApproximation: "preliminary_surface_level"` so its heights do not claim exactness.
- `REPORT.md`: recorded the implementation, reproducible evidence, measurements, limitation, and recommendations.

## EVIDENCE

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo check -p steel-worldgen -p steel-worldgen-wasm 2>&1 | tail -30
```

Output:

```text
    Checking steel-worldgen v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b6-biome-tile/steel-worldgen)
    Checking steel-worldgen-wasm v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b6-biome-tile/steel-worldgen-wasm)
    Finished `dev` profile [unoptimized] target(s) in 0.62s
```

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p steel-worldgen biome_tile_matches_full_biomes_at_sample_positions -- --nocapture 2>&1 | tail -30
```

Output:

```text
    Finished `test` profile [unoptimized] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/build/steel-worldgen/2f0af61d91c0ea5a/out/steel_worldgen-2f0af61d91c0ea5a)

running 1 test
test surface_sampler::tests::biome_tile_matches_full_biomes_at_sample_positions ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 96 filtered out; finished in 10.83s
```

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p steel-worldgen -p steel-worldgen-wasm
```

Output tail:

```text
test result: ok. 94 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 167.04s

     Running unittests src/lib.rs (target/debug/build/steel-worldgen-wasm/a743b6c4723a01de/out/steel_worldgen_wasm-a743b6c4723a01de)

running 3 tests
test tests::generated_marker_response_has_unique_complete_bounds ... ok
test tests::terrain_tile_serializes_canonical_cherry_vegetation_states ... ok
test tests::terrain_tile_serializes_final_surface_blocks_parallel_to_samples ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.00s

   Doc-tests steel_worldgen

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests steel_worldgen_wasm

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The targeted regression covers seed 1 Overworld at `(0,0)`, size 64, resolution 1 and `(-71,-93)`, size 64, resolution 4. The broader committed measurement found mismatches and therefore disproves that preliminary-surface Y sampling preserves full-path biome indices on all measured ground.

## MEASUREMENTS

The committed harness is `surface_sampler::tests::measure_coarse_surface_tiles` in `steel-worldgen/src/surface_sampler.rs`.

Exact invocation:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p steel-worldgen measure_coarse_surface_tiles -- --ignored --nocapture
```

Output:

```text
contiguous_4x4_64 full=84136.411 ms coarse=18642.635 ms biome=543.340 ms full/biome=154.850x coarse/biome=34.311x
single_256 full=67595.601 ms coarse=18192.576 ms biome=523.978 ms full/biome=129.005x coarse/biome=34.720x
single_256_accuracy samples=66049 height_exact=96 height_median_abs_error=14 height_max_abs_error=52 biome_index_mismatches=64 present_mismatches=0
test surface_sampler::tests::measure_coarse_surface_tiles ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 96 filtered out; finished in 641.89s
```

These are debug-profile wall-clock medians over three order-rotated repetitions, seed 1 Overworld. The 4-by-4 workload covers contiguous 64-block tiles from `(0,0)` through `(192,192)` at resolution 1. The single-tile workload covers `(0,0)` through `(256,256)` at resolution 1. Ratios and accuracy figures are printed by the committed harness.

Chosen height source: `preliminary_surface_level`. On the measured 256-block tile it was exact for 96 of 66,049 samples, with median absolute difference 14 blocks and maximum absolute difference 52 blocks from the full path's highest non-fluid solid. It is an estimate from the preliminary density router, not an exact generated surface. It was chosen over flat sea level because it retains measured relief at low cost; an exact or height-only density-column approach was not implemented, because it would need a separate measured prototype to establish cost and exact solid/aquifer semantics.

Correct negative result: the brief requires biome palette/indices to match the full path. Although the scoped regression grounds passed, the larger measured tile had 64 biome-index mismatches because biome selection is vertically sensitive and the approximate height changes quart Y. Therefore this implementation does not establish acceptance item 1 generally. Presence had zero mismatches on this one measured Overworld tile, but that does not establish accurate presence across dimensions or terrain without solid ground.

## DEVIATIONS

- Added the optional serialized `heightApproximation` marker, which was not named explicitly but is required to prevent the approximate height field from claiming precision it lacks. Exact/coarse responses omit it and their generation paths were not changed.
- Extended the existing ignored coarse measurement test rather than adding a script, because the file scope forbids `scripts/` and permits `steel-worldgen/src/surface_sampler.rs`.
- Measured preliminary-surface accuracy within the three-path harness in addition to timing, because selecting a defensible approximation required direct comparison with exact heights.
- Did not implement `NoiseChunk::fill` height-only or flat-sea-level variants. Implementing multiple speculative paths would exceed the bounded goal after preliminary-surface timing and accuracy exposed the core vertical-biome conflict.

## GENERALISATIONS

- “Biome is more than an order of magnitude cheaper than coarse” is supported only for the two seed-1 Overworld debug-profile workloads printed above; it was not tested across other seeds, dimensions, release/Wasm builds, machines, resolutions, or coordinates.
- “Preliminary surface retains relief” means its output varies and was compared against exact heights on the measured seed-1 256-block Overworld tile; visual acceptability at a 12,000-block viewer span was not tested because the viewer is out of scope.
- “The new path skips density-column fill, aquifers, ore veins, surface rules, carvers, and vegetation” follows the implemented call graph; timing attribution to each skipped stage was not measured separately.
- “Presence matched” applies only to the 66,049 samples of the measured seed-1 Overworld tile; Nether, End, void/island boundaries, other seeds, and other coordinates were not measured.
- The targeted palette/index regression proves equality only for its two listed grounds. It does not override the 64 mismatches found by the broader measurement.

## RECOMMENDATIONS

- Before integration, decide whether overview biome correctness means sampling a documented approximate surface Y or strict equality with the full generated-surface Y. Both cannot be claimed from the current preliminary-height path.
- If strict equality is required, prototype and measure a height-only density/aquifer query, ideally evaluating only vertically ambiguous biome samples, then replace the current preliminary-Y biome lookup only if the committed harness proves both equality and the required speedup.
- Add viewer-side use of `terrain_tile_biome` only after resolving the 64 measured biome mismatches and validating the explicit `heightApproximation` contract; viewer changes are outside this task's scope.
- Measure presence in Nether and End before claiming it accurate there; preliminary surface alone is not evidence of a solid column in island/void terrain.
