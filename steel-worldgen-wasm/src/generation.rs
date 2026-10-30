//! Standalone generation qualification, independent of viewer detail names.

use serde::Serialize;

/// Browser contract version. Admission is deliberately bounded to Features.
pub const CAPABILITY_SCHEMA: &str = "chunkatlas-generation-capabilities/v1";

/// Unsupported operations have stable machine-readable refusal names.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationCapability {
    operation: &'static str,
    admitted: bool,
    reason: &'static str,
}

/// A capability query does not execute any generation operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationCapabilities {
    schema: &'static str,
    version: &'static str,
    maximum_generation_stage: &'static str,
    native_full_qualified: bool,
    full_detail_means: &'static str,
    operations: Vec<OperationCapability>,
}

impl GenerationCapabilities {
    pub fn current() -> Self {
        let operations = [
            "features-snapshot",
            "poi-updates",
            "block-entity-payloads",
            "pending-entity-promotion",
            "full-conversion",
            "tick-container-registration",
            "full-postprocessing",
            "fluid-tick-execution",
            "fluid-tick-order",
            "liquid-block-ticks",
            "neighbor-shape-updates",
            "lighting",
            "native-full",
        ]
        .into_iter()
        .map(OperationCapability::for_operation)
        .collect();
        Self {
            schema: CAPABILITY_SCHEMA,
            version: crate::RELEASE,
            maximum_generation_stage: "features",
            native_full_qualified: false,
            full_detail_means: "surface plus Features changes, a viewer detail level",
            operations,
        }
    }
}

impl OperationCapability {
    pub fn for_operation(operation: &str) -> Self {
        let (operation, admitted, reason) = match operation {
            "features-snapshot" => ("features-snapshot", true, "features-only"),
            "poi-updates" => ("poi-updates", false, "unsupported-native-poi-callback"),
            "block-entity-payloads" => (
                "block-entity-payloads",
                false,
                "unqualified-module-block-entity-payloads",
            ),
            "pending-entity-promotion" => (
                "pending-entity-promotion",
                false,
                "unsupported-native-pending-entity-promotion",
            ),
            "full-conversion" => (
                "full-conversion",
                false,
                "unsupported-native-full-conversion",
            ),
            "tick-container-registration" => (
                "tick-container-registration",
                false,
                "unsupported-native-tick-container-registration",
            ),
            "full-postprocessing" => (
                "full-postprocessing",
                false,
                "unsupported-native-full-postprocessing",
            ),
            "fluid-tick-execution" => (
                "fluid-tick-execution",
                false,
                "unsupported-native-fluid-tick-execution",
            ),
            "fluid-tick-order" => (
                "fluid-tick-order",
                false,
                "unsupported-native-fluid-tick-order",
            ),
            "liquid-block-ticks" => (
                "liquid-block-ticks",
                false,
                "unsupported-native-liquid-block-ticks",
            ),
            "neighbor-shape-updates" => (
                "neighbor-shape-updates",
                false,
                "unsupported-native-neighbor-shape-updates",
            ),
            "lighting" => ("lighting", false, "unsupported-native-lighting"),
            "native-full" => ("native-full", false, "unsupported-native-full"),
            _ => ("unknown", false, "unknown-operation"),
        };
        Self {
            operation,
            admitted,
            reason,
        }
    }

    pub const fn refusal(&self) -> Option<&'static str> {
        if self.admitted {
            None
        } else {
            Some(self.reason)
        }
    }
}
