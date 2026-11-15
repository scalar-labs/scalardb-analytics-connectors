//! Pushdown planning for SQL Server: the read table's current facts, and which
//! offered filters SQL Server evaluates with the same meaning.

use analytics_types::AnalyticsType;
use connector_sdk::{
    BinaryOperator, Catalog, Connector, ConnectorError, ErrorKind, ExecutionOutput, LiteralValue,
    LogicalExpression, LogicalExpressionKind, LogicalPlan, PlanningError, Session, TableRecord,
    TableReference, UnaryOperator,
};

use connector_sql::literal::MICROS_PER_DAY;

use crate::SqlServerConnector;

/// Facts about the read table, loaded from the catalog for one planning call.
pub struct PlanningFacts {
    /// The system type of the column at each base-schema position, which the
    /// comparisons SQL Server evaluates depend on.
    system_types: Vec<Option<String>>,
}

/// Resolves the binding to a table and reads its current columns.
///
/// The binding is the table's schema and name. A table that no longer exists
/// is `does not exist`, a stale registration, and one the table metadata
/// request does not find, which SQL Server may hide, is `not found`. A base
/// column that no longer exists is `does not exist`: every column of a table
/// the credential can see is shown.
pub(crate) fn planning_facts(
    session: &Session<SqlServerConnector>,
    offered_plan: &LogicalPlan,
) -> Result<(TableReference, PlanningFacts), PlanningError> {
    let invalid_binding = || {
        PlanningError::Connector(ConnectorError::new(
            ErrorKind::InvalidRequest,
            false,
            "a SQL Server table binding is a schema and a table name",
        ))
    };
    let (binding, base_schema) = offered_plan.table_read().ok_or_else(invalid_binding)?;
    let [namespace, name] = binding.components() else {
        return Err(invalid_binding());
    };
    let table = TableReference::new(vec![namespace.clone(), name.clone()])
        .map_err(|_| invalid_binding())?;
    let records = <SqlServerConnector as Connector>::Catalog::table_metadata(
        session,
        std::slice::from_ref(&table),
    )
    .and_then(ExecutionOutput::read_to_end)
    .map_err(PlanningError::Connector)?;
    let metadata = match records.into_iter().next() {
        Some(TableRecord::Read { metadata, .. }) => metadata,
        Some(TableRecord::NotFound { .. }) => {
            return Err(failure(
                ErrorKind::NotFound,
                "the registered table is not found at the data source, which hides tables \
                 the credential has no permission on",
            ));
        }
        Some(TableRecord::PermissionDenied { .. }) => {
            return Err(failure(
                ErrorKind::PermissionDenied,
                "the data source denied access to the registered table",
            ));
        }
        Some(TableRecord::DoesNotExist { .. }) | None => {
            return Err(failure(
                ErrorKind::DoesNotExist,
                "the registered table no longer exists at the data source",
            ));
        }
    };
    let system_types = base_schema
        .fields()
        .iter()
        .map(|field| {
            metadata
                .columns
                .iter()
                .find(|column| column.name == field.name())
                .map(|column| column.ty.as_ref().and_then(|ty| ty.system_type.clone()))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            failure(
                ErrorKind::DoesNotExist,
                "a registered column no longer exists at the data source",
            )
        })?;
    Ok((table, PlanningFacts { system_types }))
}

fn failure(kind: ErrorKind, message: &'static str) -> PlanningError {
    PlanningError::Connector(ConnectorError::new(kind, false, message))
}

/// Whether SQL Server evaluates the filter condition `expression` with its
/// offered meaning.
///
/// A condition is a comparison or null test, or AND, OR, and NOT over
/// conditions, which follow SQL three-valued logic in both places. SQL Server
/// has no Boolean expressions, so a bare Boolean column or literal is never a
/// condition there.
pub(crate) fn runs_remotely(expression: &LogicalExpression, facts: &PlanningFacts) -> bool {
    match &expression.kind {
        LogicalExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => runs_remotely(operand, facts),
        LogicalExpressionKind::Unary { operand, .. } => operand_runs_remotely(operand, facts),
        LogicalExpressionKind::Binary {
            operator: BinaryOperator::And | BinaryOperator::Or,
            left,
            right,
        } => runs_remotely(left, facts) && runs_remotely(right, facts),
        LogicalExpressionKind::Binary {
            operator,
            left,
            right,
        } => comparison_runs_remotely(*operator, left, right, facts),
        LogicalExpressionKind::Field { .. }
        | LogicalExpressionKind::Literal(_)
        | LogicalExpressionKind::UnsupportedScalarFunction { .. } => false,
    }
}

