# SteelMC GPU stage cost measurements

This report measures where overworld terrain generation time goes on branch `experiment/gpu-worldgen`. The worktree was `/home/jakej/gh/cubiomes-finds-worktrees/steelmc-gpu-moonshot`.

## Result

The build worked. The measured noise fill median was 2.3884 ms. The measured full chunk median was 122.08 ms. Noise fill was therefore 1.9564 percent of full chunk time.

If noise fill became free and every other stage stayed unchanged, the maximum whole-tile speedup would be 1.01995x, which rounds to 1.020x. This is the Amdahl ceiling for accelerating only the measured noise fill.

## Build

The asset update script was inspected with this exact command.

```text
sed -n '1,240p' update-minecraft-assets.sh
```

The script is not offline-capable. It uses `curl` to read Mojang manifests and uses `nix store prefetch-file` to download a server jar. It was not run. Existing extracted registry and worldgen inputs were present.

The first build attempt used the system Cargo.

```text
timeout 480s cargo check -p steel-core
```

It stopped before compilation with this output.

```text
error: failed to parse manifest at `/home/jakej/gh/cubiomes-finds-worktrees/steelmc-gpu-moonshot/Cargo.toml`

Caused by:
  the cargo feature `codegen-backend` requires a nightly version of Cargo, but this is the `stable` channel
```

The repository pins `nightly-2026-08-21` in `rust-toolchain.toml`. The successful build used that installed toolchain with this exact command.

```text
timeout 480s env PATH=/home/jakej/.rustup/toolchains/nightly-2026-08-21-x86_64-unknown-linux-gnu/bin:/usr/bin cargo check -p steel-core
```

The relevant final build output was:

```text
warning: steel-utils@0.15.2+mc26.2: Assets not found or version mismatch for Minecraft 26.2. Fetching...
warning: steel-utils@0.15.2+mc26.2: Downloading server jar for 26.2...
warning: steel-utils@0.15.2+mc26.2: Detected bootstrap jar. Extracting nested jar META-INF/versions/26.2/server-26.2.jar...
warning: steel-utils@0.15.2+mc26.2: Successfully extracted datapack and translation files for Minecraft 26.2.
Finished `dev` profile [unoptimized] target(s) in 44.77s
```

The build script populated its missing assets automatically. The build completed successfully.

## Measurement method

Criterion medians are the middle values in the three-value `time` intervals below. Every successful measurement used sample size 10. The first two measurements were launched through Cargo. That produced the optimized benchmark executable at `target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1`. Later measurements invoked that same executable directly with Cargo's implicit `--bench` argument made explicit. This avoided repeated relinking and did not change the benchmark code or Criterion settings.

`cat /proc/loadavg` ran immediately before and after every attempted timing command. The first number is the one-minute load average used for the contention label. All successful measurements had one-minute load averages below 8 before and after. No reported median is labeled as taken under contention.

## Measured numbers

| Benchmark | Criterion median | Load before | Load after | Contention |
| --- | ---: | ---: | ---: | --- |
| `overworld_biome` | 492.80 µs | 6.02 | 5.43 | No |
| `overworld_fill_from_noise` | 2.3884 ms | 5.72 | 2.68 | No |
| `overworld_build_surface` | 895.27 µs | 2.35 | 2.34 | No |
| `overworld_apply_carvers` | 70.553 µs | 2.23 | 2.11 | No |
| `overworld_generate_features` | 1.5730 ms | 2.02 | 2.06 | No |
| `overworld_full_through_carvers` | 3.7276 ms | 1.98 | 1.99 | No |
| `overworld_full_chunk` | 122.08 ms | 1.99 | 2.26 | No |
| `noise_kernel/y_scale_4x_generic` | 19.480 µs | 2.24 | 2.49 | No |
| `noise_kernel/y_scale_8x_generic` | 19.648 µs | 2.24 | 2.49 | No |

The features filter also matched `overworld_generate_features_concurrent_overlap`. Its measured median was 3.6622 ms under the same load observations. It is not used in the stage table calculations because the requested `bench_features` function measures `overworld_generate_features`.

### `overworld_biome`

Exact command:

