//! Pushdown planning: the read object's current fields, which offered
//! filters SOQL evaluates with the same meaning, and the SOQL that
//! implements the final remote plan.
//!
//! The SDK's initial decomposition reads every base field remotely and keeps
//! the filter and projection local. Two rules then move work remotely: the
//! conjuncts of the filter that SOQL evaluates with the offered meaning,
//! leaving the others in a local filter, and a projection of plain fields
//! once no local filter remains below it.

use analytics_types::{AnalyticsField, AnalyticsSchema, AnalyticsType};
use connector_sdk::{
    BinaryOperator, Catalog, Connector, ConnectorError, ErrorKind, ExecutableQueryResult,
    ExecutionOutput, LiteralValue, LogicalExpression, LogicalExpressionKind, LogicalPlan,
    LogicalRelation, Planner, PlanningError, QueryPlan, RewriteContext, RewriteRule,
    RewriteRuleSet, Session, TableRecord, TableReference, UnaryOperator,
};

use crate::{
    SalesforceConnector, SalesforceField, SoqlRead,
    catalog::{NAMESPACE, reference},
};

/// Facts about the read object, read from describe for one planning call.
pub struct PlanningFacts {
    /// The object's API name.
    object: String,
    /// The current field at each base-schema position.
    fields: Vec<SalesforceField>,
}

/// The Salesforce planner.
pub struct SalesforcePlanner;

impl Planner<SalesforceConnector> for SalesforcePlanner {
    type Facts = PlanningFacts;

    /// Resolves the binding, `sobjects` and the object's name, and describes
    /// the object now. An object or a base field that describe no longer finds
    /// fails planning as `not found`: Salesforce hides what the user may not
    /// see, so neither is known to be gone.
    fn planning_facts(
        session: &Session<SalesforceConnector>,
        offered_plan: &LogicalPlan,
    ) -> Result<(TableReference, PlanningFacts), PlanningError> {
        let invalid_binding = || {
            PlanningError::Connector(ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "a Salesforce table binding is `sobjects` and an object name",
            ))
        };
        let (binding, base_schema) = offered_plan.table_read().ok_or_else(invalid_binding)?;
        let [namespace, object] = binding.components() else {
            return Err(invalid_binding());
        };
        if namespace != NAMESPACE {
            return Err(invalid_binding());
        }
        let table = reference(object);
        let records = <SalesforceConnector as Connector>::Catalog::table_metadata(
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
                    "the Salesforce user may not read the registered object",
                ));
            }
            Some(TableRecord::NotFound { .. }) => {
                return Err(failure(
                    ErrorKind::NotFound,
                    "the registered object is not visible, and whether it still exists cannot be told",
                ));
            }
            Some(TableRecord::DoesNotExist { .. }) | None => {
                return Err(failure(
                    ErrorKind::DoesNotExist,
                    "the registered object no longer exists",
                ));
            }
        };
        let fields = base_schema
            .fields()
            .iter()
            .map(|field| metadata.field(field.name()).cloned())
            .collect::<Option<Vec<_>>>()
            // Field-level security hides fields from describe, so a missing
            // field may still exist, as the schema check also decides.
            .ok_or_else(|| {
                failure(
                    ErrorKind::NotFound,
                    "a registered field is not in the object's current describe, which may hide fields",
                )
            })?;
        Ok((
            table,
            PlanningFacts {
                object: metadata.name,
                fields,
            },
        ))
    }

    fn rewrite_rules() -> RewriteRuleSet<PlanningFacts> {
        let mut rules = RewriteRuleSet::new();
        rules.push(FilterPushdown);
        rules.push(ProjectionPushdown);
        rules
    }

    /// Renders the final remote plan as one SOQL query over the object.
    fn generate_query(
        remote_plan: &LogicalPlan,
        facts: &PlanningFacts,
    ) -> Result<ExecutableQueryResult<SoqlRead>, PlanningError> {
        Ok(match soql(remote_plan, facts) {
            Some(read) => ExecutableQueryResult::Executable(read),
            None => ExecutableQueryResult::Unsupported {
                reason: "SOQL selects at least one field".to_owned(),
            },
        })
    }
}

