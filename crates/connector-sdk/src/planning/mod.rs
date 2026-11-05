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
    ConnectorPlanner, ExecutableQueryResult, PlanningError, PushdownResult,
    RewriteRuleConnectorPlanner,
};
pub use logical_plan::{
    BOOLEAN_FUNCTIONS_URI, BinaryOperator, COMPARISON_FUNCTIONS_URI, LOGICAL_TYPE_URI,
    LiteralValue, LogicalExpression, LogicalExpressionKind, LogicalPlan, LogicalRelation,
    REMOTE_RESULT_TYPE_URL, TableBinding, UnaryOperator,
};
pub use planned_pushdown::{ExecutableQuery, PlannedPushdown};
pub use pushdown_request::PushdownRequest;
pub use query_plan::{QueryPlan, RewriteContext, RewriteRule, RewriteRuleSet};