```text
cat /proc/loadavg
timeout 720s env PATH=/home/jakej/.rustup/toolchains/nightly-2026-08-21-x86_64-unknown-linux-gnu/bin:/usr/bin cargo bench -p steel-core --features benchmark-support --bench worldgen -- overworld_biome --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
6.02 5.51 7.32 14/7387 1352393
Benchmarking overworld_biome
Benchmarking overworld_biome: Warming up for 3.0000 s
Benchmarking overworld_biome: Collecting 10 samples in estimated 5.0003 s (11k iterations)
Benchmarking overworld_biome: Analyzing
overworld_biome         time:   [453.04 µs 492.80 µs 551.78 µs]
Found 2 outliers among 10 measurements (20.00%)
  1 (10.00%) high mild
  1 (10.00%) high severe
5.43 5.20 6.81 8/6416 1398031
```

### `overworld_fill_from_noise`

Exact command:

```text
cat /proc/loadavg
timeout 720s env PATH=/home/jakej/.rustup/toolchains/nightly-2026-08-21-x86_64-unknown-linux-gnu/bin:/usr/bin cargo bench -p steel-core --features benchmark-support --bench worldgen -- overworld_fill_from_noise --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
5.72 5.26 6.82 7/6415 1398224
Benchmarking overworld_fill_from_noise
Benchmarking overworld_fill_from_noise: Warming up for 3.0000 s
Benchmarking overworld_fill_from_noise: Collecting 10 samples in estimated 5.0707 s (1980 iterations)
Benchmarking overworld_fill_from_noise: Analyzing
overworld_fill_from_noise
                        time:   [2.3401 ms 2.3884 ms 2.4590 ms]
2.68 4.22 6.10 2/5496 1407731
```

### `overworld_build_surface`

Exact command:

```text
cat /proc/loadavg
timeout 720s target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1 --bench overworld_build_surface --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
2.35 3.99 5.96 4/5500 1409123
Benchmarking overworld_build_surface
Benchmarking overworld_build_surface: Warming up for 3.0000 s
Benchmarking overworld_build_surface: Collecting 10 samples in estimated 5.1926 s (1210 iterations)
Benchmarking overworld_build_surface: Analyzing
overworld_build_surface time:   [892.03 µs 895.27 µs 899.61 µs]
2.34 3.91 5.90 1/5496 1409546
```

### `overworld_apply_carvers`

Exact command:

```text
cat /proc/loadavg
timeout 720s target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1 --bench overworld_apply_carvers --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
2.23 3.86 5.88 1/5498 1409642
Benchmarking overworld_apply_carvers
Benchmarking overworld_apply_carvers: Warming up for 3.0000 s
Benchmarking overworld_apply_carvers: Collecting 10 samples in estimated 5.2057 s (1265 iterations)
Benchmarking overworld_apply_carvers: Analyzing
overworld_apply_carvers time:   [68.260 µs 70.553 µs 72.381 µs]
2.11 3.75 5.81 5/5508 1410089
```

### Features

Exact command:

```text
cat /proc/loadavg
timeout 720s target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1 --bench overworld_generate_features --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
2.02 3.71 5.78 1/5511 1410299
Benchmarking overworld_generate_features
Benchmarking overworld_generate_features: Warming up for 3.0000 s
Warning: Unable to complete 10 samples in 5.0s. You may wish to increase target time to 9.9s.
Benchmarking overworld_generate_features: Collecting 10 samples in estimated 9.9196 s (10 iterations)
Benchmarking overworld_generate_features: Analyzing
overworld_generate_features
                        time:   [1.5598 ms 1.5730 ms 1.5860 ms]
Benchmarking overworld_generate_features_concurrent_overlap
Benchmarking overworld_generate_features_concurrent_overlap: Warming up for 2.0000 s
Benchmarking overworld_generate_features_concurrent_overlap: Collecting 10 samples in estimated 21.607 s (20 iterations)
Benchmarking overworld_generate_features_concurrent_overlap: Analyzing
overworld_generate_features_concurrent_overlap
                        time:   [3.5587 ms 3.6622 ms 3.7737 ms]
2.06 3.44 5.58 2/5544 1413023
```

### `overworld_full_through_carvers`

Exact command:

```text
cat /proc/loadavg
timeout 720s target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1 --bench overworld_full_through_carvers --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
1.98 3.40 5.56 2/5543 1413140
Benchmarking overworld_full_through_carvers
Benchmarking overworld_full_through_carvers: Warming up for 3.0000 s
Benchmarking overworld_full_through_carvers: Collecting 10 samples in estimated 5.0259 s (1210 iterations)
Benchmarking overworld_full_through_carvers: Analyzing
overworld_full_through_carvers
                        time:   [3.5867 ms 3.7276 ms 3.9216 ms]
1.99 3.33 5.50 1/5529 1414049
```

### `overworld_full_chunk`

Exact command:

