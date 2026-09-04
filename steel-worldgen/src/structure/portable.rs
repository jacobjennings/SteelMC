//! Portable swamp-hut and igloo piece placement for static terrain hosts.
//!
//! Structure starts already live in `steel-worldgen`. Native piece placers live
//! in `steel-core` and cannot follow the WASM tile path. This slice places the
//! two families whose block recipes are already parity-checked, writing through
//! [`VegetationBlockAccess`] so the tile serializer can carry them in the same
//! generated-block stream as vegetation.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::str::FromStr;

use flate2::read::GzDecoder;
use glam::IVec3;
use simdnbt::borrow::{
    Nbt as BorrowedNbt, NbtCompound as BorrowedNbtCompound,
    NbtCompoundList as BorrowedNbtCompoundList, NbtList as BorrowedNbtList, read as read_nbt,
};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, StairsShape};
use steel_registry::feature::FeatureHeightmap;
use steel_registry::shared_structs::BlockStateData;
use steel_registry::vanilla_template_pools::vanilla_template_nbt_bytes;
use steel_registry::{Registry, vanilla_blocks};
use steel_utils::{BlockPos, BlockStateId, BoundingBox, Direction, Identifier, Rotation};

use crate::state_resolver::WorldgenStateResolver;
use crate::structure::placement::{StructureSet, load_vanilla_structure_sets};
use crate::structure::swamp_hut::SwampHutPieceData;
use crate::structure::{
    ProceduralPieceData, StructureBlockIgnore, StructureMirror, StructurePiece,
    StructurePiecePayload, StructureStart, TemplateMarkerHandling, TemplatePieceData,
    TemplatePlacementAdjustment, TemplatePostProcess,
};
use crate::vegetation::VegetationBlockAccess;

/// Structure ids whose piece blocks this slice can emit.
pub const PORTABLE_STRUCTURE_IDS: [&str; 2] = ["swamp_hut", "igloo"];

const IGLOO_GENERATION_HEIGHT: i32 = 90;

/// Returns whether `structure` is one of the portable piece families.
#[must_use]
pub fn is_portable_structure(structure: &Identifier) -> bool {
    PORTABLE_STRUCTURE_IDS
        .iter()
        .any(|path| structure == &Identifier::vanilla_static(path))
}

/// Vanilla structure sets that contain only portable families.
#[must_use]
pub fn portable_structure_sets() -> Vec<(Identifier, StructureSet)> {
    load_vanilla_structure_sets()
        .into_iter()
        .filter(|(_, set)| {
            set.structures
                .iter()
                .all(|entry| is_portable_structure(&entry.structure))
        })
        .collect()
}

/// Places one portable piece into `host`, clipped to `clip`.
///
/// Non-portable payloads are ignored so desert pyramids and jigsaw families
/// cannot leak into the tile path merely because a start was generated.
///
/// # Panics
///
/// Panics if an igloo piece references a vanilla template that is not bundled
/// or cannot be parsed.
pub fn place_portable_piece<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    piece: &mut StructurePiece,
    clip: BoundingBox,
    writes: &mut Vec<BlockPos>,
) {
    match &mut piece.payload {
        StructurePiecePayload::Procedural(ProceduralPieceData::SwampHut(data)) => {
            place_swamp_hut_piece(
                host,
                registry,
                &mut piece.bounding_box,
                piece.orientation,
                data,
                clip,
                writes,
            );
        }
        StructurePiecePayload::Template(data)
            if data.marker_handling == TemplateMarkerHandling::Igloo
                || data.post_process == TemplatePostProcess::IglooTop
                || matches!(
                    data.placement_adjustment,
                    TemplatePlacementAdjustment::Igloo { .. }
                ) =>
        {
            place_igloo_template_piece(host, registry, data, clip, writes);
        }
        _ => {}
    }
}

const fn writable_chunk_box(
    chunk_x: i32,
    chunk_z: i32,
    min_y: i32,
    max_y_exclusive: i32,
) -> BoundingBox {
    let min_x = chunk_x * 16;
    let min_z = chunk_z * 16;
    BoundingBox::new(
        IVec3::new(min_x, min_y + 1, min_z),
        IVec3::new(min_x + 15, max_y_exclusive - 1, min_z + 15),
    )
}

