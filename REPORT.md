# b2-chunk-cache report

## CHANGES

- `steel-worldgen/src/surface_sampler.rs`: added a bounded 96-entry LRU `SurfaceChunkCache`, a caching tile entry point, exact-output coverage, and a reproducible ignored measurement harness.
- `steel-worldgen-wasm/src/lib.rs`: made each `SteelWorldgen` own the cache in a `RefCell` and reuse it across terrain tile calls.
- `REPORT.md`: records the implementation, commands, observed output, and limitations.

Each cached entry retains an immutable pre-carver chunk for `top_surface` sampling and a pristine post-carver chunk for vegetation reads. Each request clones the post-carver chunk into its mutable vegetation region, so feature writes cannot alter either cached representation. The named default is 96 entries. The source comment records `2 * 192 KiB * 96 = about 36 MiB` per worker and about 576 MiB for 16 workers.

## EVIDENCE

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo check -p steel-worldgen
```

Observed tail:

```text
Compiling steel-worldgen v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b2-chunk-cache/steel-worldgen)
```

The command exited successfully.

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo check -p steel-worldgen-wasm
```

Observed tail:

```text
Checking steel-worldgen v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b2-chunk-cache/steel-worldgen)
Checking steel-worldgen-wasm v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b2-chunk-cache/steel-worldgen-wasm)
Finished `dev` profile [unoptimized] target(s) in 7.05s
```

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p steel-worldgen
```

Observed tail:

```text
test surface_sampler::tests::cached_tiles_are_identical_to_uncached_tiles ... ok

test result: ok. 92 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 172.96s

Doc-tests steel_worldgen

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The exact-output test compared all requested fields for adjacent `(0, 0)` and `(64, 0)` 64-block tiles, negative-origin `(-128, -64)`, non-chunk-aligned `(7, 11)`, and a 256-block tile.

## MEASUREMENTS

The committed harness is `surface_sampler::tests::measure_surface_chunk_cache` in `steel-worldgen/src/surface_sampler.rs`. Exact invocation:

```text
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p steel-worldgen measure_surface_chunk_cache --release -- --ignored --nocapture
```

Observed output:

```text
grid cached=9509.172 ms uncached=12488.738 ms ratio=1.313x; useful_chunk cached=37.145 ms uncached=48.784 ms
isolated cached=692.792 ms uncached=634.258 ms ratio=0.916x
test surface_sampler::tests::measure_surface_chunk_cache ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 92 filtered out; finished in 70.32s
```

These are medians of three release-mode repetitions, with a fresh Overworld seed-1 sampler for each cached and uncached measurement. The contiguous grid contains 16 tiles and 256 useful chunks. On this run, useful-chunk time fell from 48.784 ms to 37.145 ms, a 23.86% reduction. The isolated cached path was 9.23% slower in this measurement, so the requested cold-path non-regression was not demonstrated.

## DEVIATIONS

- Ran `cargo check -p steel-worldgen` and `cargo check -p steel-worldgen-wasm` in addition to the required test to verify both scoped crates compile.
- The isolated-tile result did not meet the brief's desired no-slowdown evidence; it is reported without adjustment.
- `REPORT.md` was created with the repository patch mechanism rather than a bash heredoc because the active environment requires file edits through that mechanism.

## GENERALISATIONS

- The statement that feature writes cannot contaminate cached representations follows from code inspection of the clone boundary; runtime equality was tested only on the five Overworld seed-1 cases listed above, not every seed, dimension, coordinate, size, or resolution.
- The memory figures use the brief's approximately 192 KiB block-state payload per chunk and count two full chunk copies per entry; allocator, `HashMap`, heightmap, and temporary per-request clone overhead were not measured.
- Timing claims apply only to this machine, release build, Overworld seed 1, resolution 1, the stated coordinates, and three repetitions; browser/WASM timing and other seeds or dimensions were not measured.
- LRU boundedness follows from code inspection and the 96-entry capacity; eviction ordering was not separately instrumented or benchmarked.

## RECOMMENDATIONS

- Measure the same harness in the browser/WASM worker environment before translating the native 1.313x grid ratio into viewer-settle expectations.
- Investigate the isolated-tile variance with a higher-repetition, order-balanced benchmark if cold-path performance is a release gate.
