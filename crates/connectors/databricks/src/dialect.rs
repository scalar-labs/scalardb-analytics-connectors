//! Databricks SQL spelling of identifiers and typed literals.
//!
//! Every literal states its type, so a comparison with a column of the same
//! type compares values of that type, and none depends on a session setting:
//! a `timestamp` literal carries its UTC offset, and a `timestamp_ntz` literal
//! has none to apply.
//!
//! [`pushdown`](crate::pushdown) admits only literals Databricks reads as the
//! same value; the spellings of the others are never sent.

use connector_sdk::LiteralValue;
use connector_sql::{
    SqlDialect,
    literal::{MICROS_PER_DAY, civil_from_days, decimal_text, time_text},
};

use crate::DatabricksConnector;

impl SqlDialect for DatabricksConnector {
    /// Quotes with backticks, doubling embedded backticks.
    fn quote_identifier(identifier: &str) -> String {
        format!("`{}`", identifier.replace('`', "``"))
    }

    fn write_literal(literal: &LiteralValue, sql: &mut String) {
        sql.push_str(&literal_sql(literal));
    }
}

/// Quotes a string literal.
///
/// A backslash escapes the next character in a Databricks string literal, and
/// two adjacent literals are concatenated, so a quote is escaped with a
/// backslash rather than doubled.
pub(crate) fn string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn literal_sql(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Boolean(value) => (if *value { "TRUE" } else { "FALSE" }).to_owned(),
        // Integer literals with a type suffix, so each has its column's type.
        LiteralValue::Byte(value) => format!("{value}Y"),
        LiteralValue::SmallInt(value) => format!("{value}S"),
        LiteralValue::Int(value) => value.to_string(),
        LiteralValue::BigInt(value) => format!("{value}L"),
        // A `FLOAT` literal spells the value exactly widened to a double:
        // Databricks rejects a literal above the double nearest
        // `Float.MaxValue`, which the float's own shortest spelling of the
        // largest floats is, and its error quotes the literal.
        LiteralValue::Float(value) => {
            let value = f64::from(*value);
            floating(value, "FLOAT", || format!("{value}F"))
        }
        LiteralValue::Double(value) => floating(*value, "DOUBLE", || format!("{value}D")),
        // An exact decimal literal whose scale is the number of digits after
        // its point.
        LiteralValue::Decimal { value, scale, .. } => format!("{}BD", decimal_text(*value, *scale)),
        LiteralValue::Text(value) => string_literal(value),
        LiteralValue::Blob(value) => {
            let hex: String = value.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("X'{hex}'")
        }
        LiteralValue::Date(days) => format!("DATE '{}'", date_text(i64::from(*days))),
        LiteralValue::Timestamp(micros) => {
            format!("TIMESTAMP_NTZ '{}'", timestamp_text(*micros))
        }
        LiteralValue::TimestampTz(micros) => {
            format!("TIMESTAMP '{}+00:00'", timestamp_text(*micros))
        }
        LiteralValue::Time(micros) => string_literal(&time_text(*micros)),
        LiteralValue::Duration(micros) => string_literal(&format!("{micros} microseconds")),
        LiteralValue::Interval {
            months,
            days,
            nanoseconds,
        } => string_literal(&format!(
            "{months} months {days} days {nanoseconds} nanoseconds"
        )),
        LiteralValue::Null { .. } => "NULL".to_owned(),
        LiteralValue::Logical { carrier, .. } => literal_sql(carrier),
    }
}

/// Spells a floating-point literal: a finite value with its type suffix, and
/// NaN and the infinities as the cast of their names.
fn floating(value: f64, type_name: &str, spelled: impl FnOnce() -> String) -> String {
    if value.is_finite() {
        return spelled();
    }
    let name = if value.is_nan() {
        "NaN"
    } else if value > 0.0 {
        "Infinity"
    } else {
        "-Infinity"
    };
    format!("CAST('{name}' AS {type_name})")
}

