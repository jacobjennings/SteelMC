//! Fixed anchor chunks for a cursory source-port comparison, not a benchmark.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::json;
use steel_registry::REGISTRY;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_worldgen::portable_terrain::{MINECRAFT_VERSION, PortableChunk, PortableOverworld};

fn snapshot(
    output: &Path,
    seed: u64,
    x: i32,
    z: i32,
    stage: &str,
    chunk: &PortableChunk<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut palette = Vec::new();
    let mut lookup = BTreeMap::new();
    let mut indices = Vec::with_capacity(chunk.states().len());
    for state in chunk.states() {
        let mut key = state.get_block().key.to_string();
        let mut properties = REGISTRY.blocks.get_properties(*state);
        properties.sort_by(|a, b| a.0.cmp(b.0));
        if !properties.is_empty() {
            key.push('[');
            key.push_str(
                &properties
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            key.push(']');
        }
        let next = palette.len();
        let index = *lookup.entry(key.clone()).or_insert_with(|| {
            palette.push(key);
            next
        });
        indices.push(index);
    }
    let record = json!({
        "release": MINECRAFT_VERSION, "seed": seed.to_string(), "chunk": [x, z],
        "stage": stage, "stateEncoding": "sorted-properties-v1", "minY": -64, "height": 384,
        "palette": palette, "indices": indices, "heights": chunk.heights().as_slice(),
    });
    fs::write(
        output.join(format!("{seed}-{x}-{z}-{stage}.json")),
        serde_json::to_vec(&record)?,
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args().nth(1).ok_or("output directory required")?;
    let output = Path::new(&output);
    fs::create_dir_all(output)?;
    for (seed, x, z) in [(3_860_052, 75, -56), (12_345, -162, -190)] {
        let sampler = PortableOverworld::new(seed);
        let mut chunk = sampler.noise_chunk(x, z)?;
        snapshot(output, seed, x, z, "noise", &chunk)?;
        sampler.build_material(&mut chunk)?;
        snapshot(output, seed, x, z, "material", &chunk)?;
        sampler.build_carvers(&mut chunk)?;
        snapshot(output, seed, x, z, "carvers", &chunk)?;
    }
    Ok(())
}
