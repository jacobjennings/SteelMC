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
surface height/color grid with canonical final `surfaceBlocks` IDs plus a sparse
`generatedBlocks` list from the portable Features slice. It does not expose
vertical cave and overhang geometry, structures, or the complete native Features
union. The generated web package is about 7 MiB before HTTP compression.

Structure blocks are the largest remaining gap and the reason for it is a crate
boundary, not a missing algorithm. Structure starts and piece layout live in
`steel-worldgen` and already reach the browser, which is how `structure_markers`
reports exact village, portal, and stronghold positions. Every structure piece
block placer lives in `steel-core`, which depends on Tokio, the filesystem, and
the entity system and does not build for `wasm32-unknown-unknown`. Moving those
placers behind a host trait, the way the Features slice already is, is what a
browser village would take.

### Generated-block payload

`terrain_tile` returns `generatedBlocks`, a sparse list of the block states the
Features stage placed after the surface stage. Every entry carries an absolute
world X, Y and Z, a canonical block identifier, and a canonical state string.
An entry may be `minecraft:air` where a generated feature clears terrain the
surface stage had filled.

The field is deliberately general. It carries no marker saying which feature or
structure produced an entry, because the set of generatable producers grows
over time and a consumer that branches on the producer would need changing
every time it does. Classify an entry by its block. Packed ice from an ice
spike is a solid cube and cherry leaves are foliage, and only the block
identifier says which.

The field was named `vegetationBlocks` while trees and grass were the only
things it carried. It was renamed when ice spikes joined them, because a
consumer that assumed vegetation would have drawn a packed ice spire as a
crossed foliage sprite.

The list is a palette. `generatedBlockPalette` holds each distinct state once,
`generatedBlockPositions` holds a flat `x, y, z` triple per placement, and
`generatedBlockIndices` holds one palette index per placement. A tile in an ice
spikes biome carries tens of thousands of placements drawn from two distinct
states, and repeating the identifier strings on each one cost more than every
other field in the response combined. Measured on one such tile set, the
palette took the response from 5,588,691 bytes to 1,556,501.

Which features run in the browser is decided by
`steel-worldgen::vegetation::is_portable_sparse_feature`, which walks a
feature's placement modifiers, its configured kind, its providers, its block
predicates, and the blocks it would place, and answers yes only when every part
is supported. It must stay conservative in one direction: answering yes for a
feature the slice cannot finish would place some of its blocks and drop the
rest, which is worse than placing none. Vanilla seeds each feature from its own
index rather than sequentially, so refusing a feature does not disturb the
randomness of the ones that run.

Thirty-nine of the two hundred and sixty-two vanilla placed features qualify.
What is still refused, and why:

- **Some trees.** The straight trunk placer, the blob, pine and spruce foliage
  placers, and the fallen tree feature are ported and verified, which covers
  oak, birch, spruce and pine. Seven trunk placers (forking, giant, fancy, dark
  oak, mega jungle, bending, upwards branching) and six foliage placers
  (acacia, bush, fancy, jungle, mega pine, random spread) are not.

  Biomes do not name a tree directly. They name a selector such as
  `trees_taiga`, which picks between several trees, and the selector runs only
  when every branch of it does. `trees_cherry`, `trees_grove`, `trees_taiga`,
  `trees_snowy` and `trees_birch` are supported. The rest each reach a fancy
  oak.
- **The fancy trunk and foliage placers**, the large oak, which block
  `trees_plains`, `trees_water`, `trees_meadow`, `trees_flower_forest`,
  `trees_windswept_hills` and the forest selector. They were written once and
  removed, not because they disagreed with anything but because **no fixture
  grows one to compare**. Among the selectors the slice supports only meadow
  and ocean can produce a large oak, both rarely, and a scan of forty-nine
  chunks around a meadow found none. Untested is not verified.

  A fixture has to come first, and there are two ways to get one. A seed scan
  at scale: run `SurfaceSampler::selected_vegetation_transaction_snapshot` over
  a few thousand meadow and ocean chunks and keep the first that contains an
  oak log, which is minutes of compute rather than the seconds a small scan
  gets. Or a forced-placement test mode: call the tree feature directly at a
  chosen position on both sides, skipping the selector and its rarity roll
  entirely, which is faster and also the only way to reach a placer no biome
  can produce at all.

  Tree placers consume random numbers in a fixed order and a mistake produces
  forests that look entirely reasonable and are not the seed's forests. Any
  port is therefore checked against the native runner rather than reviewed by
  eye. The fixture is `portable_features_match_native` in
  `steel-core::worldgen::chunk_stage_hashes`. It now asserts that both sides
  start from identical pre-feature terrain before it compares any feature,
  because feature parity measured on top of differing ground is meaningless. It selects exactly the features
  the portable slice claims, compares the full set of blocks each side changed
  with no allowlist to hide behind, and requires the two to be equal. It runs
  eight chunks across three seeds and reports how many trunk and leaf blocks it
  actually compared, so it cannot pass by comparing nothing.

  The fixture compares each feature family on its own, trees and ground
  vegetation, because a bug in one family must not be able to convict another.
  A ground vegetation difference once got the fallen tree feature refused, and
  it later reproduced with every tree placer switched off. Both families now
  match the native runner across all eleven chunks.

  They disagree when run together, and that is kept as a reproduction in
  `known_portable_feature_divergences`, which is ignored by default:

  - Seed 1, chunk (-124, -128), a taiga. Five ground plants out of two hundred
    and ninety differ once trees also run.
  - Seed 7, chunk (50, -98), a meadow. Five short grass placements out of
    twenty differ with no trees involved at all, isolating to
    `patch_grass_meadow` running alone.

  What is ruled out for both, by assertions the fixture makes before it
  compares anything: the pre-feature terrain is identical block for block, and
  so is the pre-feature `WORLD_SURFACE_WG` heightmap. The native survival rule
  is the same block tag check the portable slice makes. What is left is that
  the two sides read a different surface height *during* the stage, after a
  feature has already placed something in that column. The diverging candidates
  sit on the same columns, exactly one block higher on the portable side.
  Vanilla's proto chunk updates that heightmap on every block write, which is
  what the portable slice does by rescanning the column, so the native
  heightmap maintenance during the feature stage is the suspect rather than the
  portable one. Fixing it therefore means changing the server generator, which
  needs its own check against the recorded Minecraft chunk hashes.

  - Seed 12345, chunk (-26, -20), an ocean. Unrelated and underground: the
    terrain disagrees before any feature runs, at two blocks where the native
    carvers cut a cave and the portable carvers do not. The only carver parity
    fixture until now covered a single chunk.
- **Mushrooms.** Survival depends on the light level at the placement position
  and there is no lighting stage. Guessing would carpet daylit meadows.
- **Column plants** such as sugar cane, cactus, kelp and bamboo. These are
  separate configured feature kinds, not a per-block survival rule.
- **Attached and hanging blocks** such as vines and glow lichen. They need a
  face-sturdiness query the slice does not implement.
- **Anything using a height range, an environment scan, a per-layer count, or a
  noise-based count.** These modifiers need world state the slice does not
  carry, which is most of the ore and underground decoration set.

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
