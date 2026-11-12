//! Planning offered reads for a SQL-speaking Connector.
//!
//! The SDK's initial decomposition reads the whole base table remotely and
//! keeps the offered filter and projection local. Two rules then move work
//! remotely: the whole filter, when the Connector accepts its condition, and a
//! projection of plain columns once no local filter remains below it. A filter
//! moves whole or not at all; splitting conjuncts is left to a later rule.

use std::marker::PhantomData;

use connector_sdk::{
    ExecutableQueryResult, LogicalExpressionKind, LogicalPlan, LogicalRelation, Planner,
    PlanningError, QueryPlan, RewriteContext, RewriteRule, RewriteRuleSet, Session, TableReference,
};

use crate::{SqlConnector, SqlRead, translate};

/// What one planning call knows about the read table.
pub struct PlanningMetadata<C: SqlConnector> {
    table: TableReference,
    facts: C::PlanningFacts,
}

/// The planner of a SQL-speaking Connector: the shared filter and projection
/// rules over the Connector's semantic decision, and SQL generation through
/// its dialect.
pub struct SqlPlanner<C>(PhantomData<fn() -> C>);

impl<C: SqlConnector> Planner<C> for SqlPlanner<C> {
    type Facts = PlanningMetadata<C>;

    fn planning_facts(
        session: &Session<C>,
        offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, PlanningMetadata<C>), PlanningError> {
        let (table, facts) = C::planning_facts(session, offered_plan)?;
        Ok((table.clone(), PlanningMetadata { table, facts }))
    }

    fn rewrite_rules() -> RewriteRuleSet<PlanningMetadata<C>> {
        let mut rules = RewriteRuleSet::new();
        rules.push(FilterPushdown::<C>(PhantomData));
        rules.push(ProjectionPushdown);
        rules
    }

    /// Renders the final remote plan as one statement over the table's
    /// physical relation, whose names are the table reference's names and
    /// whose columns are the base schema's field names.
    ///
    /// A plan the dialect cannot render is `Unsupported`.
    fn generate_query(
        remote_plan: &LogicalPlan,
        metadata: &PlanningMetadata<C>,
    ) -> Result<ExecutableQueryResult<SqlRead>, PlanningError> {
        let Some((_, base_schema)) = remote_plan.table_read() else {
            return Ok(ExecutableQueryResult::Unsupported {
                reason: "the remote plan reads no table".to_owned(),
            });
        };
        let columns: Vec<String> = base_schema
            .fields()
            .iter()
            .map(|field| field.name().to_owned())
            .collect();
        Ok(
            match translate::<C>(remote_plan, metadata.table.names(), &columns) {
                Ok(rendered) => ExecutableQueryResult::Executable(SqlRead { sql: rendered.sql }),
                Err(error) => ExecutableQueryResult::Unsupported {
                    reason: error.to_string(),
                },
            },
        )
    }
}

/// Moves the whole offered filter into the remote plan when Connector `C`
/// accepts its condition; otherwise leaves it local.
struct FilterPushdown<C>(PhantomData<fn() -> C>);

impl<C: SqlConnector> RewriteRule<PlanningMetadata<C>> for FilterPushdown<C> {
    fn id(&self) -> &'static str {
        "sql.filter-pushdown"
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        context: RewriteContext<'_, PlanningMetadata<C>>,
    ) -> Result<QueryPlan, PlanningError> {
        let Some(local) = plan.local_plan() else {
            return Ok(plan);
        };
        let (project, filter) = match local.relation() {
            LogicalRelation::Project {
                input,
                expressions,
                output_mapping,
            } => (Some((expressions, output_mapping)), input.as_ref()),
            relation => (None, relation),
        };
        let LogicalRelation::Filter { input, condition } = filter else {
            return Ok(plan);
        };
        if !matches!(input.as_ref(), LogicalRelation::RemoteResult { .. })
            || !C::runs_remotely(condition, &context.metadata.facts)
        {
            return Ok(plan);
        }

        let remote = plan.remote_plan();
        let remote_relation = LogicalRelation::Filter {
            input: Box::new(remote.relation().clone()),
            condition: condition.clone(),
        };
        // Without a local projection left, the local plan's root names, which
        // may rename the output, move to the remote plan.
        let output_names = match project {
            Some(_) => remote.output_names(),
            None => local.output_names(),
        };
        let remote_plan = remote
            .clone()
            .with_root(remote_relation, output_names.to_vec());
        let local_plan = project.map(|(expressions, output_mapping)| {
            local.clone().with_root(
                LogicalRelation::Project {
                    input: input.clone(),
                    expressions: expressions.clone(),
                    output_mapping: output_mapping.clone(),
                },
                local.output_names().to_vec(),
            )
        });
        Ok(QueryPlan::new(remote_plan, local_plan))
    }
}

/// Moves a projection of plain columns into the remote plan when it is the
/// only local work left.
struct ProjectionPushdown;

impl<M> RewriteRule<M> for ProjectionPushdown {
    fn id(&self) -> &'static str {
        "sql.projection-pushdown"
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        _context: RewriteContext<'_, M>,
    ) -> Result<QueryPlan, PlanningError> {
        let Some(local) = plan.local_plan() else {
            return Ok(plan);
        };
        let (input, expressions, output_mapping) = match local.relation() {
            LogicalRelation::Project {
                input,
                expressions,
                output_mapping,
            } => (input, expressions, output_mapping),
            _ => return Ok(plan),
        };
        let plain = expressions
            .iter()
            .all(|expression| matches!(expression.kind, LogicalExpressionKind::Field { .. }));
        if !plain || !matches!(input.as_ref(), LogicalRelation::RemoteResult { .. }) {
            return Ok(plan);
        }
        let remote = plan.remote_plan();
        let remote_plan = remote.clone().with_root(
            LogicalRelation::Project {
                input: Box::new(remote.relation().clone()),
                expressions: expressions.clone(),
                output_mapping: output_mapping.clone(),
            },
            local.output_names().to_vec(),
        );
        Ok(QueryPlan::new(remote_plan, None))
    }
}
