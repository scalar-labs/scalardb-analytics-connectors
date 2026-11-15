//! Pushdown planning for MySQL: the read table's current facts, and which
//! offered filters MySQL evaluates with the same meaning.

use analytics_types::AnalyticsType;
use connector_sdk::{
    BinaryOperator, Catalog, Connector, ConnectorError, ErrorKind, ExecutionOutput, LiteralValue,
    LogicalExpression, LogicalExpressionKind, LogicalPlan, PlanningError, Session, TableRecord,
    TableReference,
};

use crate::MysqlConnector;

/// Facts about the read table, loaded from `information_schema` for one
/// planning call.
pub struct PlanningFacts {
    /// The current facts of the column at each base-schema position.
    columns: Vec<ColumnFacts>,
}

/// What planning needs to know about one current column.
struct ColumnFacts {
    /// `DATA_TYPE`, such as `varchar` or `char`.
    data_type: String,
    /// `COLLATION_NAME`, for a character column.
    collation: Option<String>,
}

/// The collation under which `utf8mb4` strings compare as their bytes: binary
/// and `NO PAD`, so trailing spaces count.
const BYTE_COLLATION: &str = "utf8mb4_0900_bin";

/// Resolves the binding to a table and reads its current columns.
///
/// The binding is the table's database and name. A table or base column that
/// no longer exists is `does not exist`, a stale registration; a table, or a
/// column of a table that may hide columns, that `information_schema` does
/// not show is `not found`, as in the SDK's schema check.
pub(crate) fn planning_facts(
    session: &Session<MysqlConnector>,
    offered_plan: &LogicalPlan,
) -> Result<(TableReference, PlanningFacts), PlanningError> {
    let invalid_binding = || {
        PlanningError::Connector(ConnectorError::new(
            ErrorKind::InvalidRequest,
            false,
            "a MySQL table binding is a database and a table name",
        ))
    };
    let (binding, base_schema) = offered_plan.table_read().ok_or_else(invalid_binding)?;
    let [schema, name] = binding.components() else {
        return Err(invalid_binding());
    };
    let table =
        TableReference::new(vec![schema.clone(), name.clone()]).map_err(|_| invalid_binding())?;
    let records = <MysqlConnector as Connector>::Catalog::table_metadata(
        session,
        std::slice::from_ref(&table),
    )
    .and_then(ExecutionOutput::read_to_end)
    .map_err(PlanningError::Connector)?;
    let metadata = match records.into_iter().next() {
        Some(TableRecord::Read { metadata, .. }) => metadata,
        Some(TableRecord::NotFound { .. }) => {
            return Err(PlanningError::Connector(not_found(
                "the registered table is not found; the account may not see it",
            )));
        }
        Some(TableRecord::PermissionDenied { .. }) => {
            return Err(PlanningError::Connector(ConnectorError::new(
                ErrorKind::PermissionDenied,
                false,
                "the account may not read the registered table",
            )));
        }
        Some(TableRecord::DoesNotExist { .. }) | None => {
            return Err(PlanningError::Connector(does_not_exist(
                "the registered table no longer exists at the data source",
            )));
        }
    };
    let columns = base_schema
        .fields()
        .iter()
        .map(|field| {
            let column = metadata
                .columns
                .iter()
                .find(|column| column.name == field.name())?;
            Some(ColumnFacts {
                data_type: column.data_type.clone(),
                collation: column.collation.clone(),
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            PlanningError::Connector(if metadata.columns_may_be_hidden {
                not_found("a registered column is not found; the account may not see it")
            } else {
                does_not_exist("a registered column no longer exists at the data source")
            })
        })?;
    Ok((table, PlanningFacts { columns }))
}

fn does_not_exist(message: &'static str) -> ConnectorError {
    ConnectorError::new(ErrorKind::DoesNotExist, false, message)
}

fn not_found(message: &'static str) -> ConnectorError {
    ConnectorError::new(ErrorKind::NotFound, false, message)
}

/// Whether MySQL evaluates `expression` with its offered meaning.
///
/// Boolean composition and null tests follow SQL three-valued logic in both
/// places. A comparison qualifies only between operands of one type whose
/// MySQL comparison agrees with the Analytics comparison
/// (`docs/analytics-types.md#comparison`):
///
/// - Booleans, signed and unsigned integers, decimals, dates, and timestamps
///   order identically; a Boolean column is a `tinyint(1)` or `bit(1)` that
///   MySQL compares with `TRUE` and `FALSE` as 1 and 0, and a row holding
///   another value, which no Boolean can carry, fails the read that returns
///   it; the session time zone is UTC, so `TIMESTAMP` columns compare as UTC
///   instants;
/// - text compares as bytes only on `varchar` and text columns under
///   `utf8mb4_0900_bin`; other collations fold case, accents, or trailing
///   spaces, and `char` columns drop trailing spaces;
/// - enums, years, floating-point values, and binary strings stay local.
pub(crate) fn runs_remotely(expression: &LogicalExpression, facts: &PlanningFacts) -> bool {
    match &expression.kind {
        LogicalExpressionKind::Field { .. } => true,
        LogicalExpressionKind::Literal(literal) => literal_runs_remotely(literal),
        LogicalExpressionKind::Unary { operand, .. } => runs_remotely(operand, facts),
        LogicalExpressionKind::Binary {
            operator,
            left,
            right,
        } => match operator {
            BinaryOperator::And | BinaryOperator::Or => {
                runs_remotely(left, facts) && runs_remotely(right, facts)
            }
            _ => comparison_runs_remotely(left, right, facts),
        },
        LogicalExpressionKind::UnsupportedScalarFunction { .. } => false,
    }
}

fn comparison_runs_remotely(
    left: &LogicalExpression,
    right: &LogicalExpression,
    facts: &PlanningFacts,
) -> bool {
    let operands = [left, right];
    let simple = operands.iter().all(|operand| {
        matches!(
            operand.kind,
            LogicalExpressionKind::Field { .. } | LogicalExpressionKind::Literal(_)
        ) && runs_remotely(operand, facts)
    });
    if !simple || left.data_type != right.data_type {
        return false;
    }
    match &left.data_type {
        AnalyticsType::Boolean
        | AnalyticsType::Byte
        | AnalyticsType::SmallInt
        | AnalyticsType::Int
        | AnalyticsType::BigInt
        | AnalyticsType::UnsignedByte
        | AnalyticsType::UnsignedSmallInt
        | AnalyticsType::UnsignedInt
        | AnalyticsType::UnsignedBigInt
        | AnalyticsType::Decimal(_)
        | AnalyticsType::Date
        | AnalyticsType::Timestamp
        | AnalyticsType::TimestampTz => true,
        AnalyticsType::Text => operands.iter().all(|operand| match operand.kind {
            LogicalExpressionKind::Field { position } => {
                facts.columns.get(position).is_some_and(byte_ordered_text)
            }
            _ => true,
        }),
        _ => false,
    }
}

/// Whether this text column compares and orders its values as their UTF-8
/// bytes, as the Analytics comparison does.
fn byte_ordered_text(column: &ColumnFacts) -> bool {
    column.data_type != "char" && column.collation.as_deref() == Some(BYTE_COLLATION)
}

/// The first and last day MySQL's temporal types are read for here, in days
/// since 1970-01-01: 0001-01-01 through 9999-12-31.
const MYSQL_DATE_DAYS: std::ops::RangeInclusive<i32> = -719_162..=2_932_896;

/// Microseconds in one day.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Whether MySQL reads the literal as the same value.
///
/// MySQL dates and datetimes hold years 1 through 9999, and a text literal
/// with NUL is kept local.
fn literal_runs_remotely(literal: &LiteralValue) -> bool {
    match literal {
        LiteralValue::Date(days) => MYSQL_DATE_DAYS.contains(days),
        LiteralValue::Timestamp(micros) | LiteralValue::TimestampTz(micros) => {
            i32::try_from(micros.div_euclid(MICROS_PER_DAY))
                .is_ok_and(|days| MYSQL_DATE_DAYS.contains(&days))
        }
        LiteralValue::Text(text) => !text.contains('\0'),
        LiteralValue::Float(_)
        | LiteralValue::Double(_)
        | LiteralValue::Time(_)
        | LiteralValue::Duration(_)
        | LiteralValue::Interval { .. } => false,
        LiteralValue::Logical { carrier, .. } => literal_runs_remotely(carrier),
        _ => true,
    }
}
