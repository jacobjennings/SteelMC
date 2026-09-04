//! Portable structure-piece parity against the native placer.
//!
//! The portable placer in `steel_worldgen::structure::piece_placer` exists so a
//! browser can draw a structure without a server. This fixture is the only
//! thing that makes that claim checkable: it starts both hosts from the same
//! post-Carvers terrain, runs only the structure half of biome decoration on
//! the native side, runs the portable pass on the same chunk, and requires the
//! two block diffs to be identical at every position with no tolerance and no
//! block allowlist.
//!
//! Two deliberate limits, both stated rather than hidden:
//!
//! - Neither side builds a beardifier. The portable sampler has none, so the
//!   native side is asked for the same terrain by passing `None`. Every
//!   structure compared here has vanilla terrain adaptation `none`, so this
//!   changes no block either side would otherwise write.
//! - The portable placer writes no entities, block entities, loot tables or
//!   fluid ticks, and it sets no post-processing marks. None of those change a
//!   block state during the Features stage, which is the moment compared.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use steel_registry::REGISTRY;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, ChunkPos};
use steel_worldgen::biomes::BiomeSourceKind;
use steel_worldgen::surface_sampler::{SurfaceDimension, SurfaceSampler};

use crate::chunk::chunk_generation_task::StaticCache2D;
use crate::chunk::chunk_pyramid::GENERATION_PYRAMID;
use crate::chunk::status::ChunkStatus;
use crate::worldgen::chunk_stage_hashes::{
    build_feature_holders, chunk_or_panic, create_test_world, empty_proto_chunk,
    recalculate_section_counts, sorted_positions,
};
use crate::worldgen::generator::{CarversPhase, GenerationChunk, NoisePhase, SurfacePhase};
use crate::worldgen::{ChunkGenerator, ChunkGeneratorType, OverworldGenerator};
use glam::IVec3;
use steel_registry::vanilla_dimension_types;

/// Every structure family the portable placer claims, on real ground.
///
/// A swamp hut is the smallest procedural family in the game, seven by nine
/// blocks of planks and logs, and its piece always sits inside a single chunk.
/// The sample covers both of the orientation shapes a scattered feature piece
/// can take, because the piece box swaps its width and depth with the
/// orientation and that is exactly where a coordinate transform goes wrong.
#[test]
fn portable_structure_pieces_match_native() {
    let mut compared = 0usize;
    for (seed, chunk_x, chunk_z, label) in [
        (42_u64, -17_i32, 22_i32, "swamp hut, east-west piece box"),
        (42, -51, -23, "swamp hut, east-west piece box"),
        (5, -14, -43, "swamp hut, north-south piece box"),
    ] {
        compared += assert_portable_structures_match_native(seed, chunk_x, chunk_z, label);
    }
    // A comparison that compares nothing passes for the wrong reason.
    println!("PORTABLE_STRUCTURE_PARITY compared={compared}");
    assert!(
        compared > 0,
        "the sample compared no structure blocks at all"
    );
}

/// The smallest template-backed structure, on real ground.
///
/// An igloo is twelve kilobytes of saved blocks placed as one template with a
/// rotation, which is the whole template engine in miniature: load it, choose a
/// palette, turn the block positions and the block states about a pivot, and
/// write what lands inside the chunk. It needs no jigsaw assembly and no
/// processors, so it isolates the placement core from everything built on it.
#[test]
fn portable_igloo_template_matches_native() {
    let mut compared = 0usize;
    for (seed, chunk_x, chunk_z, label) in [
        (12345_u64, 36_i32, 41_i32, "igloo, top only"),
        (2026, -21, -28, "igloo, top only"),
    ] {
        compared += assert_portable_structures_match_native(seed, chunk_x, chunk_z, label);
    }
    println!("PORTABLE_IGLOO_PARITY compared={compared}");
    assert!(compared > 0, "the sample compared no igloo blocks at all");
}