/// Places every portable piece that intersects `chunk_x`/`chunk_z`.
pub fn place_portable_starts_in_chunk<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    starts: &mut [StructureStart],
    chunk_x: i32,
    chunk_z: i32,
) -> Vec<BlockPos> {
    let clip = writable_chunk_box(chunk_x, chunk_z, host.min_y(), host.max_y_exclusive());
    let mut writes = Vec::new();
    for start in starts {
        if !is_portable_structure(&start.structure) {
            continue;
        }
        for piece in &mut start.pieces {
            if piece.bounding_box.intersects(clip) {
                place_portable_piece(host, registry, piece, clip, &mut writes);
            }
        }
    }
    writes
}

fn record_write(writes: &mut Vec<BlockPos>, pos: BlockPos) {
    if !writes.contains(&pos) {
        writes.push(pos);
    }
}

fn set_host_block<H: VegetationBlockAccess>(
    host: &mut H,
    writes: &mut Vec<BlockPos>,
    pos: BlockPos,
    state: BlockStateId,
) {
    host.set_block_state(pos, state);
    record_write(writes, pos);
}

const fn orientation_transform(orientation: Option<Direction>) -> (StructureMirror, Rotation) {
    match orientation {
        None | Some(Direction::North | Direction::Up | Direction::Down) => {
            (StructureMirror::None, Rotation::None)
        }
        Some(Direction::South) => (StructureMirror::LeftRight, Rotation::None),
        Some(Direction::West) => (StructureMirror::LeftRight, Rotation::Clockwise90),
        Some(Direction::East) => (StructureMirror::None, Rotation::Clockwise90),
    }
}

const fn world_pos(
    bounding_box: BoundingBox,
    orientation: Option<Direction>,
    x: i32,
    y: i32,
    z: i32,
) -> BlockPos {
    let world_y = if orientation.is_some() {
        y + bounding_box.min_y()
    } else {
        y
    };
    let (world_x, world_z) = match orientation {
        None | Some(Direction::Up | Direction::Down) => (x, z),
        Some(Direction::North) => (bounding_box.min_x() + x, bounding_box.max_z() - z),
        Some(Direction::South) => (bounding_box.min_x() + x, bounding_box.min_z() + z),
        Some(Direction::West) => (bounding_box.max_x() - z, bounding_box.min_z() + x),
        Some(Direction::East) => (bounding_box.min_x() + z, bounding_box.min_z() + x),
    };
    BlockPos::new(world_x, world_y, world_z)
}

fn is_replaceable_by_structures(state: BlockStateId) -> bool {
    state.is_air()
        || state.has_fluid()
        || state.get_block() == &vanilla_blocks::GLOW_LICHEN
        || state.get_block() == &vanilla_blocks::SEAGRASS
        || state.get_block() == &vanilla_blocks::TALL_SEAGRASS
}

