//! Fixed-anchor delivery measurements. No test suite is run.
use serde_json::json;
use std::{fs, path::Path};
use steel_core::worldgen::standalone::{GeneratedBlockFilter, TileDetail};
use steel_worldgen_wasm::{Generator, canonical_block_state_key};
fn main() -> Result<(), String> {
    let output = std::env::args().nth(1).ok_or("output directory required")?;
    let output = Path::new(&output);
    fs::create_dir_all(output).map_err(|e| e.to_string())?;
    for (seed, cx, cz) in [("3860052", 75, -56), ("12345", -162, -190)] {
        let mut g = Generator::new(seed, "overworld", Some(256))?;
        let before = g.noise_volume_json(cx, cz, -64, 320, 1)?;
        let tile = g.worldgen().tile(
            cx * 16,
            cz * 16,
            16,
            1,
            TileDetail::Full,
            GeneratedBlockFilter::All,
        );
        let expected = tile
            .generated_blocks
            .iter()
            .filter(|b| {
                let column = (b.z.rem_euclid(16) * 17 + b.x.rem_euclid(16)) as usize;
                b.y < i32::from(tile.heights[column])
            })
            .map(|b| {
                json!([
                    (b.y + 64) * 256 + b.z.rem_euclid(16) * 16 + b.x.rem_euclid(16),
                    canonical_block_state_key(b.state)
                ])
            })
            .collect::<Vec<_>>();
        if expected.is_empty() {
            return Err("No below-surface changes observed".to_owned());
        }
        let payload = g.decorated_volume_json(cx, cz, -64, 320, 1)?;
        let value: serde_json::Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
        for entry in &expected {
            let index = entry[0].as_u64().ok_or("invalid index")? as usize;
            let palette = value["voxels"][index].as_u64().ok_or("missing voxel")? as usize;
            if value["stateKeys"][palette] != entry[1] {
                return Err("Changed state lost in volume".to_owned());
            }
        }
        let after = g.noise_volume_json(cx, cz, -64, 320, 1)?;
        if before != after {
            return Err("Noise payload changed after decoration".to_owned());
        }
        let record = json!({"seed":seed,"chunkX":cx,"chunkZ":cz,"expectedBelowSurface":expected,"noiseUnchanged":true,"volume":value});
        fs::write(output.join(format!("{seed}.json")), record.to_string())
            .map_err(|e| e.to_string())?;
        println!(
            "{seed}: {} below-surface changes delivered, noise unchanged",
            expected.len()
        );
    }
    Ok(())
}
