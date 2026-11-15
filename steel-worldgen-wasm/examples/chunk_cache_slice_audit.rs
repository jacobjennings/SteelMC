//! Measures Steel chunk-cache hit rates for 64x64 Full slices against 256x256 tiles
//! and verifies binary tile encoding parity.

use serde::Serialize;
use std::env;
use std::fs;
use std::time::Instant;
use steel_core::worldgen::standalone::{StandaloneStats, TileDetail};
use steel_worldgen_wasm::{BINARY_TILE_MAGIC, BINARY_TILE_VERSION, Generator};

#[derive(Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct AggregatedCacheStats {
    starts_generated: u64,
    starts_hits: u64,
    starts_hit_rate: f64,
    biomes_generated: u64,
    biomes_hits: u64,
    biomes_hit_rate: f64,
    terrain_generated: u64,
    terrain_hits: u64,
    terrain_carve_upgrades: u64,
    terrain_hit_rate: f64,
    decorated_generated: u64,
    decorated_hits: u64,
    decorated_hit_rate: f64,
    feature_regions: u64,
}

fn rate(hits: u64, misses: u64) -> f64 {
    let total = hits + misses;
    if total == 0 {
        0.0
    } else {
        (hits as f64) / (total as f64)
    }
}

fn summarize_stats(list: &[StandaloneStats]) -> AggregatedCacheStats {
    let mut starts_generated = 0;
    let mut starts_hits = 0;
    let mut biomes_generated = 0;
    let mut biomes_hits = 0;
    let mut terrain_generated = 0;
    let mut terrain_hits = 0;
    let mut terrain_carve_upgrades = 0;
    let mut decorated_generated = 0;
    let mut decorated_hits = 0;
    let mut feature_regions = 0;
    for s in list {
        starts_generated += s.starts_generated;
        starts_hits += s.starts_hits;
        biomes_generated += s.biomes_generated;
        biomes_hits += s.biomes_hits;
        terrain_generated += s.terrain_generated;
        terrain_hits += s.terrain_hits;
        terrain_carve_upgrades += s.terrain_carve_upgrades;
        decorated_generated += s.decorated_generated;
        decorated_hits += s.decorated_hits;
        feature_regions += s.feature_regions;
    }
    let terrain_misses = terrain_generated;
    AggregatedCacheStats {
        starts_generated,
        starts_hits,
        starts_hit_rate: rate(starts_hits, starts_generated),
        biomes_generated,
        biomes_hits,
        biomes_hit_rate: rate(biomes_hits, biomes_generated),
        terrain_generated,
        terrain_hits,
        terrain_carve_upgrades,
        terrain_hit_rate: rate(terrain_hits, terrain_misses),
        decorated_generated,
        decorated_hits,
        decorated_hit_rate: rate(decorated_hits, decorated_generated),
        feature_regions,
    }
}

fn spatial_hash(block_x: i32, block_z: i32) -> i32 {
    block_x.wrapping_mul(0x1f1f_1f1f) ^ block_z.wrapping_mul(0x5f35_6495)
}

