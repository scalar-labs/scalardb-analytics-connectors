//! Pushdown planning: what the read table's columns read from its items, which
//! filter conjuncts a `Query`'s key condition evaluates with the offered
//! meaning, and the `Scan` or `Query` that implements the final remote plan.
//!
//! The SDK's initial decomposition reads every base column remotely and keeps
//! the filter and projection local. Two rules then move work remotely: the
//! partition key's equality with a literal, with at most one comparison of the
//! sort key, which a `Query` reads as its key condition, leaving every other
//! conjunct local; and a projection of plain columns once no local filter
//! remains below it, which the read implements as its projection expression,
//! or by reading whole items when it selects the `item` column.
//!
//! No other filter runs remotely: a DynamoDB filter expression is applied
//! after items are read, so it saves no read capacity, and it treats a
//! missing attribute as false rather than NULL.

use analytics_types::{AnalyticsField, AnalyticsType};
use connector_sdk::{
    BinaryOperator, Catalog, Connector, ConnectorError, ErrorKind, ExecutableQueryResult,
    ExecutionOutput, LiteralValue, LogicalExpression, LogicalExpressionKind, LogicalPlan,
    LogicalRelation, Planner, PlanningError, QueryPlan, RewriteContext, RewriteRule,
    RewriteRuleSet, Session, TableRecord, TableReference,
};

use crate::{
    Comparison, DynamoDbConnector, KeyComparison, KeyType, KeyValue, Projected, TableRead,
    catalog::{ITEM, NAMESPACE, reference},
};

/// The longest partition key value DynamoDB accepts, in bytes.
const PARTITION_KEY_LIMIT: usize = 2048;
/// The longest sort key value DynamoDB accepts, in bytes.
const SORT_KEY_LIMIT: usize = 1024;

/// Facts about the read table, read from `DescribeTable` for one planning
/// call.
pub struct PlanningFacts {
    /// The table's name.
    table: String,
    /// What the column at each base-schema position reads from an item.
    columns: Vec<Projected>,
    /// The name of the table's partition key.
    partition_key: Option<String>,
    /// The name of the table's sort key, if it has one.
    sort_key: Option<String>,
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
        let key = |key_type| {
            metadata
                .keys()
                .find(|key| key.key_type == key_type)
                .map(|key| key.attribute_name.clone())
        };
        let (partition_key, sort_key) = (key(KeyType::Hash), key(KeyType::Range));
        Ok((
            table,
            PlanningFacts {
                table: metadata.name,
                columns,
                partition_key,
                sort_key,
            },
        ))
    }

    fn rewrite_rules() -> RewriteRuleSet<PlanningFacts> {
        let mut rules = RewriteRuleSet::new();
        rules.push(KeyConditionPushdown);
        rules.push(ProjectionPushdown);
        rules
    }

    /// Renders the final remote plan as one `Scan` of the table, or one
    /// `Query` when it filters by a key condition.
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

/// Moves the partition key's equality with a literal, and at most one
/// comparison of the sort key with a literal, from the offered filter into
/// the remote plan, which a `Query` reads as its key condition; the other
/// conjuncts stay local.
///
/// The conjuncts are those of the filter's top-level `AND`s, so the rows
/// that pass both the key condition and the local filter are exactly those
/// that pass the offered filter. The first qualifying conjunct of each key
/// moves; a later one stays local. Without a partition key equality nothing
/// moves, since a key condition requires one.
struct KeyConditionPushdown;

impl RewriteRule<PlanningFacts> for KeyConditionPushdown {
    fn id(&self) -> &'static str {
        "dynamodb.key-condition-pushdown"
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        context: RewriteContext<'_, PlanningFacts>,
    ) -> Result<QueryPlan, PlanningError> {
        let facts = context.metadata;
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
        if !matches!(input.as_ref(), LogicalRelation::RemoteResult { .. }) {
            return Ok(plan);
        }
        let mut conjuncts = conjuncts(condition);
        let position = |key: Option<&String>, conjuncts: &[LogicalExpression]| {
            let key = key?;
            conjuncts.iter().position(|conjunct| {
                key_comparison(conjunct, facts).is_some_and(|found| &found.attribute == key)
            })
        };
        let Some(partition) = position(facts.partition_key.as_ref(), &conjuncts) else {
            return Ok(plan);
        };
        let mut remote_conjuncts = vec![conjuncts.remove(partition)];
        if let Some(sort) = position(facts.sort_key.as_ref(), &conjuncts) {
            remote_conjuncts.push(conjuncts.remove(sort));
        }
        let remote_condition =
            conjunction(remote_conjuncts).expect("the partition key's equality moves");

        let remote = plan.remote_plan();
        let remote_relation = LogicalRelation::Filter {
            input: Box::new(remote.relation().clone()),
            condition: remote_condition,
        };
        // The remote filter keeps its input's schema, so the local plan's
        // remote result still describes the remote output.
        let mut local_relation = input.as_ref().clone();
        if let Some(condition) = conjunction(conjuncts) {
            local_relation = LogicalRelation::Filter {
                input: Box::new(local_relation),
                condition,
            };
        }
        if let Some((expressions, output_mapping)) = project {
            local_relation = LogicalRelation::Project {
                input: Box::new(local_relation),
                expressions: expressions.clone(),
                output_mapping: output_mapping.clone(),
            };
        }
        // Without local work left, the local plan's root names, which may
        // rename the output, move to the remote plan.
        let (output_names, local_plan) =
            if matches!(local_relation, LogicalRelation::RemoteResult { .. }) {
                (local.output_names().to_vec(), None)
            } else {
                (
                    remote.output_names().to_vec(),
                    Some(
                        local
                            .clone()
                            .with_root(local_relation, local.output_names().to_vec()),
                    ),
                )
            };
        let remote_plan = remote.clone().with_root(remote_relation, output_names);
        Ok(QueryPlan::new(remote_plan, local_plan))
    }
}

