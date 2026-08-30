//! Native re-export of the portable placed-feature ordering.
//!
//! The Features runner and static vegetation host must derive the same global
//! feature indices, because those indices are part of vanilla's decoration RNG
//! seed. Keep the sorter in `steel-worldgen` rather than maintaining copies.

pub(super) use steel_worldgen::vegetation::{FeatureSorter, FeatureStepData};
