//! Direct source-port readbacks and explicit portable refusal boundaries.

use super::ground_cover::{
    BlockStorage, ChunkAccess, decline, optional_weighted, simple, weighted,
};
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::feature::*;
use steel_registry::{REGISTRY, vanilla_blocks};
use steel_utils::BlockPos;
use steel_utils::random::worldgen_random::WorldgenRandom;
use steel_utils::value_providers::IntProvider;
use steel_worldgen::portable_feature;
use steel_worldgen::portable_feature::GroundCoverAccess;

// Direct readbacks exercise refusal, nested continuation, fallback and both
// native weighted selector sizes. They do not write or select biome features.
pub fn collect<W: BlockStorage>(
    world: &mut ChunkAccess<'_, W>,
    seed: u64,
    pos: BlockPos,
) -> serde_json::Value {
    use steel_utils::random::Random as _;
    let mut random = WorldgenRandom::from_seed(seed);
    let providers = [
        BlockStateProviderKind::Reference(
            &steel_registry::vanilla_block_state_providers::SOIL_BENEATH_TREE,
        ),
        decline(),
        optional_weighted(),
        BlockStateProviderKind::RuleBased {
            fallback: Some(Box::new(simple(&vanilla_blocks::DIRT))),
            rules: vec![],
        },
        weighted(1, &vanilla_blocks::SHORT_GRASS, &vanilla_blocks::FERN),
        weighted(3, &vanilla_blocks::SHORT_GRASS, &vanilla_blocks::FERN),
        weighted(
            64,
            &vanilla_blocks::OAK_LEAVES,
            &vanilla_blocks::BIRCH_LEAVES,
        ),
    ];
    let mut samples = Vec::new();
    for provider in providers {
        for _ in 0..12 {
            let optional = world.optional_state(&provider, &mut random, pos);
            let required = world.state(&provider, &mut random, pos);
            samples.push(serde_json::json!({
                "optional": optional.map(super::state_key),
                "required": super::state_key(required),
            }));
        }
    }
    let next_int = random.next_i32();
    let ground = pos.offset(0, -4, 0);
    let saved = world.0.read(ground);
    assert_ne!(
        saved.get_block(),
        &vanilla_blocks::AIR,
        "non-air retained ground required"
    );
    let mut live_random = WorldgenRandom::from_seed(seed);
    let mut control = WorldgenRandom::from_seed(seed);
    let mut live = Vec::new();
    let leaf = BlockStateData {
        block: &vanilla_blocks::OAK_LEAVES,
        properties: &[("distance", "2"), ("persistent", "true")],
    };
    let log = BlockStateData {
        block: &vanilla_blocks::OAK_LOG,
        properties: &[("axis", "x")],
    };
    for state in [
        saved,
        steel_worldgen::state_resolver::WorldgenStateResolver::feature_block_state_from_data(
            &REGISTRY,
            &leaf,
            "comparison leaf",
        ),
        steel_worldgen::state_resolver::WorldgenStateResolver::feature_block_state_from_data(
            &REGISTRY,
            &log,
            "comparison log",
        ),
    ] {
        if state != saved {
            assert!(world.0.write(ground, state));
        }
        let optional = world.optional_state(&decline(), &mut live_random, ground);
        let required = world.state(&decline(), &mut live_random, ground);
        let mut row = serde_json::json!({
            "position": [ground.x(), ground.y(), ground.z()],
            "retained": super::state_key(world.0.read(ground)),
            "optional": optional.map(super::state_key), "required": super::state_key(required),
        });
        if state != saved {
            let data = if state.get_block() == &vanilla_blocks::OAK_LEAVES {
                leaf.clone()
            } else {
                log.clone()
            };
            let simple = world.state(
                &BlockStateProviderKind::Simple {
                    state: data.clone(),
                },
                &mut live_random,
                ground,
            );
            let weighted = world.state(
                &BlockStateProviderKind::Weighted {
                    entries: vec![WeightedBlockState { data, weight: 1 }],
                },
                &mut live_random,
                ground,
            );
            row["simple"] = super::state_key(simple).into();
            row["weighted"] = super::state_key(weighted).into();
            control.next_i32_bounded(1);
        }
        live.push(row);
    }
    assert!(world.0.write(ground, saved));
    let mut rejections = Vec::new();
    for (name, weights) in [
        ("negative", vec![-1]),
        ("empty", vec![]),
        ("zero-total", vec![0]),
        ("overflow", vec![i32::MAX, 1]),
    ] {
        let provider = BlockStateProviderKind::Weighted {
            entries: weights
                .into_iter()
                .map(|weight| WeightedBlockState {
                    data: BlockStateData {
                        block: &vanilla_blocks::STONE,
                        properties: &[],
                    },
                    weight,
                })
                .collect(),
        };
        for required in [false, true] {
            rejections.push(rejection(world, seed, ground, name, &provider, required));
        }
    }
    let mut unsupported = Vec::new();
    for (name, provider) in unsupported_providers() {
        for required in [false, true] {
            unsupported.push(rejection(world, seed, ground, name, &provider, required));
        }
    }
    serde_json::json!({"stateEncoding": "sorted-properties-v1", "samples": samples, "nextInt": next_int,
        "liveReadbacks": live, "liveNextInt": live_random.next_i32(), "liveControlNextInt": control.next_i32(),
        "weightRejections": rejections, "unsupportedRejections": unsupported})
}