fn place_swamp_hut_piece<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    bounding_box: &mut BoundingBox,
    orientation: Option<Direction>,
    data: &mut SwampHutPieceData,
    clip: BoundingBox,
    writes: &mut Vec<BlockPos>,
) {
    if !update_average_ground_height(host, bounding_box, &mut data.height_position, clip, 0) {
        return;
    }
    let spruce_planks = vanilla_blocks::SPRUCE_PLANKS.default_state();
    let oak_log = vanilla_blocks::OAK_LOG.default_state();
    let oak_fence = vanilla_blocks::OAK_FENCE.default_state();
    let air = vanilla_blocks::AIR.default_state();
    let mut placer = ScatteredPlacer {
        host,
        registry,
        bounding_box: *bounding_box,
        orientation,
        clip,
        writes,
    };
    placer.generate_box(1, 1, 1, 5, 1, 7, spruce_planks);
    placer.generate_box(1, 4, 2, 5, 4, 7, spruce_planks);
    placer.generate_box(2, 1, 0, 4, 1, 0, spruce_planks);
    placer.generate_box(2, 2, 2, 3, 3, 2, spruce_planks);
    placer.generate_box(1, 2, 3, 1, 3, 6, spruce_planks);
    placer.generate_box(5, 2, 3, 5, 3, 6, spruce_planks);
    placer.generate_box(2, 2, 7, 4, 3, 7, spruce_planks);
    placer.generate_box(1, 0, 2, 1, 3, 2, oak_log);
    placer.generate_box(5, 0, 2, 5, 3, 2, oak_log);
    placer.generate_box(1, 0, 7, 1, 3, 7, oak_log);
    placer.generate_box(5, 0, 7, 5, 3, 7, oak_log);
    placer.place_block(oak_fence, 2, 3, 2);
    placer.place_block(oak_fence, 3, 3, 7);
    placer.place_block(air, 1, 3, 4);
    placer.place_block(air, 5, 3, 4);
    placer.place_block(air, 5, 3, 5);
    placer.place_block(vanilla_blocks::POTTED_RED_MUSHROOM.default_state(), 1, 3, 5);
    placer.place_block(vanilla_blocks::CRAFTING_TABLE.default_state(), 3, 2, 6);
    placer.place_block(vanilla_blocks::CAULDRON.default_state(), 4, 2, 6);
    placer.place_block(oak_fence, 1, 2, 1);
    placer.place_block(oak_fence, 5, 2, 1);

    let north_stairs = stairs(Direction::North);
    let east_stairs = stairs(Direction::East);
    let west_stairs = stairs(Direction::West);
    let south_stairs = stairs(Direction::South);
    placer.generate_box(0, 4, 1, 6, 4, 1, north_stairs);
    placer.generate_box(0, 4, 2, 0, 4, 7, east_stairs);
    placer.generate_box(6, 4, 2, 6, 4, 7, west_stairs);
    placer.generate_box(0, 4, 8, 6, 4, 8, south_stairs);
    placer.place_block(stairs_shape(north_stairs, StairsShape::OuterRight), 0, 4, 1);
    placer.place_block(stairs_shape(north_stairs, StairsShape::OuterLeft), 6, 4, 1);
    placer.place_block(stairs_shape(south_stairs, StairsShape::OuterLeft), 0, 4, 8);
    placer.place_block(stairs_shape(south_stairs, StairsShape::OuterRight), 6, 4, 8);

    for z in [2, 7] {
        for x in [1, 5] {
            placer.fill_column_down(oak_log, x, -1, z);
        }
    }
}

fn stairs(facing: Direction) -> BlockStateId {
    vanilla_blocks::SPRUCE_STAIRS
        .default_state()
        .set_value(&BlockStateProperties::FACING, facing)
}

fn stairs_shape(state: BlockStateId, shape: StairsShape) -> BlockStateId {
    state.set_value(&BlockStateProperties::STAIRS_SHAPE, shape)
}

fn update_average_ground_height<H: VegetationBlockAccess>(
    host: &H,
    bounding_box: &mut BoundingBox,
    height_position: &mut Option<i32>,
    clip: BoundingBox,
    offset: i32,
) -> bool {
    if height_position.is_some() {
        return true;
    }

    let mut total = 0;
    let mut count = 0;
    for z in bounding_box.min_z()..=bounding_box.max_z() {
        for x in bounding_box.min_x()..=bounding_box.max_x() {
            if clip.contains_blockpos(BlockPos::new(x, 64, z)) {
                total += host.height_at(FeatureHeightmap::MotionBlockingNoLeaves, x, z);
                count += 1;
            }
        }
    }

    if count == 0 {
        return false;
    }

    let adjusted = total / count;
    *height_position = Some(adjusted);
    let dy = adjusted - bounding_box.min_y() + offset;
    *bounding_box = bounding_box.translate(IVec3::new(0, dy, 0));
    true
}

struct ScatteredPlacer<'a, H: VegetationBlockAccess> {
    host: &'a mut H,
    registry: &'a Registry,
    bounding_box: BoundingBox,
    orientation: Option<Direction>,
    clip: BoundingBox,
    writes: &'a mut Vec<BlockPos>,
}

impl<H: VegetationBlockAccess> ScatteredPlacer<'_, H> {
    fn place_block(&mut self, state: BlockStateId, x: i32, y: i32, z: i32) {
        let pos = world_pos(self.bounding_box, self.orientation, x, y, z);
        if !self.clip.contains_blockpos(pos) {
            return;
        }
        let (mirror, rotation) = orientation_transform(self.orientation);
        let state = transform_state(self.registry, state, mirror, rotation);
        set_host_block(self.host, self.writes, pos, state);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors vanilla StructurePiece.generateBox parameters"
    )]
    fn generate_box(
        &mut self,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        state: BlockStateId,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    self.place_block(state, x, y, z);
                }
            }
        }
    }

    fn fill_column_down(&mut self, state: BlockStateId, x: i32, start_y: i32, z: i32) {
        let mut pos = world_pos(self.bounding_box, self.orientation, x, start_y, z);
        if !self.clip.contains_blockpos(pos) {
            return;
        }
        let (mirror, rotation) = orientation_transform(self.orientation);
        let state = transform_state(self.registry, state, mirror, rotation);
        while is_replaceable_by_structures(self.host.block_state(pos))
            && pos.y() > self.host.min_y() + 1
        {
            set_host_block(self.host, self.writes, pos, state);
            pos = pos.below();
        }
    }
}

