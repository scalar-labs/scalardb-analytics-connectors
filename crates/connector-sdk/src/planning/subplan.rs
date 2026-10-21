//! Validated and persisted remote plan state.

use analytics_types::AnalyticsSchema;
use prost::Message;
use substrait::proto::Plan;

use super::{PlanningContext, SourceBinding, ValidationFingerprint};
use crate::{RequestError, substrait::validate_candidate};

/// A validated Substrait plan offered to one connector rule.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedCandidate {
    plan: Plan,
    output_schema: AnalyticsSchema,
    source_bindings: Vec<SourceBinding>,
    requires_retained_operator: bool,
}

impl ValidatedCandidate {
    pub(crate) fn new(
        plan: Plan,
        output_schema: AnalyticsSchema,
        source_bindings: Vec<SourceBinding>,
        requires_retained_operator: bool,
    ) -> Self {
        Self {
            plan,
            output_schema,
            source_bindings,
            requires_retained_operator,
        }
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn output_schema(&self) -> &AnalyticsSchema {
        &self.output_schema
    }
    pub fn source_bindings(&self) -> &[SourceBinding] {
        &self.source_bindings
    }
    pub fn requires_retained_operator(&self) -> bool {
        self.requires_retained_operator
    }
}

/// Serializable state stored in a Query Engine scan or table handle.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedSubplan {
    plan: Plan,
    output_schema: AnalyticsSchema,
    source_bindings: Vec<SourceBinding>,
    validation_fingerprint: ValidationFingerprint,
}

impl PlannedSubplan {
    pub(crate) fn from_candidate(
        candidate: ValidatedCandidate,
        validation_fingerprint: ValidationFingerprint,
    ) -> Self {
        Self {
            plan: candidate.plan,
            output_schema: candidate.output_schema,
            source_bindings: candidate.source_bindings,
            validation_fingerprint,
        }
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn encode_plan(&self) -> Vec<u8> {
        self.plan.encode_to_vec()
    }
    pub fn output_schema(&self) -> &AnalyticsSchema {
        &self.output_schema
    }
    pub fn source_bindings(&self) -> &[SourceBinding] {
        &self.source_bindings
    }
    pub fn validation_fingerprint(&self) -> ValidationFingerprint {
        self.validation_fingerprint
    }

    pub fn serialize(&self) -> SerializedPlannedSubplan {
        SerializedPlannedSubplan {
            substrait_plan: self.plan.encode_to_vec(),
            validation_fingerprint: self.validation_fingerprint,
        }
    }

    pub fn restore(
        serialized: SerializedPlannedSubplan,
        context: &PlanningContext,
    ) -> Result<Self, RequestError> {
        let (candidate, fingerprint) = validate_candidate(&serialized.substrait_plan, context)?;
        if fingerprint != serialized.validation_fingerprint {
            return Err(RequestError::invalid(
                "planned subplan fingerprint does not match the plan and planning context",
            ));
        }
        Ok(Self::from_candidate(candidate, fingerprint))
    }
}

/// Process-independent bytes used to persist a planned subplan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerializedPlannedSubplan {
    substrait_plan: Vec<u8>,
    validation_fingerprint: ValidationFingerprint,
}

impl SerializedPlannedSubplan {
    pub fn new(substrait_plan: Vec<u8>, validation_fingerprint: ValidationFingerprint) -> Self {
        Self {
            substrait_plan,
            validation_fingerprint,
        }
    }

    pub fn substrait_plan(&self) -> &[u8] {
        &self.substrait_plan
    }
    pub fn validation_fingerprint(&self) -> ValidationFingerprint {
        self.validation_fingerprint
    }
}
