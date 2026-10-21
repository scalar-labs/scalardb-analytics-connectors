//! Operator-specific planning rules and their coordination.

use super::{PlannedSubplan, PlanningContext, ValidatedCandidate};
use crate::{RequestError, substrait::validate_candidate};

/// The planner pattern represented by a Substrait candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PushdownRule {
    Read,
    Projection,
    Filter,
    Limit,
    Aggregation,
    Sort,
    TopN,
    Join,
}

/// Whether the Query Engine can retain the native operator above the scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidualPolicy {
    ExactOnly,
    MayRetainOperator,
}

/// Connector-specific support for one validated candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushdownDecision {
    Exact,
    Retained,
    Unsupported { reason: String },
}

/// The result consumed by the Query Engine adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum PushdownResult {
    Exact { plan: PlannedSubplan },
    Retained { plan: PlannedSubplan },
    Unsupported { reason: String },
}

/// A connector's plan-aware pushdown rules.
pub trait ConnectorPlanner {
    fn plan_read(
        &self,
        candidate: &ValidatedCandidate,
        context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError>;
    fn push_projection(
        &self,
        candidate: &ValidatedCandidate,
        context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError>;
    fn push_filter(
        &self,
        candidate: &ValidatedCandidate,
        context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError>;

    fn push_limit(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(unsupported("limit pushdown is not implemented"))
    }
    fn push_aggregation(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(unsupported("aggregation pushdown is not implemented"))
    }
    fn push_sort(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(unsupported("sort pushdown is not implemented"))
    }
    fn push_top_n(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(unsupported("TopN pushdown is not implemented"))
    }
    fn push_join(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(unsupported("join pushdown is not implemented"))
    }
}

fn unsupported(reason: &str) -> PushdownDecision {
    PushdownDecision::Unsupported {
        reason: reason.to_owned(),
    }
}

/// A planning attempt supplied by a Query Engine adapter.
#[derive(Clone, Copy, Debug)]
pub struct PlanningAttempt<'a> {
    pub rule: PushdownRule,
    pub candidate: &'a [u8],
    pub residual_policy: ResidualPolicy,
    pub context: &'a PlanningContext,
}

/// Validates, dispatches, and packages one pushdown attempt.
pub fn plan_pushdown(
    planner: &impl ConnectorPlanner,
    attempt: PlanningAttempt<'_>,
) -> Result<PushdownResult, PlanningError> {
    let (candidate, fingerprint) = validate_candidate(attempt.candidate, attempt.context)?;
    let decision = match attempt.rule {
        PushdownRule::Read => planner.plan_read(&candidate, attempt.context)?,
        PushdownRule::Projection => planner.push_projection(&candidate, attempt.context)?,
        PushdownRule::Filter => planner.push_filter(&candidate, attempt.context)?,
        PushdownRule::Limit => planner.push_limit(&candidate, attempt.context)?,
        PushdownRule::Aggregation => planner.push_aggregation(&candidate, attempt.context)?,
        PushdownRule::Sort => planner.push_sort(&candidate, attempt.context)?,
        PushdownRule::TopN => planner.push_top_n(&candidate, attempt.context)?,
        PushdownRule::Join => planner.push_join(&candidate, attempt.context)?,
    };
    match decision {
        PushdownDecision::Exact
            if attempt.rule == PushdownRule::Filter && candidate.requires_retained_operator() =>
        {
            Err(PlanningError::BestEffortRequiresResidual)
        }
        PushdownDecision::Exact => Ok(PushdownResult::Exact {
            plan: PlannedSubplan::from_candidate(candidate, fingerprint),
        }),
        PushdownDecision::Retained
            if attempt.residual_policy == ResidualPolicy::MayRetainOperator =>
        {
            Ok(PushdownResult::Retained {
                plan: PlannedSubplan::from_candidate(candidate, fingerprint),
            })
        }
        PushdownDecision::Retained => Err(PlanningError::ResidualRequired),
        PushdownDecision::Unsupported { reason } => Ok(PushdownResult::Unsupported { reason }),
    }
}

/// Failure at the shared planning boundary.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanningError {
    #[error(transparent)]
    InvalidCandidate(#[from] RequestError),
    #[error(
        "connector requires a retained native operator, but this attempt requires exact pushdown"
    )]
    ResidualRequired,
    #[error("a best-effort Substrait candidate cannot remove the corresponding native operator")]
    BestEffortRequiresResidual,
    #[error("connector planning failed: {reason}")]
    Connector { reason: String },
}
