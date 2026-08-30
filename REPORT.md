# b2b-build-opt report

## CHANGES

- `scripts/build-wasm-worldgen.sh`: added a documented `WASM_SIMD=0|1` switch, made SIMD the measured default, added optional `WASM_OPT` post-processing with an explicit missing-tool error, and reports the effective configuration and byte size.
- `scripts/bench-wasm-worldgen.mjs`: changed constructor timing from one observation to the median of three and prints the timing repetition policy.
- `REPORT.md`: records the reproducible commands, measurements, limits, and recommendation required by the brief.

## EVIDENCE

Initial tool versions and Binaryen installation (user-local, no repository dependency change):

```text
$ command -v wasm-opt || true; wasm-opt --version 2>/dev/null || true; rustc --version; wasm-bindgen --version
rustc 1.100.0-nightly (8925ea358 2026-08-20)
wasm-bindgen 0.2.126

$ npm install --prefix "$HOME/.local/lib/codex-binaryen" binaryen@132.0.0 && "$HOME/.local/lib/codex-binaryen/node_modules/binaryen/bin/wasm-opt" --version
added 1 package in 397ms
wasm-opt version 132 (version_132)
```

The four build/benchmark commands were run in the foreground, with `PATH` exported before Cargo as required:

```text
export PATH="$HOME/.cargo/bin:$HOME/.local/lib/codex-binaryen/node_modules/binaryen/bin:$PATH"
scripts/build-wasm-worldgen.sh target/wasm-matrix-baseline | tee /tmp/b2b-baseline-build.log
node scripts/bench-wasm-worldgen.mjs target/wasm-matrix-baseline | tee /tmp/b2b-baseline-bench.log

WASM_OPT=-Oz scripts/build-wasm-worldgen.sh target/wasm-matrix-opt | tee /tmp/b2b-opt-build.log
node scripts/bench-wasm-worldgen.mjs target/wasm-matrix-opt | tee /tmp/b2b-opt-bench.log

WASM_SIMD=1 scripts/build-wasm-worldgen.sh target/wasm-matrix-simd | tee /tmp/b2b-simd-build.log
node scripts/bench-wasm-worldgen.mjs target/wasm-matrix-simd | tee /tmp/b2b-simd-bench.log

WASM_SIMD=1 WASM_OPT=-Oz scripts/build-wasm-worldgen.sh target/wasm-matrix-simd-opt | tee /tmp/b2b-simd-opt-build.log
node scripts/bench-wasm-worldgen.mjs target/wasm-matrix-simd-opt | tee /tmp/b2b-simd-opt-bench.log
```

The baseline commands above ran before the later commit that changed the default to SIMD; under the final script, the reproducible baseline spelling is `WASM_SIMD=0 scripts/build-wasm-worldgen.sh ...`. Build output:

```text
baseline: wasm-bindgen 0.2.126; simd128 enabled: 0; wasm-opt level: none; 11281969 bytes
SIMD:     wasm-bindgen 0.2.126; simd128 enabled: 1; wasm-opt level: none; 11130012 bytes
-Oz:      wasm-bindgen 0.2.126; simd128 enabled: 0; wasm-opt level: -Oz; 8280009 bytes
both:     wasm-bindgen 0.2.126; simd128 enabled: 1; wasm-opt level: -Oz; 8203492 bytes
```

Feature validation command and output (Binaryen prints `--enable-simd` for simd128):

```text
$ for wasm_file in target/wasm-matrix-baseline/steel_worldgen_wasm_bg.wasm target/wasm-matrix-simd/steel_worldgen_wasm_bg.wasm target/wasm-matrix-opt/steel_worldgen_wasm_bg.wasm target/wasm-matrix-simd-opt/steel_worldgen_wasm_bg.wasm; do printf '%s: ' "$wasm_file"; wasm-opt "$wasm_file" --print-features -o /dev/null 2>&1 | tr '\n' ' '; printf '\n'; done
target/wasm-matrix-baseline/steel_worldgen_wasm_bg.wasm: --enable-mutable-globals --enable-nontrapping-float-to-int --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --enable-bulk-memory-opt --enable-call-indirect-overlong
target/wasm-matrix-simd/steel_worldgen_wasm_bg.wasm: --enable-mutable-globals --enable-nontrapping-float-to-int --enable-simd --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --enable-bulk-memory-opt --enable-call-indirect-overlong
target/wasm-matrix-opt/steel_worldgen_wasm_bg.wasm: --enable-mutable-globals --enable-nontrapping-float-to-int --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --enable-bulk-memory-opt --enable-call-indirect-overlong
target/wasm-matrix-simd-opt/steel_worldgen_wasm_bg.wasm: --enable-mutable-globals --enable-nontrapping-float-to-int --enable-simd --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --enable-bulk-memory-opt --enable-call-indirect-overlong
```

Browser support was checked against Browserslist 4.28.8's current `wasm-simd` compatibility data:

