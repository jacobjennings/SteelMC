# Java 26.3 portable terrain source port

`steel-worldgen::portable_terrain` implements the Overworld Noise, Material and Carvers
stages without server chunk storage. It starts from upstream
`6a2b22c386ac33a29ba51b08b6f087853a45a62c` (`0.15.3+mc26.3`). The older
portable adapter at `4ff548acd84c0f09af6ca61a4a2eaa2d22e1b3c4` is a host-interface
reference only. No old generation source or binary is relabeled.

This intermediate API is not the viewer's Full terrain module. It does not
execute structure density, structures or Features. It has no browser
ABI or Wasm package yet. Its two anchor chunks do not cover the terrain demanded
by the complete horizon, pan and close-plant cameras.

## Source contracts

The official Java 26.3 runtime SHA-256 is
`a362163eec5d1612d520772bc16e5b39c09e3b234fdc045f56bf544284ee8ae6`.
ChunkAtlas's `tools/steel-26-3/collect-material-source.py` retains hashes and
source for the relevant classes through the existing pinned collector.

| Native contract | Portable implementation |
| --- | --- |
| `NoiseBasedChunkGenerator.doFill` and `NoiseChunk` | `PortableOverworld::noise_chunk` calls the upstream 26.3 float density interpolator and aquifer, and retains material density/richness values before surface writes. |
| `MaterialSystem.buildSurface` and `OreVeinRule.compile` | `SurfaceStage::build_surface` evaluates material rules for every solid block. The prefilled ore values remain separate from mutated block storage. Fluid-bearing states reset the appropriate traversal context. |
| `RandomState` constructor and `NoiseRouterData.bootstrapOverworld` | Material uses `chunkSurfaceLevel`, which wraps the raw surface function with `InterpolatedFunction(..., 16, 1)`. Aquifers continue using the raw surface function. |
| `InterpolatedFunction.Sampler.fillCell` and `MaterialRuleContext.getMinSurfaceLevel` | `MaterialSurfaceGrid` interpolates float corners in Z/X order. The material context floors the interpolated value before adding surface depth minus eight. |
| `SteepCondition.compute` | Preserve negative X or positive Z gradients of at least four blocks. |
| `BiomeManager.getFiddledDistance` | Preserve native Z/Y/X double addition order and first-minimum corner selection. |
| `MaterialSystem` extensions | Surface noise, clay bands, temperature and extension helpers come from upstream 26.3. Eroded badlands stops specifically at water. Frozen oceans return when the extension height is at most two. |

Registries and worldgen definitions come from the existing upstream build
scripts and extracted data. This port does not hand-author extracted assets.
The server's generation entry points remain unchanged.

## API and comparison

Create one `PortableOverworld` for a seed, request a `noise_chunk` with chunk
coordinates, then call `build_material` and `build_carvers` in that order. A chunk borrows its generator and
retains private material values. The material transition rejects a different
generator and repeated execution. Block storage follows relative Y, Z, X order.
Carvers retain the Noise aquifer and pre-Material ore buffers until their mask
has been applied. Completed Carvers chunks release those temporary buffers.
Fluid postprocessing positions remain available for the next stage.
The height array is first-available Y above the highest non-air state, including
fluid. It is not a solid-floor height or final decorated surface.

The `portable_terrain` example writes block-state palettes with sorted property names, 98,304 block
indices and 256 first-available heights for each of Noise, Material and Carvers at these fixed chunks:

- seed 3860052, chunk 75,-56, containing anchor 1204,-884
- seed 12345, chunk -162,-190, containing anchor -2592,-3040

ChunkAtlas's `NativePortableTerrain.java` uses the original 26.3 noise, aquifer,
material and carver machinery without starting a server. `compare-port.py` retains
input hashes and every disagreement. These are cursory comparisons. They cover block identifiers and properties at those anchors. They do not
qualify vegetation, Full terrain or rendered cameras. Fluid-update position
comparisons are recorded separately below.
No test suite is run by the build or comparison entry points.

## Remaining implementation

