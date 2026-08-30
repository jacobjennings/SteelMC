# b5-chunk-memory report

## CHANGES

- `steel-worldgen/src/surface_sampler.rs`: replaced retained flat post-Carvers chunks with a lossless section-palettized representation, expanded retained chunks back to the existing mutable flat format before vegetation, and extended the committed cache sweep to print misses, worker totals, and before/after entry sizes.
- `REPORT.md`: recorded the investigation, commands, real outputs, measurements, deviations, limits, and recommendations for this task.

No change was made to `steel-worldgen/Cargo.toml` or `steel-worldgen-wasm/src/lib.rs`.

## INVESTIGATION AND APPROACH

`steel-core/Cargo.toml` contains `steel-worldgen.workspace = true`, so making `steel-worldgen` depend on `steel-core` would create a dependency cycle. `steel-core` also directly depends on `steel-crypto`, `steel-macros`, `steel-protocol`, Tokio/Tokio-util, futures, reqwest, RSA, zstd, flate2, and the rest of its server stack. By contrast, `cargo tree -p steel-worldgen-wasm --depth 2` showed the current WASM crate reaching `steel-worldgen`, `steel-registry`, `steel-utils`, `steel-math`, Rayon, serde, wasm-bindgen, and their existing dependencies, but not `steel-core`. A `steel-core` dependency was therefore rejected without changing the manifest.

I chose approach B, palettized retention. It is lossless, stays inside `steel-worldgen`, and does not require assumptions about current or future feature reads. Generation and feature placement still write the existing flat `Vec<BlockStateId>`; only cache insertion packs each 16x16x16 section into a state palette and bit-packed indices. Cache consumption reconstructs the flat mutable chunk.

The vertical-window claims were checked even though that approach was not selected:

- Confirmed: pre-Carvers `top_surface` is copied into `pre_carver_surface`, and normal tile surface samples read that summary rather than the retained post-Carvers chunk.
- Not confirmed: vegetation is not constrained in this adapter to a statically evident surface band. `InMemoryVegetationRegion::block_state` accepts arbitrary in-height Y positions, and `feature_height_at` scans from the top through the full retained height. A fixed band would therefore need broader feature-by-feature proof or fallback behavior.
- Not confirmed: deep carver output is read by vegetation through the reconstructed post-Carvers chunks. It is true that carvers finish before cache insertion and do not run again on that retained object, but it is not true that nothing reads their deep output afterward.
- Confirmed for this API: `SteelWorldgen::noise_volume_chunk` calls `SurfaceSampler::noise_volume_chunk` directly and does not access `surface_chunk_cache`; the surface cache is used by `terrain_tile` and `terrain_tile_coarse`.
- The “tall tree is about 30 blocks” example was not used as a bound because the generic vegetation access interfaces do not enforce it.

## EVIDENCE

Command:

```text
rg -n "steel-worldgen|steel_worldgen" steel-core/Cargo.toml Cargo.toml steel-core -g 'Cargo.toml'
```

Relevant output:

```text
Cargo.toml:10:    "steel-worldgen",
Cargo.toml:15:    "steel-worldgen-wasm",
Cargo.toml:33:steel-worldgen = { path = "steel-worldgen" }
steel-core/Cargo.toml:23:steel-worldgen.workspace = true
```

Command:

```text
cargo tree -p steel-worldgen-wasm --depth 2
```

Relevant output:

```text
steel-worldgen-wasm v0.15.2+mc26.2
├── serde v1.0.229
├── serde_json v1.0.151
├── steel-registry v0.15.2+mc26.2
├── steel-utils v0.15.2+mc26.2
├── steel-worldgen v0.15.2+mc26.2
│   ├── glam v0.33.2
│   ├── rayon v1.12.0
│   ├── rustc-hash v2.1.3
│   ├── sha2 v0.11.0
│   ├── steel-math v0.15.2+mc26.2
│   ├── steel-registry v0.15.2+mc26.2
│   ├── steel-utils v0.15.2+mc26.2
│   ├── tracing v0.1.44
│   └── wincode v0.6.0
└── wasm-bindgen v0.2.126
```

Command:

```text
cargo test -p steel-worldgen surface_sampler::tests::cached_tiles_are_identical_to_uncached_tiles -- --exact
```

Output tail:

```text
test surface_sampler::tests::cached_tiles_are_identical_to_uncached_tiles ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 95 filtered out; finished in 176.08s
```

Command:

```text
cargo test -p steel-worldgen surface_sampler::tests::measure_surface_chunk_cache -- --ignored --exact --nocapture
```

Output:

