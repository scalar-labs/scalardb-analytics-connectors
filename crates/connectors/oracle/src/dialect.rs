//! Oracle spelling of identifiers and typed literals.
//!
//! Every literal is written in a form whose type and value do not depend on
//! the session's NLS settings: decimals in plain decimal notation, which
//! Oracle reads as an exact `NUMBER`, and timestamps as ANSI `TIMESTAMP`
//! literals.

use analytics_types::AnalyticsType;
use connector_sdk::LiteralValue;
use connector_sql::{
    SqlDialect,
    literal::{self, MICROS_PER_DAY, cast, civil_from_days, decimal_text, time_text},
};

use crate::OracleConnector;

impl SqlDialect for OracleConnector {
    /// Encloses the identifier in double quotes. Oracle names cannot contain
    /// a double quote, so none is escaped.
    fn quote_identifier(identifier: &str) -> String {
        literal::quote_identifier(identifier)
    }

    fn write_literal(literal: &LiteralValue, sql: &mut String) {
        sql.push_str(&literal_sql(literal));
    }
}

/// The Oracle literal of `literal`.
///
/// It is written only for a remote plan that pushdown approved, and pushdown
/// accepts only `Timestamp`, `TimestampTz`, `Decimal`, and `Null` literals
/// (`literal_runs_remotely` in [`crate::pushdown`]), so the other arms are
/// unreachable. A literal pushdown comes to accept gets its rendering in the
/// same change.
fn literal_sql(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Timestamp(micros) => format!("TIMESTAMP '{}'", timestamp_text(*micros)),
        LiteralValue::TimestampTz(micros) => {
            format!("TIMESTAMP '{} +00:00'", timestamp_text(*micros))
        }
        LiteralValue::Decimal { value, scale, .. } => decimal_text(*value, *scale),
        LiteralValue::Null { data_type } => match type_name(data_type) {
            Some(type_name) => cast("NULL", type_name),
            None => "NULL".to_owned(),
        },
        LiteralValue::Boolean(_)
        | LiteralValue::Byte(_)
        | LiteralValue::SmallInt(_)
        | LiteralValue::Int(_)
        | LiteralValue::BigInt(_)
        | LiteralValue::Float(_)
        | LiteralValue::Double(_)
        | LiteralValue::Text(_)
        | LiteralValue::Blob(_)
        | LiteralValue::Date(_)
        | LiteralValue::Time(_)
        | LiteralValue::Duration(_)
        | LiteralValue::Interval { .. }
        | LiteralValue::Logical { .. } => {
            unreachable!(
                "Oracle pushdown runs no literal of this type remotely, so none is written"
            )
        }
    }
}

/// The Oracle type that carries an Analytics type, for typed NULLs; `None`
/// for a type Oracle has no counterpart of.
fn type_name(data_type: &AnalyticsType) -> Option<&'static str> {
    let name = match data_type {
        AnalyticsType::Boolean => "BOOLEAN",
        AnalyticsType::Byte | AnalyticsType::UnsignedByte => "NUMBER(3)",
        AnalyticsType::SmallInt | AnalyticsType::UnsignedSmallInt | AnalyticsType::Year => {
            "NUMBER(5)"
        }
        AnalyticsType::Int | AnalyticsType::UnsignedInt => "NUMBER(10)",
        AnalyticsType::BigInt | AnalyticsType::UnsignedBigInt => "NUMBER(20)",
        AnalyticsType::Decimal(_) => "NUMBER",
        AnalyticsType::Float => "BINARY_FLOAT",
        AnalyticsType::Double => "BINARY_DOUBLE",
        AnalyticsType::Text
        | AnalyticsType::Json
        | AnalyticsType::Enum(_)
        | AnalyticsType::Uuid
        | AnalyticsType::TimeWithTimeZone => "NVARCHAR2(2000)",
        AnalyticsType::Blob => "RAW(2000)",
        AnalyticsType::Date => "DATE",
        AnalyticsType::Time => "INTERVAL DAY(0) TO SECOND(6)",
        AnalyticsType::Timestamp => "TIMESTAMP(6)",
        AnalyticsType::TimestampTz => "TIMESTAMP(6) WITH TIME ZONE",
        AnalyticsType::Duration => "INTERVAL DAY(9) TO SECOND(6)",
        AnalyticsType::Interval
        | AnalyticsType::Array(_)
        | AnalyticsType::Map(_)
        | AnalyticsType::Struct(_) => return None,
    };
    Some(name)
}

/// Formats days from the Unix epoch as `YYYY-MM-DD`.
fn date_text(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Formats microseconds from the Unix epoch as `YYYY-MM-DD hh:mm:ss.ffffff`.
fn timestamp_text(micros: i64) -> String {
    let days = micros.div_euclid(MICROS_PER_DAY);
    let time = time_text(micros.rem_euclid(MICROS_PER_DAY));
    format!("{} {time}", date_text(days))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(value: LiteralValue) -> String {
        let mut sql = String::new();
        OracleConnector::write_literal(&value, &mut sql);
        sql
    }

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(
            OracleConnector::quote_identifier("Mixed Case"),
            "\"Mixed Case\""
        );
    }

    #[test]
    fn timestamps_use_ansi_literals() {
        assert_eq!(
            literal(LiteralValue::Timestamp(-1)),
            "TIMESTAMP '1969-12-31 23:59:59.999999'"
        );
        assert_eq!(
            literal(LiteralValue::TimestampTz(1_577_836_800_000_001)),
            "TIMESTAMP '2020-01-01 00:00:00.000001 +00:00'"
        );
    }

    #[test]
    fn decimals_keep_their_exact_values() {
        assert_eq!(
            literal(LiteralValue::Decimal {
                value: -5,
                precision: 5,
                scale: 3
            }),
            "-0.005"
        );
    }

    #[test]
    fn nulls_are_typed() {
        assert_eq!(
            literal(LiteralValue::Null {
                data_type: AnalyticsType::Timestamp
            }),
            "CAST(NULL AS TIMESTAMP(6))"
        );
    }
}
