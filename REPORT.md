# b1-wasm-gate report

## CHANGES

- `scripts/build-wasm-worldgen.sh`: added a reproducible release build using the pinned Rust toolchain and exactly wasm-bindgen 0.2.126, with output directory selection and artifact metadata.
- `scripts/bench-wasm-worldgen.mjs`: added a Node timing harness for construction, terrain tiles, halo-normalized tile cost, and one noise volume.
- `REPORT.md`: recorded the build, artifact comparison, benchmark baseline, acceptance result, and check failures.

## EVIDENCE

Exact CLI installation command:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
if command -v wasm-bindgen >/dev/null 2>&1 && [ "$(wasm-bindgen --version)" = "wasm-bindgen 0.2.126" ]; then wasm-bindgen --version; else cargo install wasm-bindgen-cli --version 0.2.126 --locked; fi
```

Output tail:

```text
Finished `release` profile [optimized] target(s) in 14.22s
Installing /home/jakej/.cargo/bin/wasm-bindgen
Installing /home/jakej/.cargo/bin/wasm-bindgen-test-runner
Installing /home/jakej/.cargo/bin/wasm2es6js
Installed package `wasm-bindgen-cli v0.2.126` (executables `wasm-bindgen`, `wasm-bindgen-test-runner`, `wasm2es6js`)
```

Exact build command:

```sh
scripts/build-wasm-worldgen.sh
```

Output tail:

```text
Compiling steel-worldgen-wasm v0.15.2+mc26.2 (/home/jakej/gh/cubiomes-finds-worktrees/rw-b1-wasm-gate/steel-worldgen-wasm)
Finished `release` profile [optimized] target(s) in 1m 36s
wasm-bindgen version: wasm-bindgen 0.2.126
WASM byte size: 11281969 (target/wasm-pkg/steel_worldgen_wasm_bg.wasm)
```

During that build, `steel-utils` also reported that it fetched initially absent or mismatched Minecraft 26.2 assets and successfully extracted the datapack and translation files.

Exact syntax checks:

```sh
sh -n scripts/build-wasm-worldgen.sh
node --check scripts/bench-wasm-worldgen.mjs
```

Both produced no output and exited 0.

Required TypeScript check:

```sh
npx tsc -b
```

Real output tail (exit 1):

```text
This is not the tsc command you are looking for

To get access to the TypeScript compiler, tsc, from the command line either:

- Use npm install typescript to first add TypeScript to your project before using npx
- Use yarn to avoid accidentally running code from un-installed packages
```

Required Vitest check:

```sh
npx vitest run
```

Real output tail (exit 1):

```text
RUN  v4.1.11 /home/jakej/gh/cubiomes-finds-worktrees/rw-b1-wasm-gate

No test files found, exiting with code 1

include: **/*.{test,spec}.?(c|m)[jt]s?(x)
exclude:  **/node_modules/**, **/.git/**
```

The mandated TypeScript checks are therefore not clean. Resolving them would require TypeScript/Vitest project setup or dependency changes outside this task's file scope.

## MEASUREMENTS

Artifact comparison command: a Node stdin script read both files with `readFile`, created `WebAssembly.Module` objects, obtained `WebAssembly.Module.customSections(module, "target_features")`, decoded their feature vectors, and printed byte lengths and `simd128` presence.

```text
fresh: bytes=11281969 target_features=[+bulk-memory, +bulk-memory-opt, +call-indirect-overlong, +multivalue, +mutable-globals, +nontrapping-fptoint, +reference-types, +sign-ext] simd128=false
shipped: bytes=8861547 target_features=[+mutable-globals, +nontrapping-fptoint, +bulk-memory, +sign-ext, +reference-types, +multivalue] simd128=false
```

`simd128` is absent from both the freshly built and shipped modules.

Benchmark commands:

```sh
node scripts/bench-wasm-worldgen.mjs target/wasm-pkg
node scripts/bench-wasm-worldgen.mjs /home/jakej/gh/cubiomes-finds-viewer/public/steel-worldgen
```

The terrain values below are medians of three repetitions. The quotient is `median_ms / (size / 16 + 4)^2`.

| Measurement | Fresh build ms | Fresh ms/halo chunk | Shipped ms | Shipped ms/halo chunk |
|---|---:|---:|---:|---:|
| constructor | 1260.808 | — | 1222.390 | — |
| terrain 16, resolution 1 | 441.721 | 17.669 | 430.326 | 17.213 |
| terrain 32, resolution 1 | 656.570 | 18.238 | 636.095 | 17.669 |
| terrain 64, resolution 1 | 1055.663 | 16.495 | 1036.753 | 16.199 |
| terrain 128, resolution 1 | 2295.156 | 15.939 | 2317.131 | 16.091 |
| terrain 256, resolution 1 | 6168.716 | 15.422 | 6013.155 | 15.033 |
| terrain 64, resolution 1 (resolution sweep) | 1057.522 | 16.524 | 1098.024 | 17.157 |
| terrain 64, resolution 4 | 1021.920 | 15.968 | 1045.859 | 16.342 |
| terrain 64, resolution 64 | 1087.487 | 16.992 | 1100.707 | 17.199 |
| noise_volume_chunk(10, 10, -64, 128, 1) | 5.047 | — | 7.325 | — |

Acceptance calculation command used the printed resolution-1 quotients and computed `(fresh / shipped - 1) * 100` for each size:

```text
16: 2.649%
32: 3.220%
64: 1.827%
128: -0.945%
256: 2.588%
maximum absolute difference: 3.220%
```

The fresh build passes the stated gate: its measured per-halo-chunk values are within 25% of the shipped module at every requested resolution-1 tile size. The measured quotients are close to the expected 17 ms: 15.422–18.238 ms fresh and 15.033–17.669 ms shipped.

## DEVIATIONS

- I ran `node --check` and `sh -n` in addition to the requested checks to validate the two added scripts without introducing test infrastructure.
- The required `npx tsc -b` and `npx vitest run` commands failed because this Rust repository/worktree has no usable TypeScript compiler project and no Vitest test files. I did not modify dependencies, configuration, or out-of-scope files to manufacture passing checks.
- The benchmark harness measures the resolution-sweep rows three times each as well as the explicitly repeated size sweep, providing consistent median reporting.

## GENERALISATIONS

- “The fresh build passes the stated gate” is based only on one foreground run per package on this machine, comparing the five requested resolution-1 tile sizes; it does not establish performance across other machines, seeds, dimensions, coordinates, builds, or runtime versions.
- “The measured quotients are close to the expected 17 ms” is based only on the five resolution-1 size rows from these two runs; it does not establish asymptotic behavior or other resolutions.
- “`simd128` is absent” was tested on exactly the fresh and specified shipped WASM files by decoding their `target_features` custom sections; it says nothing about other artifacts or implicit engine behavior.
- “The build is reproducible” here means the checked-in script successfully rebuilt the package once with the pinned local toolchain and exact wasm-bindgen version; byte-for-byte determinism across clean environments was not tested.

## RECOMMENDATIONS

- Add repository-owned TypeScript/Vitest configuration only if JavaScript tooling checks are intended to apply to root-level `.mjs` utility scripts; that work is outside this task's `scripts/` file scope and dependency restrictions.
- Investigate the fresh module's additional `+bulk-memory-opt` and `+call-indirect-overlong` target features, and its 2,420,422-byte size increase, if artifact parity matters beyond this performance gate.
