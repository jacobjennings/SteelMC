//! Native snapshots of the browser module's generation path at fixed anchors.
//!
//! Writes viewer payloads and complete chunk snapshots for comparison with
//! native Java 26.3. Snapshots include the POI records a server would save
//! after running the chunk's recorded POI manager tasks. `poi-states.json`
//! lists the runtime POI type of every block state. Runs no tests.
//!
//! Usage: `terrain_snapshot OUTPUT [SOURCE_RADIUS] [SEED:CHUNK_X:CHUNK_Z ...]`
//!
//! Listed anchors replace the two fixed anchors.

use std::fs;
use std::path::Path;
use std::time::Instant;

use rustc_hash::FxHashMap;
use serde_json::json;
use steel_core::poi::PoiTask;
use steel_core::worldgen::standalone::{ChunkSnapshot, TileDetail};
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::BlockStateId;
use steel_worldgen_wasm::{Generator, canonical_block_state_key};

/// Seed and chunk of each fixed anchor.
const ANCHORS: [(i64, i32, i32); 2] = [(3_860_052, 75, -56), (12_345, -162, -190)];
/// Viewer Full tiles are 64 blocks.
const TILE_SIZE: i32 = 64;

fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    let Some(output) = arguments.get(1).map(Path::new) else {
        panic!("usage: terrain_snapshot OUTPUT [SOURCE_RADIUS] [SEED:CHUNK_X:CHUNK_Z ...]");
    };
    let source_radius = arguments.get(2).map_or(3, |radius| {
        radius
            .parse::<i32>()
            .unwrap_or_else(|error| panic!("invalid source radius {radius}: {error}"))
    });
    let listed = arguments
        .iter()
        .skip(3)
        .map(|anchor| parse_anchor(anchor))
        .collect::<Vec<_>>();
    let selected = if listed.is_empty() {
        ANCHORS.to_vec()
    } else {
        listed
    };
    fs::create_dir_all(output).unwrap_or_else(|error| panic!("create {output:?}: {error}"));
    steel_core::bootstrap::init_globals_once();
    write_poi_states(output);
    let mut anchors = Vec::new();
    for (seed, chunk_x, chunk_z) in selected {
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
        let poi_records = snapshots
            .iter()
            .map(|snapshot| snapshot.poi.records.len())
            .sum::<usize>();
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
            "poiRecords": poi_records,
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
        "poiEncoding": "sorted-world-positions-v1",
        "poiRecords": snapshot
            .poi
            .records
            .iter()
            .map(|record| {
                json!([
                    record.pos.x(),
                    record.pos.y(),
                    record.pos.z(),
                    poi_type_key(record.poi_type_id),
                    record.free_tickets,
                ])
            })
            .collect::<Vec<_>>(),
        "poiTasks": {
            "recorded": snapshot.poi_tasks.len(),
            "adds": snapshot.poi_tasks.iter().filter(|task| matches!(task, PoiTask::Add(..))).count(),
            "removes": snapshot.poi_tasks.iter().filter(|task| matches!(task, PoiTask::Remove(_))).count(),
            "added": snapshot.poi.added,
            "unchangedAdds": snapshot.poi.unchanged_adds,
            "replacedAdds": snapshot.poi.replaced_adds,
            "removed": snapshot.poi.removed,
            "missingRemoves": snapshot.poi.missing_removes,
        },
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

fn parse_anchor(anchor: &str) -> (i64, i32, i32) {
    let parts = anchor.split(':').collect::<Vec<_>>();
    let [seed, chunk_x, chunk_z] = parts.as_slice() else {
        panic!("anchor {anchor} is not SEED:CHUNK_X:CHUNK_Z");
    };
    let seed = seed
        .parse()
        .unwrap_or_else(|error| panic!("anchor {anchor}: {error}"));
    let chunk_x = chunk_x
        .parse()
        .unwrap_or_else(|error| panic!("anchor {anchor}: {error}"));
    let chunk_z = chunk_z
        .parse()
        .unwrap_or_else(|error| panic!("anchor {anchor}: {error}"));
    (seed, chunk_x, chunk_z)
}

fn poi_type_key(poi_type_id: usize) -> String {
    REGISTRY
        .poi_types
        .by_id(poi_type_id)
        .unwrap_or_else(|| panic!("unknown POI type {poi_type_id}"))
        .key
        .to_string()
}

/// The runtime state lookup used by the POI callback, for every block state.
fn write_poi_states(output: &Path) {
    let mut types = serde_json::Map::new();
    for (_, poi_type) in REGISTRY.poi_types.iter() {
        types.insert(
            poi_type.key.to_string(),
            json!({
                "maxTickets": poi_type.ticket_count,
                "validRange": poi_type.search_distance,
                "states": Vec::<String>::new(),
            }),
        );
    }
    let mut poi_states = 0;
    for raw in 0..REGISTRY.blocks.next_state_id {
        let state = BlockStateId(raw);
        let Some(poi_type_id) = REGISTRY.poi_types.type_id_for_state(state) else {
            continue;
        };
        poi_states += 1;
        let Some(states) = types
            .get_mut(&poi_type_key(poi_type_id))
            .and_then(|record| record["states"].as_array_mut())
        else {
            unreachable!("every POI type was listed");
        };
        states.push(json!(canonical_block_state_key(state)));
    }
    for record in types.values_mut() {
        if let Some(states) = record["states"].as_array_mut() {
            states.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        }
    }
    let table = json!({
        "release": steel_worldgen_wasm::RELEASE,
        "stateEncoding": "sorted-properties-v1",
        "blockStates": REGISTRY.blocks.next_state_id,
        "poiStates": poi_states,
        "types": types,
    });
    write(
        output,
        "poi-states.json",
        &serde_json::to_string_pretty(&table).unwrap_or_default(),
    );
}

fn write(output: &Path, name: &str, contents: &str) {
    let path = output.join(name);
    fs::write(&path, contents).unwrap_or_else(|error| panic!("write {path:?}: {error}"));
}
