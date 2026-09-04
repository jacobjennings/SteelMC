//! Portable placement for the village pool element used by the calibration fixture.

use steel_registry::Registry;
use steel_registry::template_pool::{PoolElement, ProcessorList};
use steel_utils::{BlockPos, BoundingBox};

use crate::structure::jigsaw::JigsawPieceData;
use crate::structure::template::{StructureTemplate, TemplatePlacement};
use crate::structure::{StructureBlockIgnore, StructureMirror};

use super::StructureBlockAccess;

const CALIBRATION_TEMPLATE: &str = "village/savanna/houses/savanna_weaponsmith_2";

pub(super) fn is_calibration_piece(data: &JigsawPieceData) -> bool {
    matches!(
        &data.pool_element,
        PoolElement::LegacySingle {
            location,
            processors: ProcessorList::Empty,
            ..
        } if location.namespace.as_ref() == "minecraft" && location.path.as_ref() == CALIBRATION_TEMPLATE
    )
}

pub(super) fn place_calibration_piece<H: StructureBlockAccess>(
    region: &mut H,
    registry: &Registry,
    data: &JigsawPieceData,
    clip: BoundingBox,
) -> bool {
    let PoolElement::LegacySingle { location, .. } = &data.pool_element else {
        return false;
    };
    let Ok(template) = StructureTemplate::load_vanilla(registry, location) else {
        return false;
    };
    let placement = TemplatePlacement {
        mirror: StructureMirror::None,
        rotation: data.rotation,
        rotation_pivot: BlockPos::ZERO,
        clip,
        block_ignore: StructureBlockIgnore::None,
        late_block_ignore: StructureBlockIgnore::StructureAndAir,
        replace_jigsaws: true,
    };
    template.place(region, registry, BlockPos(data.position), &placement)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rustc_hash::FxHashMap;
    use steel_registry::feature::FeatureHeightmap;
    use steel_registry::structure::LiquidSettingsData;
    use steel_registry::template_pool::Projection;
    use steel_registry::{REGISTRY, init_vanilla_registry, vanilla_blocks};
    use steel_utils::{BlockStateId, Identifier, Rotation};

    use super::*;

    #[derive(Default)]
    struct EmptyRegion(FxHashMap<BlockPos, BlockStateId>);

    impl StructureBlockAccess for EmptyRegion {
        fn min_y(&self) -> i32 {
            -64
        }

        fn block_state(&self, pos: BlockPos) -> BlockStateId {
            self.0
                .get(&pos)
                .copied()
                .unwrap_or_else(|| vanilla_blocks::AIR.default_state())
        }

        fn set_block_state(&mut self, pos: BlockPos, state: BlockStateId) {
            self.0.insert(pos, state);
        }

        fn height_at(&self, _kind: FeatureHeightmap, _x: i32, _z: i32) -> i32 {
            0
        }
    }

    #[test]
    fn calibration_weaponsmith_places_all_non_ignored_blocks() {
        init_vanilla_registry();
        let data = JigsawPieceData {
            pool_element: PoolElement::LegacySingle {
                location: Identifier::vanilla_static(CALIBRATION_TEMPLATE),
                processors: ProcessorList::Empty,
                projection: Projection::Rigid,
            },
            position: glam::IVec3::ZERO,
            rotation: Rotation::None,
            liquid_settings: LiquidSettingsData::ApplyWaterlogging,
        };
        let mut region = EmptyRegion::default();
        assert!(place_calibration_piece(
            &mut region,
            &REGISTRY,
            &data,
            BoundingBox::new(glam::IVec3::splat(-32), glam::IVec3::splat(32)),
        ));

        let mut kinds = BTreeMap::<String, usize>::new();
        for &state in region.0.values() {
            let block = REGISTRY.blocks.by_state_id(state).unwrap();
            *kinds.entry(block.key.to_string()).or_default() += 1;
        }
        println!(
            "CALIBRATION_WEAPONSMITH placed={} kinds={kinds:?}",
            region.0.len()
        );
        assert_eq!(region.0.len(), 384);
        for (kind, count) in [
            ("minecraft:acacia_door", 2),
            ("minecraft:acacia_fence", 1),
            ("minecraft:acacia_stairs", 62),
            ("minecraft:glass_pane", 3),
            ("minecraft:iron_bars", 8),
            ("minecraft:smooth_stone_slab", 17),
        ] {
            assert_eq!(kinds.get(kind), Some(&count), "{kind} count changed");
        }
    }
}
