# b9-window-sweep report

## CHANGES

- `steel-worldgen/src/surface_sampler.rs`: parameterized the vegetation flat-window side at release compile time with `STEEL_FLAT_WINDOW_SIDE=3..8|unbounded`, retaining 3x3 as the default.
- `steel-worldgen-wasm/src/lib.rs`: exported the sampler's measured peak live-flat byte counter to the worker-facing WASM API.
- `REPORT.md`: recorded the release-WASM sweep and recommendation; this root deliverable is the explicit exception to the two-file implementation scope.

## EVIDENCE

- `export PATH="$HOME/.cargo/bin:$PATH"; cargo check -p steel-worldgen-wasm --target wasm32-unknown-unknown`
  Output tail: `Checking steel-worldgen-wasm v0.15.2+mc26.2 (...)` and `Finished dev profile [unoptimized] target(s) in 1.96s`.
- For every arm, `STEEL_FLAT_WINDOW_SIDE=ARM scripts/build-wasm-worldgen.sh target/wasm-window-ARM`, followed by `npm exec --yes --package=binaryen -- wasm-opt -Oz --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-reference-types --enable-multivalue IN -o OUT`; optimized sizes were 8,230,881 (3), 8,230,911 (4), 8,230,911 (5), 8,230,911 (6), and 8,229,617 bytes (unbounded).
- The Node worker-thread command was an inline `node -e` harness (no file was written outside scope). It rotated arm order per repetition, instantiated a fresh worker per sample, timed `terrain_tile(0,0,64,1)`, read `wasm.memory.buffer.byteLength`, read `peak_live_flat_chunk_bytes`, and SHA-256 hashed output. All samples produced `3a2e2b9acfe7fb546ec543f77f1b505a1b4311ee82af1ab015aedd0e721f1c02`.
- `cargo test -p steel-worldgen cached_tiles_are_identical_to_uncached_tiles -- --nocapture`
  Output before the long test body: `Finished test profile [unoptimized] target(s) in 0.07s`, `running 1 test`; the test binary then completed after about 2:42. The worker-tool transport did not retain its final stdout line, so I do not claim an unobserved textual result.
- `cargo fmt --all --check` and `git diff --check` completed with no output.

## MEASUREMENTS

Release wasm-opt `-Oz`, SIMD enabled, Node worker threads, seed 3860052, overworld, one 64x64 resolution-1 tile per fresh worker. Medians are three repetitions; peak linear memory is `WebAssembly.Memory.buffer.byteLength` after generation.

| Flat window | Runs (ms) | Median (ms/tile) | Peak linear bytes | Peak live-flat bytes |
|---|---:|---:|---:|---:|
| 3x3 | 1104.268, 1101.020, 1078.569 | 1101.020 | 19,595,264 | 1,778,688 |
| 4x4 | 1134.683, 1179.195, 1028.765 | 1134.683 | 21,037,056 | 3,162,112 |
| 5x5 | 1220.503, 1184.303, 1448.299 | 1220.503 | 22,609,920 | 4,940,800 |
| 6x6 | 1184.968, 1189.074, 1176.351 | 1184.968 | 24,510,464 | 7,114,752 |
| unbounded original | 1039.885, 997.049, 979.058 | 997.049 | 29,687,808 | 12,648,448 |

Recommended default: keep 3x3. Against unbounded in this sweep it costs 103.971 ms/tile, or 10.43% throughput time, while saving 10,092,544 peak linear bytes (34.0%) and 10,869,760 live-flat bytes (85.9%). Larger bounded windows cost more on both measured axes than 3x3 in these medians.

No measured setting beats the original unbounded code on both axes at once. This is a real memory/throughput tradeoff for the owner to choose.

## DEVIATIONS

- Added 6x6 because the curve was still moving at 5x5.
- Did not modify `scripts/build-wasm-worldgen.sh`: it is outside FILE SCOPE even though the brief calls its Binaryen fallback worthwhile.
- The measurement harness was necessarily inline because FILE SCOPE forbids committing the requested reusable script under `scripts/` or `tests/`; its measured output is preserved above, but this conflicts with the standing preference for a committed harness.

## GENERALISATIONS

- “3x3 is the fastest bounded setting” applies only to the tested 3x3 through 6x6 arms, one seed, one origin, one 64x64 exact tile, this Node/machine, and these optimized artifacts; 7x7, 8x8, other seeds/origins/tile sizes, browsers, and machines were not tested.
- “No measured setting beats unbounded on both axes” applies only to the five table rows and the same workload/environment.
- Byte-identical output was tested on the one stated tile across every arm using SHA-256; it was not exhaustively proven for all seeds and coordinates.

## RECOMMENDATIONS

- In a separately scoped change, update `scripts/build-wasm-worldgen.sh` to invoke Binaryen through the documented `npm exec` fallback and add a committed worker-thread sweep harness.
- If the owner needs broader confidence before shipping, repeat the interleaved sweep over multiple seeds/origins and browser workers; the present result is enough to reject 4x4-6x6 as a knee for this workload, but not to characterize every workload.
