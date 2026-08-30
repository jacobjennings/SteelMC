# CHANGES

- `steel-worldgen/src/surface_sampler.rs`: added cached coarse tile generation that retains pre-carver samples without running carvers or vegetation, lazily stores post-carver chunks for full tiles, and includes parity and timing harnesses.
- `steel-worldgen-wasm/src/lib.rs`: exposed `terrain_tile_coarse` and added an optional validated `cache_capacity` constructor argument whose omission preserves capacity 160.
- `REPORT.md`: records the requested changes, reproducible evidence, measurements, deviations, generalisations, and recommendations.

# EVIDENCE

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen coarse_tiles_match_full_tiles_except_vegetation -- --nocapture
```

Output tail:

```text
running 1 test
test surface_sampler::tests::coarse_tiles_match_full_tiles_except_vegetation ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 95 filtered out; finished in 113.11s
```

This committed test covers `(0, 0, 64)`, `(7, 11, 64)`, `(-71, -93, 64)`, and `(256, 256, 256)` at resolution 1. It compares `samples_per_side`, `heights`, `colors`, `biomes`, `biome_indices`, `present`, `surface_blocks`, and `min_y`, and asserts that coarse `vegetation_blocks` is empty.

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen cached_tiles_are_identical_to_uncached_tiles -- --nocapture
```

Output tail:

```text
running 1 test
test surface_sampler::tests::cached_tiles_are_identical_to_uncached_tiles ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 95 filtered out; finished in 168.30s
```

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt --all --check; cargo check -p steel-worldgen; cargo check -p steel-worldgen-wasm; git diff --check; git status --short; git log -3 --oneline
```

Output:

```text
    Finished `dev` profile [unoptimized] target(s) in 0.07s
    Finished `dev` profile [unoptimized] target(s) in 0.08s
294f34ff0 Skip carvers for coarse surface chunks
ddbd5b6dc Test and measure coarse surface tiles
9b6a6f8f6 Add coarse surface tile generation
```

The empty output from `cargo fmt --all --check`, `git diff --check`, and `git status --short` indicates that those commands found no formatting diff, whitespace error, or uncommitted file at that point.

# MEASUREMENTS

The committed harness is `surface_sampler::tests::measure_coarse_surface_tiles` in `steel-worldgen/src/surface_sampler.rs`. It performs three repetitions, alternates full/coarse order, takes each median, and prints its derived ratio.

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo test --release -p steel-worldgen measure_coarse_surface_tiles -- --ignored --nocapture
```

Output tail:

```text
running 1 test
contiguous_4x4_64 full=7923.430 ms coarse=2391.053 ms ratio=3.314x
test surface_sampler::tests::measure_coarse_surface_tiles ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 95 filtered out; finished in 30.53s
```

# DEVIATIONS

- The cache entry's post-carver chunk became optional so coarse generation can cache pre-carver columns without carving. A later full request lazily regenerates and carves that chunk; this was necessary to make “does not carve” true while retaining the existing cache type and full-tile output.
- No files outside the two scoped Rust files and this required report were changed.

# GENERALISATIONS

- Tile parity is established only for seed 1 in the overworld, resolution 1, and the four committed cases listed above; it was not measured for other seeds, dimensions, or resolutions.
- The 3.314x timing ratio applies only to this release-profile run of three repetitions for the contiguous 4-by-4 grid of 64-block overworld tiles at seed 1 and resolution 1; it does not establish performance on other machines, browser/WASM builds, dimensions, seeds, cache capacities, or traversal patterns.
- Constructor backward compatibility is established by Rust compilation and wasm-bindgen's optional trailing `Option<u32>` argument shape; generated JavaScript was not executed in a browser during this task.

# RECOMMENDATIONS

- Run the committed timing harness on the target browser/WASM worker hardware before selecting a production cache capacity.
