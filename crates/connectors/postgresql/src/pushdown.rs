//! Pushdown planning for PostgreSQL: the read table's current facts, and which
//! offered filters PostgreSQL evaluates with the same meaning.

use std::collections::HashMap;

use analytics_types::AnalyticsType;
use connector_sdk::{
    BinaryOperator, Catalog, Connector, ConnectorError, ErrorKind, ExecutionOutput, LiteralValue,
    LogicalExpression, LogicalExpressionKind, LogicalPlan, PlanningError, Session, TableRecord,
    TableReference,
};
use connector_sql::{
    catalog::{boolean, int64, read_catalog, required, text},
    literal::quote_string,
};

use crate::{BaseType, PostgresConnector};

/// Facts about the read table, loaded from `pg_catalog` for one planning call.
pub struct PlanningFacts {
    /// The current facts of the column at each base-schema position.
    columns: Vec<ColumnFacts>,
}

/// What planning needs to know about one current column.
struct ColumnFacts {
    /// The type the column's comparisons follow, through any domains.
    base_type: BaseType,
    /// The column's collation, for a collatable column.
    collation: Option<Collation>,
}

/// A column's collation, as planning needs it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Collation {
    /// `pg_collation.oid`; two columns with different collations cannot be
    /// compared without naming one.
    oid: i64,
    /// `pg_collation.collisdeterministic`: equal strings are byte-equal.
    deterministic: bool,
}

/// Resolves the binding to a table and reads its current columns.
///
/// The binding is the table's namespace and name. The column types come from
/// the table's metadata; collations, which only planning needs, are read for
/// this call. A table or base column that no longer exists is
/// `stale registration`.
pub(crate) fn planning_facts(
    session: &Session<PostgresConnector>,
    offered_plan: &LogicalPlan,
) -> Result<(TableReference, PlanningFacts), PlanningError> {
    let invalid_binding = || {
        PlanningError::Connector(ConnectorError::new(
            ErrorKind::InvalidRequest,
            false,
            "a PostgreSQL table binding is a namespace and a table name",
        ))
    };
    let (binding, base_schema) = offered_plan.table_read().ok_or_else(invalid_binding)?;
    let [namespace, name] = binding.components() else {
        return Err(invalid_binding());
    };
    let table = TableReference::new(vec![namespace.clone(), name.clone()])
        .map_err(|_| invalid_binding())?;
    let records = <PostgresConnector as Connector>::Catalog::table_metadata(
        session,
        std::slice::from_ref(&table),
    )
    .and_then(ExecutionOutput::read_to_end)
    .map_err(PlanningError::Connector)?;
    let Some(TableRecord::Read { metadata, .. }) = records.into_iter().next() else {
        return Err(PlanningError::Connector(stale(
            "the registered table no longer exists at the data source",
        )));
    };
    let collations = collations(session, namespace, name)?;
    let columns = base_schema
        .fields()
        .iter()
        .map(|field| {
            let column = metadata
                .columns
                .iter()
                .find(|column| column.name == field.name())?;
            Some(ColumnFacts {
                base_type: column.base_type.clone(),
                collation: collations.get(&column.name).copied(),
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            PlanningError::Connector(stale(
                "a registered column no longer exists at the data source",
            ))
        })?;
    Ok((table, PlanningFacts { columns }))
}

/// Reads the collation of each collatable column of the table, keyed by
/// column name.
fn collations(
    session: &Session<PostgresConnector>,
    namespace: &str,
    name: &str,
) -> Result<HashMap<String, Collation>, PlanningError> {
    let sql = format!(
        "SELECT a.attname::text, co.oid::int8, co.collisdeterministic \
         FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_collation co ON co.oid = a.attcollation \
         WHERE n.nspname = {} AND c.relname = {} AND a.attnum > 0 AND NOT a.attisdropped",
        quote_string(namespace),
        quote_string(name)
    );
    read_catalog(session.access().connection(), |catalog| {
        let mut collations = HashMap::new();
        for batch in &catalog.query(sql)? {
            let (names, oids, deterministic) =
                (text(batch, 0)?, int64(batch, 1)?, boolean(batch, 2)?);
            for (row, name) in names.into_iter().enumerate() {
                collations.insert(
                    required(name)?.to_owned(),
                    Collation {
                        oid: oids.value(row),
                        deterministic: deterministic.value(row),
                    },
                );
            }
        }
        Ok(collations)
    })
    .map_err(PlanningError::Connector)
}

fn stale(message: &'static str) -> ConnectorError {
    ConnectorError::new(ErrorKind::StaleRegistration, false, message)
}

/// Whether PostgreSQL evaluates `expression` with its offered meaning.
///
/// Boolean composition and null tests follow SQL three-valued logic in both
/// places. A comparison qualifies only between operands of one type whose
/// PostgreSQL comparison agrees with the Analytics comparison:
///
/// - integers, decimals, Booleans, dates, times, timestamps, and UUIDs order
///   identically;
/// - text equality agrees only on `text` and `varchar` columns with a
///   deterministic collation, where equal strings are byte-equal; `character(n)`
///   ignores trailing spaces, and text ordering depends on the collation;
/// - floating-point comparison differs for NaN, enum ordering follows the
///   declared order, and interval comparison normalizes months to 30 days, so
///   none of them qualifies.
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
            comparison => comparison_runs_remotely(*comparison, left, right, facts),
        },
        LogicalExpressionKind::UnsupportedScalarFunction { .. } => false,
    }
}