fn place_igloo_template_piece<H: VegetationBlockAccess>(
    host: &mut H,
    registry: &Registry,
    data: &TemplatePieceData,
    clip: BoundingBox,
    writes: &mut Vec<BlockPos>,
) {
    let template = match PortableTemplate::load_vanilla(registry, &data.template_id) {
        Ok(template) => template,
        Err(err) => panic!("{err}"),
    };
    let position = adjusted_igloo_position(
        host,
        data.template_position,
        data.mirror,
        data.rotation,
        BlockPos(data.rotation_pivot),
        match data.placement_adjustment {
            TemplatePlacementAdjustment::Igloo { template_offset } => {
                IVec3::new(template_offset.0, template_offset.1, template_offset.2)
            }
            _ => IVec3::ZERO,
        },
    );
    let template_box = template.bounding_box_with_transform(
        position,
        data.rotation,
        data.mirror,
        BlockPos(data.rotation_pivot),
    );
    if !template_box.intersects(clip) {
        return;
    }
    template.place_in_clip(
        host,
        registry,
        position,
        data.mirror,
        data.rotation,
        BlockPos(data.rotation_pivot),
        data.block_ignore,
        clip,
        writes,
    );
    if data.post_process == TemplatePostProcess::IglooTop {
        post_process_igloo_top(
            host,
            position,
            data.mirror,
            data.rotation,
            BlockPos(data.rotation_pivot),
            clip,
            writes,
        );
    }
}

fn adjusted_igloo_position<H: VegetationBlockAccess>(
    host: &H,
    position: IVec3,
    mirror: StructureMirror,
    rotation: Rotation,
    pivot: BlockPos,
    template_offset: IVec3,
) -> BlockPos {
    let raw_position = BlockPos(position);
    let entrance_relative = calculate_relative_position(
        BlockPos(IVec3::new(3 - template_offset.x, 0, -template_offset.z)),
        mirror,
        rotation,
        pivot,
    );
    let entrance_pos = raw_position.offset(
        entrance_relative.x(),
        entrance_relative.y(),
        entrance_relative.z(),
    );
    let height = host.height_at(
        FeatureHeightmap::WorldSurfaceWg,
        entrance_pos.x(),
        entrance_pos.z(),
    );
    raw_position.offset(0, height - IGLOO_GENERATION_HEIGHT - 1, 0)
}

fn post_process_igloo_top<H: VegetationBlockAccess>(
    host: &mut H,
    position: BlockPos,
    mirror: StructureMirror,
    rotation: Rotation,
    pivot: BlockPos,
    clip: BoundingBox,
    writes: &mut Vec<BlockPos>,
) {
    let trapdoor_relative =
        calculate_relative_position(BlockPos(IVec3::new(3, 0, 5)), mirror, rotation, pivot);
    let trapdoor_pos = position.offset(
        trapdoor_relative.x(),
        trapdoor_relative.y(),
        trapdoor_relative.z(),
    );
    if !clip.contains_blockpos(trapdoor_pos) {
        return;
    }
    let below_state = host.block_state(trapdoor_pos.below());
    if below_state.is_air() || below_state.get_block() == &vanilla_blocks::LADDER {
        return;
    }
    set_host_block(
        host,
        writes,
        trapdoor_pos,
        vanilla_blocks::SNOW_BLOCK.default_state(),
    );
}

const fn calculate_relative_position(
    pos: BlockPos,
    mirror: StructureMirror,
    rotation: Rotation,
    pivot: BlockPos,
) -> BlockPos {
    let (x, z) = match mirror {
        StructureMirror::None => (pos.x(), pos.z()),
        StructureMirror::FrontBack => (-pos.x(), pos.z()),
        StructureMirror::LeftRight => (pos.x(), -pos.z()),
    };
    BlockPos(rotation.transform_pos(IVec3::new(x, pos.y(), z), pivot.0))
}

struct PortableTemplate {
    size: IVec3,
    blocks: Vec<(BlockPos, BlockStateId)>,
}

