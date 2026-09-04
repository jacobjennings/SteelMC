//! Portable structure template loading and block placement.
//!
//! A template structure is a saved lump of blocks: a size, a palette of block
//! states, and a list of positions with a palette index, all in gzipped NBT.
//! `steel-registry` already bundles the vanilla template bytes, so a browser
//! needs no new asset: it needs a reader, a rotation, and a write loop.
//!
//! This is the smallest workable slice of vanilla's `StructureTemplate`. What it
//! carries:
//!
//! - loading one gzipped NBT template from the bundled bytes,
//! - the single-palette and multi-palette forms, with vanilla's positional
//!   palette choice,
//! - vanilla's block ordering (full blocks, then other blocks, then blocks with
//!   NBT), because later writes overwrite earlier ones at the same position,
//! - mirror and rotation about a pivot, for positions and for block states,
//! - the structure-block and air ignore filters,
//! - the clip box, so a piece writes only into the chunk being decorated.
//!
//! What it deliberately does not carry, each of which makes a piece refuse
//! rather than place something wrong:
//!
//! - structure processors, so any template with a processor list is refused,
//! - waterlogging and liquid settling, so `apply_waterlogging` is refused,
//! - entities, block entities, and loot tables, which change no block state,
//! - the neighbour shape update pass, which needs the server's per-block
//!   behaviour table. [`StructureTemplate::shape_sensitive_blocks`] reports
//!   whether a template contains a block whose vanilla shape update could
//!   change its own state, so a caller can refuse instead of guessing.
//!
//! That last omission has been measured rather than assumed. An igloo top is
//! identical to the native placer at every block. An igloo basement disagrees
//! on three iron bars out of 404, along the edge where a template block meets
//! ground the template did not place, because vanilla connects them to it and
//! this slice cannot. The reproduction is
//! `portable_igloo_basement_needs_the_block_shape_update_pass` in `steel-core`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{Cursor, Read as _};
use std::str::FromStr as _;

use flate2::read::GzDecoder;
use glam::IVec3;
use simdnbt::borrow::{
    Nbt as BorrowedNbt, NbtCompound as BorrowedNbtCompound,
    NbtCompoundList as BorrowedNbtCompoundList, NbtList as BorrowedNbtList, read as read_nbt,
};
use steel_registry::shared_structs::BlockStateData;
use steel_registry::{Registry, blocks, vanilla_blocks, vanilla_template_pools};
use steel_utils::random::Random as _;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::{BlockPos, BlockStateId, BoundingBox, Identifier, Rotation};

use crate::state_resolver::WorldgenStateResolver;
use crate::structure::piece_placer::StructureBlockAccess;
use crate::structure::state_transform::transform_state;
use crate::structure::{StructureBlockIgnore, StructureMirror};

/// One block of a loaded template.
#[derive(Debug, Clone)]
pub struct TemplateBlock {
    /// Position inside the template, before any transform.
    pub pos: BlockPos,
    /// Palette state, before any transform.
    pub state: BlockStateId,
    /// Whether the saved block carried block-entity NBT.
    ///
    /// The payload itself is dropped. A browser draws blocks, not chests, and
    /// vanilla overwrites its own barrier placeholder with the final state at
    /// the same position, so no block outcome depends on it.
    pub has_nbt: bool,
    /// A structure block's `metadata` string, the instruction a family placer
    /// reads off a data marker.
    pub metadata: Option<String>,
    /// A jigsaw block's replacement state after pool-element placement.
    pub final_state: Option<BlockStateId>,
}

/// A loaded vanilla structure template.
#[derive(Debug, Clone)]
pub struct StructureTemplate {
    size: IVec3,
    palettes: Vec<Vec<TemplateBlock>>,
    entity_count: usize,
}

/// How one template placement is transformed and clipped.
#[derive(Debug, Clone, Copy)]
pub struct TemplatePlacement {
    /// Mirror applied before rotation.
    pub mirror: StructureMirror,
    /// Rotation about `rotation_pivot`.
    pub rotation: Rotation,
    /// Pivot the rotation turns around, in template coordinates.
    pub rotation_pivot: BlockPos,
    /// Only positions inside this box are written.
    pub clip: BoundingBox,
    /// Palette states this placement refuses before any processing.
    pub block_ignore: StructureBlockIgnore,
    /// Palette states this placement refuses after processing.
    pub late_block_ignore: StructureBlockIgnore,
}

impl StructureTemplate {
    /// Loads one bundled vanilla template by its registry key.
    ///
    /// # Errors
    /// Returns a message when the template is not bundled, or when its NBT does
    /// not have the shape vanilla writes.
    pub fn load_vanilla(registry: &Registry, key: &Identifier) -> Result<Self, String> {
        let Some(bytes) = vanilla_template_pools::vanilla_template_nbt_bytes(key) else {
            return Err(format!("vanilla structure template {key} is not bundled"));
        };
        Self::load_gzip_nbt(registry, bytes, &key.to_string())
    }

