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
source and is integrated into the viewer through Web Workers. Its tile DTO is a
surface height/color grid with canonical final `surfaceBlocks` IDs plus sparse
portable vegetation placements. It does not expose vertical cave and overhang
geometry, structures, or the complete native Features union. The generated web
package is about 7 MiB before HTTP compression.

### Final-surface and foliage boundary

`terrain_tile` now fills an in-memory 16×16 noise/aquifer chunk and executes
the same reusable `steel-worldgen` Surface kernel used by the native generator.
`surfaceBlocks` is a required, sample-parallel array of canonical final
non-fluid top-block IDs (or `minecraft:air` where no solid column exists).
The retained terrain height is the Y of that same final solid state, leaving
the viewer's water plane independent. Consumers may select an exact renderer
asset directly for a supplied canonical ID when that asset exists;
unrepresented IDs must remain explicitly unmodeled rather than being collapsed
into a lookalike atlas cell.

The implementation boundary is concrete:

1. `steel-core::worldgen::generator::VanillaGenerator::fill_from_noise` builds
   the mutable noise/aquifer column state in a `GenerationChunk<NoisePhase>`.
2. `VanillaGenerator::build_surface` and the WASM host both evaluate generated
   surface rules through `SurfaceStage`, `SurfaceSystem`, and a
   `SurfaceBlockAccess` host. The browser host supplies the exact aquifer
   preliminary-surface corners and a 3×3 chunk biome-palette ring consumed by
   the shared fuzzed-biome lookup.
3. The portable host runs the reusable Carvers stage against its mutable
   post-Surface chunks, including exact aquifer reconstruction and
   `WORLD_SURFACE_WG` updates.
4. A deliberately bounded Features slice then executes real placed-feature
   modifier streams, seeded feature indices, cherry-tree trunk/foliage/
   beehive/leaf-distance algorithms, pink petals, and grass against a
   post-Carvers one-chunk source halo. Tree crowns are retained across source
   chunk boundaries and emitted as canonical final block states.

This is transaction-parity coverage for the selected placed features, not a
claim of whole-Features final-state parity. Native co-resident features share a
`WorldGenRegion` and can alter later collision, survival, and heightmap inputs.
The native regression fixture therefore compares the selected chains on the
same post-Carvers input before those omitted writes; extending the WASM slice to
the full native feature union remains future work.

The shared surface implementation lives below `steel-core`, so it remains safe
for `wasm32-unknown-unknown`; native `GenerationChunk<SurfacePhase>` adapts to
the same traits. Feature/foliage output remains a subsequent
region-generation slice; do not synthesize decoration from biome, height, or
random noise.

The existing `steel-core` generator cannot simply be linked into the browser
leaf crate: its unconditional Tokio networking feature currently pulls in Mio,
which rejects `wasm32-unknown-unknown`. A WASM-ready surface kernel must be
extracted below that server dependency boundary, rather than weakening the
target check or adding a browser-only approximation.

## Milestones

1. Add deterministic native-versus-WASM vectors for biome, density, and complete
   chunk output.
2. Replace the JSON bridge with a stable byte-oriented tile API to reduce copies.
3. Expand the portable selected-feature slice to the complete Features union.
4. Add structures and host-provided persistence, then evaluate optional threaded
   WASM separately.

Do not add browser conditionals to vanilla generation algorithms. Target-specific
code should stay in adapters, scheduling, persistence, and dependency boundaries.
