//! Measures a cold Full square under different decoration batch sides and
//! worker assignment policies. Workers are independent generators run one
//! after another, so the report gives CPU time per worker and per stage.
//!
//! Usage: `full_square_timing OUT.json [SEED] [CENTER_X] [CENTER_Z] [TILES] [WORKERS] [CASES]`
//! `CASES` is a comma-separated list of `SIDE:POLICY` pairs, where `POLICY` is
//! `spread`, `rows`, `blocks` or `single`.

use serde::Serialize;
use std::env;
use std::fs;
use std::time::Instant;
use steel_core::worldgen::standalone::{StandaloneStats, TileDetail};
use steel_worldgen_wasm::Generator;

const TILE_BLOCKS: i32 = 64;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    batch_side: u32,
    policy: String,
    workers: usize,
    workers_used: usize,
    tiles: usize,
    output_chunks: usize,
    total_ms: f64,
    max_worker_ms: f64,
    mean_used_worker_ms: f64,
    worker_ms: Vec<f64>,
    worker_tiles: Vec<usize>,
    starts_ms: f64,
    biomes_ms: f64,
    terrain_ms: f64,
    holders_ms: f64,
    features_ms: f64,
    changes_ms: f64,
    starts_generated: u64,
    biomes_generated: u64,
    terrain_generated: u64,
    terrain_hits: u64,
    decorated_generated: u64,
    decorated_hits: u64,
    feature_regions: u64,
    binary_bytes: usize,
    payload_digest: String,
    tile_digests: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    seed: String,
    center_x: i32,
    center_z: i32,
    tiles_per_side: i32,
    tile_blocks: i32,
    cases: Vec<CaseReport>,
    /// For each batch side, whether every policy produced identical payloads.
    policy_independent: Vec<(u32, bool)>,
    /// Tiles whose payload differs between batch side one and each other side.
    tiles_changed_against_side_one: Vec<(u32, usize)>,
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Tile indices grouped by worker, in the order each worker generates them.
fn assign(policy: &str, side: usize, workers: usize) -> Vec<Vec<usize>> {
    let count = side * side;
    let mut order: Vec<usize> = Vec::with_capacity(count);
    match policy {
        "spread" => {
            let mut groups = vec![Vec::new(); workers];
            for index in 0..count {
                groups[index % workers].push(index);
            }
            return groups;
        }
        "single" => {
            for row in 0..side {
                for step in 0..side {
                    let column = if row % 2 == 0 { step } else { side - 1 - step };
                    order.push(row * side + column);
                }
            }
            let mut groups = vec![Vec::new(); workers];
            groups[0] = order;
            return groups;
        }
        "rows" => {
            for row in 0..side {
                for step in 0..side {
                    let column = if row % 2 == 0 { step } else { side - 1 - step };
                    order.push(row * side + column);
                }
            }
        }
        "blocks" => {
            // Two-row bands walked column by column, so each contiguous run
            // is a compact block two tiles deep.
            let mut band = 0;
            let mut forward = true;
            while band < side {
                let rows = if band + 1 < side { 2 } else { 1 };
                for step in 0..side {
                    let column = if forward { step } else { side - 1 - step };
                    for offset in 0..rows {
                        let row = if step % 2 == 0 {
                            band + offset
                        } else {
                            band + rows - 1 - offset
                        };
                        order.push(row * side + column);
                    }
                }
                band += rows;
                forward = !forward;
            }
        }
        other => panic!("unknown policy {other}"),
    }
    let mut groups = vec![Vec::new(); workers];
    for (position, index) in order.into_iter().enumerate() {
        groups[position * workers / count].push(index);
    }
    groups
}

