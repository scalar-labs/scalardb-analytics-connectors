//! A complete pushdown request and its validated planning input.

use ::substrait::proto::Plan;

use super::{ConnectorPlanner, LogicalPlan, PlanningError, PushdownResult};

/// One complete pushdown region supplied by a Query Engine adapter.
#[derive(Clone, Debug)]
pub struct PushdownRequest {
    /// Complete Substrait region offered for one atomic pushdown decision.
    pub offered_plan: Plan,
}

impl PushdownRequest {
    /// Validates this request and delegates planning to the Connector.
    pub fn plan(self, planner: &impl ConnectorPlanner) -> Result<PushdownResult, PlanningError> {
        let offered_plan = LogicalPlan::validate(self.offered_plan)?;
        planner.plan(&offered_plan)
    }
}