```text
cat /proc/loadavg
timeout 720s target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1 --bench --exact overworld_full_chunk --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
1.99 3.31 5.48 1/5531 1414093
Benchmarking overworld_full_chunk
Benchmarking overworld_full_chunk: Warming up for 1.0000 s
Warning: Unable to complete 10 samples in 10.0s. You may wish to increase target time to 10.6s.
Benchmarking overworld_full_chunk: Collecting 10 samples in estimated 10.568 s (10 iterations)
Benchmarking overworld_full_chunk: Analyzing
overworld_full_chunk    time:   [121.33 ms 122.08 ms 122.90 ms]
2.26 3.29 5.43 1/5530 1414799
```

### `bench_noise_kernel`

Exact command:

```text
cat /proc/loadavg
timeout 720s target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1 --bench noise_kernel --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Raw load and Criterion lines:

```text
2.24 3.27 5.41 1/5529 1414909
Benchmarking noise_kernel/y_scale_4x_generic
Benchmarking noise_kernel/y_scale_4x_generic: Warming up for 1.0000 s
Benchmarking noise_kernel/y_scale_4x_generic: Collecting 10 samples in estimated 5.0006 s (256k iterations)
Benchmarking noise_kernel/y_scale_4x_generic: Analyzing
noise_kernel/y_scale_4x_generic
                        time:   [19.438 µs 19.480 µs 19.504 µs]
                        thrpt:  [105.00 Melem/s 105.13 Melem/s 105.36 Melem/s]
Benchmarking noise_kernel/y_scale_8x_generic
Benchmarking noise_kernel/y_scale_8x_generic: Warming up for 1.0000 s
Benchmarking noise_kernel/y_scale_8x_generic: Collecting 10 samples in estimated 5.0004 s (244k iterations)
Benchmarking noise_kernel/y_scale_8x_generic: Analyzing
noise_kernel/y_scale_8x_generic
                        time:   [18.598 µs 19.648 µs 20.463 µs]
                        thrpt:  [100.08 Melem/s 104.23 Melem/s 110.12 Melem/s]
Found 3 outliers among 10 measurements (30.00%)
  1 (10.00%) low severe
  1 (10.00%) low mild
  1 (10.00%) high mild
2.49 3.27 5.37 1/5529 1415415
```

## Failed timing attempts

The first Cargo benchmark command omitted the target's required `benchmark-support` feature. It exited immediately and produced no timing.

```text
cat /proc/loadavg
timeout 720s env PATH=/home/jakej/.rustup/toolchains/nightly-2026-08-21-x86_64-unknown-linux-gnu/bin:/usr/bin cargo bench -p steel-core --bench worldgen -- overworld_biome --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Its load observations and error were:

```text
4.36 5.18 7.23 3/6919 1347667
error: target `worldgen` in package `steel-core` requires the features: `benchmark-support`
4.36 5.18 7.23 1/6919 1347671
```

The first direct executable command omitted Cargo's implicit `--bench` flag. Criterion ran test mode and produced no timing.

```text
cat /proc/loadavg
timeout 720s target/release/build/steel-core/f1ad226bdd348fc1/out/worldgen-f1ad226bdd348fc1 overworld_build_surface --sample-size 10
run_status=$?
cat /proc/loadavg
exit "$run_status"
```

Its load observations and output were:

```text
2.50 4.13 6.05 3/5504 1407878
Testing overworld_build_surface
Success
2.46 4.09 6.03 1/5499 1408011
```

## Inferred numbers

These values are arithmetic derived from the measured Criterion medians. They are not separate timing measurements.

Noise fill as a fraction of generation through carvers:

```text
2.3884 ms / 3.7276 ms = 0.6407339843
0.6407339843 * 100 = 64.0734 percent
```

Noise fill as a fraction of a full chunk:

```text
2.3884 ms / 122.08 ms = 0.0195642202
0.0195642202 * 100 = 1.9564 percent
```

The remainder of full chunk time is:

```text
100 percent - 1.9564 percent = 98.0436 percent
```

The maximum whole-tile speedup if noise fill became free is:

```text
1 / (1 - 0.0195642202) = 1.019954617x
```

Rounded to three decimal places, the Amdahl ceiling is 1.020x. Even an infinitely fast noise fill cannot improve the measured full chunk by more than about 1.995 percent because the other 98.0436 percent remains on the CPU.

For context only, making noise fill free inside the through-carvers subset would have this local ceiling:

```text
1 / (1 - 0.6407339843) = 2.783452808x
```

That 2.783x subset ceiling is not the whole-tile ceiling.
