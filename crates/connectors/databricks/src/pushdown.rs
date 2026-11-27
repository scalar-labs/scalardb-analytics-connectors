//! Pushdown planning for Databricks: the read table's current facts, and
//! which offered filters Databricks evaluates with the same meaning.

use analytics_types::AnalyticsType;
use connector_sdk::{
    BinaryOperator, Catalog, Connector, ConnectorError, ErrorKind, ExecutionOutput, LiteralValue,
    LogicalExpression, LogicalExpressionKind, LogicalPlan, PlanningError, Session, TableRecord,
    TableReference,
};

use crate::{DatabricksConnector, catalog::DataType};

/// Facts about the read table, loaded from `information_schema` for one
/// planning call.
pub struct PlanningFacts {
    /// The current type of the column at each base-schema position; `None`
    /// when its `full_data_type` is no type the grammar reads.
    columns: Vec<Option<DataType>>,
}

/// The collation under which strings compare as their UTF-8 bytes, which is
/// the Analytics comparison. A string that names no collation has it.
const BYTE_COLLATION: &str = "UTF8_BINARY";

/// The first and last day the connector sends date and timestamp literals
/// for, in days since 1970-01-01: 0001-01-01 through 9999-12-31, the years a
/// literal spells with four digits.
const LITERAL_DAYS: std::ops::RangeInclusive<i32> = -719_162..=2_932_896;