fn comparison_runs_remotely(
    operator: BinaryOperator,
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
        | AnalyticsType::SmallInt
        | AnalyticsType::Int
        | AnalyticsType::BigInt
        | AnalyticsType::Decimal(_)
        | AnalyticsType::Date
        | AnalyticsType::Time
        | AnalyticsType::Timestamp
        | AnalyticsType::TimestampTz
        | AnalyticsType::Uuid => true,
        AnalyticsType::Text => {
            let columns: Option<Vec<&ColumnFacts>> = operands
                .iter()
                .filter_map(|operand| match operand.kind {
                    LogicalExpressionKind::Field { position } => Some(facts.columns.get(position)),
                    _ => None,
                })
                .collect();
            let Some(columns) = columns else {
                return false;
            };
            // Two columns with different collations cannot be compared
            // without naming one.
            let one_collation = columns
                .windows(2)
                .all(|pair| pair[0].collation == pair[1].collation);
            matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual)
                && columns.iter().all(|column| byte_equal_text(column))
                && one_collation
        }
        _ => false,
    }
}

/// Whether equal values of this text column are exactly the byte-equal ones.
fn byte_equal_text(column: &ColumnFacts) -> bool {
    matches!(column.base_type.name.as_str(), "text" | "varchar")
        && column.base_type.schema == "pg_catalog"
        && column
            .collation
            .is_some_and(|collation| collation.deterministic)
}

/// Whether `text` is a UUID's canonical spelling: 32 lowercase hexadecimal
/// digits grouped 8-4-4-4-12 by hyphens. PostgreSQL accepts other spellings
/// and normalizes them, so only this one compares as the Analytics text does.
fn canonical_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// The first and last day PostgreSQL's `date` holds, in days since
/// 1970-01-01: Julian day 0 (4713 BC) through the last day before Julian day
/// 2147483494 (5874897 AD).
const POSTGRES_DATE_DAYS: std::ops::RangeInclusive<i32> = -2_440_588..=2_145_042_905;

/// The first instant PostgreSQL's `timestamp` holds, in microseconds since
/// 1970-01-01 (Julian day 0, 4713 BC). Its upper bound (294276 AD) lies
/// beyond every 64-bit value.
const POSTGRES_TIMESTAMP_MIN_MICROS: i64 = -210_866_803_200_000_000;

/// Whether PostgreSQL reads the literal as the same value.
///
/// PostgreSQL text cannot hold NUL, a UUID compares by value while Analytics
/// compares its canonical lowercase text, intervals hold microseconds, and
/// dates and timestamps have a narrower range than the Analytics types.
fn literal_runs_remotely(literal: &LiteralValue) -> bool {
    match literal {
        LiteralValue::Date(days) => POSTGRES_DATE_DAYS.contains(days),
        LiteralValue::Timestamp(micros) | LiteralValue::TimestampTz(micros) => {
            *micros >= POSTGRES_TIMESTAMP_MIN_MICROS
        }
        LiteralValue::Text(text) => !text.contains('\0'),
        LiteralValue::Interval { nanoseconds, .. } => nanoseconds % 1_000 == 0,
        LiteralValue::Logical {
            data_type: AnalyticsType::Uuid,
            carrier,
        } => {
            matches!(carrier.as_ref(), LiteralValue::Text(text) if canonical_uuid(text))
        }
        LiteralValue::Logical { carrier, .. } => literal_runs_remotely(carrier),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::canonical_uuid;

    #[test]
    fn only_the_canonical_uuid_spelling_is_canonical() {
        assert!(canonical_uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"));
        for other in [
            "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11",
            "a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}",
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a1g",
            "not a uuid",
        ] {
            assert!(!canonical_uuid(other), "{other}");
        }
    }
}
