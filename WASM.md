# WebAssembly readiness

This branch explores compiling Steel's deterministic world-generation kernel for
`wasm32-unknown-unknown`. It deliberately keeps browser bindings and the terrain
wire format outside the existing server crates so upstream changes remain easy
to merge.

## Architecture

The shared boundary should be a synchronous, deterministic world-generation
kernel:

```text
seed + dimension + chunk coordinates
                 |
          worldgen kernel
            /          \
 native service       wasm worker
 HTTP/binary API      JavaScript API
            \          /
          terrain tile DTO
```

The browser adapter belongs in a small leaf crate and must run in a Web Worker.
The existing native server remains a supported adapter for large requests,
shared caches, and devices that cannot afford local generation. Neither adapter
should own generation rules or produce a different terrain representation.

Start single-threaded. Browser threads can be added behind a separate feature
later because they require `SharedArrayBuffer` and cross-origin isolation
headers, which would make ordinary static hosting less portable.

## Current status

Run the readiness check with:

```bash
./scripts/check-wasm-worldgen.sh
```

The workspace dependency features no longer force Tokio networking or native
filesystem support into a WASM build. Filesystem-backed saved data reports an
unsupported host adapter on `wasm32`; an ephemeral manager remains usable.
`steel-worldgen`, including its generated registry dependencies, now compiles
for `wasm32-unknown-unknown` through the pinned 32-bit-capable `simdnbt` fork.

The `steel-worldgen-wasm` leaf crate exposes a reusable seeded sampler for the
Overworld, Nether, and End. It runs the real Steel density functions and biome
source and is integrated into the viewer through Web Workers. Its current tile
DTO is a surface height/color grid, so it does not yet expose vertical cave and
overhang geometry or run the native Features pipeline (surface blocks,
structures, trees, and decoration). Those remain available through the native
service. The generated web package is about 7 MiB before HTTP compression.

### Final-surface and foliage boundary

`terrain_tile` currently ends after `steel-worldgen::surface_sampler` has
sampled density heights. It has not executed the canonical Surface phase, so a
missing `surfaceBlocks` field means exactly "no post-surface-rule block IDs",
not stone or a viewer-selected material. Consumers may select an exact renderer
asset directly for a supplied canonical ID when that asset exists; unrepresented
IDs must remain explicitly unmodeled rather than being collapsed into a
lookalike atlas cell. The viewer contract validates this optional
`surfaceBlocks` array as one canonical block key per terrain sample.

The implementation boundary is presently concrete:

1. `steel-core::worldgen::generator::VanillaGenerator::fill_from_noise` builds
   the mutable noise/aquifer column state in a `GenerationChunk<NoisePhase>`.
2. `VanillaGenerator::build_surface` then evaluates generated surface rules
   against that complete column through `SurfaceSystem`,
   `FuzzedBiomeColumn`, and `GenerationChunk<SurfacePhase>`.
3. Decoration is later run by `FeatureDecorationRunner` against a
   `WorldGenRegion`; tree and foliage placements can cross chunk boundaries and
   cannot be reduced to independent terrain samples without changing vanilla
   placement semantics.

Therefore the next correct WASM slice is a reusable surface-stage kernel that
owns the same mutable 16x16 noise/aquifer columns and `SurfaceSystem` inputs as
step 2, exposed as `surfaceBlocks` after the final top-block selection. It must
be shared with the native terrain adapter rather than reimplementing the rules
in JavaScript. Feature/foliage output remains a subsequent region-generation
slice; do not synthesize decoration from biome, height, or random noise.

## Milestones

1. Add deterministic native-versus-WASM vectors for biome, density, and complete
   chunk output.
2. Replace the JSON bridge with a stable byte-oriented tile API to reduce copies.
3. Expose full block-state sections and the Features pipeline to the browser.
4. Add structures and host-provided persistence, then evaluate optional threaded
   WASM separately.

Do not add browser conditionals to vanilla generation algorithms. Target-specific
code should stay in adapters, scheduling, persistence, and dependency boundaries.
