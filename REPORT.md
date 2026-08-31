# CHANGES

- `steel-worldgen/src/surface_sampler.rs`: Added committed peak-flat-memory and throughput measurement, and changed vegetation generation to retain a compact lossless halo while expanding only the current 3x3 chunk window.
- `REPORT.md`: Recorded the requested changes, commands, outputs, limitations, and recommendations.

# EVIDENCE

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen measure_worker_flat_chunk_peak_and_throughput -- --ignored --nocapture
```

Output:

```text
single_64 before_elapsed_ms=6092.507 after_elapsed_ms=5825.820 throughput_ratio=1.046x before_peak_live_flat_chunk_bytes=12648448 after_peak_live_flat_chunk_bytes=1778688 flat_memory_reduction=7.111x retained_payload_bytes=539372
single_256 before_elapsed_ms=76814.102 after_elapsed_ms=75627.216 throughput_ratio=1.016x before_peak_live_flat_chunk_bytes=79052800 after_peak_live_flat_chunk_bytes=1778688 flat_memory_reduction=44.444x retained_payload_bytes=1423016
contiguous_4x4_64 before_elapsed_ms=99185.782 after_elapsed_ms=96722.382 throughput_ratio=1.025x before_peak_live_flat_chunk_bytes=12648448 after_peak_live_flat_chunk_bytes=1778688 flat_memory_reduction=7.111x retained_payload_bytes=1423016
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 97 filtered out; finished in 178.24s
```

Commands and real output tails:

```text
$ export PATH="$HOME/.cargo/bin:$PATH"; cargo check -p steel-worldgen
Finished `dev` profile [unoptimized] target(s) in 0.08s

$ export PATH="$HOME/.cargo/bin:$PATH"; cargo check -p steel-worldgen-wasm
Checking steel-worldgen-wasm v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b8-worker-memory/steel-worldgen-wasm)
Finished `dev` profile [unoptimized] target(s) in 1.08s

$ export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen cached_tiles_are_identical_to_uncached_tiles -- --nocapture
test surface_sampler::tests::cached_tiles_are_identical_to_uncached_tiles ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 97 filtered out; finished in 184.38s

$ export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-core portable_carvers_match_native_cherry_grove_fixture -- --nocapture
test worldgen::chunk_stage_hashes::portable_carvers_match_native_cherry_grove_fixture ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2468 filtered out; finished in 5.48s

$ export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-core portable_selected_features_match_native_cherry_grove_fixture -- --nocapture
test worldgen::chunk_stage_hashes::portable_selected_features_match_native_cherry_grove_fixture ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2468 filtered out; finished in 10.53s

$ export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen-wasm
test tests::generated_marker_response_has_unique_complete_bounds ... ok
test tests::terrain_tile_serializes_canonical_cherry_vegetation_states ... ok
test tests::terrain_tile_serializes_final_surface_blocks_parallel_to_samples ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.95s
```

# MEASUREMENTS

The committed harness is `surface_sampler::tests::measure_worker_flat_chunk_peak_and_throughput` in `steel-worldgen/src/surface_sampler.rs`, invoked by the first EVIDENCE command. It records the baseline measured at commit `8527890aa` and prints all derived ratios itself.

- 64-block tile: 12,648,448 bytes before and 1,778,688 bytes after; harness-printed reduction 7.111x.
- 256-block tile: 79,052,800 bytes before and 1,778,688 bytes after; harness-printed reduction 44.444x.
- Contiguous 4x4 grid: 99,185.782 ms before and 96,722.382 ms after; harness-printed throughput ratio 1.025x.

# DEVIATIONS

- `steel-worldgen-wasm/src/lib.rs` required no change because it delegates tile generation to the sampler and owns no flat vegetation halo.
- The report was created with the repository patch mechanism rather than a bash heredoc because the active higher-priority file-editing rule requires that mechanism.
- In addition to the requested checks, the full `steel-worldgen-wasm` unit-test target was run to verify the serialized output boundary.

# GENERALISATIONS

- The claim that output is unchanged is tested by the existing cached-versus-uncached tile case, the native Carvers fixture, the native selected-Features cherry-grove fixture, and the three WASM tests. It was not tested for every seed, dimension, coordinate, or tile size.
- The flat-memory reduction is measured for seed 1, Overworld, origin 0/0, 64- and 256-block tiles in the native debug test build. It is not a measurement of total WASM linear memory or browser allocator high-water behavior.
- The throughput figures are one before run and one after run in the native debug test build on this machine. They are not a statistically powered performance result and do not establish a general speedup.
- The 3x3 live-window claim is measured through nine equal-size Overworld flat chunk payloads. It does not include compact cache payloads, allocator metadata, generation temporaries, output vectors, or the WASM runtime.

# RECOMMENDATIONS

- Re-run the live browser capture to measure the resulting WASM linear-memory high-water mark; this task measures the dominant flat allocation directly but cannot infer how much the browser's 29,491,200-byte high-water mark falls.
- Run a repeated optimized/WASM throughput benchmark if a release decision needs tighter performance confidence than the single debug-profile comparison.