// Independent streams expose any draw before a named refusal. Exercise both
// required and optional entry points so an error cannot become a live-state read.
fn rejection<W: BlockStorage>(
    world: &mut ChunkAccess<'_, W>,
    seed: u64,
    pos: BlockPos,
    name: &str,
    provider: &BlockStateProviderKind,
    required: bool,
) -> serde_json::Value {
    use steel_utils::random::Random as _;
    let mut random = WorldgenRandom::from_seed(seed);
    let mut control = WorldgenRandom::from_seed(seed);
    let error = if required {
        portable_feature::providers::state(world, &REGISTRY, provider, &mut random, pos).err()
    } else {
        portable_feature::providers::optional_state(world, &REGISTRY, provider, &mut random, pos)
            .err()
    };
    serde_json::json!({"case": name, "stage": "sampling", "required": required, "error": error,
        "nextInt": random.next_i32(), "controlNextInt": control.next_i32()})
}

// Explicit configurations exercise only the unsupported-family dispatch arms.
// No noise, property or block-selection implementation is implied here.
fn unsupported_providers() -> Vec<(&'static str, BlockStateProviderKind)> {
    fn noise() -> FeatureNoiseParameters {
        FeatureNoiseParameters {
            base_amplitude: 1.0,
            base_octave: 0,
            octave_count: 1,
            normalize: true,
            amplitude_modifiers: vec![1.0],
        }
    }
    vec![
        (
            "rotated",
            BlockStateProviderKind::RotatedBlock {
                state: Box::new(simple(&vanilla_blocks::OAK_LOG)),
                direction: None,
            },
        ),
        (
            "randomized-int",
            BlockStateProviderKind::RandomizedInt {
                property: "distance".into(),
                source: Box::new(simple(&vanilla_blocks::OAK_LEAVES)),
                values: IntProvider::Constant(2),
            },
        ),
        (
            "noise",
            BlockStateProviderKind::Noise(NoiseProvider {
                noise: noise(),
                scale: 1.0,
                seed: 0,
                states: vec![],
            }),
        ),
        (
            "noise-threshold",
            BlockStateProviderKind::NoiseThreshold(NoiseThresholdProvider {
                noise: noise(),
                scale: 1.0,
                seed: 0,
                threshold: 0.0,
                high_chance: 0.5,
                default_state: BlockStateData {
                    block: &vanilla_blocks::STONE,
                    properties: &[],
                },
                low_states: vec![],
                high_states: vec![],
            }),
        ),
        (
            "dual-noise",
            BlockStateProviderKind::DualNoise(DualNoiseProvider {
                noise: noise(),
                scale: 1.0,
                seed: 0,
                slow_noise: noise(),
                slow_scale: 1.0,
                states: vec![],
                variety: [1, 1],
            }),
        ),
        (
            "copy-properties",
            BlockStateProviderKind::CopyProperties {
                source: Box::new(simple(&vanilla_blocks::OAK_LOG)),
            },
        ),
        (
            "random-block",
            BlockStateProviderKind::RandomBlock {
                blocks: BlockHolderSet::Entries(vec![&vanilla_blocks::STONE]),
            },
        ),
    ]
}