fn nanos_ms(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let out_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "full-square-timing.json".to_owned());
    let seed = args.get(2).cloned().unwrap_or_else(|| "14760316".to_owned());
    let number = |index: usize, fallback: i32| -> i32 {
        args.get(index)
            .map_or(fallback, |text| text.parse().unwrap_or_else(|_| panic!("bad argument {text}")))
    };
    let center_x = number(3, 0);
    let center_z = number(4, 0);
    let side = number(5, 13);
    let workers = usize::try_from(number(6, 28)).unwrap_or(28);
    let case_list = args.get(7).cloned().unwrap_or_else(|| {
        "1:spread,4:spread,4:rows,4:blocks,4:single,8:blocks".to_owned()
    });
    let side_tiles = usize::try_from(side).unwrap_or(13);
    let count = side_tiles * side_tiles;
    // The square is centred on the tile that holds the centre block.
    let first_tile_x = center_x.div_euclid(TILE_BLOCKS) - side / 2;
    let first_tile_z = center_z.div_euclid(TILE_BLOCKS) - side / 2;

    let mut cases: Vec<CaseReport> = Vec::new();
    for case in case_list.split(',') {
        let (batch, policy) = case.split_once(':').unwrap_or_else(|| panic!("bad case {case}"));
        let batch_side: u32 = batch.parse().unwrap_or_else(|_| panic!("bad batch side {batch}"));
        let groups = assign(policy, side_tiles, workers);
        let mut worker_ms = Vec::with_capacity(workers);
        let mut worker_tiles = Vec::with_capacity(workers);
        let mut tile_digests = vec![0_u64; count];
        let mut binary_bytes = 0;
        let mut totals = StandaloneStats::default();
        for group in &groups {
            worker_tiles.push(group.len());
            if group.is_empty() {
                worker_ms.push(0.0);
                continue;
            }
            // Generator construction is warm-pool work, outside the cold request.
            let mut generator = Generator::new(&seed, "overworld", Some(256))
                .unwrap_or_else(|error| panic!("generator init failed: {error}"));
            generator
                .set_coarse_surface_source("bounded")
                .unwrap_or_else(|error| panic!("bounded source failed: {error}"));
            generator
                .set_decoration_batch_chunks(batch_side)
                .unwrap_or_else(|error| panic!("batch side failed: {error}"));
            let started = Instant::now();
            for index in group {
                let tile_x = first_tile_x + i32::try_from(index % side_tiles).unwrap_or(0);
                let tile_z = first_tile_z + i32::try_from(index / side_tiles).unwrap_or(0);
                let payload = generator
                    .terrain_binary(
                        tile_x * TILE_BLOCKS,
                        tile_z * TILE_BLOCKS,
                        TILE_BLOCKS as u32,
                        1,
                        TileDetail::Full,
                    )
                    .unwrap_or_else(|error| panic!("{error}"));
                binary_bytes += payload.len();
                tile_digests[*index] = fnv(&payload);
            }
            worker_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            let stats = generator.stats();
            totals.starts_generated += stats.starts_generated;
            totals.biomes_generated += stats.biomes_generated;
            totals.terrain_generated += stats.terrain_generated;
            totals.terrain_hits += stats.terrain_hits;
            totals.decorated_generated += stats.decorated_generated;
            totals.decorated_hits += stats.decorated_hits;
            totals.feature_regions += stats.feature_regions;
            totals.starts_nanos += stats.starts_nanos;
            totals.biomes_nanos += stats.biomes_nanos;
            totals.terrain_nanos += stats.terrain_nanos;
            totals.holders_nanos += stats.holders_nanos;
            totals.features_nanos += stats.features_nanos;
            totals.changes_nanos += stats.changes_nanos;
        }
        let total_ms: f64 = worker_ms.iter().sum();
        let used = worker_tiles.iter().filter(|tiles| **tiles > 0).count();
        let mut digest_bytes = Vec::with_capacity(count * 8);
        for digest in &tile_digests {
            digest_bytes.extend_from_slice(&digest.to_le_bytes());
        }
        let report = CaseReport {
            batch_side,
            policy: policy.to_owned(),
            workers,
            workers_used: used,
            tiles: count,
            output_chunks: count * 16,
            total_ms,
            max_worker_ms: worker_ms.iter().copied().fold(0.0, f64::max),
            mean_used_worker_ms: total_ms / used.max(1) as f64,
            worker_ms,
            worker_tiles,
            starts_ms: nanos_ms(totals.starts_nanos),
            biomes_ms: nanos_ms(totals.biomes_nanos),
            terrain_ms: nanos_ms(totals.terrain_nanos),
            holders_ms: nanos_ms(totals.holders_nanos),
            features_ms: nanos_ms(totals.features_nanos),
            changes_ms: nanos_ms(totals.changes_nanos),
            starts_generated: totals.starts_generated,
            biomes_generated: totals.biomes_generated,
            terrain_generated: totals.terrain_generated,
            terrain_hits: totals.terrain_hits,
            decorated_generated: totals.decorated_generated,
            decorated_hits: totals.decorated_hits,
            feature_regions: totals.feature_regions,
            binary_bytes,
            payload_digest: format!("{:016x}", fnv(&digest_bytes)),
            tile_digests: tile_digests.iter().map(|digest| format!("{digest:016x}")).collect(),
        };
        eprintln!(
            "side {} {:>7}: total {:>9.0} ms, slowest worker {:>8.0} ms, terrain {} ({:.0} ms), biomes {} ({:.0} ms), starts {} ({:.0} ms), regions {} ({:.0} ms), holders {:.0} ms, changes {:.0} ms, digest {}",
            report.batch_side,
            report.policy,
            report.total_ms,
            report.max_worker_ms,
            report.terrain_generated,
            report.terrain_ms,
            report.biomes_generated,
            report.biomes_ms,
            report.starts_generated,
            report.starts_ms,
            report.feature_regions,
            report.features_ms,
            report.holders_ms,
            report.changes_ms,
            report.payload_digest,
        );
        cases.push(report);
    }

    let mut sides: Vec<u32> = cases.iter().map(|case| case.batch_side).collect();
    sides.sort_unstable();
    sides.dedup();
    let policy_independent = sides
        .iter()
        .map(|side| {
            let mut digests = cases
                .iter()
                .filter(|case| case.batch_side == *side)
                .map(|case| case.payload_digest.as_str());
            let first = digests.next();
            (*side, digests.all(|digest| Some(digest) == first))
        })
        .collect();
    let baseline = cases.iter().find(|case| case.batch_side == 1);
    let tiles_changed_against_side_one = baseline.map_or_else(Vec::new, |baseline| {
        sides
            .iter()
            .filter(|side| **side != 1)
            .filter_map(|side| {
                let case = cases.iter().find(|case| case.batch_side == *side)?;
                let changed = case
                    .tile_digests
                    .iter()
                    .zip(baseline.tile_digests.iter())
                    .filter(|(left, right)| left != right)
                    .count();
                Some((*side, changed))
            })
            .collect()
    });
    let report = Report {
        seed,
        center_x,
        center_z,
        tiles_per_side: side,
        tile_blocks: TILE_BLOCKS,
        cases,
        policy_independent,
        tiles_changed_against_side_one,
    };
    let json = serde_json::to_string_pretty(&report).unwrap_or_else(|error| panic!("{error}"));
    fs::write(&out_path, json + "\n").unwrap_or_else(|error| panic!("{error}"));
    eprintln!("wrote {out_path}");
}