```text
$ npx --yes browserslist 'supports wasm-simd' | awk '$1 == "chrome" || $1 == "firefox" || $1 == "safari" { print }' | sort -k1,1 -k2,2V | awk '!seen[$1]++ { print }'
chrome 91
firefox 89
safari 16.4
```

Scoped static checks:

```text
$ sh -n scripts/build-wasm-worldgen.sh
(no output; exit 0)
$ node --check scripts/bench-wasm-worldgen.mjs
(no output; exit 0)
$ git diff --check
(no output; exit 0)
```

As instructed for this SteelMC task, `npx tsc -b` and `npx vitest run` were not run.

## MEASUREMENTS

Every displayed timing is the median of three calls from the committed `scripts/bench-wasm-worldgen.mjs` harness. Terrain quotient is milliseconds per halo-inclusive chunk.

| Configuration | WASM bytes | Constructor ms | 64 ms | 64 ms/chunk | 256 ms | 256 ms/chunk |
|---|---:|---:|---:|---:|---:|---:|
| baseline, no SIMD/pass | 11,281,969 | 1081.680 | 952.336 | 14.880 | 5416.667 | 13.542 |
| simd128 | 11,130,012 | 956.418 | 877.365 | 13.709 | 4975.490 | 12.439 |
| baseline + `-Oz` | 8,280,009 | 1057.081 | 950.965 | 14.859 | 5156.353 | 12.891 |
| simd128 + `-Oz` | 8,203,492 | 971.431 | 870.503 | 13.602 | 5010.166 | 12.525 |

Derived figures were produced with this exact command:

```text
$ awk 'BEGIN { base=11281969; simd=11130012; opt=8280009; both=8203492; b64=952.336; s64=877.365; bo64=950.965; so64=870.503; b256=5416.667; s256=4975.490; bo256=5156.353; so256=5010.166; printf "size reduction baseline->-Oz: %.2f%%\n",100*(base-opt)/base; printf "size reduction baseline->SIMD+-Oz: %.2f%%\n",100*(base-both)/base; printf "64 SIMD change: %.2f%%; 64 SIMD+-Oz change: %.2f%%\n",100*(s64-b64)/b64,100*(so64-b64)/b64; printf "256 SIMD change: %.2f%%; 256 SIMD+-Oz change: %.2f%%\n",100*(s256-b256)/b256,100*(so256-b256)/b256; printf "64 -Oz change: %.2f%%; 256 -Oz change: %.2f%%\n",100*(bo64-b64)/b64,100*(bo256-b256)/b256 }'
size reduction baseline->-Oz: 26.61%
size reduction baseline->SIMD+-Oz: 27.29%
64 SIMD change: -7.87%; 64 SIMD+-Oz change: -8.59%
256 SIMD change: -8.14%; 256 SIMD+-Oz change: -7.50%
64 -Oz change: -0.14%; 256 -Oz change: -4.81%
```

Recommendation: use simd128 plus `wasm-opt -Oz` as the shipped default. It produced the smallest module (8,203,492 bytes, 27.29% below the fresh baseline), while its 64 and 256 terrain medians were respectively 8.59% and 7.50% lower than baseline; `-Oz` showed no meaningful slowdown in these measured cases. The script defaults SIMD on, while keeping `WASM_SIMD=0` as the compatibility escape hatch; `WASM_OPT=-Oz` remains explicit because Binaryen is an external optional tool.

## DEVIATIONS

- Installed Binaryen 132 beneath `~/.local/lib/codex-binaryen` because `wasm-opt` was absent and the brief explicitly permitted a user-local installation.
- Changed constructor measurement to a median of three because acceptance requires at least three repetitions for compared timings.
- Made SIMD default after the matrix showed consistent reductions at both requested terrain sizes; the size pass remains opt-in because the brief calls it optional and the repository does not provision Binaryen.

## GENERALISATIONS

- “SIMD is a win” is based on one machine/runtime, one seed and dimension, terrain sizes 64 and 256 at resolution 1, with three calls per median; it was not tested across browsers, machines, seeds, dimensions, or long-run distributions.
- “`-Oz` showed no meaningful slowdown” is limited to the same harness and observations; it is not a claim about every exported function or runtime.
- Browser minimums are the minimum versions returned by Browserslist 4.28.8 for `supports wasm-simd`; they were not independently exercised in installed browser binaries.
- “Closes the size gap” compares the measured fresh baseline to the optimized artifacts; it does not prove which exact unpublished shipped-pipeline flags originally produced the ancestor's 8,861,547-byte artifact.

## RECOMMENDATIONS

- Provision a pinned Binaryen version in the release/CI environment and invoke the build with `WASM_OPT=-Oz`; this is outside the scoped scripts and was not changed.
- Benchmark the chosen configuration in supported browsers before release if browser-specific performance, rather than Node's WebAssembly engine, is release-critical.
