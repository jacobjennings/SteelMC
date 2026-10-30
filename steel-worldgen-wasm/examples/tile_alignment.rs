//! Fixed-anchor request-alignment measurements, without running a test suite.
//! Usage: tile_alignment OUTPUT

use std::fs;
use std::path::Path;

use serde_json::json;
use steel_core::worldgen::standalone::{GeneratedBlockFilter, TileDetail};
use steel_worldgen_wasm::{Generator, canonical_block_state_key};

fn main() -> Result<(), String> {
    let output = std::env::args().nth(1).ok_or("usage: tile_alignment OUTPUT")?;
    let output = Path::new(&output);
    fs::create_dir_all(output).map_err(|error| error.to_string())?;
    for (seed, x, z) in [("3860052", 1152, -896), ("12345", -2624, -3072)] {
        // Four adjacent tiles plus overlapping probes across both seams, their
        // intersection, non-chunk-aligned crops and the four subdivisions.
        let requests = [
            ("base", x, z, 64),
            ("east", x + 64, z, 64),
            ("south", x, z + 64, 64),
            ("diagonal", x + 64, z + 64, 64),
            ("x-seam", x + 32, z, 64),
            ("z-seam", x, z + 32, 64),
            ("cross-seam", x + 32, z + 32, 64),
            ("unaligned", x + 1, z - 1, 64),
            ("quarter-nw", x, z, 32),
            ("quarter-ne", x + 32, z, 32),
            ("quarter-sw", x, z + 32, 32),
            ("quarter-se", x + 32, z + 32, 32),
        ];
        for (arm, capacity, reverse) in [("forward", 256, false), ("reverse", 25, true)] {
            let mut generator = Generator::new(seed, "overworld", Some(capacity))?;
            for index in 0..requests.len() {
                let index = if reverse { requests.len() - index - 1 } else { index };
                let (name, origin_x, origin_z, size) = requests[index];
                let all = generator.worldgen().tile(
                    origin_x, origin_z, size, 1, TileDetail::Full, GeneratedBlockFilter::All,
                );
                let blocks = all.generated_blocks.iter().map(|block| {
                    json!([block.x, block.y, block.z, canonical_block_state_key(block.state)])
                }).collect::<Vec<_>>();
                let payload = generator.terrain_json(
                    origin_x, origin_z, size, 1, TileDetail::Full, Some(true),
                )?;
                let payload: serde_json::Value = serde_json::from_str(&payload)
                    .map_err(|error| error.to_string())?;
                let record = json!({
                    "release": "26.3", "seed": seed, "arm": arm, "request": name,
                    "origin": [origin_x, origin_z], "size": size,
                    "stateEncoding": "sorted-properties-v1", "allChangedBlocks": blocks,
                    "browser": payload,
                });
                let file = output.join(format!("{seed}-{arm}-{name}.json"));
                fs::write(file, record.to_string()).map_err(|error| error.to_string())?;
                println!("{seed} {arm} {name}: {} all-height changed blocks", blocks.len());
            }
        }
    }
    Ok(())
}