Continue with [the dependent implementation task](https://beads.thejenningshome.com/#/board/ca-w2zye).
Port the selected vegetation paths from 26.3 source,
connect the browser surface/tile ABI, build actual Wasm, and qualify the camera
terrain. Publish that module before updating the viewer artifact, runtime,
supported release and provenance together. Run the full cached-module preflight
then. Plant submission stays off and existing legal text stays unchanged.

## Portable Carvers contracts

`portable_carver` owns cave and canyon geometry independently of server chunk
storage. Its block host extends the Material-stage host with retained fluid
postprocessing positions. `build_carvers` rejects cross-generator chunks and
out-of-order or repeated stage application.

The pinned 26.3 `NoiseBasedChunkGenerator.generateCarvers` visits a 17 by 17
source-chunk square in X/Z order. Each biome's carver list reseeds LegacyRandom
with seed plus list index before its probability draw. Cave rooms, tunnel
splits and canyon width factors keep source RNG and float arithmetic order.
`WorldCarver.carveEllipsoid` clips to mask bounds minY plus one through
minY plus height minus eight. Its lower-Y loop is exclusive.

`CarvingMask` indexes Y within Z within X. It visits contiguous bit ranges,
splitting each at column boundaries. `applyCarvingMask` resets the grass flag
for each visited segment, checks the generated `uncarvable` block tag, asks the
retained aquifer for density zero and records fluid postprocessing positions.
Grass and mycelium checks use block identity, including nondefault properties.
Top material uses the same float chunk-surface function and prefilled ore
values as Material, with fuzzed biome access and live world-surface gradients.

ChunkAtlas's `collect-carver-source.py` retains the eight source classes and
17 class hashes from the same pinned 26.3 runtime. No extracted registry data
was hand-authored. The existing server's carver entry points are unchanged.


## Retained fluid postprocessing positions

Noise retains each fluid position marked by the aquifer after its substance
lookup, matching `NoiseBasedChunkGenerator.doFill`. Carvers appends its own
marks. Later block replacements do not remove earlier marks. The API keeps
repeated marks because `ProtoChunk.markPosForPostProcessing` appends to a
section list without deduplication.

The snapshot example adds `sorted-world-positions-v1` fluid positions at Noise,
Material and Carvers. The native collector calls the actual `doFill` method and
unpacks `ProtoChunk` section lists with `unpackOffsetCoordinates`. Comparison
uses multisets, including duplicate counts, rather than insertion order.

The two fixed chunks retain 0 and 203 Noise marks respectively. After Carvers
they retain 0 and 274 marks. Native and Rust positions agree at all three stages.
This qualifies retained positions at these anchors only. It does not execute
fluid ticks or prove update order, Features, complete camera footprints or
rendered acceptance. Viewer module promotion remains unavailable.

## Portable ground-cover kernels

`portable_feature` implements the Java 26.3 `ProjectedRandomPatchySquare` and
`RandomNeighborSpreadFeature` loops independently of server region storage.
The server calls these same kernels through `GroundCoverRegion`. That binding
retains its existing predicate, state-provider, holder-set and block-write
implementations. `GroundCoverAccess` requires each of these operations from
its host. It supplies no missing-world or missing-provider fallback.

Projection preserves X/Z traversal, the per-cell probability draw, projection
through the block below and optional provider sampling. Native bytecode confirms
that projection decrements its budget after moving. A zero budget can therefore
move down once. Neighbor spread writes its origin before sampling attempts,
consumes X/Y/Z offset draws in order and accepts exactly one of six neighbors.
Later attempts see earlier writes. Both algorithms preserve the native true
return value independently of individual write acceptance.

The snapshot example accepts an optional `ground-cover` argument. After the
ordinary Noise/Material/Carvers snapshots, it makes two isolated feature calls
on each retained chunk, then records the next RNG integer. Its explicit
comparison configurations use a size-two short-grass projection with maximum
height three, then 64 oak-leaf neighbor-spread attempts with horizontal offsets
-2..2 and vertical offsets -1..1. The Java collector invokes the actual pinned
native feature classes with those same configurations. These configurations
are comparison inputs only. They are not extracted biome feature definitions
and never run in the portable generator's stage pipeline.

At seed 3860052, projection changes 18 states and spread changes 13. At seed
12345, they change 17 and 10. All five stages agree in block identifiers,
sorted properties, heights and retained fluid-position multisets at both
chunks. The next RNG integers agree at 749985977 and 1920669674. This comparison
covers simple providers, air predicates and direct neighbor sets. It does not
cover optional provider refusal, weighted providers, write refusal, natural
biome decoration, cross-chunk feature ordering or selected native plants.

The complete decoration region, selected vegetation, structure density,
structure/feature interactions, browser ABI, dependency Wasm portability,
artifact publication and full camera qualification remain on the linked
continuation. The current source increment does not admit a viewer release.

## Retained feature-region storage

`PortableRegion` takes a complete rectangular set of this generator's completed
Carvers chunks. It rejects empty, duplicate, mixed-generator, wrong-stage and
incomplete input. It owns the chunks without regenerating neighbors. All feature
calls share their current states, heightmaps and retained fluid-position lists.

`write_window` opens a horizontal zone around a retained center chunk. The full
zone must already be present. `RegionWindow::can_write` follows the pinned 26.3
`WorldGenRegion.isWithinWriteZone` chessboard rule. A rejected write returns false
before looking up storage. Negative coordinates use floor division. Reads can
reach retained neighbors outside the write zone. Missing chunks and unsupported
vertical reads fail explicitly. This view is for fresh Overworld storage and does
not implement upgrading chunks or native out-of-build-height reads.

The view exposes `write_state`, not the complete native `setBlock` operation.
The feature host still owes POI, block-entity and postprocessing effects, provider
and survival dispatch, native placement modifiers and feature selection/order.
Those paths have no fallback in this storage API. Structure density remains
absent from the retained terrain. These chunks must not be promoted to Full.

The portable example's `region` mode retains 3 by 3 Noise/Material/Carvers chunks
around each existing anchor. It applies the same explicit size-two grass
projection at the center chunk's positive corner with radius-zero write rejection.
Two successive leaf-spread calls then share a radius-one window and RNG, including
an origin in the diagonal neighbor. Native comparison uses the native feature
kernels with an explicit storage-only region host. Both hosts count accepted and
rejected writes. These configurations exercise storage boundaries and ordered
calls, not naturally selected vegetation or native biome decoration order.

The source contract is the existing Java 26.3 `WorldGenRegion.class` SHA-256
`f33962d43745ecd21ca163ab713c2af2f8fbcc84b490c285e4b5d8ea0f3b744e`.
Its retained decompiled source SHA-256 is
`d19dcd3701d950f4d8b604ee2efb3c044cb19499ef0025b0517196c3b8bb3339`.

[The implementation continuation](https://beads.thejenningshome.com/#/board/ca-4jrrw)
retains natural decoration, complete demanded terrain, full write side effects,
Wasm packaging and every viewer qualification requirement.

## Portable state-provider sampling

`portable_feature::providers` consumes the existing generated
`BlockStateProviderKind` data. It resolves references, simple states, weighted
states and rule-based states without a server dependency. `ProviderAccess`
supplies fallible live predicates and block reads. Native optional refusal is
`Ok(None)`. An unavailable provider or world operation is an error and cannot
become a state. The other seven provider families return errors naming the
unavailable family.

The source contracts are Java 26.3 `BlockStateProvider`, `SimpleStateProvider`,
`WeightedStateProvider`, `RuleBasedStateProvider`, `WeightedList`, `Weighted`
and `WeightedRandom`, retained by ChunkAtlas's `collect-provider-source.py`.
Weighted selection consumes one bounded integer, including a single-state list.
It preserves entry order and zero-weight entries. Negative weights, empty total
weight and total-weight overflow are refused before consuming RNG. The compact
native selector's ordered subtraction is equivalent to its flat selector.
Rules run in source order. A true rule whose nested optional provider declines
continues to the next rule. Fallback runs only after all rules decline.
Required sampling of an ordinary refusal reads the current block, exactly as
native `RuleBasedStateProvider.getState` does. This does not authorize an
absent-world fallback.

The example's `providers` mode retains the `region` comparison footprint and
all Noise/Material/Carvers snapshots. Its explicit projection uses nested air
rules and weighted short grass/fern. Spreading uses weighted oak/birch leaves
and shares storage and RNG across two calls. Direct provider readbacks cover the generated soil provider reference,
optional refusal, required live-state reads, nested continuation, fallback and
weighted totals 1, 3 and 64 with zero-weight entries. These configurations are
comparison inputs, not hand-authored native registry assets or biome features.

This increment does not integrate natural biome decoration or extend the
server's provider dispatch. Survival, native placement/order, remaining provider
families, complete setBlock side effects, structures, browser ABI and Wasm,
artifact promotion and rendered scene qualification remain unfinished. No
viewer package is qualified by these provider comparisons.