fn failure(kind: ErrorKind, message: &'static str) -> PlanningError {
    PlanningError::Connector(ConnectorError::new(kind, false, message))
}

/// Moves the conjuncts of the offered filter that SOQL evaluates with the
/// offered meaning into the remote plan, and keeps the others local.
///
/// The conjuncts are those of the filter's top-level `AND`s. Because the
/// remote and local filters are joined by `AND`, the rows that pass both are
/// exactly those that pass the offered filter.
struct FilterPushdown;

impl RewriteRule<PlanningFacts> for FilterPushdown {
    fn id(&self) -> &'static str {
        "salesforce.filter-pushdown"
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        context: RewriteContext<'_, PlanningFacts>,
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
        if !matches!(input.as_ref(), LogicalRelation::RemoteResult { .. }) {
            return Ok(plan);
        }
        let (remote_conjuncts, local_conjuncts): (Vec<_>, Vec<_>) = conjuncts(condition)
            .into_iter()
            .partition(|conjunct| runs_remotely(conjunct, context.metadata));
        let Some(remote_condition) = conjunction(remote_conjuncts) else {
            return Ok(plan);
        };
        let local_filter = conjunction(local_conjuncts);

        let remote = plan.remote_plan();
        let remote_relation = LogicalRelation::Filter {
            input: Box::new(remote.relation().clone()),
            condition: remote_condition,
        };
        // The remote filter keeps its input's schema, so the local plan's
        // remote result still describes the remote output.
        let mut local_relation = input.as_ref().clone();
        if let Some(condition) = local_filter {
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

/// Moves a projection of plain fields into the remote plan when it is the
/// only local work left and selects at least one field.
struct ProjectionPushdown;

impl RewriteRule<PlanningFacts> for ProjectionPushdown {
    fn id(&self) -> &'static str {
        "salesforce.projection-pushdown"
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

/// Whether SOQL evaluates the filter condition `expression` with its offered
/// meaning.
///
/// A filter keeps the rows whose condition is true. A SOQL comparison is
/// false where the offered comparison is NULL, and with only `AND` and `OR`
/// above them, a condition is true in SOQL exactly when it is true under
/// three-valued logic. `NOT` would turn those false results true, so it stays
/// local. Each comparison is between one filterable field and a literal,
/// because SOQL cannot compare two fields.
pub(crate) fn runs_remotely(expression: &LogicalExpression, facts: &PlanningFacts) -> bool {
    match &expression.kind {
        LogicalExpressionKind::Binary {
            operator: BinaryOperator::And | BinaryOperator::Or,
            left,
            right,
        } => runs_remotely(left, facts) && runs_remotely(right, facts),
        LogicalExpressionKind::Binary {
            operator,
            left,
            right,
        } => comparison(*operator, left, right, facts).is_some(),
        LogicalExpressionKind::Unary {
            operator: UnaryOperator::IsNull | UnaryOperator::IsNotNull,
            operand,
        } => filterable_field(operand, facts).is_some(),
        LogicalExpressionKind::Unary {
            operator: UnaryOperator::Not,
            ..
        }
        | LogicalExpressionKind::Field { .. }
        | LogicalExpressionKind::Literal(_)
        | LogicalExpressionKind::UnsupportedScalarFunction { .. } => false,
    }
}

/// A comparison SOQL evaluates with the Analytics comparison, as the field,
/// the operator with the field on its left, and the literal.
///
/// It qualifies only for these Analytics types and Salesforce field types,
/// whose SOQL comparison orders and equates as the Analytics comparison does
/// (`docs/analytics-types.md#comparison`):
///
/// - `boolean` as Boolean, for equality only, since SOQL does not order
///   Booleans;
/// - `int`, `long`, `double`, and `percent` as numbers;
/// - `date` and `datetime` chronologically, with a literal SOQL can write.
///
/// Text fields compare without regard to case and order by the user's
/// locale, IDs compare 15- and 18-character forms as equal, and a currency
/// comparison in a multi-currency org converts currencies, so none of them
/// qualifies.
fn comparison<'a>(
    operator: BinaryOperator,
    left: &'a LogicalExpression,
    right: &'a LogicalExpression,
    facts: &'a PlanningFacts,
) -> Option<(&'a SalesforceField, BinaryOperator, &'a LiteralValue)> {
    let (field, operator, literal) = match (&left.kind, &right.kind) {
        (LogicalExpressionKind::Field { .. }, LogicalExpressionKind::Literal(literal)) => {
            (left, operator, literal)
        }
        (LogicalExpressionKind::Literal(literal), LogicalExpressionKind::Field { .. }) => {
            (right, flipped(operator), literal)
        }
        _ => return None,
    };
    if left.data_type != right.data_type {
        return None;
    }
    let source = filterable_field(field, facts)?;
    let ordered = !matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual);
    let qualifies = match (source.field_type.as_str(), &field.data_type, literal) {
        ("boolean", AnalyticsType::Boolean, LiteralValue::Boolean(_)) => !ordered,
        ("int", AnalyticsType::Int, LiteralValue::Int(_))
        | ("long", AnalyticsType::BigInt, LiteralValue::BigInt(_))
        | ("double" | "percent", AnalyticsType::Decimal(_), LiteralValue::Decimal { .. }) => true,
        ("date", AnalyticsType::Date, LiteralValue::Date(days)) => SOQL_DAYS.contains(days),
        ("datetime", AnalyticsType::TimestampTz, LiteralValue::TimestampTz(micros)) => {
            micros % MICROS_PER_SECOND == 0
                && i32::try_from(micros.div_euclid(MICROS_PER_DAY))
                    .is_ok_and(|days| SOQL_DAYS.contains(&days))
        }
        _ => false,
    };
    qualifies.then_some((source, operator, literal))
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

/// The current field `expression` reads, when it is a field SOQL can filter
/// on.
fn filterable_field<'a>(
    expression: &LogicalExpression,
    facts: &'a PlanningFacts,
) -> Option<&'a SalesforceField> {
    let LogicalExpressionKind::Field { position } = expression.kind else {
        return None;
    };
    facts.fields.get(position).filter(|field| field.filterable)
}

