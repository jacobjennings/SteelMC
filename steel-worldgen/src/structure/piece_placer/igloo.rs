//! Portable igloo piece placement.
//!
//! An igloo is the smallest template-backed structure in the game: twelve
//! kilobytes of saved blocks, one template, no jigsaw assembly, and an optional
//! basement. The top is placed here. The basement is refused, and the reason is
//! stated in [`place_igloo_piece`], because its ladder shaft is a stack of
//! separate template pieces whose chest marker needs loot handling a terrain
//! view does not have.

use glam::IVec3;
use steel_registry::feature::FeatureHeightmap;
use steel_registry::structure::LiquidSettingsData;
use steel_registry::{Registry, vanilla_blocks};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_utils::{BlockPos, BoundingBox};

use crate::structure::template::{
    StructureTemplate, TemplatePlacement, calculate_relative_position,
};
use crate::structure::{
    TemplateMarkerHandling, TemplatePieceData, TemplatePlacementAdjustment, TemplatePlacementClip,
    TemplatePostProcess, TemplateProcessorList,
};

use super::StructureBlockAccess;

/// The Y the igloo templates are saved at, before the ground offset.
const IGLOO_GENERATION_HEIGHT: i32 = 90;

/// Whether this template piece is one the portable slice can place.
///
/// The answer is deliberately narrow. Everything it refuses is refused because
/// this slice has no processors, no waterlogging, no jigsaw replacement, and no
/// per-block shape behaviour, not because the family is uninteresting.
#[must_use]
pub(super) fn is_portable_template_piece(data: &TemplatePieceData) -> bool {
    data.processors == TemplateProcessorList::Empty
        && data.liquid_settings == LiquidSettingsData::IgnoreWaterlogging
        && data.placement_clip == TemplatePlacementClip::CenterChunk
        && matches!(
            data.placement_adjustment,
            TemplatePlacementAdjustment::Igloo { .. }
        )
        && data.marker_handling == TemplateMarkerHandling::Igloo
        && matches!(
            data.post_process,
            TemplatePostProcess::None | TemplatePostProcess::IglooTop
        )
}

pub(super) fn place_igloo_piece<H: StructureBlockAccess>(
    region: &mut H,
    registry: &Registry,
    data: &mut TemplatePieceData,
    piece_bounding_box: &mut BoundingBox,
    clip: BoundingBox,
) -> bool {
    let template = match StructureTemplate::load_vanilla(registry, &data.template_id) {
        Ok(template) => template,
        Err(_) => return false,
    };
    // Entities are not blocks. The igloo basement carries a villager and a
    // zombie villager, and refusing the piece over them threw away the whole
    // laboratory: 155 blocks the native placer wrote and the portable one did
    // not. A terrain view simply draws no entities.
    //
    // A jigsaw block is different, because leaving one standing would be a
    // block state vanilla does not produce. No piece this module accepts can
    // contain one, so this is a guard against a future family, not a live case.
    if template.contains_jigsaw(registry) {
        return false;
    }

    let TemplatePlacementAdjustment::Igloo { template_offset } = data.placement_adjustment else {
        return false;
    };
    let placement = TemplatePlacement {
        mirror: data.mirror,
        rotation: data.rotation,
        rotation_pivot: BlockPos(data.rotation_pivot),
        clip,
        block_ignore: data.block_ignore,
        late_block_ignore: data.late_block_ignore,
        replace_jigsaws: false,
    };
    let position = adjusted_igloo_position(
        region,
        data.template_position,
        &placement,
        IVec3::new(template_offset.0, template_offset.1, template_offset.2),
    );

    let template_box = template.bounding_box_with_transform(position, &placement);
    *piece_bounding_box = template_box;
    if !template_box.intersects(clip) {
        return false;
    }

    if !template.place(region, registry, position, &placement) {
        return false;
    }

    for (marker_pos, metadata) in template.data_markers(registry, position, &placement) {
        // Vanilla clears the marker itself and then turns the block below it
        // into a loot chest. The chest is already a block in the template, so
        // only the cleared marker is a block change a terrain view can see.
        if metadata == "chest" {
            region.set_block_state(marker_pos, vanilla_blocks::AIR.default_state());
        }
    }

    if data.post_process == TemplatePostProcess::IglooTop {
        post_process_igloo_top(region, position, &placement);
    }
    true
}

/// Vanilla `IglooPieces.IglooPiece.getGroundLevelDelta` applied to the position.
///
/// The igloo does not sit on the average ground under its footprint. It sits on
/// the ground under one specific column, the one in front of its door, which is
/// why it can lean out of a slope.
fn adjusted_igloo_position<H: StructureBlockAccess>(
    region: &H,
    position: IVec3,
    placement: &TemplatePlacement,
    template_offset: IVec3,
) -> BlockPos {
    let raw_position = BlockPos(position);
    let entrance_relative = calculate_relative_position(
        BlockPos(IVec3::new(3 - template_offset.x, 0, -template_offset.z)),
        placement.mirror,
        placement.rotation,
        placement.rotation_pivot,
    );
    let entrance_pos = raw_position.offset(
        entrance_relative.x(),
        entrance_relative.y(),
        entrance_relative.z(),
    );
    let height = region.height_at(
        FeatureHeightmap::WorldSurfaceWg,
        entrance_pos.x(),
        entrance_pos.z(),
    );
    raw_position.offset(0, height - IGLOO_GENERATION_HEIGHT - 1, 0)
}

/// Vanilla `IglooPieces.IglooPiece.postProcess` snow cap over the trapdoor.
fn post_process_igloo_top<H: StructureBlockAccess>(
    region: &mut H,
    position: BlockPos,
    placement: &TemplatePlacement,
) {
    let trapdoor_relative = calculate_relative_position(
        BlockPos(IVec3::new(3, 0, 5)),
        placement.mirror,
        placement.rotation,
        placement.rotation_pivot,
    );
    let trapdoor_pos = position.offset(
        trapdoor_relative.x(),
        trapdoor_relative.y(),
        trapdoor_relative.z(),
    );
    let below_state = region.block_state(trapdoor_pos.below());
    if below_state.is_air() || below_state.get_block() == &vanilla_blocks::LADDER {
        return;
    }

    region.set_block_state(trapdoor_pos, vanilla_blocks::SNOW_BLOCK.default_state());
}
