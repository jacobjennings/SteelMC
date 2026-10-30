//! Native snapshots of the browser module's generation path at fixed anchors.
//!
//! Writes viewer payloads and complete chunk snapshots for comparison with
//! native Java 26.3. Runs no tests.
//!
//! Usage: `terrain_snapshot OUTPUT [SOURCE_RADIUS]`

use std::fs;
use std::path::Path;
use std::time::Instant;

use rustc_hash::FxHashMap;
use serde_json::json;
use steel_core::worldgen::standalone::{ChunkSnapshot, TileDetail};
use steel_worldgen_wasm::{Generator, canonical_block_state_key};

/// Seed and chunk of each fixed anchor.
const ANCHORS: [(i64, i32, i32); 2] = [(3_860_052, 75, -56), (12_345, -162, -190)];
/// Viewer Full tiles are 64 blocks.
const TILE_SIZE: i32 = 64;

fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    let Some(output) = arguments.get(1).map(Path::new) else {
        panic!("usage: terrain_snapshot OUTPUT [SOURCE_RADIUS]");
    };
    let source_radius = arguments.get(2).map_or(3, |radius| {
        radius
            .parse::<i32>()
            .unwrap_or_else(|error| panic!("invalid source radius {radius}: {error}"))
    });
    fs::create_dir_all(output).unwrap_or_else(|error| panic!("create {output:?}: {error}"));
    let mut anchors = Vec::new();
    for (seed, chunk_x, chunk_z) in ANCHORS {
        let started = Instant::now();
        let mut generator = Generator::new(&seed.to_string(), "overworld", Some(256))
            .unwrap_or_else(|error| panic!("generator: {error}"));
        let construct_ms = started.elapsed().as_secs_f64() * 1000.0;
        let tile_x = (chunk_x * 16).div_euclid(TILE_SIZE) * TILE_SIZE;
        let tile_z = (chunk_z * 16).div_euclid(TILE_SIZE) * TILE_SIZE;
        let mut tiles = Vec::new();
        for (name, detail) in [
            ("biome", TileDetail::Biome),
            ("coarse", TileDetail::Coarse),
            ("full", TileDetail::Full),
        ] {
            let started = Instant::now();
            let payload = generator
                .terrain_json(tile_x, tile_z, TILE_SIZE as u32, 1, detail, Some(true))
                .unwrap_or_else(|error| panic!("{name} tile: {error}"));
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let value: serde_json::Value = serde_json::from_str(&payload)
                .unwrap_or_else(|error| panic!("{name} tile JSON: {error}"));
            write(output, &format!("{seed}-tile-{name}.json"), &payload);
            tiles.push(json!({
                "detail": name,
                "originX": tile_x,
                "originZ": tile_z,
                "elapsedMs": elapsed_ms,
                "bytes": payload.len(),
                "generatedBlocks": value["generatedBlockIndices"].as_array().map_or(0, Vec::len),
                "generatedPalette": value["generatedBlockPalette"],
            }));
        }
        let markers = generator
            .structure_markers_json(chunk_x * 16 + 8, chunk_z * 16 + 8, 512)
            .unwrap_or_else(|error| panic!("markers: {error}"));
        write(output, &format!("{seed}-markers.json"), &markers);

        let started = Instant::now();
        let snapshots = generator
            .worldgen()
            .decorated_chunk_snapshots((chunk_x, chunk_z), source_radius);
        let decorate_ms = started.elapsed().as_secs_f64() * 1000.0;
        for snapshot in &snapshots {
            let distance = (snapshot.chunk_x - chunk_x)
                .abs()
                .max((snapshot.chunk_z - chunk_z).abs());
            write_snapshot(output, seed, snapshot, distance);
        }
        anchors.push(json!({
            "seed": seed.to_string(),
            "chunk": [chunk_x, chunk_z],
            "constructMs": construct_ms,
            "tiles": tiles,
            "sourceRadius": source_radius,
            "snapshotChunks": snapshots.len(),
            "decorateMs": decorate_ms,
            "stats": format!("{:?}", generator.worldgen().stats()),
        }));
    }
    let summary = json!({
        "release": steel_worldgen_wasm::RELEASE,
        "generator": steel_worldgen_wasm::GENERATOR,
        "featureOrder": "x-then-z ascending",
        "anchors": anchors,
    });
    write(
        output,
        "summary.json",
        &serde_json::to_string_pretty(&summary).unwrap_or_default(),
    );
}

fn write_snapshot(output: &Path, seed: i64, snapshot: &ChunkSnapshot, distance: i32) {
    let mut palette = Vec::new();
    let mut lookup = FxHashMap::default();
    let indices = snapshot
        .states
        .iter()
        .map(|state| {
            *lookup.entry(*state).or_insert_with(|| {
                palette.push(canonical_block_state_key(*state));
                palette.len() - 1
            })
        })
        .collect::<Vec<_>>();
    let record = json!({
        "release": "26.3",
        "seed": seed.to_string(),
        "chunk": [snapshot.chunk_x, snapshot.chunk_z],
        "stage": "features",
        "anchorDistance": distance,
        "stateEncoding": "sorted-properties-v1",
        "minY": -64,
        "height": 384,
        "fluidUpdateEncoding": "sorted-world-positions-v1",
        "fluidUpdates": snapshot
            .postprocessing
            .iter()
            .map(|(x, y, z)| [*x, *y, *z])
            .collect::<Vec<_>>(),
        "palette": palette,
        "indices": indices,
    });
    write(
        output,
        &format!(
            "{seed}-{}-{}-features.json",
            snapshot.chunk_x, snapshot.chunk_z
        ),
        &record.to_string(),
    );
}

fn write(output: &Path, name: &str, contents: &str) {
    let path = output.join(name);
    fs::write(&path, contents).unwrap_or_else(|error| panic!("write {path:?}: {error}"));
}