/// Whether a comparison or null-test operand runs at SQL Server: a column, or
/// a literal SQL Server reads as the same value.
fn operand_runs_remotely(operand: &LogicalExpression, facts: &PlanningFacts) -> bool {
    match &operand.kind {
        LogicalExpressionKind::Field { position } => *position < facts.system_types.len(),
        LogicalExpressionKind::Literal(literal) => literal_runs_remotely(literal),
        _ => false,
    }
}

/// Whether SQL Server compares the two operands as the Analytics comparison
/// does (`docs/analytics-types.md#comparison`):
///
/// - integers, decimals, Booleans, dates, `time`, and `datetime2` order
///   identically;
/// - `uniqueidentifier` equality agrees; its order is SQL Server's own byte
///   order, and Uuid has none;
/// - `datetime` and `smalldatetime` are compared after conversion to
///   `datetime2`, which keeps their exact 1/300-second ticks, while the value
///   read through ODBC is rounded to the millisecond, so they stay local;
/// - text comparisons follow the collation and ignore trailing spaces, so they
///   stay local, and floating-point, binary, and `datetimeoffset` comparisons
///   are not yet evaluated remotely.
fn comparison_runs_remotely(
    operator: BinaryOperator,
    left: &LogicalExpression,
    right: &LogicalExpression,
    facts: &PlanningFacts,
) -> bool {
    let operands = [left, right];
    if !operands
        .iter()
        .all(|operand| operand_runs_remotely(operand, facts))
        || left.data_type != right.data_type
    {
        return false;
    }
    let system_types_are = |allowed: &[&str]| {
        operands.iter().all(|operand| match operand.kind {
            LogicalExpressionKind::Field { position } => facts.system_types[position]
                .as_deref()
                .is_some_and(|system_type| allowed.contains(&system_type)),
            _ => true,
        })
    };
    match &left.data_type {
        AnalyticsType::Boolean
        | AnalyticsType::UnsignedByte
        | AnalyticsType::SmallInt
        | AnalyticsType::Int
        | AnalyticsType::BigInt
        | AnalyticsType::Decimal(_)
        | AnalyticsType::Date
        | AnalyticsType::Time => true,
        AnalyticsType::Timestamp => system_types_are(&["datetime2"]),
        AnalyticsType::Uuid => {
            matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual)
        }
        _ => false,
    }
}

/// The days of SQL Server's `date` range, 0001-01-01 to 9999-12-31, from
/// 1970-01-01.
const SQL_SERVER_DAYS: std::ops::RangeInclusive<i64> = -719_162..=2_932_896;

/// Whether SQL Server reads the literal as the same value.
///
/// Its dates and timestamps start in year 1 and end in year 9999, its
/// decimals have at most 38 digits and no negative scale, and a UUID literal
/// must be the canonical spelling that the Analytics text compares.
fn literal_runs_remotely(literal: &LiteralValue) -> bool {
    match literal {
        LiteralValue::Date(days) => SQL_SERVER_DAYS.contains(&i64::from(*days)),
        LiteralValue::Timestamp(micros) => {
            SQL_SERVER_DAYS.contains(&micros.div_euclid(MICROS_PER_DAY))
        }
        LiteralValue::Decimal {
            precision, scale, ..
        } => *precision <= 38 && *scale >= 0,
        LiteralValue::Logical {
            data_type: AnalyticsType::Uuid,
            carrier,
        } => matches!(carrier.as_ref(), LiteralValue::Text(text) if canonical_uuid(text)),
        LiteralValue::Logical { carrier, .. } => literal_runs_remotely(carrier),
        LiteralValue::Boolean(_)
        | LiteralValue::SmallInt(_)
        | LiteralValue::Int(_)
        | LiteralValue::BigInt(_)
        | LiteralValue::Time(_)
        | LiteralValue::Null { .. } => true,
        _ => false,
    }
}

/// Whether `text` is a UUID's canonical spelling: 32 lowercase hexadecimal
/// digits grouped 8-4-4-4-12 by hyphens.
fn canonical_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}