/// Microseconds in one day.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Resolves the binding to a table and reads its current columns.
///
/// The binding is the table's catalog, schema, and name. A table
/// `information_schema` does not show is `not found`, since it may be hidden;
/// Unity Catalog shows every column of a table it shows, so a base column
/// that is gone no longer exists.
pub(crate) fn planning_facts(
    session: &Session<DatabricksConnector>,
    offered_plan: &LogicalPlan,
) -> Result<(TableReference, PlanningFacts), PlanningError> {
    let invalid_binding = || {
        PlanningError::Connector(ConnectorError::new(
            ErrorKind::InvalidRequest,
            false,
            "a Databricks table binding is a catalog, a schema, and a table name",
        ))
    };
    let (binding, base_schema) = offered_plan.table_read().ok_or_else(invalid_binding)?;
    let [catalog, schema, name] = binding.components() else {
        return Err(invalid_binding());
    };
    let table = TableReference::new(vec![catalog.clone(), schema.clone(), name.clone()])
        .map_err(|_| invalid_binding())?;
    let records = <DatabricksConnector as Connector>::Catalog::table_metadata(
        session,
        std::slice::from_ref(&table),
    )
    .and_then(ExecutionOutput::read_to_end)
    .map_err(PlanningError::Connector)?;
    let metadata = match records.into_iter().next() {
        Some(TableRecord::Read { metadata, .. }) => metadata,
        Some(TableRecord::PermissionDenied { .. }) => {
            return Err(PlanningError::Connector(ConnectorError::new(
                ErrorKind::PermissionDenied,
                false,
                "the principal may not read the registered table",
            )));
        }
        Some(TableRecord::NotFound { .. } | TableRecord::DoesNotExist { .. }) | None => {
            return Err(PlanningError::Connector(ConnectorError::new(
                ErrorKind::NotFound,
                false,
                "the registered table is not found; the principal may not see it",
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
            Some(DataType::parse(&column.full_data_type))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            PlanningError::Connector(ConnectorError::new(
                ErrorKind::DoesNotExist,
                false,
                "a registered column no longer exists at the data source",
            ))
        })?;
    Ok((table, PlanningFacts { columns }))
}

/// Whether Databricks evaluates `expression` with its offered meaning.
///
/// Boolean composition and null tests follow SQL three-valued logic in both
/// places. A comparison qualifies only between operands of one type whose
/// Databricks comparison agrees with the Analytics comparison
/// (`docs/analytics-types.md#comparison`):
///
/// - Booleans, integers, decimals, dates, timestamps, and floating-point
///   values order identically; Databricks, like Analytics, takes NaN as
///   equal to itself and larger than any other value, and -0 as equal to 0;
///   literals state their type, and a `timestamp` literal its UTC offset;
/// - text compares as bytes only on `string` and `varchar` columns under
///   `UTF8_BINARY`; other collations fold case or accents or ignore trailing
///   spaces, and `char` values are padded with spaces;
/// - binary values and the types the mapping does not map stay local.
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
        | AnalyticsType::Float
        | AnalyticsType::Double
        | AnalyticsType::Decimal(_)
        | AnalyticsType::Date
        | AnalyticsType::Timestamp
        | AnalyticsType::TimestampTz => true,
        AnalyticsType::Text => operands.iter().all(|operand| match operand.kind {
            LogicalExpressionKind::Field { position } => facts
                .columns
                .get(position)
                .is_some_and(|column| column.as_ref().is_some_and(byte_ordered_text)),
            _ => true,
        }),
        _ => false,
    }
}

/// Whether this text column compares and orders its values as their UTF-8
/// bytes, as the Analytics comparison does.
fn byte_ordered_text(column: &DataType) -> bool {
    match column {
        DataType::String { collation } | DataType::Varchar { collation, .. } => {
            collation.as_deref().is_none_or(is_byte_collation)
        }
        _ => false,
    }
}

/// Whether a collation name, which Databricks reads without regard to case
/// and may qualify with `system.builtin.`, is `UTF8_BINARY`.
fn is_byte_collation(collation: &str) -> bool {
    let name = collation
        .rsplit_once('.')
        .map_or(collation, |(_, name)| name);
    name.eq_ignore_ascii_case(BYTE_COLLATION)
}

/// Whether Databricks reads the literal as the same value.
///
/// Dates and timestamps are sent for years 1 through 9999.
fn literal_runs_remotely(literal: &LiteralValue) -> bool {
    match literal {
        LiteralValue::Date(days) => LITERAL_DAYS.contains(days),
        LiteralValue::Timestamp(micros) | LiteralValue::TimestampTz(micros) => {
            i32::try_from(micros.div_euclid(MICROS_PER_DAY))
                .is_ok_and(|days| LITERAL_DAYS.contains(&days))
        }
        LiteralValue::Blob(_)
        | LiteralValue::Time(_)
        | LiteralValue::Duration(_)
        | LiteralValue::Interval { .. }
        | LiteralValue::Logical { .. } => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use analytics_types::DecimalType;
    use connector_sdk::UnaryOperator;

    use super::*;

    fn facts(types: &[&str]) -> PlanningFacts {
        PlanningFacts {
            columns: types.iter().map(|text| DataType::parse(text)).collect(),
        }
    }

    fn field(position: usize, data_type: AnalyticsType) -> LogicalExpression {
        LogicalExpression {
            kind: LogicalExpressionKind::Field { position },
            data_type,
            nullable: true,
        }
    }

    fn literal(value: LiteralValue, data_type: AnalyticsType) -> LogicalExpression {
        LogicalExpression {
            kind: LogicalExpressionKind::Literal(value),
            data_type,
            nullable: true,
        }
    }

    fn binary(
        operator: BinaryOperator,
        left: LogicalExpression,
        right: LogicalExpression,
    ) -> LogicalExpression {
        LogicalExpression {
            kind: LogicalExpressionKind::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: AnalyticsType::Boolean,
            nullable: true,
        }
    }

    fn unary(operator: UnaryOperator, operand: LogicalExpression) -> LogicalExpression {
        LogicalExpression {
            kind: LogicalExpressionKind::Unary {
                operator,
                operand: Box::new(operand),
            },
            data_type: AnalyticsType::Boolean,
            nullable: true,
        }
    }

    fn text(value: &str) -> LogicalExpression {
        literal(LiteralValue::Text(value.to_owned()), AnalyticsType::Text)
    }

    #[test]
    fn same_type_comparisons_of_ordered_types_run_remotely() {
        let decimal = AnalyticsType::Decimal(DecimalType::new(10, 2).unwrap());
        let facts = facts(&["bigint"; 1]);
        for (left, right) in [
            (
                field(0, AnalyticsType::BigInt),
                literal(LiteralValue::BigInt(1), AnalyticsType::BigInt),
            ),
            (
                field(0, decimal.clone()),
                literal(
                    LiteralValue::Decimal {
                        value: 125,
                        precision: 10,
                        scale: 2,
                    },
                    decimal,
                ),
            ),
            (
                field(0, AnalyticsType::Double),
                literal(LiteralValue::Double(f64::NAN), AnalyticsType::Double),
            ),
            (
                field(0, AnalyticsType::Date),
                literal(LiteralValue::Date(0), AnalyticsType::Date),
            ),
            (
                field(0, AnalyticsType::TimestampTz),
                literal(LiteralValue::TimestampTz(0), AnalyticsType::TimestampTz),
            ),
            (
                field(0, AnalyticsType::Timestamp),
                literal(LiteralValue::Timestamp(0), AnalyticsType::Timestamp),
            ),
            (
                field(0, AnalyticsType::Boolean),
                literal(LiteralValue::Boolean(true), AnalyticsType::Boolean),
            ),
        ] {
            for operator in [
                BinaryOperator::Equal,
                BinaryOperator::NotEqual,
                BinaryOperator::LessThan,
                BinaryOperator::LessThanOrEqual,
                BinaryOperator::GreaterThan,
                BinaryOperator::GreaterThanOrEqual,
            ] {
                assert!(runs_remotely(
                    &binary(operator, left.clone(), right.clone()),
                    &facts
                ));
            }
        }
    }

    #[test]
    fn composition_and_null_tests_run_remotely_only_as_a_whole() {
        let facts = facts(&["int", "string collate UTF8_LCASE"]);
        let remote = binary(
            BinaryOperator::Equal,
            field(0, AnalyticsType::Int),
            literal(LiteralValue::Int(1), AnalyticsType::Int),
        );
        let local = binary(
            BinaryOperator::Equal,
            field(1, AnalyticsType::Text),
            text("a"),
        );
        let null_test = unary(UnaryOperator::IsNull, field(1, AnalyticsType::Text));
        assert!(runs_remotely(&null_test, &facts));
        assert!(runs_remotely(
            &unary(
                UnaryOperator::Not,
                binary(BinaryOperator::Or, remote.clone(), null_test.clone())
            ),
            &facts
        ));
        assert!(runs_remotely(
            &unary(UnaryOperator::IsNotNull, field(0, AnalyticsType::Int)),
            &facts
        ));
        for operator in [BinaryOperator::And, BinaryOperator::Or] {
            assert!(!runs_remotely(
                &binary(operator, remote.clone(), local.clone()),
                &facts
            ));
        }
        assert!(!runs_remotely(&unary(UnaryOperator::Not, local), &facts));
    }

    #[test]
    fn text_runs_remotely_only_under_the_byte_collation() {
        let facts = facts(&[
            "string",
            "string collate UTF8_BINARY",
            "varchar(10)",
            "varchar(10) collate system.builtin.utf8_binary",
            "string collate UTF8_LCASE",
            "string collate UNICODE",
            "string collate UTF8_BINARY_RTRIM",
            "char(5)",
            "char(5) collate UTF8_BINARY",
            "not a type <",
        ]);
        let compare = |position| {
            binary(
                BinaryOperator::LessThan,
                field(position, AnalyticsType::Text),
                text("it's"),
            )
        };
        for position in 0..4 {
            assert!(runs_remotely(&compare(position), &facts), "{position}");
        }
        for position in 4..10 {
            assert!(!runs_remotely(&compare(position), &facts), "{position}");
        }
        assert!(runs_remotely(
            &binary(
                BinaryOperator::Equal,
                field(0, AnalyticsType::Text),
                field(2, AnalyticsType::Text)
            ),
            &facts
        ));
        assert!(!runs_remotely(
            &binary(
                BinaryOperator::Equal,
                field(0, AnalyticsType::Text),
                field(4, AnalyticsType::Text)
            ),
            &facts
        ));
    }

    #[test]
    fn mixed_types_and_unordered_types_stay_local() {
        let facts = facts(&["int", "binary", "array<int>"]);
        assert!(!runs_remotely(
            &binary(
                BinaryOperator::Equal,
                field(0, AnalyticsType::Int),
                literal(LiteralValue::BigInt(1), AnalyticsType::BigInt),
            ),
            &facts
        ));
        assert!(!runs_remotely(
            &binary(
                BinaryOperator::Equal,
                field(1, AnalyticsType::Blob),
                literal(LiteralValue::Blob(vec![1]), AnalyticsType::Blob),
            ),
            &facts
        ));
        let array = AnalyticsType::Array(analytics_types::ArrayType::new(AnalyticsType::Int, true));
        assert!(!runs_remotely(
            &binary(
                BinaryOperator::Equal,
                field(2, array.clone()),
                field(2, array)
            ),
            &facts
        ));
        assert!(!runs_remotely(
            &LogicalExpression {
                kind: LogicalExpressionKind::UnsupportedScalarFunction {
                    uri: String::new(),
                    name: "upper".to_owned(),
                    arguments: vec![field(0, AnalyticsType::Text)],
                },
                data_type: AnalyticsType::Text,
                nullable: true,
            },
            &facts
        ));
    }

    #[test]
    fn dates_and_timestamps_outside_four_digit_years_stay_local() {
        let facts = facts(&["date", "timestamp"]);
        let date = |days| {
            binary(
                BinaryOperator::Equal,
                field(0, AnalyticsType::Date),
                literal(LiteralValue::Date(days), AnalyticsType::Date),
            )
        };
        assert!(runs_remotely(&date(-719_162), &facts));
        assert!(runs_remotely(&date(2_932_896), &facts));
        assert!(!runs_remotely(&date(-719_163), &facts));
        assert!(!runs_remotely(&date(2_932_897), &facts));
        let timestamp = |micros| {
            binary(
                BinaryOperator::Equal,
                field(1, AnalyticsType::TimestampTz),
                literal(
                    LiteralValue::TimestampTz(micros),
                    AnalyticsType::TimestampTz,
                ),
            )
        };
        assert!(runs_remotely(
            &timestamp(2_932_897 * MICROS_PER_DAY - 1),
            &facts
        ));
        assert!(!runs_remotely(
            &timestamp(2_932_897 * MICROS_PER_DAY),
            &facts
        ));
        assert!(!runs_remotely(&timestamp(i64::MIN), &facts));
    }
}
