//! Pushdown planning models and their transitions.
//!
//! A [`PushdownRequest`] validates its Substrait input into a [`LogicalPlan`].
//! A [`ConnectorPlanner`] transforms that plan through complete [`QueryPlan`]
//! states and returns a validated [`PlannedPushdown`].

mod connector_planner;
mod logical_plan;
mod planned_pushdown;
mod pushdown_request;
mod query_plan;

pub use connector_planner::{
    ConnectorPlanner, PlanningError, PushdownResult, RewriteRuleConnectorPlanner,
};
pub use logical_plan::{LogicalPlan, LogicalRelation, TableBinding};
pub use planned_pushdown::{ExecutableQuery, PlannedPushdown};
pub use pushdown_request::PushdownRequest;
pub use query_plan::{QueryPlan, RewriteContext, RewriteRule, RewriteRuleSet};