    /// Loads one gzipped NBT template.
    ///
    /// # Errors
    /// Returns a message describing the first field that did not parse.
    pub fn load_gzip_nbt(registry: &Registry, bytes: &[u8], context: &str) -> Result<Self, String> {
        let mut decoder = GzDecoder::new(bytes);
        let mut data = Vec::new();
        decoder
            .read_to_end(&mut data)
            .map_err(|err| format!("failed to decompress structure template {context}: {err}"))?;

        let nbt = read_nbt(&mut Cursor::new(&data))
            .map_err(|err| format!("failed to parse structure template {context}: {err}"))?;
        let BorrowedNbt::Some(root) = nbt else {
            return Err(format!("structure template {context} is empty"));
        };
        let compound = root.as_compound();

        let size = read_vec3(compound.list("size"), context, "size")?;
        let palettes = read_palettes(registry, &compound, context)?;
        let blocks_list = compound
            .list("blocks")
            .and_then(|list| list.compounds())
            .ok_or_else(|| format!("structure template {context} has non-compound blocks list"))?;

        let mut loaded = Vec::with_capacity(palettes.len());
        for palette in &palettes {
            loaded.push(read_blocks(registry, &blocks_list, palette, context)?);
        }

        let entity_count = compound
            .list("entities")
            .and_then(|list| list.compounds())
            .map_or(0, |entities| entities.len());

        Ok(Self {
            size,
            palettes: loaded,
            entity_count,
        })
    }

    /// The template's untransformed size.
    #[must_use]
    pub const fn size(&self) -> IVec3 {
        self.size
    }

    /// How many entities the saved template carries. A portable host draws none.
    #[must_use]
    pub const fn entity_count(&self) -> usize {
        self.entity_count
    }

    /// Vanilla's `StructureTemplate.getBoundingBox` under mirror and rotation.
    #[must_use]
    pub fn bounding_box_with_transform(
        &self,
        position: BlockPos,
        placement: &TemplatePlacement,
    ) -> BoundingBox {
        let corner1 = calculate_relative_position(
            BlockPos::ZERO,
            placement.mirror,
            placement.rotation,
            placement.rotation_pivot,
        );
        let corner2 = calculate_relative_position(
            BlockPos(self.size - 1),
            placement.mirror,
            placement.rotation,
            placement.rotation_pivot,
        );
        BoundingBox::new(position.0 + corner1.0, position.0 + corner2.0)
    }

    /// Vanilla's positional palette choice for one placement position.
    fn palette(&self, position: BlockPos) -> Option<&[TemplateBlock]> {
        if self.palettes.is_empty() {
            return None;
        }
        let Ok(bound) = i32::try_from(self.palettes.len()) else {
            panic!("structure template palette count exceeds i32 range");
        };
        let mut random = LegacyRandom::from_seed(block_pos_seed(position) as u64);
        let index = random.next_i32_bounded(bound);
        Some(&self.palettes[index as usize])
    }

    /// Writes the template into a portable region and reports what it wrote.
    ///
    /// Returns `false` without writing anything when the template is empty or
    /// degenerate, matching vanilla's early refusal.
    pub fn place<H: StructureBlockAccess>(
        &self,
        region: &mut H,
        registry: &Registry,
        position: BlockPos,
        placement: &TemplatePlacement,
    ) -> bool {
        let Some(palette) = self.palette(position) else {
            return false;
        };
        if palette.is_empty()
            || [self.size.x, self.size.y, self.size.z]
                .iter()
                .any(|&axis| axis < 1)
        {
            return false;
        }

        for block in palette {
            if placement.block_ignore.ignores(registry, block.state)
                || placement.late_block_ignore.ignores(registry, block.state)
            {
                continue;
            }
            let world_pos = self.transformed_position(position, block.pos, placement);
            if !placement.clip.contains_blockpos(world_pos) {
                continue;
            }
            let final_state =
                transform_state(registry, block.state, placement.mirror, placement.rotation);
            region.set_block_state(world_pos, final_state);
        }
        true
    }

    /// Vanilla's structure-block data markers, in world coordinates.
    ///
    /// A template's markers are the saved structure blocks. Their metadata
    /// string is the instruction, and the family placer decides what it means.
    #[must_use]
    pub fn data_markers(
        &self,
        registry: &Registry,
        position: BlockPos,
        placement: &TemplatePlacement,
    ) -> Vec<(BlockPos, String)> {
        let Some(palette) = self.palette(position) else {
            return Vec::new();
        };
        let mut markers = Vec::new();
        for block in palette {
            let Some(block_ref) = registry.blocks.by_state_id(block.state) else {
                continue;
            };
            if block_ref != &vanilla_blocks::STRUCTURE_BLOCK {
                continue;
            }
            let world_pos = self.transformed_position(position, block.pos, placement);
            if placement.clip.contains_blockpos(world_pos) {
                markers.push((world_pos, block.metadata.clone().unwrap_or_default()));
            }
        }
        markers
    }

