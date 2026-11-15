//! PostgreSQL spelling of identifiers and typed literals.
//!
//! Literals rely on the fixed session setting `standard_conforming_strings =
//! on`, under which a backslash in a quoted string is an ordinary character.

use analytics_types::AnalyticsType;
use connector_sdk::LiteralValue;
use connector_sql::{
    SqlDialect,
    literal::{
        MICROS_PER_DAY, cast, civil_from_days, decimal_text, quote_identifier, quote_string,
        time_text,
    },
};

use crate::PostgresConnector;

impl SqlDialect for PostgresConnector {
    fn quote_identifier(identifier: &str) -> String {
        quote_identifier(identifier)
    }

    fn write_literal(literal: &LiteralValue, sql: &mut String) {
        sql.push_str(&literal_sql(literal));
    }
}

fn literal_sql(literal: &LiteralValue) -> String {
    let typed = |text: &str, type_name: &str| cast(&quote_string(text), type_name);
    match literal {
        LiteralValue::Boolean(value) => (if *value { "TRUE" } else { "FALSE" }).to_owned(),
        LiteralValue::Byte(value) => cast(&value.to_string(), "smallint"),
        LiteralValue::SmallInt(value) => cast(&value.to_string(), "smallint"),
        LiteralValue::Int(value) => cast(&value.to_string(), "integer"),
        LiteralValue::BigInt(value) => cast(&value.to_string(), "bigint"),
        LiteralValue::Float(value) => {
            typed(&float_text(f64::from(*value), value.to_string()), "real")
        }
        LiteralValue::Double(value) => {
            typed(&float_text(*value, value.to_string()), "double precision")
        }
        LiteralValue::Text(value) => typed(value, "text"),
        LiteralValue::Blob(value) => {
            let hex: String = value.iter().map(|byte| format!("{byte:02x}")).collect();
            typed(&format!("\\x{hex}"), "bytea")
        }
        LiteralValue::Date(days) => typed(&date_text(i64::from(*days)), "date"),
        LiteralValue::Time(micros) => typed(&time_text(*micros), "time"),
        LiteralValue::Timestamp(micros) => typed(&timestamp_text(*micros), "timestamp"),
        LiteralValue::TimestampTz(micros) => {
            typed(&format!("{}+00", timestamp_text(*micros)), "timestamptz")
        }
        LiteralValue::Duration(micros) => typed(&format!("{micros} microseconds"), "interval"),
        LiteralValue::Interval {
            months,
            days,
            nanoseconds,
        } => typed(
            &format!(
                "{months} months {days} days {} microseconds",
                nanoseconds / 1_000
            ),
            "interval",
        ),
        // The exact text is cast to unconstrained `numeric`: PostgreSQL has
        // no negative scale, and a typmod with scale 0 could not hold every
        // value of a negative-scale decimal.
        LiteralValue::Decimal { value, scale, .. } => {
            typed(&decimal_text(*value, *scale), "numeric")
        }
        LiteralValue::Null { data_type } => match type_name(data_type) {
            Some(name) => cast("NULL", &name),
            None => "NULL".to_owned(),
        },
        LiteralValue::Logical { data_type, carrier } => match (data_type, carrier.as_ref()) {
            // An enum label is written untyped so that it takes the enum type
            // of the column it is compared with.
            (AnalyticsType::Enum(_), LiteralValue::Text(label)) => quote_string(label),
            (data_type, LiteralValue::Text(text)) => match type_name(data_type) {
                Some(name) => typed(text, &name),
                None => quote_string(text),
            },
            (_, carrier) => literal_sql(carrier),
        },
    }
}

/// The PostgreSQL type that carries an Analytics type, for typed literals.
fn type_name(data_type: &AnalyticsType) -> Option<String> {
    let name = match data_type {
        AnalyticsType::Boolean => "boolean",
        AnalyticsType::Byte | AnalyticsType::SmallInt => "smallint",
        AnalyticsType::Int => "integer",
        AnalyticsType::BigInt => "bigint",
        AnalyticsType::Float => "real",
        AnalyticsType::Double => "double precision",
        AnalyticsType::Decimal(decimal) => {
            return Some(format!(
                "numeric({}, {})",
                decimal.precision(),
                decimal.scale().max(0)
            ));
        }
        AnalyticsType::Text => "text",
        AnalyticsType::Blob => "bytea",
        AnalyticsType::Date => "date",
        AnalyticsType::Time => "time",
        AnalyticsType::Timestamp => "timestamp",
        AnalyticsType::TimestampTz => "timestamptz",
        AnalyticsType::Duration | AnalyticsType::Interval => "interval",
        AnalyticsType::Uuid => "uuid",
        AnalyticsType::Json => "json",
        AnalyticsType::TimeWithTimeZone => "timetz",
        AnalyticsType::Enum(_)
        | AnalyticsType::Year
        | AnalyticsType::UnsignedByte
        | AnalyticsType::UnsignedSmallInt
        | AnalyticsType::UnsignedInt
        | AnalyticsType::UnsignedBigInt
        | AnalyticsType::Array(_)
        | AnalyticsType::Map(_)
        | AnalyticsType::Struct(_) => return None,
    };
    Some(name.to_owned())
}