impl PortableTemplate {
    fn load_vanilla(registry: &Registry, key: &Identifier) -> Result<Self, String> {
        let Some(bytes) = vanilla_template_nbt_bytes(key) else {
            return Err(format!("vanilla structure template {key} is not bundled"));
        };
        Self::load_gzip_nbt(registry, bytes, &key.to_string())
    }

    fn load_gzip_nbt(registry: &Registry, bytes: &[u8], context: &str) -> Result<Self, String> {
        let mut decoder = GzDecoder::new(bytes);
        let mut data = Vec::new();
        decoder
            .read_to_end(&mut data)
            .map_err(|err| format!("failed to decompress structure template {context}: {err}"))?;
        let nbt = read_nbt(&mut Cursor::new(&data))
            .map_err(|err| format!("failed to parse structure template {context}: {err}"))?;
        let root = match nbt {
            BorrowedNbt::Some(root) => root,
            BorrowedNbt::None => {
                return Err(format!("structure template {context} is empty"));
            }
        };
        let compound = root.as_compound();
        let size = read_vec3(compound.list("size"), context, "size")?;
        let palettes = read_palettes(registry, &compound, context)?;
        let Some(palette) = palettes.first() else {
            return Err(format!("structure template {context} has empty palettes"));
        };
        let blocks = compound
            .list("blocks")
            .and_then(|list| list.compounds())
            .ok_or_else(|| format!("structure template {context} has non-compound blocks list"))?;
        Ok(Self {
            size,
            blocks: read_blocks(&blocks, palette, context)?,
        })
    }

    fn bounding_box_with_transform(
        &self,
        position: BlockPos,
        rotation: Rotation,
        mirror: StructureMirror,
        pivot: BlockPos,
    ) -> BoundingBox {
        let corner1 = calculate_relative_position(BlockPos::ZERO, mirror, rotation, pivot);
        let corner2 =
            calculate_relative_position(BlockPos(self.size - IVec3::ONE), mirror, rotation, pivot);
        BoundingBox::new(position.0 + corner1.0, position.0 + corner2.0)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "template placement needs the same transform, ignore, and clip inputs vanilla uses"
    )]
    fn place_in_clip<H: VegetationBlockAccess>(
        &self,
        host: &mut H,
        registry: &Registry,
        position: BlockPos,
        mirror: StructureMirror,
        rotation: Rotation,
        pivot: BlockPos,
        block_ignore: StructureBlockIgnore,
        clip: BoundingBox,
        writes: &mut Vec<BlockPos>,
    ) {
        for &(template_pos, state) in &self.blocks {
            if block_ignore.ignores(registry, state) {
                continue;
            }
            let world_offset = calculate_relative_position(template_pos, mirror, rotation, pivot);
            let world_pos = position.offset(world_offset.x(), world_offset.y(), world_offset.z());
            if !clip.contains_blockpos(world_pos) {
                continue;
            }
            let final_state = transform_state(registry, state, mirror, rotation);
            set_host_block(host, writes, world_pos, final_state);
        }
    }
}

fn read_vec3(
    list: Option<BorrowedNbtList<'_, '_>>,
    context: &str,
    field: &str,
) -> Result<IVec3, String> {
    let ints = list
        .and_then(|list| list.ints())
        .ok_or_else(|| format!("structure template {context} has non-int {field} list"))?;
    if ints.len() < 3 {
        return Err(format!(
            "structure template {context} {field} list has fewer than 3 entries"
        ));
    }
    Ok(IVec3::new(ints[0], ints[1], ints[2]))
}

fn read_palettes(
    registry: &Registry,
    compound: &BorrowedNbtCompound<'_, '_>,
    context: &str,
) -> Result<Vec<Vec<BlockStateId>>, String> {
    if let Some(palette) = compound.list("palette").and_then(|list| list.compounds()) {
        return Ok(vec![read_palette(registry, &palette, context)?]);
    }
    let palettes = compound
        .list("palettes")
        .and_then(|list| list.lists())
        .ok_or_else(|| format!("structure template {context} is missing palette or palettes"))?;
    if palettes.is_empty() {
        return Err(format!(
            "structure template {context} has empty palettes list"
        ));
    }
    let mut result = Vec::with_capacity(palettes.len());
    for palette in palettes {
        let entries = palette.compounds().ok_or_else(|| {
            format!("structure template {context} has non-compound palette entry")
        })?;
        result.push(read_palette(registry, &entries, context)?);
    }
    Ok(result)
}