    /// Whether the template contains a jigsaw block.
    ///
    /// Vanilla replaces each jigsaw with its saved `final_state` after
    /// placement. This slice does not, so a caller must refuse such a template
    /// rather than leave a jigsaw block standing in the world.
    #[must_use]
    pub fn contains_jigsaw(&self, registry: &Registry) -> bool {
        self.palettes.iter().flatten().any(|block| {
            registry
                .blocks
                .by_state_id(block.state)
                .is_some_and(|block_ref| block_ref == &vanilla_blocks::JIGSAW)
        })
    }

    /// Whether the template holds a block a vanilla shape update could change.
    ///
    /// Vanilla runs a neighbour shape pass after placing a template, which
    /// needs the server's per-block behaviour table. This slice does not have
    /// one, so a caller must check this and refuse a template that would need
    /// it rather than draw a state vanilla would have corrected.
    #[must_use]
    pub fn shape_sensitive_blocks(&self, registry: &Registry) -> Vec<Identifier> {
        let mut found = Vec::new();
        for palette in &self.palettes {
            for block in palette {
                let Some(block_ref) = registry.blocks.by_state_id(block.state) else {
                    continue;
                };
                if !block_ref.config.dynamic_shape {
                    continue;
                }
                if !found.contains(&block_ref.key) {
                    found.push(block_ref.key.clone());
                }
            }
        }
        found
    }

    fn transformed_position(
        &self,
        position: BlockPos,
        template_pos: BlockPos,
        placement: &TemplatePlacement,
    ) -> BlockPos {
        let _ = self;
        let transformed = calculate_relative_position(
            template_pos,
            placement.mirror,
            placement.rotation,
            placement.rotation_pivot,
        );
        position.offset(transformed.x(), transformed.y(), transformed.z())
    }
}

/// Vanilla's `StructureTemplate.calculateRelativePosition`.
#[must_use]
pub const fn calculate_relative_position(
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

/// Vanilla's `Mth.getSeed` over a block position.
#[must_use]
pub const fn block_pos_seed(pos: BlockPos) -> i64 {
    let mut seed = (pos.x().wrapping_mul(3_129_871) as i64)
        ^ (pos.z() as i64).wrapping_mul(116_129_781)
        ^ (pos.y() as i64);
    seed = seed
        .wrapping_mul(seed)
        .wrapping_mul(42_317_861)
        .wrapping_add(seed.wrapping_mul(11));
    seed >> 16
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
        let name = Identifier::from_str(Cow::from(name.to_str()).as_ref()).map_err(|err| {
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
    registry: &Registry,
    blocks_list: &BorrowedNbtCompoundList<'_, '_>,
    palette: &[BlockStateId],
    context: &str,
) -> Result<Vec<TemplateBlock>, String> {
    let mut full_blocks = Vec::new();
    let mut other_blocks = Vec::new();
    let mut block_entities = Vec::new();

    for block in blocks_list.clone() {
        let pos = read_vec3(block.list("pos"), context, "block pos")?;
        let state_index = block
            .int("state")
            .ok_or_else(|| format!("structure template {context} block is missing state"))?;
        let state_index = usize::try_from(state_index).map_err(|_| {
            format!("structure template {context} has an out-of-range palette state {state_index}")
        })?;
        let Some(&state) = palette.get(state_index) else {
            return Err(format!(
                "structure template {context} state index {state_index} exceeds palette length {}",
                palette.len()
            ));
        };
        let nbt = block.compound("nbt");
        let info = TemplateBlock {
            pos: BlockPos::new(pos.x, pos.y, pos.z),
            state,
            has_nbt: nbt.is_some(),
            metadata: nbt
                .as_ref()
                .and_then(|nbt| nbt.string("metadata"))
                .map(|value| value.to_str().into_owned()),
            final_state: nbt
                .as_ref()
                .and_then(|nbt| nbt.string("final_state"))
                .and_then(|value| {
                    WorldgenStateResolver::block_state_from_string(
                        registry,
                        value.to_str().as_ref(),
                    )
                }),
        };

        if info.has_nbt {
            block_entities.push(info);
        } else if is_static_full_block(registry, state) {
            full_blocks.push(info);
        } else {
            other_blocks.push(info);
        }
    }

    sort_block_infos(&mut full_blocks);
    sort_block_infos(&mut other_blocks);
    sort_block_infos(&mut block_entities);

    full_blocks.extend(other_blocks);
    full_blocks.extend(block_entities);
    Ok(full_blocks)
}

fn is_static_full_block(registry: &Registry, state: BlockStateId) -> bool {
    let Some(block) = registry.blocks.by_state_id(state) else {
        return false;
    };
    !block.config.dynamic_shape
        && blocks::shapes::is_shape_full_block(registry.blocks.get_static_collision_shape(state))
}

fn sort_block_infos(blocks: &mut [TemplateBlock]) {
    blocks.sort_by(|left, right| {
        left.pos
            .y()
            .cmp(&right.pos.y())
            .then(left.pos.x().cmp(&right.pos.x()))
            .then(left.pos.z().cmp(&right.pos.z()))
    });
}
