//! SQL Server spelling of identifiers and typed literals.
//!
//! Every literal is typed with `CAST`, so that SQL Server compares it as the
//! Analytics type it carries. Dates and times are written in the ISO 8601
//! forms SQL Server reads the same under every `LANGUAGE` and `DATEFORMAT`
//! setting.

use analytics_types::AnalyticsType;
use connector_sdk::LiteralValue;
use connector_sql::{
    SqlDialect,
    literal::{MICROS_PER_DAY, cast, civil_from_days, decimal_text, time_text},
};

use crate::{SqlServerConnector, catalog::nstring};

impl SqlDialect for SqlServerConnector {
    /// Encloses the identifier in brackets, doubling closing brackets.
    fn quote_identifier(identifier: &str) -> String {
        format!("[{}]", identifier.replace(']', "]]"))
    }

    fn write_literal(literal: &LiteralValue, sql: &mut String) {
        sql.push_str(&literal_sql(literal));
    }
}

fn literal_sql(literal: &LiteralValue) -> String {
    let typed = |text: &str, type_name: &str| cast(&nstring(text), type_name);
    match literal {
        LiteralValue::Boolean(value) => cast(if *value { "1" } else { "0" }, "bit"),
        LiteralValue::Byte(value) => cast(&value.to_string(), "smallint"),
        LiteralValue::SmallInt(value) => cast(&value.to_string(), "smallint"),
        LiteralValue::Int(value) => cast(&value.to_string(), "int"),
        LiteralValue::BigInt(value) => cast(&value.to_string(), "bigint"),
        LiteralValue::Float(value) => cast(&float_text(f64::from(*value)), "real"),
        LiteralValue::Double(value) => cast(&float_text(*value), "float"),
        LiteralValue::Text(value) => typed(value, "nvarchar(max)"),
        LiteralValue::Blob(value) => {
            let hex: String = value.iter().map(|byte| format!("{byte:02X}")).collect();
            cast(&format!("0x{hex}"), "varbinary(max)")
        }
        LiteralValue::Date(days) => typed(&date_text(i64::from(*days)), "date"),
        LiteralValue::Time(micros) => typed(&time_text(*micros), "time(7)"),
        LiteralValue::Timestamp(micros) => typed(&timestamp_text(*micros), "datetime2(7)"),
        LiteralValue::TimestampTz(micros) => typed(
            &format!("{}+00:00", timestamp_text(*micros)),
            "datetimeoffset(7)",
        ),
        LiteralValue::Decimal {
            value,
            precision,
            scale,
        } => typed(
            &decimal_text(*value, *scale),
            &format!("decimal({precision}, {})", scale.max(&0)),
        ),
        LiteralValue::Null { data_type } => cast("NULL", &type_name(data_type)),
        LiteralValue::Logical { data_type, carrier } => match carrier.as_ref() {
            LiteralValue::Text(text) => typed(text, &type_name(data_type)),
            carrier => cast(&literal_sql(carrier), &type_name(data_type)),
        },
        // No column maps to these types, so no comparison with one runs at
        // SQL Server; they are still written as values of their meaning.
        LiteralValue::Duration(micros) => cast(&micros.to_string(), "bigint"),
        LiteralValue::Interval { .. } => cast("NULL", "sql_variant"),
    }
}

/// The SQL Server type that carries an Analytics type, for typed literals.
fn type_name(data_type: &AnalyticsType) -> String {
    let name = match data_type {
        AnalyticsType::Boolean => "bit",
        AnalyticsType::UnsignedByte => "tinyint",
        AnalyticsType::Byte | AnalyticsType::SmallInt => "smallint",
        AnalyticsType::Int | AnalyticsType::UnsignedSmallInt => "int",
        AnalyticsType::BigInt | AnalyticsType::UnsignedInt => "bigint",
        AnalyticsType::UnsignedBigInt => "decimal(20, 0)",
        AnalyticsType::Float => "real",
        AnalyticsType::Double => "float",
        AnalyticsType::Decimal(decimal) => {
            return format!(
                "decimal({}, {})",
                decimal.precision().min(38),
                decimal.scale().max(0)
            );
        }
        AnalyticsType::Blob => "varbinary(max)",
        AnalyticsType::Date => "date",
        AnalyticsType::Time => "time(7)",
        AnalyticsType::Timestamp => "datetime2(7)",
        AnalyticsType::TimestampTz => "datetimeoffset(7)",
        AnalyticsType::Uuid => "uniqueidentifier",
        AnalyticsType::Text
        | AnalyticsType::Json
        | AnalyticsType::Enum(_)
        | AnalyticsType::TimeWithTimeZone => "nvarchar(max)",
        AnalyticsType::Year => "smallint",
        AnalyticsType::Duration => "bigint",
        AnalyticsType::Interval
        | AnalyticsType::Array(_)
        | AnalyticsType::Map(_)
        | AnalyticsType::Struct(_) => "sql_variant",
    };
    name.to_owned()
}

/// The shortest decimal text that reads back as `value`.
///
/// SQL Server's `float` has no NaN or infinities; such a value never runs
/// remotely, and is written as NULL so that the statement stays valid.
fn float_text(value: f64) -> String {
    if value.is_finite() {
        format!("{value:?}")
    } else {
        "NULL".to_owned()
    }
}

/// Formats days from the Unix epoch as `YYYY-MM-DD`.
fn date_text(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Formats microseconds from the Unix epoch as ISO 8601 `YYYY-MM-DDThh:mm:ss.ffffff`.
fn timestamp_text(micros: i64) -> String {
    let days = micros.div_euclid(MICROS_PER_DAY);
    let time = time_text(micros.rem_euclid(MICROS_PER_DAY));
    format!("{}T{time}", date_text(days))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(value: LiteralValue) -> String {
        let mut sql = String::new();
        SqlServerConnector::write_literal(&value, &mut sql);
        sql
    }

    #[test]
    fn identifiers_and_strings_are_quoted_and_escaped() {
        assert_eq!(SqlServerConnector::quote_identifier("a]b"), "[a]]b]");
        assert_eq!(
            literal(LiteralValue::Text("it's".to_owned())),
            "CAST(N'it''s' AS nvarchar(max))"
        );
    }

    #[test]
    fn temporal_literals_use_iso_8601_forms() {
        assert_eq!(
            literal(LiteralValue::Date(-719_162)),
            "CAST(N'0001-01-01' AS date)"
        );
        assert_eq!(
            literal(LiteralValue::Timestamp(-1)),
            "CAST(N'1969-12-31T23:59:59.999999' AS datetime2(7))"
        );
        assert_eq!(
            literal(LiteralValue::Time(45_296_000_007)),
            "CAST(N'12:34:56.000007' AS time(7))"
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
            "CAST(N'-0.005' AS decimal(5, 3))"
        );
        assert_eq!(literal(LiteralValue::Double(0.1)), "CAST(0.1 AS float)");
        assert_eq!(
            literal(LiteralValue::Logical {
                data_type: AnalyticsType::UnsignedByte,
                carrier: Box::new(LiteralValue::SmallInt(255)),
            }),
            "CAST(CAST(255 AS smallint) AS tinyint)"
        );
    }
}