/// PostgreSQL's input spelling of a floating-point value.
fn float_text(value: f64, shortest: String) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        shortest
    }
}

/// Formats days from the Unix epoch as `YYYY-MM-DD`, with `BC` before year 1.
fn date_text(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    if year > 0 {
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        format!("{:04}-{month:02}-{day:02} BC", 1 - year)
    }
}

/// Formats microseconds from the Unix epoch as a PostgreSQL timestamp,
/// putting `BC` after the time.
fn timestamp_text(micros: i64) -> String {
    let days = micros.div_euclid(MICROS_PER_DAY);
    let time = time_text(micros.rem_euclid(MICROS_PER_DAY));
    let date = date_text(days);
    match date.strip_suffix(" BC") {
        Some(date) => format!("{date} {time} BC"),
        None => format!("{date} {time}"),
    }
}

#[cfg(test)]
mod tests {
    use analytics_types::EnumType;

    use super::*;

    fn literal(value: LiteralValue) -> String {
        let mut sql = String::new();
        PostgresConnector::write_literal(&value, &mut sql);
        sql
    }

    #[test]
    fn identifiers_and_strings_are_quoted_and_escaped() {
        assert_eq!(PostgresConnector::quote_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(
            literal(LiteralValue::Text("it's \\".to_owned())),
            "CAST('it''s \\' AS text)"
        );
    }

    #[test]
    fn temporal_literals_use_postgresql_input_forms() {
        assert_eq!(literal(LiteralValue::Date(0)), "CAST('1970-01-01' AS date)");
        assert_eq!(
            literal(LiteralValue::Date(-719_528)),
            "CAST('0001-01-01 BC' AS date)"
        );
        assert_eq!(
            literal(LiteralValue::Date(-719_162)),
            "CAST('0001-01-01' AS date)"
        );
        assert_eq!(
            literal(LiteralValue::Timestamp(-1)),
            "CAST('1969-12-31 23:59:59.999999' AS timestamp)"
        );
        assert_eq!(
            literal(LiteralValue::TimestampTz(1_577_836_800_000_001)),
            "CAST('2020-01-01 00:00:00.000001+00' AS timestamptz)"
        );
        assert_eq!(
            literal(LiteralValue::Timestamp(-62_135_596_800_000_001)),
            "CAST('0001-12-31 23:59:59.999999 BC' AS timestamp)"
        );
        assert_eq!(
            literal(LiteralValue::Time(45_296_000_007)),
            "CAST('12:34:56.000007' AS time)"
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
            "CAST('-0.005' AS numeric)"
        );
        assert_eq!(
            literal(LiteralValue::Decimal {
                value: 12,
                precision: 4,
                scale: -2
            }),
            "CAST('1200' AS numeric)"
        );
        assert_eq!(
            literal(LiteralValue::Decimal {
                value: 99_999,
                precision: 5,
                scale: -2
            }),
            "CAST('9999900' AS numeric)"
        );
        assert_eq!(
            literal(LiteralValue::BigInt(i64::MIN)),
            "CAST(-9223372036854775808 AS bigint)"
        );
        assert_eq!(
            literal(LiteralValue::Double(f64::NAN)),
            "CAST('NaN' AS double precision)"
        );
        assert_eq!(literal(LiteralValue::Float(0.1)), "CAST('0.1' AS real)");
    }

    #[test]
    fn logical_literals_name_their_postgresql_type() {
        assert_eq!(
            literal(LiteralValue::Logical {
                data_type: AnalyticsType::Uuid,
                carrier: Box::new(LiteralValue::Text(
                    "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".to_owned()
                )),
            }),
            "CAST('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11' AS uuid)"
        );
        assert_eq!(
            literal(LiteralValue::Logical {
                data_type: AnalyticsType::Enum(EnumType::new(vec!["ok".to_owned()])),
                carrier: Box::new(LiteralValue::Text("ok".to_owned())),
            }),
            "'ok'"
        );
    }
}
