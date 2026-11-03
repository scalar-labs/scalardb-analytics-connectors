//! Connector planning capabilities and the standard rule-based implementation.

use super::{
    ExecutableQuery, LogicalPlan, PlannedPushdown, QueryPlan, RewriteContext, RewriteRuleSet,
};
use crate::RequestError;

/// A planning proposal consumed by the Query Engine adapter.
///
/// The adapter adopts a planned result only after it can map the remote output
/// and lower the complete local plan. Rejecting a proposal leaves native state
/// unchanged and does not prevent the adapter from trying another request.
#[derive(Clone, Debug, PartialEq)]
pub enum PushdownResult {
    /// A complete decomposition and executable remote query proposed to the adapter.
    Planned { pushdown: Box<PlannedPushdown> },
    /// The Connector cannot preserve the offered semantics for this request.
    Unsupported { reason: String },
}

/// Connector-owned planning for one complete pushdown request.
pub trait ConnectorPlanner {
    /// Plans one complete request and constructs the directly executable
    /// remote query.
    ///
    /// A Data Source Session owns its planner, so this call may perform catalog
    /// or capability I/O and may cache facts for subsequent requests. Each
    /// Connector is responsible for proving and testing the equivalence of the
    /// offered plan and its remote/local decomposition.
    fn plan(&self, offered_plan: &LogicalPlan) -> Result<PushdownResult, PlanningError>;
}

/// Connector planning implemented by an ordered set of semantic rewrite rules.
///
/// Metadata is Connector-private, exists only for this planning call, and is
/// shared by the rewrite rules and executable-query construction. The common
/// implementation returns a planned result on success; a Connector that needs
/// to return `Unsupported` implements [`ConnectorPlanner`] directly.
pub trait RewriteRuleConnectorPlanner {
    /// Connector-private facts shared by rewrite rules and query generation.
    type Metadata;

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
    ) -> Result<ExecutableQuery, PlanningError>;
}

impl<T> ConnectorPlanner for T
where
    T: RewriteRuleConnectorPlanner,
{
    fn plan(&self, offered_plan: &LogicalPlan) -> Result<PushdownResult, PlanningError> {
        let metadata = self.load_metadata(offered_plan)?;
        let query_plan = self.rewrite_rules().rewrite(
            QueryPlan::initial(offered_plan),
            RewriteContext::new(offered_plan, &metadata),
        )?;
        let executable_query = self.create_executable_query(query_plan.remote_plan(), &metadata)?;
        let (remote_plan, local_plan) = query_plan.into_parts();
        let pushdown =
            PlannedPushdown::try_new(offered_plan, remote_plan, local_plan, executable_query)?;
        Ok(PushdownResult::Planned {
            pushdown: Box::new(pushdown),
        })
    }
}

/// Failure at the shared planning boundary.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanningError {
    /// The externally offered plan does not satisfy the SDK input contract.
    #[error(transparent)]
    InvalidPlan(#[from] RequestError),
    /// Connector-specific metadata loading, rewriting, or query generation failed.
    #[error("connector planning failed: {reason}")]
    Connector { reason: String },
}