fn read_palette(
    registry: &Registry,
    entries: &BorrowedNbtCompoundList<'_, '_>,
    context: &str,
) -> Result<Vec<BlockStateId>, String> {
    let mut states = Vec::with_capacity(entries.len());
    for entry in entries.clone() {
        let Some(name) = entry.string("Name") else {
            return Err(format!(
                "structure template {context} has palette entry without Name"
            ));
        };
        let name = Identifier::from_str(name.to_str().as_ref()).map_err(|err| {
            format!("structure template {context} has invalid block identifier: {err}")
        })?;
        let mut properties = BTreeMap::new();
        if let Some(props) = entry.compound("Properties") {
            for (key, value) in props.iter() {
                let Some(value) = value.string() else {
                    return Err(format!(
                        "structure template {context} has non-string property {} on {name}",
                        key.to_str()
                    ));
                };
                properties.insert(key.to_str().into_owned(), value.to_str().into_owned());
            }
        }
        states.push(WorldgenStateResolver::block_state_from_data(
            registry,
            &BlockStateData { name, properties },
            "structure template palette",
        ));
    }
    Ok(states)
}

fn read_blocks(
    blocks: &BorrowedNbtCompoundList<'_, '_>,
    palette: &[BlockStateId],
    context: &str,
) -> Result<Vec<(BlockPos, BlockStateId)>, String> {
    let mut result = Vec::new();
    for block in blocks.clone() {
        let pos = read_vec3(block.list("pos"), context, "block pos")?;
        let state_index = block
            .int("state")
            .ok_or_else(|| format!("structure template {context} block is missing state"))?;
        if state_index < 0 {
            return Err(format!(
                "structure template {context} has negative palette state {state_index}"
            ));
        }
        let state_index = usize::try_from(state_index)
            .map_err(|_| format!("structure template {context} state index does not fit usize"))?;
        let Some(&state) = palette.get(state_index) else {
            return Err(format!(
                "structure template {context} state index {state_index} exceeds palette length {}",
                palette.len()
            ));
        };
        result.push((BlockPos::new(pos.x, pos.y, pos.z), state));
    }
    Ok(result)
}

fn transform_state(
    registry: &Registry,
    state: BlockStateId,
    mirror: StructureMirror,
    rotation: Rotation,
) -> BlockStateId {
    if mirror == StructureMirror::None && rotation == Rotation::None {
        return state;
    }
    let Some(block) = registry.blocks.by_state_id(state) else {
        return state;
    };
    let mut properties = registry
        .blocks
        .get_properties(state)
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<Vec<_>>();
    mirror_string_properties(&mut properties, mirror);
    rotate_string_properties(&mut properties, rotation);
    let property_refs = properties
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let Some(rotated) = registry
        .blocks
        .state_id_from_properties(&block.key, &property_refs)
    else {
        panic!(
            "rotating block state {} produced invalid properties",
            block.key
        );
    };
    rotated
}

fn parse_direction(value: &str) -> Option<Direction> {
    match value {
        "down" => Some(Direction::Down),
        "up" => Some(Direction::Up),
        "north" => Some(Direction::North),
        "south" => Some(Direction::South),
        "west" => Some(Direction::West),
        "east" => Some(Direction::East),
        _ => None,
    }
}

fn direction_from_property_name(name: &str) -> Direction {
    match name {
        "east" => Direction::East,
        "south" => Direction::South,
        "west" => Direction::West,
        _ => Direction::North,
    }
}

const fn property_name_from_direction(direction: Direction) -> Option<&'static str> {
    match direction {
        Direction::North => Some("north"),
        Direction::East => Some("east"),
        Direction::South => Some("south"),
        Direction::West => Some("west"),
        Direction::Down | Direction::Up => None,
    }
}

const fn mirror_direction(direction: Direction, mirror: StructureMirror) -> Direction {
    match mirror {
        StructureMirror::FrontBack => match direction {
            Direction::West => Direction::East,
            Direction::East => Direction::West,
            other => other,
        },
        StructureMirror::LeftRight => match direction {
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            other => other,
        },
        StructureMirror::None => direction,
    }
}

const fn inverse_rotate_direction(rotation: Rotation, direction: Direction) -> Direction {
    match rotation {
        Rotation::None => direction,
        Rotation::Clockwise90 => Rotation::CounterClockwise90.rotate(direction),
        Rotation::Clockwise180 => Rotation::Clockwise180.rotate(direction),
        Rotation::CounterClockwise90 => Rotation::Clockwise90.rotate(direction),
    }
}

