//! Oracle spelling of identifiers and typed literals.
//!
//! Every literal is written in a form whose type and value do not depend on
//! the session's NLS settings: numbers in plain decimal notation, which Oracle
//! reads as an exact `NUMBER`, floating-point numbers with the `f` and `d`
//! suffixes of `BINARY_FLOAT` and `BINARY_DOUBLE`, and dates and timestamps as
//! ANSI `DATE` and `TIMESTAMP` literals.

use analytics_types::AnalyticsType;
use connector_sdk::LiteralValue;
use connector_sql::{
    SqlDialect,
    literal::{self, MICROS_PER_DAY, cast, civil_from_days, decimal_text, quote_string, time_text},
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
/// Oracle reads an empty string or byte string as NULL, so an empty `Text` or
/// `Blob` literal is NULL there; no comparison of either type runs remotely.
fn literal_sql(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Boolean(value) => (if *value { "TRUE" } else { "FALSE" }).to_owned(),
        LiteralValue::Byte(value) => value.to_string(),
        LiteralValue::SmallInt(value) => value.to_string(),
        LiteralValue::Int(value) => value.to_string(),
        LiteralValue::BigInt(value) => value.to_string(),
        LiteralValue::Float(value) => float_text(f64::from(*value), "f", "BINARY_FLOAT"),
        LiteralValue::Double(value) => float_text(*value, "d", "BINARY_DOUBLE"),
        LiteralValue::Text(value) => format!("N{}", quote_string(value)),
        LiteralValue::Blob(value) => {
            let hex: String = value.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("HEXTORAW('{hex}')")
        }
        LiteralValue::Date(days) => format!("DATE '{}'", date_text(i64::from(*days))),
        LiteralValue::Time(micros) => {
            format!("INTERVAL '{}' HOUR TO SECOND(6)", time_text(*micros))
        }
        LiteralValue::Timestamp(micros) => format!("TIMESTAMP '{}'", timestamp_text(*micros)),
        LiteralValue::TimestampTz(micros) => {
            format!("TIMESTAMP '{} +00:00'", timestamp_text(*micros))
        }
        LiteralValue::Decimal { value, scale, .. } => decimal_text(*value, *scale),
        LiteralValue::Null { data_type } => match type_name(data_type) {
            Some(type_name) => cast("NULL", type_name),
            None => "NULL".to_owned(),
        },
        // No column maps to a logical type, so no comparison with one runs at
        // Oracle; it is written as its carrier.
        LiteralValue::Logical { carrier, .. } => literal_sql(carrier),
        // No column maps to these types, so no comparison with one runs at
        // Oracle; they are still written as values of their meaning.
        LiteralValue::Duration(micros) => micros.to_string(),
        LiteralValue::Interval { .. } => "NULL".to_owned(),
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

/// The shortest literal that reads back as `value` in the floating-point type
/// whose literals take `suffix`, or the named constant of a NaN or an
/// infinity, such as `BINARY_DOUBLE_NAN`.
fn float_text(value: f64, suffix: &str, type_name: &str) -> String {
    if value.is_nan() {
        format!("{type_name}_NAN")
    } else if value.is_infinite() {
        let sign = if value < 0.0 { "-" } else { "" };
        format!("{sign}{type_name}_INFINITY")
    } else {
        format!("{value:e}{suffix}")
    }
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
    fn identifiers_and_strings_are_quoted_and_escaped() {
        assert_eq!(
            OracleConnector::quote_identifier("Mixed Case"),
            "\"Mixed Case\""
        );
        assert_eq!(literal(LiteralValue::Text("it's".to_owned())), "N'it''s'");
    }

    #[test]
    fn temporal_literals_use_ansi_forms() {
        assert_eq!(literal(LiteralValue::Date(-719_162)), "DATE '0001-01-01'");
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
    fn numeric_literals_keep_their_exact_values() {
        assert_eq!(
            literal(LiteralValue::Decimal {
                value: -5,
                precision: 5,
                scale: 3
            }),
            "-0.005"
        );
        assert_eq!(literal(LiteralValue::Double(0.1)), "1e-1d");
        assert_eq!(literal(LiteralValue::Float(f32::NAN)), "BINARY_FLOAT_NAN");
        assert_eq!(
            literal(LiteralValue::Double(f64::NEG_INFINITY)),
            "-BINARY_DOUBLE_INFINITY"
        );
    }
}