const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;

/// The days Salesforce date and date-time values can hold, 1700-01-01
/// through 4000-12-31, as days since 1970-01-01. A literal outside them is
/// left to the local plan.
const SOQL_DAYS: std::ops::RangeInclusive<i32> = -98_615..=741_807;

/// Renders a remote plan of shape `Project? -> Filter? -> Read` as SOQL.
///
/// Returns `None` for a plan that outputs no field, which SOQL cannot select.
fn soql(remote_plan: &LogicalPlan, facts: &PlanningFacts) -> Option<SoqlRead> {
    let relation = match remote_plan.relation() {
        LogicalRelation::Project { input, .. } => input.as_ref(),
        relation => relation,
    };
    let condition = match relation {
        LogicalRelation::Filter { condition, .. } => Some(condition),
        _ => None,
    };
    let mut selected: Vec<usize> = Vec::new();
    let mut fields = Vec::new();
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
                fields.push(AnalyticsField::new(
                    facts.fields[position].name.clone(),
                    output.data_type().clone(),
                    output.is_nullable(),
                ));
                selected.len() - 1
            }
        };
        columns.push(index);
    }
    if selected.is_empty() {
        return None;
    }
    let mut soql = String::from("SELECT ");
    soql.push_str(
        &selected
            .iter()
            .map(|position| facts.fields[*position].name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    soql.push_str(" FROM ");
    soql.push_str(&facts.object);
    if let Some(condition) = condition {
        soql.push_str(" WHERE ");
        render(condition, facts, &mut soql);
    }
    Some(SoqlRead {
        soql,
        fields: AnalyticsSchema::new(fields),
        columns,
    })
}

/// Renders a condition that [`runs_remotely`] accepted.
fn render(expression: &LogicalExpression, facts: &PlanningFacts, soql: &mut String) {
    match &expression.kind {
        LogicalExpressionKind::Binary {
            operator: operator @ (BinaryOperator::And | BinaryOperator::Or),
            left,
            right,
        } => {
            soql.push('(');
            render(left, facts, soql);
            soql.push_str(match operator {
                BinaryOperator::And => " AND ",
                _ => " OR ",
            });
            render(right, facts, soql);
            soql.push(')');
        }
        LogicalExpressionKind::Binary {
            operator,
            left,
            right,
        } => {
            let (field, operator, literal) = comparison(*operator, left, right, facts)
                .expect("the filter passed the remote-execution check");
            let literal = render_literal(literal);
            let name = &field.name;
            // SOQL `!=` is also true for null, which the offered comparison
            // is not.
            soql.push_str(&match operator {
                BinaryOperator::NotEqual => format!("({name} != {literal} AND {name} != null)"),
                operator => format!("{name} {} {literal}", symbol(operator)),
            });
        }
        LogicalExpressionKind::Unary { operator, operand } => {
            let field = filterable_field(operand, facts)
                .expect("the filter passed the remote-execution check");
            let test = match operator {
                UnaryOperator::IsNull => "=",
                _ => "!=",
            };
            soql.push_str(&format!("{} {test} null", field.name));
        }
        LogicalExpressionKind::Field { .. }
        | LogicalExpressionKind::Literal(_)
        | LogicalExpressionKind::UnsupportedScalarFunction { .. } => {
            unreachable!("the filter passed the remote-execution check")
        }
    }
}

fn symbol(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Equal => "=",
        BinaryOperator::NotEqual => "!=",
        BinaryOperator::LessThan => "<",
        BinaryOperator::LessThanOrEqual => "<=",
        BinaryOperator::GreaterThan => ">",
        BinaryOperator::GreaterThanOrEqual => ">=",
        BinaryOperator::And | BinaryOperator::Or => unreachable!("not a comparison"),
    }
}

