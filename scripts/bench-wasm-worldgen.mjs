#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { performance } from "node:perf_hooks";

const packageDirectory = resolve(process.argv[2] ?? "target/wasm-pkg");
const moduleUrl = pathToFileURL(resolve(packageDirectory, "steel_worldgen_wasm.js"));
const wasmPath = resolve(packageDirectory, "steel_worldgen_wasm_bg.wasm");
const { default: init, SteelWorldgen } = await import(moduleUrl.href);
await init({ module_or_path: await readFile(wasmPath) });

function timed(run) {
  const start = performance.now();
  run();
  return performance.now() - start;
}

function median(values) {
  return [...values].sort((left, right) => left - right)[Math.floor(values.length / 2)];
}

function medianOfThree(run) {
  return median([timed(run), timed(run), timed(run)]);
}

const constructorMs = medianOfThree(() =>
  new SteelWorldgen("3860052", "overworld").free(),
);
const generator = new SteelWorldgen("3860052", "overworld");

console.log(`package: ${packageDirectory}`);
console.log("timing_repetitions: 3 (median)");
console.log(`constructor_ms: ${constructorMs.toFixed(3)}`);
console.log("terrain size | resolution | median_ms | ms_per_halo_chunk");

for (const size of [16, 32, 64, 128, 256]) {
  const elapsed = medianOfThree(() => generator.terrain_tile(0, 0, size, 1));
  const haloChunks = (size / 16 + 4) ** 2;
  console.log(
    `${size} | 1 | ${elapsed.toFixed(3)} | ${(elapsed / haloChunks).toFixed(3)}`,
  );
}

for (const resolution of [1, 4, 64]) {
  const elapsed = medianOfThree(() => generator.terrain_tile(0, 0, 64, resolution));
  const haloChunks = (64 / 16 + 4) ** 2;
  console.log(
    `64 | ${resolution} | ${elapsed.toFixed(3)} | ${(elapsed / haloChunks).toFixed(3)}`,
  );
}

const noiseMs = timed(() => generator.noise_volume_chunk(10, 10, -64, 128, 1));
console.log(`noise_volume_chunk_ms: ${noiseMs.toFixed(3)}`);
generator.free();
