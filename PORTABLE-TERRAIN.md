# Java 26.3 portable terrain source port

`steel-worldgen::portable_terrain` implements the Overworld Noise and Material
stages without server chunk storage. It starts from upstream
`6a2b22c386ac33a29ba51b08b6f087853a45a62c` (`0.15.3+mc26.3`). The older
portable adapter at `4ff548acd84c0f09af6ca61a4a2eaa2d22e1b3c4` is a host-interface
reference only. No old generation source or binary is relabeled.

This intermediate API is not the viewer's Full terrain module. It does not
execute structure density, carvers, structures or Features. It has no browser
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
coordinates, then call `build_material`. A chunk borrows its generator and
retains private material values. The material transition rejects a different
generator and repeated execution. Block storage follows relative Y, Z, X order.
The height array is first-available Y above the highest non-air state, including
fluid. It is not a solid-floor height or final decorated surface.

The `portable_terrain` example writes block-identifier palettes, 98,304 block
indices and 256 first-available heights for each stage of these fixed chunks:

- seed 3860052, chunk 75,-56, containing anchor 1204,-884
- seed 12345, chunk -162,-190, containing anchor -2592,-3040

ChunkAtlas's `NativePortableTerrain.java` uses the original 26.3 noise, aquifer
and material machinery without starting a server. `compare-port.py` retains
input hashes and every disagreement. These are cursory comparisons. They do not
qualify block properties, carvers, vegetation, Full terrain or rendered cameras.
No test suite is run by the build or comparison entry points.

## Remaining implementation

Continue with [the dependent implementation task](https://beads.thejenningshome.com/#/board/ca-ydrsk).
Port the required carvers and selected vegetation paths from 26.3 source,
connect the browser surface/tile ABI, build actual Wasm, and qualify the camera
terrain. Publish that module before updating the viewer artifact, runtime,
supported release and provenance together. Run the full cached-module preflight
then. Plant submission stays off and existing legal text stays unchanged.
