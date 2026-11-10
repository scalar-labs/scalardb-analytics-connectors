//! Connector planning capabilities and the standard rule-based implementation.

use super::{LogicalPlan, PlannedPushdown, QueryPlan, RewriteContext, RewriteRuleSet};
use crate::{ConnectorError, RequestError};

/// A planning proposal consumed by the Query Engine adapter.
///
/// The adapter adopts a planned result only after it can map the remote output
/// and lower the complete local plan. Rejecting a proposal leaves native state
/// unchanged and does not prevent the adapter from trying another request.
///
/// This type intentionally does not implement `Debug` because a planned result
/// contains a [`PlannedPushdown`] with raw boundary plans and opaque execution
/// instructions.
#[derive(Clone, PartialEq)]
pub enum PushdownResult<Q> {
    /// A complete decomposition and executable remote query proposed to the adapter.
    Planned { pushdown: Box<PlannedPushdown<Q>> },
    /// The Connector cannot preserve the offered semantics for this request.
    Unsupported { reason: String },
}

/// Result of compiling a rewritten remote plan.
///
/// This type intentionally does not implement `Debug` because its executable
/// variant contains opaque Connector-owned instructions.
#[derive(Clone, PartialEq)]
pub enum ExecutableQueryResult<Q> {
    /// The remote plan can be executed with these instructions.
    Executable(Q),
    /// The Connector cannot preserve the remote plan's semantics.
    Unsupported { reason: String },
}

/// Connector-owned planning for one complete pushdown request.
pub trait ConnectorPlanner {
    /// Connector-owned instructions shared with this Connector's executor.
    type Query;

    /// Plans one complete request and constructs the directly executable
    /// remote query.
    ///
    /// A Data Source Session owns its planner, so this call may perform catalog
    /// or capability I/O and may cache facts for subsequent requests. Each
    /// Connector is responsible for proving and testing the equivalence of the
    /// offered plan and its remote/local decomposition.
    fn plan(
        &self,
        offered_plan: &LogicalPlan,
    ) -> Result<PushdownResult<Self::Query>, PlanningError>;
}

/// Connector planning implemented by an ordered set of semantic rewrite rules.
///
/// Metadata is Connector-private, exists only for this planning call, and is
/// shared by the rewrite rules and executable-query construction. The common
/// implementation preserves both successful compilation and an unsupported
/// semantic outcome.
pub trait RewriteRuleConnectorPlanner {
    /// Connector-private facts shared by rewrite rules and query generation.
    type Metadata;
    /// Connector-owned instructions shared with this Connector's executor.
    type Query;

    /// Loads the Data Source facts required to plan this validated request.
    ///
    /// This hook runs once before any rewrite rule and may perform planning I/O.
    fn load_metadata(&self, offered_plan: &LogicalPlan) -> Result<Self::Metadata, PlanningError>;

    /// Returns the ordered rules used to rewrite the initial decomposition.
    fn rewrite_rules(&self) -> &RewriteRuleSet<Self::Metadata>;

    /// Compiles the final remote logical plan into directly executable instructions.
    fn create_executable_query(
        &self,
        remote_plan: &LogicalPlan,
        metadata: &Self::Metadata,
    ) -> Result<ExecutableQueryResult<Self::Query>, PlanningError>;
}

impl<T> ConnectorPlanner for T
where
    T: RewriteRuleConnectorPlanner,
{
    type Query = T::Query;

    fn plan(
        &self,
        offered_plan: &LogicalPlan,
    ) -> Result<PushdownResult<Self::Query>, PlanningError> {
        let metadata = self.load_metadata(offered_plan)?;
        let query_plan = self.rewrite_rules().rewrite(
            QueryPlan::initial(offered_plan),
            RewriteContext::new(offered_plan, &metadata),
        )?;
        let query = match self.create_executable_query(query_plan.remote_plan(), &metadata)? {
            ExecutableQueryResult::Executable(query) => query,
            ExecutableQueryResult::Unsupported { reason } => {
                return Ok(PushdownResult::Unsupported { reason });
            }
        };
        let (remote_plan, local_plan) = query_plan.into_parts();
        let pushdown = PlannedPushdown::try_new(offered_plan, remote_plan, local_plan, query)?;
        Ok(PushdownResult::Planned {
            pushdown: Box::new(pushdown),
        })
    }
}

/// Failure at the shared planning boundary.
#[derive(Clone, Debug, thiserror::Error, PartialEq)]
pub enum PlanningError {
    /// The externally offered plan does not satisfy the SDK input contract.
    #[error(transparent)]
    InvalidPlan(#[from] RequestError),
    /// Connector-specific metadata loading, rewriting, or query generation failed.
    ///
    /// The error keeps its kind, retryability, and Data Source details so that
    /// the connector API returns them to the caller of `plan_pushdown`.
    #[error(transparent)]
    Connector(#[from] ConnectorError),
}