/// Spells a literal that [`comparison`] accepted as a SOQL literal.
fn render_literal(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Boolean(value) => value.to_string(),
        LiteralValue::Int(value) => value.to_string(),
        LiteralValue::BigInt(value) => value.to_string(),
        LiteralValue::Decimal { value, scale, .. } => decimal(*value, *scale),
        LiteralValue::Date(days) => date(i64::from(*days)),
        LiteralValue::TimestampTz(micros) => {
            let seconds = micros.div_euclid(MICROS_PER_SECOND);
            let days = seconds.div_euclid(86_400);
            let second_of_day = seconds.rem_euclid(86_400);
            format!(
                "{}T{:02}:{:02}:{:02}Z",
                date(days),
                second_of_day / 3_600,
                second_of_day / 60 % 60,
                second_of_day % 60
            )
        }
        _ => unreachable!("the literal passed the remote-execution check"),
    }
}

/// Writes an unscaled decimal with `scale` digits after the point.
fn decimal(value: i128, scale: i8) -> String {
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { "-" } else { "" };
    let scale = usize::try_from(scale)
        .expect("a pushed decimal has the scale of a mapped field, which is not negative");
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let digits = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = digits.split_at(digits.len() - scale);
    format!("{sign}{whole}.{fraction}")
}

/// Writes days since 1970-01-01 as `YYYY-MM-DD` in the proleptic Gregorian
/// calendar.
fn date(days: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}
