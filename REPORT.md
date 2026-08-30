# b3-cache-tune report

## CHANGES

- `steel-worldgen/src/surface_sampler.rs`: added cache hit/miss/eviction/peak counters and retained-payload accounting; replaced the retained full pre-carver chunk with a 256-column surface summary; expanded the committed measurement harness; extended parity coverage; and changed the measured default capacity from 96 to 160.
- `REPORT.md`: records the requested evidence, measurements, conclusions, and recommendations.

`steel-worldgen-wasm/src/lib.rs` was inspected but did not require a change because it already constructs `SurfaceChunkCache::default()`.

## EVIDENCE

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen --release surface_sampler::tests::measure_surface_chunk_cache -- --ignored --nocapture
```

Real output tail:

```text
capacity ratio hit_rate peak_chunks retained_payload_bytes evictions
96 1.298x 99.07% 96 19120128 544 (cached=9841.854 ms uncached=12770.640 ms hits=67984 misses=640)
160 1.708x 99.42% 160 31866880 240 (cached=7753.806 ms uncached=13246.378 ms hits=68224 misses=400)
256 1.689x 99.42% 256 50987008 144 (cached=7868.730 ms uncached=13291.256 ms hits=68224 misses=400)
400 1.661x 99.42% 400 79667200 0 (cached=7810.169 ms uncached=12975.442 ms hits=68224 misses=400)
isolated cached=684.449 ms uncached=682.049 ms ratio=0.996x regression=0.35%
test surface_sampler::tests::measure_surface_chunk_cache ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 93 filtered out; finished in 260.27s
```

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen --release surface_sampler::tests::measure_isolated_surface_chunk_cache -- --ignored --nocapture
```

Real output tail:

```text
isolated_order_balanced cached=686.343 ms uncached=685.294 ms ratio=0.998x regression=0.15%
test surface_sampler::tests::measure_isolated_surface_chunk_cache ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 93 filtered out; finished in 12.37s
```

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p steel-worldgen --release surface_sampler::tests::cached_tiles_are_identical_to_uncached_tiles -- --nocapture
```

Real output tail:

```text
running 1 test
test surface_sampler::tests::cached_tiles_are_identical_to_uncached_tiles ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 93 filtered out; finished in 18.88s
```

Command:

```text
export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt --all --check
```

Real output: no output; exit status 0.

The required viewer `npx` checks were not run because this is the SteelMC Rust repository, as directed.

## MEASUREMENTS

The committed harness is `surface_sampler::tests::measure_surface_chunk_cache` in `steel-worldgen/src/surface_sampler.rs`, invoked exactly as shown above. Its final compact-representation capacity curve is:

| Capacity | Ratio | Hit rate | Peak chunks | Retained payload bytes | Misses | Evictions |
|---:|---:|---:|---:|---:|---:|---:|
| 96 | 1.298x | 99.07% | 96 | 19,120,128 | 640 | 544 |
| 160 | 1.708x | 99.42% | 160 | 31,866,880 | 400 | 240 |
| 256 | 1.689x | 99.42% | 256 | 50,987,008 | 400 | 144 |
| 400 | 1.661x | 99.42% | 400 | 79,667,200 | 400 | 0 |

The traversal arithmetic is confirmed by the capacity-400 result: 400 misses and zero evictions mean the grid touched 400 distinct chunks. Sixteen independently generated 64-chunk tiles request 1,024 generations, so the generation-count ceiling is 1,024 / 400 = 2.56x. The measured wall-clock ratio is lower because tile work other than chunk generation remains.

Capacity 160 is the selected default: it is the smallest measured capacity with only 400 misses, and it had the best measured median ratio. Capacity 96 demonstrably thrashed, generating 240 extra chunks and evicting 544 entries.

The compact representation retains one full post-carver chunk plus 256 pre-carver surface-column results. This is valid for the measured code paths because cached pre-carver data is read only through `top_surface`, while vegetation still receives the required full post-carver chunks. The exact-output test covers aligned, misaligned, negative, large, reused-cache, and capacity-one eviction cases.

The order-balanced nine-repetition isolated harness measured a 0.15% slowdown (686.343 ms versus 685.294 ms), not the reported 9.2%. Both public paths use the same cache machinery for an isolated tile and neither reaches capacity; the original large difference was not reproduced. Removing the full pre-carver clone eliminates the identifiable extra miss-path memory copy.

At the chosen capacity, measured retained payload is 31,866,880 bytes per worker. Eight workers retain 254,935,040 bytes (about 243.1 MiB) of measured cache payload; sixteen retain 509,870,080 bytes (about 486.2 MiB). These totals exclude hash-table allocation, sampler state, WASM linear-memory overhead, tile responses, and browser overhead.

## DEVIATIONS

- Added a compact pre-carver summary after measurement showed each entry retained two full chunks while cached callers only consumed pre-carver top-column results. This directly addresses both memory and miss-path clone cost requested by the brief.
- Did not change `steel-worldgen-wasm/src/lib.rs`; changing it would have been unnecessary because the default cache is already used there.

## GENERALISATIONS

- The 400-distinct-chunk and 2.56x generation-count statements apply only to the committed contiguous aligned 4-by-4 traversal at 64-block tiles; they were not tested on panning, zooming, misaligned viewer requests, or another scheduling order.
- The capacity-160 performance choice was tested on this machine, seed 1, Overworld, release profile, three repetitions; it was not benchmarked across browsers, WASM engines, seeds, dimensions, or hardware.
- The compact-summary validity claim is based on all current `CachedSurfaceChunk` consumers found in this revision and the listed parity cases; it does not prove future consumers will not require additional pre-carver data.
- The isolated conclusion is based on nine order-balanced release repetitions on this machine; differences below that scale were not established as statistically significant.
- Worker-count advice below combines this Rust cache curve with the owner-provided viewer locality figures; end-to-end throughput and browser memory were not measured in this worktree.

## RECOMMENDATIONS

- Use capacity 160 with 8 Web Workers as the next viewer configuration to benchmark. It budgets about 243.1 MiB of measured cache payload, halves the current 16-worker cache total, uses the owner-provided 62.6% cardinal-neighbour locality rather than 41.2%, and preserves the best point on this capacity curve. Treat this as a benchmark candidate, not a proven end-to-end optimum.
- Measure 4 versus 8 workers end to end before shipping if a roughly 256 MiB cache-payload budget is still too high; this task has no 4-worker locality or throughput result.
- Do not demote post-carver chunks to surface summaries: neighbouring vegetation needs their full block state. The implemented cheaper representation instead summarizes only pre-carver data, whose current cache consumer needs top-column results alone.