```text
capacity ratio hit_rate misses evictions retained_payload_bytes retained_8_workers retained_16_workers
96 1.255x 99.07% 640 544 834584 6676672 13353344 (cached=103923.290 ms uncached=130468.277 ms hits=67984 peak_chunks=96)
entry_bytes capacity=96 before=199168 after_average=8693 entries=96 before_8_workers=152961024 before_16_workers=305922048
160 1.513x 99.42% 400 240 1423016 11384128 22768256 (cached=87022.975 ms uncached=131626.190 ms hits=68224 peak_chunks=160)
entry_bytes capacity=160 before=199168 after_average=8893 entries=160 before_8_workers=254935040 before_16_workers=509870080
256 1.537x 99.42% 400 144 2355296 18842368 37684736 (cached=85059.006 ms uncached=130709.242 ms hits=68224 peak_chunks=256)
entry_bytes capacity=256 before=199168 after_average=9200 entries=256 before_8_workers=407896064 before_16_workers=815792128
400 1.519x 99.42% 400 0 3530304 28242432 56484864 (cached=86114.328 ms uncached=130780.173 ms hits=68224 peak_chunks=400)
entry_bytes capacity=400 before=199168 after_average=8825 entries=400 before_8_workers=637337600 before_16_workers=1274675200
isolated cached=5950.770 ms uncached=5881.984 ms ratio=0.988x regression=1.17%
test surface_sampler::tests::measure_surface_chunk_cache ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 95 filtered out; finished in 2689.56s
```

Command:

```text
cargo check -p steel-worldgen
```

Output tail:

```text
    Checking steel-worldgen v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b5-chunk-memory/steel-worldgen)
    Finished `dev` profile [unoptimized] target(s) in 0.55s
```

Command:

```text
cargo check -p steel-worldgen-wasm
```

Output tail:

```text
   Compiling wasm-bindgen-macro v0.2.126
    Checking steel-worldgen-wasm v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b5-chunk-memory/steel-worldgen-wasm)
    Finished `dev` profile [unoptimized] target(s) in 1.09s
```

Command:

```text
cargo fmt --all --check
```

Output: no output; exit status 0.

## MEASUREMENTS

The committed harness is `surface_sampler::tests::measure_surface_chunk_cache` in `steel-worldgen/src/surface_sampler.rs`; invoke it with the exact `cargo test` command above. It prints all ratios, percentages, entry averages, and worker totals itself.

| capacity | ratio | hit rate | misses | evictions | retained bytes | 8 workers | 16 workers |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 96 | 1.255x | 99.07% | 640 | 544 | 834,584 | 6,676,672 | 13,353,344 |
| 160 | 1.513x | 99.42% | 400 | 240 | 1,423,016 | 11,384,128 | 22,768,256 |
| 256 | 1.537x | 99.42% | 400 | 144 | 2,355,296 | 18,842,368 | 37,684,736 |
| 400 | 1.519x | 99.42% | 400 | 0 | 3,530,304 | 28,242,432 | 56,484,864 |

The flat baseline is 199,168 bytes per full retained entry. Measured palettized averages were 8,693, 8,893, 9,200, and 8,825 bytes at capacities 96, 160, 256, and 400 respectively. At capacity 400, the harness printed flat-baseline totals of 637,337,600 bytes for 8 workers and 1,274,675,200 bytes for 16 workers, versus measured palettized totals of 28,242,432 and 56,484,864 bytes.

The smaller entry did not let a larger capacity beat 1.708x in this sweep. The best measured ratio was 1.537x at capacity 256; capacity 400 reached zero evictions but measured 1.519x. Relative to the stated 2.56x ceiling, no measured configuration came close enough to exceed the prior 1.708x result.

## DEVIATIONS

- I extended the existing measurement test’s printed columns and entry-size lines because the acceptance criteria require derived percentages and worker totals to be emitted by committed code.
- The first sweep invocation exposed missing test-module imports added with the harness extension. I stopped that failed invocation, fixed and committed the imports, verified the test compiled with `--no-run`, then restarted the exact full measurement command.
- I did not extend the parity test with a later full request after a windowed entry because approach B is lossless and the acceptance criterion requires that extension only for approach A. The existing parity test was not relaxed or changed.

## GENERALISATIONS

- “The representation is lossless” was tested by the unchanged exact-output cache parity test on its existing seed-1 Overworld cases, including positive, negative, offset, eviction, and 256-block tile cases; it was not exhaustively tested over every seed, dimension, block state, or feature.
- “Memory improved sharply” is supported by payload accounting for the single seed-1 Overworld 4x4 traversal at capacities 96, 160, 256, and 400; it does not measure allocator metadata, `HashMap` overhead, transient decompression buffers, browser runtime overhead, other seeds, or other dimensions.
- “The performance premise was refuted” applies to this one debug-profile native test run on this machine and its median-of-three harness; it does not establish release-WASM browser performance on every device.
- “Adding `steel-core` would bloat the WASM dependency graph” is an inference from `steel-core`'s direct dependency manifest and the current WASM tree; no cyclic manifest change or impossible WASM build was attempted.
- “The cave/noise-volume API does not use this cache” was checked on the current Rust call path in `steel-worldgen-wasm/src/lib.rs`; it does not claim that every viewer-side caller always selects that API for every cave visualization mode.

## RECOMMENDATIONS

- Profile the release WASM build in the viewer before deciding whether the large payload reduction justifies the packing and reconstruction CPU cost; this task’s native debug sweep shows a negative timing result.
- If speed remains binding, investigate a retention layout that supports vegetation reads without reconstructing every full halo chunk, while preserving lossless deep access. That architectural work should be separately scoped.
- Consider moving a reusable packed paletted container into a lower-level crate already shared by `steel-core` and `steel-worldgen`; do not add the cyclic `steel-core` dependency to `steel-worldgen`.