fn preferred_worker(tile_x: i32, tile_z: i32, worker_count: usize) -> usize {
    let block_x = tile_x >> 2;
    let block_z = tile_z >> 2;
    let modulus = i32::try_from(worker_count).unwrap_or(1);
    let rem = ((spatial_hash(block_x, block_z) % modulus) + modulus) % modulus;
    usize::try_from(rem).unwrap_or(0)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    name: String,
    description: String,
    output_chunks: u64,
    jobs: usize,
    workers_used: usize,
    elapsed_ms: f64,
    binary_bytes_total: usize,
    json_bytes_total: usize,
    terrain_generated_per_output_chunk: f64,
    starts_generated_per_output_chunk: f64,
    stats: AggregatedCacheStats,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditReport {
    seed: String,
    binary_header_verified: bool,
    cases: Vec<CaseReport>,
}

fn new_gen(seed: &str) -> Generator {
    let mut g = Generator::new(seed, "overworld", Some(256))
        .unwrap_or_else(|err| panic!("generator init failed: {err}"));
    g.set_coarse_surface_source("bounded")
        .unwrap_or_else(|err| panic!("bounded source failed: {err}"));
    g
}

fn verify_binary_header(bytes: &[u8]) -> bool {
    if bytes.len() < 44 {
        return false;
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    magic == BINARY_TILE_MAGIC && version == BINARY_TILE_VERSION
}

fn main() {
    let out_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "chunk-cache-slice-audit.json".to_owned());
    let seed = "14760316";
    let mut cases = Vec::new();
    let mut header_ok = true;

    // Case 1: Single 256x256 Full tile (256 chunks) on 1 worker
    {
        let mut g = new_gen(seed);
        let t0 = Instant::now();
        let bin = g
            .terrain_binary(-512, 0, 256, 4, TileDetail::Full)
            .unwrap_or_else(|e| panic!("{e}"));
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        header_ok &= verify_binary_header(&bin);
        let stats = summarize_stats(&[g.stats()]);
        let json = g
            .terrain_json(-512, 0, 256, 4, TileDetail::Full, Some(true))
            .unwrap_or_else(|e| panic!("{e}"));
        cases.push(CaseReport {
            name: "single-256x256-1worker".to_owned(),
            description: "One 256x256 Full tile (256 chunks) on a single cold worker".to_owned(),
            output_chunks: 256,
            jobs: 1,
            workers_used: 1,
            elapsed_ms,
            binary_bytes_total: bin.len(),
            json_bytes_total: json.len(),
            terrain_generated_per_output_chunk: stats.terrain_generated as f64 / 256.0,
            starts_generated_per_output_chunk: stats.starts_generated as f64 / 256.0,
            stats,
        });
    }

    // Case 2: Sixteen 64x64 Full slices (256 chunks) on 1 worker (same locality block)
    {
        let mut g = new_gen(seed);
        let t0 = Instant::now();
        let mut bin_bytes = 0;
        for sz in 0..4 {
            for sx in 0..4 {
                let bin = g
                    .terrain_binary(-512 + sx * 64, sz * 64, 64, 4, TileDetail::Full)
                    .unwrap_or_else(|e| panic!("{e}"));
                header_ok &= verify_binary_header(&bin);
                bin_bytes += bin.len();
            }
        }
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let stats = summarize_stats(&[g.stats()]);
        let mut json_bytes = 0;
        for sz in 0..4 {
            for sx in 0..4 {
                let j = g
                    .terrain_json(-512 + sx * 64, sz * 64, 64, 4, TileDetail::Full, Some(true))
                    .unwrap_or_else(|e| panic!("{e}"));
                json_bytes += j.len();
            }
        }
        cases.push(CaseReport {
            name: "slices-16x-64x64-1worker".to_owned(),
            description: "Sixteen 64x64 Full slices (256 chunks) on one worker (4x4 locality block)".to_owned(),
            output_chunks: 256,
            jobs: 16,
            workers_used: 1,
            elapsed_ms,
            binary_bytes_total: bin_bytes,
            json_bytes_total: json_bytes,
            terrain_generated_per_output_chunk: stats.terrain_generated as f64 / 256.0,
            starts_generated_per_output_chunk: stats.starts_generated as f64 / 256.0,
            stats,
        });
    }

    // Case 3: Sixteen 64x64 Full slices spread across 16 distinct cold workers (maximal parallelism, zero inter-slice sharing)
    {
        let mut workers: Vec<Generator> = (0..16).map(|_| new_gen(seed)).collect();
        let t0 = Instant::now();
        let mut bin_bytes = 0;
        for (idx, worker) in workers.iter_mut().enumerate() {
            let sx = i32::try_from(idx % 4).unwrap_or(0);
            let sz = i32::try_from(idx / 4).unwrap_or(0);
            let bin = worker
                .terrain_binary(-512 + sx * 64, sz * 64, 64, 4, TileDetail::Full)
                .unwrap_or_else(|e| panic!("{e}"));
            bin_bytes += bin.len();
        }
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let raw_stats: Vec<StandaloneStats> = workers.iter().map(Generator::stats).collect();
        let stats = summarize_stats(&raw_stats);
        cases.push(CaseReport {
            name: "slices-16x-64x64-16workers".to_owned(),
            description: "Sixteen 64x64 Full slices (256 chunks) spread 1-per-worker across 16 cold workers".to_owned(),
            output_chunks: 256,
            jobs: 16,
            workers_used: 16,
            elapsed_ms,
            binary_bytes_total: bin_bytes,
            json_bytes_total: 0,
            terrain_generated_per_output_chunk: stats.terrain_generated as f64 / 256.0,
            starts_generated_per_output_chunk: stats.starts_generated as f64 / 256.0,
            stats,
        });
    }

    // Case 4: 3x3 of 256x256 Full tiles (2,304 chunks) across 28 workers (main's 256-grid Full behavior: only 9 workers used)
    {
        let mut workers: Vec<Generator> = (0..28).map(|_| new_gen(seed)).collect();
        let mut used = [false; 28];
        let t0 = Instant::now();
        let mut bin_bytes = 0;
        let mut cursor = 0_usize;
        for tz in -1..=1 {
            for tx in -3..=-1 {
                let mut w = preferred_worker(tx, tz, 28);
                if used[w] {
                    w = cursor;
                }
                used[w] = true;
                cursor = (cursor + 1) % 28;
                let bin = workers[w]
                    .terrain_binary(tx * 256, tz * 256, 256, 4, TileDetail::Full)
                    .unwrap_or_else(|e| panic!("{e}"));
                bin_bytes += bin.len();
            }
        }
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let raw_stats: Vec<StandaloneStats> = workers.iter().map(Generator::stats).collect();
        let stats = summarize_stats(&raw_stats);
        cases.push(CaseReport {
            name: "square-3x3-256x256-9of28workers".to_owned(),
            description: "3x3 square of 256x256 Full tiles (2,304 chunks) on 28-worker pool (9 workers active)".to_owned(),
            output_chunks: 2304,
            jobs: 9,
            workers_used: used.iter().filter(|x| **x).count(),
            elapsed_ms,
            binary_bytes_total: bin_bytes,
            json_bytes_total: 0,
            terrain_generated_per_output_chunk: stats.terrain_generated as f64 / 2304.0,
            starts_generated_per_output_chunk: stats.starts_generated as f64 / 2304.0,
            stats,
        });
    }

    // Case 5: Same 2,304-chunk square as 144 x 64x64 slices distributed across all 28 workers with 4-candidate block spillover
    {
        let mut workers: Vec<Generator> = (0..28).map(|_| new_gen(seed)).collect();
        let mut loads = [0_usize; 28];
        let mut used = [false; 28];
        let t0 = Instant::now();
        let mut bin_bytes = 0;
        let mut slices = Vec::new();
        for tz in -4..8 {
            for tx in -12..0 {
                slices.push((tx, tz));
            }
        }
        for (idx, (sx, sz)) in slices.iter().copied().enumerate() {
            if idx > 0 && idx % 28 == 0 {
                loads = [0; 28];
            }
            let pref = preferred_worker(sx, sz, 28);
            let mut chosen = None;
            for off in 0..4 {
                let c = (pref + off) % 28;
                if loads[c] == 0 {
                    chosen = Some(c);
                    break;
                }
            }
            let w = chosen.unwrap_or_else(|| {
                loads
                    .iter()
                    .position(|l| *l == 0)
                    .unwrap_or(idx % 28)
            });
            loads[w] += 1;
            used[w] = true;
            let bin = workers[w]
                .terrain_binary(sx * 64, sz * 64, 64, 4, TileDetail::Full)
                .unwrap_or_else(|e| panic!("{e}"));
            bin_bytes += bin.len();
        }
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let raw_stats: Vec<StandaloneStats> = workers.iter().map(Generator::stats).collect();
        let stats = summarize_stats(&raw_stats);
        cases.push(CaseReport {
            name: "square-12x12-64x64-28workers".to_owned(),
            description: "Same 2,304-chunk square split into 144 x 64x64 slices across all 28 workers".to_owned(),
            output_chunks: 2304,
            jobs: 144,
            workers_used: used.iter().filter(|x| **x).count(),
            elapsed_ms,
            binary_bytes_total: bin_bytes,
            json_bytes_total: 0,
            terrain_generated_per_output_chunk: stats.terrain_generated as f64 / 2304.0,
            starts_generated_per_output_chunk: stats.starts_generated as f64 / 2304.0,
            stats,
        });
    }

    // Case 6 & 7: Close 2,704-chunk square (13x13 of 64x64 at resolution 1) cold pool vs Coarse-warm pool
    for warm_coarse in [false, true] {
        let mut workers: Vec<Generator> = (0..28).map(|_| new_gen(seed)).collect();
        let mut slices = Vec::new();
        for sz in -4..9 {
            for sx in -14..-1 {
                slices.push((sx, sz));
            }
        }
        if warm_coarse {
            let mut loads = [0_usize; 28];
            for (idx, (sx, sz)) in slices.iter().copied().enumerate() {
                if idx > 0 && idx % 28 == 0 {
                    loads = [0; 28];
                }
                let pref = preferred_worker(sx, sz, 28);
                let mut chosen = None;
                for off in 0..4 {
                    let c = (pref + off) % 28;
                    if loads[c] == 0 {
                        chosen = Some(c);
                        break;
                    }
                }
                let w = chosen.unwrap_or_else(|| loads.iter().position(|l| *l == 0).unwrap_or(idx % 28));
                loads[w] += 1;
                let _ = workers[w]
                    .terrain_binary(sx * 64, sz * 64, 64, 1, TileDetail::Coarse)
                    .unwrap_or_else(|e| panic!("{e}"));
            }
            for worker in &mut workers {
                worker.reset_stats();
            }
        }
        let mut loads = [0_usize; 28];
        let mut used = [false; 28];
        let t0 = Instant::now();
        let mut bin_bytes = 0;
        let mut json_bytes = 0;
        for (idx, (sx, sz)) in slices.iter().copied().enumerate() {
            if idx > 0 && idx % 28 == 0 {
                loads = [0; 28];
            }
            let pref = preferred_worker(sx, sz, 28);
            let mut chosen = None;
            for off in 0..4 {
                let c = (pref + off) % 28;
                if loads[c] == 0 {
                    chosen = Some(c);
                    break;
                }
            }
            let w = chosen.unwrap_or_else(|| loads.iter().position(|l| *l == 0).unwrap_or(idx % 28));
            loads[w] += 1;
            used[w] = true;
            let bin = workers[w]
                .terrain_binary(sx * 64, sz * 64, 64, 1, TileDetail::Full)
                .unwrap_or_else(|e| panic!("{e}"));
            bin_bytes += bin.len();
        }
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let raw_stats: Vec<StandaloneStats> = workers.iter().map(Generator::stats).collect();
        let stats = summarize_stats(&raw_stats);
        // Also measure JSON size on warm cache without changing stats above
        for (idx, (sx, sz)) in slices.iter().copied().enumerate() {
            let w = idx % 28;
            let j = workers[w]
                .terrain_json(sx * 64, sz * 64, 64, 1, TileDetail::Full, Some(true))
                .unwrap_or_else(|e| panic!("{e}"));
            json_bytes += j.len();
        }
        cases.push(CaseReport {
            name: if warm_coarse {
                "close-13x13-64x64-2704chunks-warm-pool".to_owned()
            } else {
                "close-13x13-64x64-2704chunks-cold-pool".to_owned()
            },
            description: if warm_coarse {
                "Close 13x13 Full square (2,704 chunks) on Coarse-warm 28-worker pool".to_owned()
            } else {
                "Close 13x13 Full square (2,704 chunks) on cold-recreated 28-worker pool".to_owned()
            },
            output_chunks: 2704,
            jobs: 169,
            workers_used: used.iter().filter(|x| **x).count(),
            elapsed_ms,
            binary_bytes_total: bin_bytes,
            json_bytes_total: json_bytes,
            terrain_generated_per_output_chunk: stats.terrain_generated as f64 / 2704.0,
            starts_generated_per_output_chunk: stats.starts_generated as f64 / 2704.0,
            stats,
        });
    }

    let report = AuditReport {
        seed: seed.to_owned(),
        binary_header_verified: header_ok,
        cases,
    };
    let serialized = serde_json::to_string_pretty(&report)
        .unwrap_or_else(|err| panic!("serialize report failed: {err}"));
    fs::write(&out_path, serialized)
        .unwrap_or_else(|err| panic!("write report failed: {err}"));
    println!("Wrote chunk cache audit report to {out_path}");
}
