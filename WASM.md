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
filesystem support into a WASM build. Filesystem-backed saved data and structure
generation are excluded on `wasm32` until persistence has an explicit host
interface. Native `steel-worldgen` still compiles with the same default API.

The next blocking dependency is `simdnbt`, which intentionally supports only
64-bit targets. It enters through the full `steel-registry` and `steel-utils`
crates even though noise, biome, and density generation do not need network NBT
encoding. The preferred fix is to give world generation a narrow generated-data
crate/API containing only its block, biome, dimension, feature, and structure
inputs. That avoids maintaining a browser fork of `simdnbt` and also reduces the
WASM binary substantially.

## Milestones

1. Split the minimal generated worldgen registry boundary from protocol/gameplay
   serialization and make `steel-worldgen` compile for `wasm32-unknown-unknown`.
2. Add deterministic native-versus-WASM vectors for biome, density, and complete
   chunk output.
3. Add a leaf `steel-worldgen-wasm` crate with a stable byte-oriented API and no
   browser DOM dependency.
4. Integrate it through a Web Worker while retaining the same DTO and cache keys
   used by the native backend.
5. Add structures and host-provided persistence, then evaluate optional threaded
   WASM separately.

Do not add browser conditionals to vanilla generation algorithms. Target-specific
code should stay in adapters, scheduling, persistence, and dependency boundaries.
