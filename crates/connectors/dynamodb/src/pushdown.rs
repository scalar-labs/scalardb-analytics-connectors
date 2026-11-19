//! Pushdown planning: what the read table's columns read from its items, and
//! the `Scan` that implements the final remote plan.
//!
//! The SDK's initial decomposition reads every base column remotely and keeps
//! the filter and projection local. One rule then moves a projection of plain
//! columns remotely once no local filter remains below it, which the `Scan`
//! implements as its projection expression, or by reading whole items when it
//! selects the `item` column. Filters stay local: a DynamoDB
//! filter expression is applied after items are read, so it saves no read
//! capacity, and it treats a missing attribute as false rather than NULL.

use analytics_types::AnalyticsField;
use connector_sdk::{
    Catalog, Connector, ConnectorError, ErrorKind, ExecutableQueryResult, ExecutionOutput,
    LogicalExpressionKind, LogicalPlan, LogicalRelation, Planner, PlanningError, QueryPlan,
    RewriteContext, RewriteRule, RewriteRuleSet, Session, TableRecord, TableReference,
};

use crate::{
    DynamoDbConnector, Projected, TableRead,
    catalog::{ITEM, NAMESPACE, reference},
};

/// Facts about the read table, read from `DescribeTable` for one planning
/// call.
pub struct PlanningFacts {
    /// The table's name.
    table: String,
    /// What the column at each base-schema position reads from an item.
    columns: Vec<Projected>,
}

/// The DynamoDB planner.
pub struct DynamoDbPlanner;

impl Planner<DynamoDbConnector> for DynamoDbPlanner {
    type Facts = PlanningFacts;

    /// Resolves the binding, the namespace and the table's name, and
    /// describes the table now. A base column reads the key attribute of its
    /// name, or else the whole item when it is the `item` column; any other
    /// fails planning as `does not exist`, since `DescribeTable` hides no key
    /// attribute.
    fn planning_facts(
        session: &Session<DynamoDbConnector>,
        offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, PlanningFacts), PlanningError> {
        let invalid_binding = || {
            PlanningError::Connector(ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "a DynamoDB table binding is the namespace and a table name",
            ))
        };
        let (binding, base_schema) = offered_plan.table_read().ok_or_else(invalid_binding)?;
        let [namespace, name] = binding.components() else {
            return Err(invalid_binding());
        };
        if namespace != NAMESPACE {
            return Err(invalid_binding());
        }
        let table = reference(name);
        let records = <DynamoDbConnector as Connector>::Catalog::table_metadata(
            session,
            std::slice::from_ref(&table),
        )
        .and_then(ExecutionOutput::read_to_end)
        .map_err(PlanningError::Connector)?;
        let metadata = match records.into_iter().next() {
            Some(TableRecord::Read { metadata, .. }) => metadata,
            Some(TableRecord::PermissionDenied { .. }) => {
                return Err(failure(
                    ErrorKind::PermissionDenied,
                    "the credentials may not describe the registered table",
                ));
            }
            Some(TableRecord::NotFound { .. }) => {
                return Err(failure(
                    ErrorKind::NotFound,
                    "the registered table cannot be described, and whether it still exists cannot be told",
                ));
            }
            Some(TableRecord::DoesNotExist { .. }) | None => {
                return Err(failure(
                    ErrorKind::DoesNotExist,
                    "the registered table no longer exists",
                ));
            }
        };
        let columns = base_schema
            .fields()
            .iter()
            .map(|field| {
                if metadata
                    .keys()
                    .any(|key| key.attribute_name == field.name())
                {
                    Some(Projected::Attribute(field.clone()))
                } else {
                    (field.name() == ITEM).then_some(Projected::Item)
                }
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                failure(
                    ErrorKind::DoesNotExist,
                    "a registered column is neither a key attribute of the table nor `item`",
                )
            })?;
        Ok((
            table,
            PlanningFacts {
                table: metadata.name,
                columns,
            },
        ))
    }

    fn rewrite_rules() -> RewriteRuleSet<PlanningFacts> {
        let mut rules = RewriteRuleSet::new();
        rules.push(ProjectionPushdown);
        rules
    }

    /// Renders the final remote plan as one `Scan` of the table.
    fn generate_query(
        remote_plan: &LogicalPlan,
        facts: &PlanningFacts,
    ) -> Result<ExecutableQueryResult<TableRead>, PlanningError> {
        Ok(match read(remote_plan, facts) {
            Some(read) => ExecutableQueryResult::Executable(read),
            None => ExecutableQueryResult::Unsupported {
                reason: "a DynamoDB read outputs at least one column".to_owned(),
            },
        })
    }
}

fn failure(kind: ErrorKind, message: &'static str) -> PlanningError {
    PlanningError::Connector(ConnectorError::new(kind, false, message))
}

/// Moves a projection of plain columns into the remote plan when it is the
/// only local work left and selects at least one column.
struct ProjectionPushdown;

impl RewriteRule<PlanningFacts> for ProjectionPushdown {
    fn id(&self) -> &'static str {
        "dynamodb.projection-pushdown"
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        _context: RewriteContext<'_, PlanningFacts>,
    ) -> Result<QueryPlan, PlanningError> {
        let Some(local) = plan.local_plan() else {
            return Ok(plan);
        };
        let LogicalRelation::Project {
            input,
            expressions,
            output_mapping,
        } = local.relation()
        else {
            return Ok(plan);
        };
        let plain = expressions
            .iter()
            .all(|expression| matches!(expression.kind, LogicalExpressionKind::Field { .. }));
        if !plain
            || output_mapping.is_empty()
            || !matches!(input.as_ref(), LogicalRelation::RemoteResult { .. })
        {
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

/// Renders a remote plan of shape `Project? -> Read` as a `Scan`.
///
/// Returns `None` for a plan that outputs no column, which a projection
/// expression cannot name.
fn read(remote_plan: &LogicalPlan, facts: &PlanningFacts) -> Option<TableRead> {
    let mut selected: Vec<usize> = Vec::new();
    let mut values = Vec::new();
    let mut columns = Vec::new();
    for (position, output) in remote_plan
        .output_field_positions()
        .into_iter()
        .zip(remote_plan.output_schema().fields())
    {
        let index = match selected.iter().position(|known| *known == position) {
            Some(index) => index,
            None => {
                selected.push(position);
                values.push(match &facts.columns[position] {
                    Projected::Attribute(attribute) => Projected::Attribute(AnalyticsField::new(
                        attribute.name(),
                        output.data_type().clone(),
                        output.is_nullable(),
                    )),
                    Projected::Item => Projected::Item,
                });
                selected.len() - 1
            }
        };
        columns.push(index);
    }
    if selected.is_empty() {
        return None;
    }
    Some(TableRead {
        table: facts.table.clone(),
        values,
        columns,
    })
}
