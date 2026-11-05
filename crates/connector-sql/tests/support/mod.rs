use std::fmt::Write;

use connector_sdk::LiteralValue;
use connector_sql::SqlDialect;

/// A diagnostic dialect that makes values received from `LogicalPlan` visible
/// without claiming the spelling rules of any supported Data Source.
pub(crate) struct TestDialect;

impl SqlDialect for TestDialect {
    fn quote_identifier(&self, identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    fn write_literal(&self, literal: &LiteralValue, sql: &mut String) {
        match literal {
            LiteralValue::Boolean(value) => write_marker(sql, "boolean", value),
            LiteralValue::Byte(value) => write_marker(sql, "byte", value),
            LiteralValue::SmallInt(value) => write_marker(sql, "small_int", value),
            LiteralValue::Int(value) => write_marker(sql, "int", value),
            LiteralValue::BigInt(value) => write_marker(sql, "big_int", value),
            LiteralValue::Float(value) => write_marker(sql, "float", value),
            LiteralValue::Double(value) => write_marker(sql, "double", value),
            LiteralValue::Text(value) => write_marker(sql, "text", value),
            LiteralValue::Blob(value) => {
                sql.push_str("test_literal('blob:");
                for byte in value {
                    write!(sql, "{byte:02x}").unwrap();
                }
                sql.push_str("')");
            }
            LiteralValue::Date(value) => write_marker(sql, "date", value),
            LiteralValue::Time(value) => write_marker(sql, "time", value),
            LiteralValue::Timestamp(value) => write_marker(sql, "timestamp", value),
            LiteralValue::TimestampTz(value) => write_marker(sql, "timestamp_tz", value),
            LiteralValue::Duration(value) => write_marker(sql, "duration", value),
            LiteralValue::Interval {
                months,
                days,
                nanoseconds,
            } => write_marker(
                sql,
                "interval",
                format_args!("{months},{days},{nanoseconds}"),
            ),
            LiteralValue::Decimal {
                value,
                precision,
                scale,
            } => write_marker(sql, "decimal", format_args!("{value},{precision},{scale}")),
            LiteralValue::Null { data_type } => {
                write_marker(sql, "null", format_args!("{data_type:?}"));
            }
            LiteralValue::Logical { data_type, carrier } => {
                write!(sql, "test_logical('{data_type:?}', ").unwrap();
                self.write_literal(carrier, sql);
                sql.push(')');
            }
        }
    }
}

fn write_marker(sql: &mut String, kind: &str, value: impl std::fmt::Display) {
    write!(sql, "test_literal('{kind}:").unwrap();
    for character in value.to_string().chars() {
        if character == '\'' {
            sql.push('\'');
        }
        sql.push(character);
    }
    sql.push_str("')");
}