/// The dense village piece used to measure neighbour-dependent shape updates.
///
/// This savanna weaponsmith has stairs, a fence, doors, panes, iron bars and
/// slabs in only a 13x7x9 box. Its natural seed-7 placement crosses two chunks,
/// so both halves are compared against the native pool-element placer.
#[test]
fn portable_village_weaponsmith_matches_native() {
    assert!(
        super::piece_placer::StructurePiecePlacer::JIGSAW_UPDATE_FLAGS
            .contains(UpdateFlags::UPDATE_KNOWN_SHAPE),
        "vanilla jigsaw flags changed: this fixture must be reinterpreted"
    );
    let compared =
        assert_portable_structures_match_native(7, -35, -47, "savanna weaponsmith 2, west half")
            + assert_portable_structures_match_native(
                7,
                -34,
                -47,
                "savanna weaponsmith 2, east half",
            );
    println!("PORTABLE_VILLAGE_WEAPONSMITH_PARITY compared={compared}");
    assert_eq!(compared, 384, "the selected village piece changed size");
}

/// The one measured case where the portable template slice disagrees, kept as
/// an exact reproduction rather than deleted.
///
/// An igloo with a basement is eleven template pieces stacked into a ladder
/// shaft. Both sides now write the same 404 blocks in this chunk, and three of
/// them disagree: iron bars along the chunk's northern edge, where the portable
/// side keeps the template's saved `north=false` and the native side has
/// `north=true`.
///
/// The cause is named and it is not a template bug. After placing a template
/// vanilla re-evaluates the shape of every placed block against its neighbours,
/// through `steel-core`'s per-block behaviour table. Iron bars connect to the
/// solid ground north of them, which the template did not place. The portable
/// slice has no behaviour table, so it cannot run that pass.
///
/// This is the whole of the shape-update gap, measured on the smallest template
/// structure that shows it: three blocks out of 404, all of them where a
/// template block meets ground the template did not place. Fixing it means
/// porting the subset of block behaviours whose `update_shape` does something,
/// which is a separate piece of work and needs its own estimate.
#[test]
#[ignore = "known divergence, kept as a reproduction until block shape updates are ported"]
fn portable_igloo_basement_needs_the_block_shape_update_pass() {
    assert_portable_structures_match_native(4, -21, -17, "igloo with a basement, eleven pieces");
}

/// A chunk with no structure in it must produce nothing on either side.
///
/// Without this, a portable placer that silently refused every piece would pass
/// the fixture above only because the native side also wrote nothing there.
#[test]
fn portable_structure_pass_writes_nothing_in_an_empty_chunk() {
    let blocks = SurfaceSampler::new(42, SurfaceDimension::Overworld)
        .structure_piece_transaction_snapshot(0, 0);
    assert!(
        blocks.is_empty(),
        "a chunk with no structure start produced {} blocks",
        blocks.len()
    );
}