/// Formats days from the Unix epoch as `YYYY-MM-DD`.
fn date_text(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Formats microseconds from the Unix epoch as `YYYY-MM-DD HH:MM:SS.ffffff`.
fn timestamp_text(micros: i64) -> String {
    format!(
        "{} {}",
        date_text(micros.div_euclid(MICROS_PER_DAY)),
        time_text(micros.rem_euclid(MICROS_PER_DAY))
    )
}

#[cfg(test)]
mod tests {
    use analytics_types::AnalyticsType;

    use super::*;

    fn literal(value: LiteralValue) -> String {
        let mut sql = String::new();
        DatabricksConnector::write_literal(&value, &mut sql);
        sql
    }

    #[test]
    fn identifiers_are_quoted_with_doubled_backticks() {
        assert_eq!(DatabricksConnector::quote_identifier("orders"), "`orders`");
        assert_eq!(DatabricksConnector::quote_identifier("a`b"), "`a``b`");
        assert_eq!(DatabricksConnector::quote_identifier("``"), "``````");
        assert_eq!(DatabricksConnector::quote_identifier("a b.c"), "`a b.c`");
    }

    #[test]
    fn strings_escape_quotes_and_backslashes() {
        assert_eq!(string_literal("plain"), "'plain'");
        assert_eq!(string_literal("it's"), "'it\\'s'");
        assert_eq!(string_literal("a\\b"), "'a\\\\b'");
        assert_eq!(string_literal("\\'"), "'\\\\\\''");
        assert_eq!(literal(LiteralValue::Text("ü\n".to_owned())), "'ü\n'");
    }

    #[test]
    fn integers_carry_their_type() {
        assert_eq!(literal(LiteralValue::Boolean(true)), "TRUE");
        assert_eq!(literal(LiteralValue::Boolean(false)), "FALSE");
        assert_eq!(literal(LiteralValue::Byte(i8::MIN)), "-128Y");
        assert_eq!(literal(LiteralValue::SmallInt(-7)), "-7S");
        assert_eq!(literal(LiteralValue::Int(i32::MAX)), "2147483647");
        assert_eq!(
            literal(LiteralValue::BigInt(i64::MIN)),
            "-9223372036854775808L"
        );
    }

    #[test]
    fn floating_point_literals_keep_their_value_and_special_values() {
        assert_eq!(literal(LiteralValue::Float(1.5)), "1.5F");
        assert_eq!(literal(LiteralValue::Float(-0.0)), "-0F");
        assert_eq!(literal(LiteralValue::Float(0.1)), "0.10000000149011612F");
        // Not above Float.MaxValue as a double, 3.4028234663852886E38, and
        // read back as the same float.
        for value in [f32::MAX, f32::MIN, f32::MIN_POSITIVE, f32::from_bits(1)] {
            let text = literal(LiteralValue::Float(value));
            let digits = text.strip_suffix('F').unwrap();
            assert!(
                digits.parse::<f64>().unwrap().abs() <= f64::from(f32::MAX),
                "{text}"
            );
            assert_eq!(digits.parse::<f32>().unwrap(), value, "{text}");
        }
        assert_eq!(literal(LiteralValue::Double(0.1)), "0.1D");
        assert_eq!(
            literal(LiteralValue::Double(1e300)),
            format!("1{}D", "0".repeat(300))
        );
        assert_eq!(
            literal(LiteralValue::Float(f32::NAN)),
            "CAST('NaN' AS FLOAT)"
        );
        assert_eq!(
            literal(LiteralValue::Double(f64::INFINITY)),
            "CAST('Infinity' AS DOUBLE)"
        );
        assert_eq!(
            literal(LiteralValue::Double(f64::NEG_INFINITY)),
            "CAST('-Infinity' AS DOUBLE)"
        );
        assert_eq!(
            literal(LiteralValue::Float(f32::NEG_INFINITY)),
            "CAST('-Infinity' AS FLOAT)"
        );
    }

    #[test]
    fn decimals_keep_their_scale() {
        for (value, precision, scale, expected) in [
            (1250, 4, 2, "12.50BD"),
            (-5, 5, 3, "-0.005BD"),
            (0, 3, 2, "0.00BD"),
            (7, 1, 0, "7BD"),
            (i128::from(i64::MAX), 38, 10, "922337203.6854775807BD"),
        ] {
            assert_eq!(
                literal(LiteralValue::Decimal {
                    value,
                    precision,
                    scale
                }),
                expected
            );
        }
    }

    #[test]
    fn temporal_literals_do_not_depend_on_the_session_time_zone() {
        assert_eq!(literal(LiteralValue::Date(0)), "DATE '1970-01-01'");
        assert_eq!(literal(LiteralValue::Date(-719_162)), "DATE '0001-01-01'");
        assert_eq!(literal(LiteralValue::Date(2_932_896)), "DATE '9999-12-31'");
        assert_eq!(
            literal(LiteralValue::Timestamp(-1)),
            "TIMESTAMP_NTZ '1969-12-31 23:59:59.999999'"
        );
        assert_eq!(
            literal(LiteralValue::TimestampTz(1_577_836_800_000_001)),
            "TIMESTAMP '2020-01-01 00:00:00.000001+00:00'"
        );
    }

    #[test]
    fn binary_and_null_literals() {
        assert_eq!(literal(LiteralValue::Blob(vec![0x00, 0xab])), "X'00AB'");
        assert_eq!(
            literal(LiteralValue::Null {
                data_type: AnalyticsType::Int
            }),
            "NULL"
        );
    }
}