fn parse_stair_shape(shape: &str) -> Option<&'static str> {
    match shape {
        "straight" => Some("straight"),
        "inner_left" => Some("inner_left"),
        "inner_right" => Some("inner_right"),
        "outer_left" => Some("outer_left"),
        "outer_right" => Some("outer_right"),
        _ => None,
    }
}

fn mirror_stair_shape(
    direction: Direction,
    shape: &str,
    mirror: StructureMirror,
) -> Option<(Direction, &'static str)> {
    match mirror {
        StructureMirror::LeftRight if matches!(direction, Direction::North | Direction::South) => {
            Some((
                direction.opposite(),
                match shape {
                    "outer_left" => "outer_right",
                    "inner_right" => "inner_left",
                    "inner_left" => "inner_right",
                    "outer_right" => "outer_left",
                    "straight" => "straight",
                    _ => return None,
                },
            ))
        }
        StructureMirror::FrontBack if matches!(direction, Direction::West | Direction::East) => {
            Some((
                direction.opposite(),
                match shape {
                    "outer_left" => "outer_right",
                    "inner_right" => "inner_left",
                    "inner_left" => "inner_right",
                    "outer_right" => "outer_left",
                    "straight" => "straight",
                    _ => return None,
                },
            ))
        }
        StructureMirror::LeftRight | StructureMirror::FrontBack => Some((
            direction,
            match shape {
                "outer_left" => "inner_left",
                "inner_left" => "outer_left",
                "inner_right" => "outer_right",
                "outer_right" => "inner_right",
                "straight" => "straight",
                _ => return None,
            },
        )),
        StructureMirror::None => None,
    }
}

fn mirror_string_properties(properties: &mut [(String, String)], mirror: StructureMirror) {
    if mirror == StructureMirror::None {
        return;
    }
    let original = properties.to_vec();
    let facing = original
        .iter()
        .find(|(name, _)| name == "facing")
        .and_then(|(_, value)| parse_direction(value));
    let stair_shape = original
        .iter()
        .find(|(name, _)| name == "shape")
        .and_then(|(_, value)| parse_stair_shape(value));
    let mirrored_stairs = facing
        .zip(stair_shape)
        .and_then(|(direction, shape)| mirror_stair_shape(direction, shape, mirror));
    for (name, value) in properties.iter_mut() {
        match name.as_str() {
            "facing" => {
                if let Some((mirrored_facing, _)) = mirrored_stairs {
                    mirrored_facing.as_str().clone_into(value);
                } else if let Some(direction) = parse_direction(value) {
                    mirror_direction(direction, mirror)
                        .as_str()
                        .clone_into(value);
                }
            }
            "shape" => {
                if let Some((_, mirrored_shape)) = mirrored_stairs {
                    mirrored_shape.clone_into(value);
                }
            }
            "north" | "east" | "south" | "west" => {
                let from = direction_from_property_name(name);
                let source = mirror_direction(from, mirror);
                if let Some(source_name) = property_name_from_direction(source)
                    && let Some((_, source_value)) = original
                        .iter()
                        .find(|(original_name, _)| original_name == source_name)
                {
                    value.clone_from(source_value);
                }
            }
            _ => {}
        }
    }
}

fn rotate_string_properties(properties: &mut [(String, String)], rotation: Rotation) {
    if rotation == Rotation::None {
        return;
    }
    let original = properties.to_vec();
    for (name, value) in properties.iter_mut() {
        match name.as_str() {
            "axis"
                if matches!(
                    rotation,
                    Rotation::Clockwise90 | Rotation::CounterClockwise90
                ) =>
            {
                match value.as_str() {
                    "x" => "z".clone_into(value),
                    "z" => "x".clone_into(value),
                    _ => {}
                }
            }
            "facing" => {
                if let Some(direction) = parse_direction(value) {
                    rotation.rotate(direction).as_str().clone_into(value);
                }
            }
            "north" | "east" | "south" | "west" => {
                let from = direction_from_property_name(name);
                let source = inverse_rotate_direction(rotation, from);
                if let Some(source_name) = property_name_from_direction(source)
                    && let Some((_, source_value)) = original
                        .iter()
                        .find(|(original_name, _)| original_name == source_name)
                {
                    value.clone_from(source_value);
                }
            }
            _ => {}
        }
    }
}
