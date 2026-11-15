//! MySQL spelling of identifiers and typed literals.
//!
//! Literals rely on the fixed session settings: `sql_mode` holds
//! `NO_BACKSLASH_ESCAPES`, under which a backslash in a quoted string is an
//! ordinary character, and `time_zone` is `+00:00`, under which a `TIMESTAMP`
//! column compares with a datetime literal as a UTC instant.
//!
//! [`pushdown`](crate::pushdown) admits only literals that MySQL reads as the
//! same value; the spellings of the others are never sent.

use analytics_types::AnalyticsType;
use connector_sdk::LiteralValue;
use connector_sql::{
    SqlDialect,
    literal::{MICROS_PER_DAY, civil_from_days, decimal_text, quote_string, time_text},
};

use crate::MysqlConnector;

impl SqlDialect for MysqlConnector {
    /// Quotes with backticks, doubling embedded backticks.
    fn quote_identifier(identifier: &str) -> String {
        format!("`{}`", identifier.replace('`', "``"))
    }

    fn write_literal(literal: &LiteralValue, sql: &mut String) {
        sql.push_str(&literal_sql(literal));
    }
}

fn literal_sql(literal: &LiteralValue) -> String {
    match literal {
        LiteralValue::Boolean(value) => (if *value { "TRUE" } else { "FALSE" }).to_owned(),
        LiteralValue::Byte(value) => value.to_string(),
        LiteralValue::SmallInt(value) => value.to_string(),
        LiteralValue::Int(value) => value.to_string(),
        LiteralValue::BigInt(value) => value.to_string(),
        LiteralValue::Float(value) => format!("CAST({value} AS FLOAT)"),
        LiteralValue::Double(value) => format!("CAST({value} AS DOUBLE)"),
        // An exact-value literal: MySQL reads digits with a point as DECIMAL.
        LiteralValue::Decimal { value, scale, .. } => decimal_text(*value, *scale),
        LiteralValue::Text(value) => quote_string(value),
        LiteralValue::Blob(value) => {
            let hex: String = value.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("X'{hex}'")
        }
        LiteralValue::Date(days) => format!("DATE '{}'", date_text(i64::from(*days))),
        LiteralValue::Time(micros) => format!("TIME '{}'", time_text(*micros)),
        LiteralValue::Timestamp(micros) | LiteralValue::TimestampTz(micros) => {
            format!("CAST('{}' AS DATETIME(6))", timestamp_text(*micros))
        }
        LiteralValue::Duration(micros) => quote_string(&format!("{micros} microseconds")),
        LiteralValue::Interval {
            months,
            days,
            nanoseconds,
        } => quote_string(&format!(
            "{months} months {days} days {nanoseconds} nanoseconds"
        )),
        LiteralValue::Null { .. } => "NULL".to_owned(),
        // A decimal wider than Substrait's arrives as its validated canonical
        // text, which is also MySQL's exact-value spelling; quoted, MySQL
        // would compare it as a floating-point number.
        LiteralValue::Logical {
            data_type: AnalyticsType::Decimal(_),
            carrier,
        } => match carrier.as_ref() {
            LiteralValue::Text(text) => text.clone(),
            carrier => literal_sql(carrier),
        },
        // Unsigned integers arrive in a wider signed integer or a decimal,
        // whose spelling is the same number.
        LiteralValue::Logical { carrier, .. } => literal_sql(carrier),
    }
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
    use analytics_types::EnumType;

    use super::*;

    fn literal(value: LiteralValue) -> String {
        let mut sql = String::new();
        MysqlConnector::write_literal(&value, &mut sql);
        sql
    }

    #[test]
    fn identifiers_and_strings_are_quoted_and_escaped() {
        assert_eq!(MysqlConnector::quote_identifier("a`b"), "`a``b`");
        assert_eq!(
            literal(LiteralValue::Text("it's \\".to_owned())),
            "'it''s \\'"
        );
        assert_eq!(literal(LiteralValue::Blob(vec![0x00, 0xab])), "X'00AB'");
    }

    #[test]
    fn temporal_literals_use_mysql_input_forms() {
        assert_eq!(literal(LiteralValue::Date(0)), "DATE '1970-01-01'");
        assert_eq!(literal(LiteralValue::Date(-719_162)), "DATE '0001-01-01'");
        assert_eq!(
            literal(LiteralValue::Timestamp(-1)),
            "CAST('1969-12-31 23:59:59.999999' AS DATETIME(6))"
        );
        assert_eq!(
            literal(LiteralValue::TimestampTz(1_577_836_800_000_001)),
            "CAST('2020-01-01 00:00:00.000001' AS DATETIME(6))"
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
        assert_eq!(
            literal(LiteralValue::Decimal {
                value: 12,
                precision: 4,
                scale: -2
            }),
            "1200"
        );
        assert_eq!(
            literal(LiteralValue::BigInt(i64::MIN)),
            "-9223372036854775808"
        );
    }

    #[test]
    fn unsigned_and_wide_decimal_literals_are_numbers() {
        use analytics_types::DecimalType;
        assert_eq!(
            literal(LiteralValue::Logical {
                data_type: AnalyticsType::UnsignedBigInt,
                carrier: Box::new(LiteralValue::Decimal {
                    value: i128::from(u64::MAX),
                    precision: 20,
                    scale: 0
                }),
            }),
            "18446744073709551615"
        );
        let wide = format!("-{}.{}", "9".repeat(55), "9".repeat(10));
        assert_eq!(
            literal(LiteralValue::Logical {
                data_type: AnalyticsType::Decimal(DecimalType::new(65, 10).unwrap()),
                carrier: Box::new(LiteralValue::Text(wide.clone())),
            }),
            wide
        );
    }

    #[test]
    fn enum_labels_are_strings() {
        assert_eq!(
            literal(LiteralValue::Logical {
                data_type: AnalyticsType::Enum(EnumType::new(vec!["ok".to_owned()])),
                carrier: Box::new(LiteralValue::Text("ok".to_owned())),
            }),
            "'ok'"
        );
    }
}
