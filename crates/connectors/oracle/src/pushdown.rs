//! Pushdown planning for Oracle: the read table, and which offered filters
//! Oracle evaluates with the same meaning.

use std::ops::RangeInclusive;

use analytics_types::AnalyticsType;
use connector_sdk::{
    BinaryOperator, ConnectorError, ErrorKind, LiteralValue, LogicalExpression,
    LogicalExpressionKind, LogicalPlan, PlanningError, TableReference, UnaryOperator,
};
use connector_sql::literal::MICROS_PER_DAY;

/// Resolves the binding to the table the remote query reads.
///
/// The binding is the table's owner and name. Planning reads nothing from
/// Oracle: whether a comparison runs there depends only on the Analytics types
/// in the offered plan, and the read's schema check confirms the table and
/// columns before any row returns.
pub(crate) fn planning_facts(
    offered_plan: &LogicalPlan,
) -> Result<(TableReference, ()), PlanningError> {
    let invalid_binding = || {
        PlanningError::Connector(ConnectorError::new(
            ErrorKind::InvalidRequest,
            false,
            "an Oracle table binding is an owner and a table name",
        ))
    };
    let (binding, _) = offered_plan.table_read().ok_or_else(invalid_binding)?;
    let [owner, name] = binding.components() else {
        return Err(invalid_binding());
    };
    let table =
        TableReference::new(vec![owner.clone(), name.clone()]).map_err(|_| invalid_binding())?;
    Ok((table, ()))
}

/// Whether Oracle evaluates the filter condition `expression` with its
/// offered meaning.
///
/// A condition is a comparison or null test, or AND, OR, and NOT over
/// conditions, which follow SQL three-valued logic in both places. A bare
/// Boolean column or literal is not evaluated remotely, because no comparison
/// of `Boolean` is.
pub(crate) fn runs_remotely(expression: &LogicalExpression) -> bool {
    match &expression.kind {
        LogicalExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => runs_remotely(operand),
        LogicalExpressionKind::Unary { operand, .. } => operand_runs_remotely(operand),
        LogicalExpressionKind::Binary {
            operator: BinaryOperator::And | BinaryOperator::Or,
            left,
            right,
        } => runs_remotely(left) && runs_remotely(right),
        LogicalExpressionKind::Binary { left, right, .. } => comparison_runs_remotely(left, right),
        LogicalExpressionKind::Field { .. }
        | LogicalExpressionKind::Literal(_)
        | LogicalExpressionKind::UnsupportedScalarFunction { .. } => false,
    }
}

/// Whether a comparison or null-test operand runs at Oracle: a column, or a
/// literal Oracle reads as the same value.
fn operand_runs_remotely(operand: &LogicalExpression) -> bool {
    match &operand.kind {
        LogicalExpressionKind::Field { .. } => true,
        LogicalExpressionKind::Literal(literal) => literal_runs_remotely(literal),
        _ => false,
    }
}

/// Whether Oracle compares the two operands as the Analytics comparison does
/// (`docs/analytics-types.md#comparison`):
///
/// - `NUMBER` is compared by its exact decimal value;
/// - `DATE` and `TIMESTAMP` are compared by date and time, and `TIMESTAMP
///   WITH TIME ZONE` and `WITH LOCAL TIME ZONE` by instant;
/// - text comparisons follow `CHAR` blank padding, the collation, and the
///   NLS settings, so they stay local, and floating-point, binary, and
///   `BOOLEAN` comparisons are not yet evaluated remotely.
fn comparison_runs_remotely(left: &LogicalExpression, right: &LogicalExpression) -> bool {
    operand_runs_remotely(left)
        && operand_runs_remotely(right)
        && left.data_type == right.data_type
        && matches!(
            left.data_type,
            AnalyticsType::Decimal(_) | AnalyticsType::Timestamp | AnalyticsType::TimestampTz
        )
}

/// The days of the years 1 to 9999, which an ANSI date or timestamp literal
/// can write, from 1970-01-01.
const LITERAL_DAYS: RangeInclusive<i64> = -719_162..=2_932_896;

/// The days 1582-10-05 to 1582-10-14, from 1970-01-01, which Oracle's
/// calendar skips from the Julian to the Gregorian calendar: Oracle reads a
/// literal of one of them as 1582-10-15.
const SKIPPED_DAYS: RangeInclusive<i64> = -141_437..=-141_428;

/// Whether Oracle reads the literal as the same value.
///
/// An ANSI timestamp literal is written from year 1 to year 9999 with the
/// fields of the proleptic Gregorian date, which Oracle reads as the date of
/// the same fields, except on the days its calendar skips. A decimal keeps
/// its exact value, because it compares only with a `NUMBER` column of its
/// own precision and scale, which holds at most 38 digits.
fn literal_runs_remotely(literal: &LiteralValue) -> bool {
    match literal {
        LiteralValue::Timestamp(micros) | LiteralValue::TimestampTz(micros) => {
            let day = micros.div_euclid(MICROS_PER_DAY);
            LITERAL_DAYS.contains(&day) && !SKIPPED_DAYS.contains(&day)
        }
        LiteralValue::Decimal { .. } | LiteralValue::Null { .. } => true,
        _ => false,
    }
}
