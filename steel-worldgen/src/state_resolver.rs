use std::str::FromStr as _;
use steel_registry::blocks::BlockRef;
use steel_registry::feature;
use steel_registry::shared_structs;
use steel_registry::{Registry, RegistryExt};
use steel_utils::{BlockStateId, Identifier};

/// Resolves vanilla JSON/NBT block-state data to Steel block-state ids.
pub struct WorldgenStateResolver;

impl WorldgenStateResolver {
    /// Resolves vanilla's command-style block-state string used by jigsaw NBT.
    #[must_use]
    pub fn block_state_from_string(registry: &Registry, value: &str) -> Option<BlockStateId> {
        let identifier_end = value
            .char_indices()
            .find_map(|(index, character)| {
                (character != ':' && !Identifier::valid_char(character)).then_some(index)
            })
            .unwrap_or(value.len());
        if identifier_end == 0 {
            return None;
        }
        let identifier = Identifier::from_str(&value[..identifier_end]).ok()?;
        let block = registry.blocks.by_key(&identifier)?;

        let rest = &value[identifier_end..];
        let mut properties = Vec::new();
        if let Some(rest) = rest.strip_prefix('[') {
            let end = rest.find(']')?;
            let encoded = &rest[..end];
            if !encoded.is_empty() {
                for property in encoded.split(',') {
                    properties.push(property.split_once('=')?);
                }
            }
        }

        registry
            .blocks
            .state_id_from_block_defaulted_properties(block, properties)
    }

    /// Resolves a block state from data.
    ///
    /// # Panics
    /// Panics if the block is not in the registry or if the state properties are invalid.
    #[must_use]
    pub fn block_state_from_data(
        registry: &Registry,
        data: &shared_structs::BlockStateData,
        context: &str,
    ) -> BlockStateId {
        let Some(block) = registry.blocks.by_key(&data.name) else {
            panic!("{context} references unknown block {}", data.name);
        };
        Self::block_state_from_parts(
            registry,
            block,
            &data.name,
            data.properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
            context,
        )
    }

    /// Resolves a feature block state from data.
    ///
    /// # Panics
    /// Panics if the state properties are invalid.
    #[must_use]
    pub fn feature_block_state_from_data(
        registry: &Registry,
        data: &feature::BlockStateData,
        context: &str,
    ) -> BlockStateId {
        Self::block_state_from_parts(
            registry,
            data.block,
            &data.block.key,
            data.properties.iter().copied(),
            context,
        )
    }

    fn block_state_from_parts<'a>(
        registry: &Registry,
        block: BlockRef,
        block_name: &steel_utils::Identifier,
        data_properties: impl IntoIterator<Item = (&'a str, &'a str)>,
        context: &str,
    ) -> BlockStateId {
        let Some(state) = registry
            .blocks
            .state_id_from_block_defaulted_properties(block, data_properties)
        else {
            panic!("{context} references unknown or invalid state {block_name}");
        };
        state
    }
}
