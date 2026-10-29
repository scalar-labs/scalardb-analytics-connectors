//! Planner-coupled connector pushdown contracts.

mod context;
mod rules;
mod subplan;

pub use context::{NullOrdering, PlanningContext, TableReference, ValidationFingerprint};
pub use rules::{
    ConnectorPlanner, PlanningAttempt, PlanningError, PushdownDecision, PushdownResult,
    PushdownRule, ResidualPolicy, plan_pushdown,
};
pub use subplan::{PlannedSubplan, SerializedPlannedSubplan, ValidatedCandidate};