/// The key comparison `expression` is, when a `Query`'s key condition
/// evaluates it with its offered meaning.
///
/// It is a comparison of a key column with a literal of its type: the
/// partition key for equality, the sort key for equality or order. Key
/// columns are `Text` or `Blob`, which DynamoDB compares by UTF-8 bytes and
/// unsigned bytes, as the Analytics comparison does, and every item has its
/// key attributes, so no NULL arises. DynamoDB refuses a key value that is
/// empty or longer than the key allows, so such a literal stays local.
fn key_comparison(expression: &LogicalExpression, facts: &PlanningFacts) -> Option<KeyComparison> {
    let LogicalExpressionKind::Binary {
        operator,
        left,
        right,
    } = &expression.kind
    else {
        return None;
    };
    let (position, operator, literal) = match (&left.kind, &right.kind) {
        (LogicalExpressionKind::Field { position }, LogicalExpressionKind::Literal(literal)) => {
            (*position, *operator, literal)
        }
        (LogicalExpressionKind::Literal(literal), LogicalExpressionKind::Field { position }) => {
            (*position, flipped(*operator), literal)
        }
        _ => return None,
    };
    if left.data_type != right.data_type {
        return None;
    }
    let Some(Projected::Attribute(field)) = facts.columns.get(position) else {
        return None;
    };
    let (value, length) = match (field.data_type(), literal) {
        (AnalyticsType::Text, LiteralValue::Text(text)) => (KeyValue::S(text.clone()), text.len()),
        (AnalyticsType::Blob, LiteralValue::Blob(bytes)) => {
            (KeyValue::B(bytes.clone()), bytes.len())
        }
        _ => return None,
    };
    let comparison = match operator {
        BinaryOperator::Equal => Comparison::Equal,
        BinaryOperator::LessThan => Comparison::Less,
        BinaryOperator::LessThanOrEqual => Comparison::LessOrEqual,
        BinaryOperator::GreaterThan => Comparison::Greater,
        BinaryOperator::GreaterThanOrEqual => Comparison::GreaterOrEqual,
        BinaryOperator::NotEqual | BinaryOperator::And | BinaryOperator::Or => return None,
    };
    let limit = if facts.partition_key.as_deref() == Some(field.name()) {
        if comparison != Comparison::Equal {
            return None;
        }
        PARTITION_KEY_LIMIT
    } else if facts.sort_key.as_deref() == Some(field.name()) {
        SORT_KEY_LIMIT
    } else {
        return None;
    };
    (1..=limit).contains(&length).then(|| KeyComparison {
        attribute: field.name().to_owned(),
        comparison,
        value,
    })
}

/// The operator that compares the same operands with their sides swapped.
fn flipped(operator: BinaryOperator) -> BinaryOperator {
    match operator {
        BinaryOperator::LessThan => BinaryOperator::GreaterThan,
        BinaryOperator::LessThanOrEqual => BinaryOperator::GreaterThanOrEqual,
        BinaryOperator::GreaterThan => BinaryOperator::LessThan,
        BinaryOperator::GreaterThanOrEqual => BinaryOperator::LessThanOrEqual,
        other => other,
    }
}

/// The operands of `expression`'s top-level `AND`s, left to right.
fn conjuncts(expression: &LogicalExpression) -> Vec<LogicalExpression> {
    match &expression.kind {
        LogicalExpressionKind::Binary {
            operator: BinaryOperator::And,
            left,
            right,
        } => {
            let mut conjuncts = self::conjuncts(left);
            conjuncts.extend(self::conjuncts(right));
            conjuncts
        }
        _ => vec![expression.clone()],
    }
}

/// Joins conditions with `AND`, left-deep; `None` when there are none.
fn conjunction(conditions: Vec<LogicalExpression>) -> Option<LogicalExpression> {
    conditions
        .into_iter()
        .reduce(|left, right| LogicalExpression {
            data_type: AnalyticsType::Boolean,
            nullable: left.nullable || right.nullable,
            kind: LogicalExpressionKind::Binary {
                operator: BinaryOperator::And,
                left: Box::new(left),
                right: Box::new(right),
            },
        })
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

/// Renders a remote plan of shape `Project? -> Filter? -> Read` as a `Scan`,
/// or as a `Query` whose key condition is the filter.
///
/// Returns `None` for a plan that outputs no column, which a projection
/// expression cannot name.
fn read(remote_plan: &LogicalPlan, facts: &PlanningFacts) -> Option<TableRead> {
    let relation = match remote_plan.relation() {
        LogicalRelation::Project { input, .. } => input.as_ref(),
        relation => relation,
    };
    let key_condition = match relation {
        LogicalRelation::Filter { condition, .. } => conjuncts(condition)
            .iter()
            .map(|conjunct| {
                key_comparison(conjunct, facts)
                    .expect("only key comparisons move into the remote plan")
            })
            .collect(),
        _ => Vec::new(),
    };
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
        key_condition,
        values,
        columns,
    })
}
