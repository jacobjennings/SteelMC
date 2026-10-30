//! This module contains the `WorldGenContext` struct, which is used to provide context for chunk generation.

use std::sync::{Arc, Weak};

use enum_dispatch::enum_dispatch;
use steel_registry::dimension_type::DimensionTypeRef;
use steel_worldgen::density_functions::{
    end::EndNoises, nether::NetherNoises, overworld::OverworldNoises,
};

use crate::world::World;
use crate::worldgen::generator::{
    ChunkGenerator, EmptyChunkGenerator, FlatChunkGenerator, VanillaGenerator,
};

/// Type alias for overworld generator.
pub type OverworldGenerator = VanillaGenerator<OverworldNoises>;

/// Type alias for nether generator.
pub type NetherGenerator = VanillaGenerator<NetherNoises>;

/// Type alias for end generator.
pub type EndGenerator = VanillaGenerator<EndNoises>;

#[expect(
    missing_docs,
    reason = "variants are named after their dimension; self-explanatory"
)]
#[enum_dispatch(ChunkGenerator)]
pub enum ChunkGeneratorType {
    Flat(FlatChunkGenerator),
    Empty(EmptyChunkGenerator),
    Overworld(OverworldGenerator),
    Nether(NetherGenerator),
    End(EndGenerator),
    //Custom(Box<dyn ChunkGenerator>),
}

/// Context for world generation.
pub struct WorldGenContext {
    /// The chunk generator to use.
    pub generator: Arc<ChunkGeneratorType>,
    /// Weak reference to the world (to avoid circular Arc reference).
    /// Use `world()` to get a strong reference when needed.
    world: Weak<World>,
    /// Cached dimension minimum build Y. Immutable for the world's lifetime;
    /// cached here so per-block queries avoid a `Weak<World>::upgrade` (a
    /// cross-thread atomic refcount round-trip) on every call.
    min_y: i32,
    /// Cached dimension build height. See [`Self::min_y`].
    height: i32,
    /// Cached dimension sea level. Immutable for the world's lifetime. Read
    /// per-column by the `freeze_top_layer` feature (snow/ice placement);
    /// cached here so it avoids a `Weak<World>::upgrade` (cross-thread atomic
    /// on the shared `Arc<World>`) on every column. See [`Self::min_y`].
    sea_level: i32,
    /// World facts for generation without a live world, such as the browser
    /// module. Server contexts read them from the world instead.
    standalone: Option<StandaloneWorldFacts>,
}

/// World facts that generation reads when no live world exists.
#[derive(Clone, Copy)]
struct StandaloneWorldFacts {
    seed: i64,
    dimension_type: DimensionTypeRef,
}

impl WorldGenContext {
    /// Creates a new `WorldGenContext`.
    ///
    /// `min_y`/`height` are the dimension's build bounds (`DimensionType::min_y`
    /// / `::height`); they are cached rather than read from the world per call.
    #[must_use]
    pub const fn new(
        generator: Arc<ChunkGeneratorType>,
        world: Weak<World>,
        min_y: i32,
        height: i32,
        sea_level: i32,
    ) -> Self {
        Self {
            generator,
            world,
            min_y,
            height,
            sea_level,
            standalone: None,
        }
    }

    /// Creates a context for generation without a live world.
    ///
    /// Generated chunks carry no world reference. Stages that need world
    /// state read the seed and dimension type given here.
    #[must_use]
    pub fn new_standalone(
        generator: Arc<ChunkGeneratorType>,
        seed: i64,
        dimension_type: DimensionTypeRef,
        sea_level: i32,
    ) -> Self {
        Self {
            generator,
            world: Weak::new(),
            min_y: dimension_type.min_y,
            height: dimension_type.height,
            sea_level,
            standalone: Some(StandaloneWorldFacts {
                seed,
                dimension_type,
            }),
        }
    }

    /// Returns the world seed.
    ///
    /// # Panics
    /// Panics if a server context's world has been dropped.
    #[must_use]
    pub fn seed(&self) -> i64 {
        match self.standalone {
            Some(facts) => facts.seed,
            None => self.world().seed(),
        }
    }

    /// Returns the dimension type being generated.
    ///
    /// # Panics
    /// Panics if a server context's world has been dropped.
    #[must_use]
    pub fn dimension_type(&self) -> DimensionTypeRef {
        match self.standalone {
            Some(facts) => facts.dimension_type,
            None => self.world().dimension_type,
        }
    }

    /// Returns the scheduled-tick delay of a fluid in this dimension.
    ///
    /// # Panics
    /// Panics if a server context's world has been dropped.
    #[must_use]
    pub fn fluid_tick_delay(&self, fluid: steel_registry::fluid::FluidRef) -> i32 {
        let behavior = crate::behavior::FLUID_BEHAVIORS.get_behavior(fluid);
        match self.standalone {
            Some(facts) => behavior.tick_delay_for_dimension(facts.dimension_type.fast_lava),
            None => behavior.tick_delay(&self.world()),
        }
    }

    /// Returns the dimension's sea level (cached; see [`Self::min_y`]).
    #[must_use]
    pub const fn sea_level(&self) -> i32 {
        self.sea_level
    }

    /// Gets a strong reference to the world.
    ///
    /// # Panics
    /// Panics if the world has been dropped.
    #[must_use]
    pub fn world(&self) -> Arc<World> {
        self.world.upgrade().expect("World has been dropped")
    }

    /// Gets a weak reference to the world.
    ///
    /// This is useful for passing to chunks without creating a strong reference cycle.
    #[must_use]
    pub fn weak_world(&self) -> Weak<World> {
        self.world.clone()
    }

    /// Returns the minimum Y coordinate of the world.
    #[must_use]
    pub const fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Returns the total height of the world in blocks.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// Returns the minimum Y coordinate used by `WorldGenerationContext`.
    #[must_use]
    pub fn generation_min_y(&self) -> i32 {
        self.min_y.max(self.generator.min_y())
    }

    /// Returns the height used by `WorldGenerationContext`.
    #[must_use]
    pub fn generation_height(&self) -> i32 {
        self.height.min(self.generator.gen_depth())
    }

    #[must_use]
    /// How many sections this dimension has
    pub const fn section_count(&self) -> usize {
        (self.height / 16) as usize
    }
}
