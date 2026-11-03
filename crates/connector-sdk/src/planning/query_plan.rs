//! A complete remote/local query plan and its semantic rewrites.

use super::{LogicalPlan, LogicalRelation, PlanningError};

/// Connector-owned planning inputs shared by every rewrite rule.
pub struct RewriteContext<'a, M> {
    /// Complete logical plan supplied by the Query Engine.
    pub offered_plan: &'a LogicalPlan,
    /// Connector-private facts loaded for this planning call.
    pub metadata: &'a M,
}

impl<M> Copy for RewriteContext<'_, M> {}

impl<M> Clone for RewriteContext<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, M> RewriteContext<'a, M> {
    /// Combines common request state with Connector-private planning metadata.
    pub(crate) fn new(offered_plan: &'a LogicalPlan, metadata: &'a M) -> Self {
        Self {
            offered_plan,
            metadata,
        }
    }
}

/// One complete remote/local query plan.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryPlan {
    remote_plan: LogicalPlan,
    local_plan: Option<LogicalPlan>,
}

impl QueryPlan {
    /// Creates one complete remote/local decomposition.
    pub fn new(remote_plan: LogicalPlan, local_plan: Option<LogicalPlan>) -> Self {
        Self {
            remote_plan,
            local_plan,
        }
    }

    pub fn remote_plan(&self) -> &LogicalPlan {
        &self.remote_plan
    }

    pub fn local_plan(&self) -> Option<&LogicalPlan> {
        self.local_plan.as_ref()
    }

    /// Splits the decomposition into the plans consumed by query generation.
    pub fn into_parts(self) -> (LogicalPlan, Option<LogicalPlan>) {
        (self.remote_plan, self.local_plan)
    }

    /// Creates the complete plan used before any pushdown rule is applied.
    ///
    /// Every operator remains in the local plan. Only its terminal `Read`
    /// is replaced with `RemoteResult`, and that read becomes the initial
    /// remote plan. Rewrite rules can then move work across this boundary.
    pub fn initial(offered_plan: &LogicalPlan) -> Self {
        let (local_relation, remote_read) =
            replace_read_with_remote_result(offered_plan.relation().clone());
        let LogicalRelation::Read {
            substrait_schema, ..
        } = &remote_read
        else {
            unreachable!("the initial remote relation is the validated read")
        };
        let remote_names = substrait_schema.names.clone();
        let local_plan = offered_plan
            .clone()
            .with_root(local_relation, offered_plan.output_names().to_vec());
        let remote_plan = offered_plan.clone().with_root(remote_read, remote_names);

        Self::new(remote_plan, Some(local_plan))
    }
}

/// One deterministic, Connector-owned semantic rewrite.
pub trait RewriteRule<M>: Send + Sync {
    /// Stable diagnostic name for the rule.
    fn id(&self) -> &'static str;

    /// Converts one complete query plan into another complete query plan.
    ///
    /// The rule decides the semantic tree transformation. `LogicalPlan`
    /// construction owns plan-level derived state such as output schemas.
    /// Returning the input unchanged is the rule's no-op result.
    fn rewrite(
        &self,
        plan: QueryPlan,
        context: RewriteContext<'_, M>,
    ) -> Result<QueryPlan, PlanningError>;
}

/// Ordered composition of Connector rewrite rules.
pub struct RewriteRuleSet<M> {
    rules: Vec<Box<dyn RewriteRule<M>>>,
}

impl<M> Default for RewriteRuleSet<M> {
    fn default() -> Self {
        Self { rules: Vec::new() }
    }
}

impl<M> RewriteRuleSet<M> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a rule to the ordered rewrite sequence.
    pub fn push<R>(&mut self, rule: R)
    where
        R: RewriteRule<M> + 'static,
    {
        self.rules.push(Box::new(rule));
    }

    /// Applies every rule in insertion order to one evolving complete plan.
    ///
    /// An unchanged return value is a no-op and does not stop later rules. The
    /// first error stops the sequence and is returned to the caller.
    pub(crate) fn rewrite(
        &self,
        mut plan: QueryPlan,
        context: RewriteContext<'_, M>,
    ) -> Result<QueryPlan, PlanningError> {
        for rule in &self.rules {
            plan = rule.rewrite(plan, context)?;
        }
        Ok(plan)
    }
}

/// Returns a new local tree with `RemoteResult` and the removed remote read.
///
/// This establishes the initial decomposition: the remote side performs only
/// the read, while every operator above it remains on the local side.
fn replace_read_with_remote_result(
    relation: LogicalRelation,
) -> (LogicalRelation, LogicalRelation) {
    match relation {
        LogicalRelation::Read {
            substrait_schema,
            base_schema,
            table_binding,
        } => {
            let local = LogicalRelation::RemoteResult {
                substrait_schema: substrait_schema.clone(),
                output_schema: base_schema.clone(),
            };
            let remote = LogicalRelation::Read {
                substrait_schema,
                base_schema,
                table_binding,
            };
            (local, remote)
        }
        LogicalRelation::Filter { input, condition } => {
            let (input, remote) = replace_read_with_remote_result(*input);
            (
                LogicalRelation::Filter {
                    input: Box::new(input),
                    condition,
                },
                remote,
            )
        }
        LogicalRelation::Project {
            input,
            expressions,
            output_mapping,
        } => {
            let (input, remote) = replace_read_with_remote_result(*input);
            (
                LogicalRelation::Project {
                    input: Box::new(input),
                    expressions,
                    output_mapping,
                },
                remote,
            )
        }
        LogicalRelation::RemoteResult { .. } => {
            unreachable!("validated offered plans terminate in a read")
        }
    }
}