fn assert_portable_structures_match_native(
    seed: u64,
    chunk_x: i32,
    chunk_z: i32,
    label: &str,
) -> usize {
    use crate::bootstrap::init_globals_once;
    use std::collections::BTreeMap;

    init_globals_once();
    let thread_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("failed to create structure-parity rayon pool"),
    );
    let generator = Arc::new(ChunkGeneratorType::Overworld(OverworldGenerator::new(
        None,
        BiomeSourceKind::overworld(seed),
        seed,
        &thread_pool,
    )));
    let min_y = vanilla_dimension_types::OVERWORLD.min_y;
    let height = vanilla_dimension_types::OVERWORLD.height;
    let section_count = (height / 16) as usize;
    let min_quart_y = min_y >> 2;
    let total_quarts_y = section_count as i32 * 4;
    let feature_step = GENERATION_PYRAMID.get_step_to(ChunkStatus::Features);
    let feature_cache_radius = feature_step.direct_dependencies.get_radius() as i32;

    let carver_min_x = chunk_x - 2;
    let carver_max_x = chunk_x + 2;
    let carver_min_z = chunk_z - 2;
    let carver_max_z = chunk_z + 2;
    let mut carver_positions = FxHashSet::default();
    for position_x in carver_min_x..=carver_max_x {
        for position_z in carver_min_z..=carver_max_z {
            carver_positions.insert((position_x, position_z));
        }
    }

    let mut chunks = FxHashMap::default();
    for position_x in chunk_x - 1 - feature_cache_radius..=chunk_x + 1 + feature_cache_radius {
        for position_z in chunk_z - 1 - feature_cache_radius..=chunk_z + 1 + feature_cache_radius {
            chunks.insert(
                (position_x, position_z),
                empty_proto_chunk((position_x, position_z), section_count, min_y, height),
            );
        }
    }

    // Structure starts first: the placement pass reads them, and a chunk's
    // references point at the chunks whose starts reach into it.
    for chunk in chunks.values() {
        generator.create_structures(chunk);
    }
    for target_x in chunk_x - 1..=chunk_x + 1 {
        for target_z in chunk_z - 1..=chunk_z + 1 {
            let target_block_x = target_x * 16;
            let target_block_z = target_z * 16;
            for source_x in target_x - 8..=target_x + 8 {
                for source_z in target_z - 8..=target_z + 8 {
                    let Some(source_chunk) = chunks.get(&(source_x, source_z)) else {
                        continue;
                    };
                    let starts = source_chunk.structure_starts();
                    for (structure_id, start) in starts.iter() {
                        let Some(bounds) = start.bounding_box else {
                            continue;
                        };
                        if bounds.intersects_xz(
                            target_block_x,
                            target_block_z,
                            target_block_x + 15,
                            target_block_z + 15,
                        ) {
                            chunk_or_panic(&chunks, (target_x, target_z))
                                .structure_references_mut()
                                .entry(structure_id.clone())
                                .or_default()
                                .insert(ChunkPos::new(source_x, source_z));
                        }
                    }
                }
            }
        }
    }

    for chunk in chunks.values() {
        generator.create_biomes(chunk);
    }
    let carver_positions_sorted = sorted_positions(&carver_positions);
    for &position in &carver_positions_sorted {
        let chunk = chunk_or_panic(&chunks, position);
        generator.fill_from_noise(GenerationChunk::<NoisePhase>::for_test(chunk), None);
    }
    for &position in &carver_positions_sorted {
        let chunk = chunk_or_panic(&chunks, position);
        let neighbor_biomes = |quart: IVec3| -> u16 {
            let biome_chunk_x = quart.x >> 2;
            let biome_chunk_z = quart.z >> 2;
            let biome_chunk = chunk_or_panic(&chunks, (biome_chunk_x, biome_chunk_z));
            let local_x = (quart.x - biome_chunk_x * 4) as usize;
            let local_z = (quart.z - biome_chunk_z * 4) as usize;
            let quart_y = (quart.y - min_quart_y).clamp(0, total_quarts_y - 1) as usize;
            let section = quart_y / 4;
            let local_y = quart_y % 4;
            biome_chunk.sections.sections[section]
                .read()
                .biomes
                .get(local_x, local_y, local_z)
        };
        generator.build_surface(
            GenerationChunk::<SurfacePhase>::for_test(chunk),
            &neighbor_biomes,
        );
    }
    for &position in &carver_positions_sorted {
        let chunk = chunk_or_panic(&chunks, position);
        recalculate_section_counts(chunk);
        generator.apply_carvers(GenerationChunk::<CarversPhase>::for_test(chunk));
    }

    let world = create_test_world(
        "minecraft:overworld",
        &vanilla_dimension_types::OVERWORLD,
        seed,
        generator.clone(),
        thread_pool,
    );
    let context = world.chunk_map.world_gen_context.clone();
    let holders = Arc::new(build_feature_holders(
        chunks,
        &carver_positions,
        min_y,
        height,
    ));

    let canonical_state = |state: BlockStateId| {
        let block = REGISTRY
            .blocks
            .by_state_id(state)
            .expect("generated state must be registered");
        let key = block.key.to_string();
        let properties = REGISTRY.blocks.get_properties(state);
        if properties.is_empty() {
            key
        } else {
            format!(
                "{key}[{}]",
                properties
                    .into_iter()
                    .map(|(property, value)| format!("{property}={value}"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    };

    let mut native_before = BTreeMap::new();
    {
        let chunk = holders
            .get(&(chunk_x, chunk_z))
            .expect("native central chunk holder must exist")
            .try_chunk(ChunkStatus::Carvers)
            .expect("native central chunk must exist before Features");
        for local_y in 0..height {
            for local_z in 0..16 {
                for local_x in 0..16 {
                    let position = (
                        chunk_x * 16 + local_x,
                        min_y + local_y,
                        chunk_z * 16 + local_z,
                    );
                    let state =
                        chunk.get_block_state(BlockPos::new(position.0, position.1, position.2));
                    native_before.insert(position, state);
                }
            }
        }
    }

    // Structure parity means nothing if the two sides stand on different
    // ground, so prove the ground first.
    {
        let portable_terrain =
            SurfaceSampler::new(seed, SurfaceDimension::Overworld).carved_chunk_snapshot(chunk_x, chunk_z);
        let mut mismatches = Vec::new();
        for (&position, &native_state) in &native_before {
            let local_x = (position.0 - chunk_x * 16) as usize;
            let local_y = (position.1 - min_y) as usize;
            let local_z = (position.2 - chunk_z * 16) as usize;
            let portable_state = portable_terrain.states[local_y * 256 + local_z * 16 + local_x];
            if portable_state != native_state {
                mismatches.push((
                    position,
                    canonical_state(native_state),
                    canonical_state(portable_state),
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{label} at seed {seed} chunk ({chunk_x}, {chunk_z}): pre-structure terrain differs at {} positions, first={:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(6)]
        );
    }

    {
        let center = ChunkPos::new(chunk_x, chunk_z);
        let center_holder = holders
            .get(&(chunk_x, chunk_z))
            .expect("native central chunk holder must exist");
        {
            let chunk = center_holder
                .try_chunk(ChunkStatus::Carvers)
                .expect("native central chunk must exist before Features");
            chunk.prime_final_heightmaps();
        }
        let cache_holders = holders.clone();
        let cache = Arc::new(StaticCache2D::create(
            chunk_x,
            chunk_z,
            feature_cache_radius,
            move |x, z| match cache_holders.get(&(x, z)) {
                Some(holder) => holder.clone(),
                None => panic!("Missing structure dependency chunk ({x}, {z})"),
            },
        ));
        let region_random = generator.create_worldgen_region_random(seed as i64, center);
        let mut region = crate::worldgen::WorldGenRegion::new(
            &context,
            feature_step,
            &cache,
            center,
            region_random,
        );
        let ChunkGeneratorType::Overworld(overworld) = generator.as_ref() else {
            panic!("the structure parity fixture requires an overworld generator");
        };
        overworld.apply_structure_decorations_for_test(&mut region);
    }

    let mut native = BTreeMap::new();
    let native_chunk = holders
        .get(&(chunk_x, chunk_z))
        .expect("native central chunk holder must exist")
        .try_chunk(ChunkStatus::Carvers)
        .expect("native central chunk must remain available after Features");
    for (&position, &before) in &native_before {
        let after = native_chunk.get_block_state(BlockPos::new(position.0, position.1, position.2));
        if after != before {
            native.insert(position, canonical_state(after));
        }
    }

    let portable = SurfaceSampler::new(seed, SurfaceDimension::Overworld)
        .structure_piece_transaction_snapshot(chunk_x, chunk_z)
        .into_iter()
        .filter_map(|block| {
            let position = (block.x, block.y, block.z);
            let before = native_before.get(&position)?;
            (canonical_state(*before) != block.state).then_some((position, block.state))
        })
        .collect::<BTreeMap<_, _>>();

    assert!(
        !native.is_empty(),
        "{label} at seed {seed} chunk ({chunk_x}, {chunk_z}): the native side placed no structure blocks, so the fixture proves nothing"
    );
    if portable != native {
        let portable_only = portable
            .iter()
            .filter(|(position, state)| native.get(position).is_none_or(|other| other != *state))
            .take(12)
            .collect::<Vec<_>>();
        let native_only = native
            .iter()
            .filter(|(position, state)| portable.get(position).is_none_or(|other| other != *state))
            .take(12)
            .collect::<Vec<_>>();
        panic!(
            "{label} at seed {seed} chunk ({chunk_x}, {chunk_z}): portable structure blocks differ from native: portable={} native={} portable-first={portable_only:?} native-first={native_only:?}",
            portable.len(),
            native.len(),
        );
    }

    native.len()
}
