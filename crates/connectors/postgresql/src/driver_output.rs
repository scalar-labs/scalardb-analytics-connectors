//! Converting the ADBC PostgreSQL driver's output to Analytics values.
//!
//! The driver returns most mapped types already in their Analytics Arrow
//! storage type, and those columns pass through. The rest arrive in a form the
//! driver chose, and each is converted without changing any value:
//!
//! | Mapping rule | Driver output | Conversion |
//! | --- | --- | --- |
//! | `numeric(p,s)` | Utf8 decimal text | Text to `Decimal128(p, s)`; `NaN` and infinities are `data` errors |
//! | `uuid` | FixedSizeBinary(16) | Bytes to canonical text |
//! | `time with time zone` | Binary wire value | Wire value to PostgreSQL's text form |
//!
//! `json`, `jsonb`, and enums arrive as Utf8, which is their storage.

use std::sync::Arc;

use analytics_types::AnalyticsType;
use arrow_array::{
    Array, ArrayRef, BinaryArray, FixedSizeBinaryArray, StringArray, builder::StringBuilder,
};
use connector_sdk::{
    ConnectorError, ErrorKind,
    convert::{decimal128_from_text, downcast, uuid_text_from_bytes},
};

/// Converts one column of the driver's output to `target`'s Analytics storage.
pub(crate) fn convert_column(
    column: &ArrayRef,
    target: &AnalyticsType,
) -> Result<ArrayRef, ConnectorError> {
    Ok(match target {
        AnalyticsType::Decimal(decimal) => Arc::new(decimal128_from_text(
            downcast::<StringArray>(column)?,
            decimal.precision(),
            decimal.scale(),
        )?),
        AnalyticsType::Uuid => Arc::new(uuid_text_from_bytes(downcast::<FixedSizeBinaryArray>(
            column,
        )?)?),
        AnalyticsType::TimeWithTimeZone => Arc::new(timetz_text(downcast::<BinaryArray>(column)?)?),
        _ => Arc::clone(column),
    })
}

/// Formats `timetz` wire values as PostgreSQL prints them, such as `10:00:00+09`.
///
/// A wire value is the big-endian time of day in microseconds followed by the
/// big-endian zone offset in seconds west of UTC.
fn timetz_text(values: &BinaryArray) -> Result<StringArray, ConnectorError> {
    let mut builder = StringBuilder::with_capacity(values.len(), values.len() * 16);
    for value in values.iter() {
        let Some(value) = value else {
            builder.append_null();
            continue;
        };
        let (Ok(micros), Ok(zone)) = (
            <[u8; 8]>::try_from(value.get(..8).unwrap_or_default()),
            <[u8; 4]>::try_from(value.get(8..).unwrap_or_default()),
        ) else {
            return Err(malformed_timetz());
        };
        builder.append_value(format_timetz(
            i64::from_be_bytes(micros),
            i32::from_be_bytes(zone),
        ));
    }
    Ok(builder.finish())
}

/// PostgreSQL's `timetz` output: fractional seconds without trailing zeros, and
/// zone minutes and seconds only when they are not zero.
fn format_timetz(micros: i64, seconds_west: i32) -> String {
    let seconds = micros / 1_000_000;
    let fraction = micros % 1_000_000;
    let mut text = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    );
    if fraction != 0 {
        let digits = format!("{fraction:06}");
        text.push('.');
        text.push_str(digits.trim_end_matches('0'));
    }
    let offset = -i64::from(seconds_west);
    text.push(if offset < 0 { '-' } else { '+' });
    let offset = offset.abs();
    text.push_str(&format!("{:02}", offset / 3_600));
    let (minutes, seconds) = (offset / 60 % 60, offset % 60);
    if minutes != 0 || seconds != 0 {
        text.push_str(&format!(":{minutes:02}"));
    }
    if seconds != 0 {
        text.push_str(&format!(":{seconds:02}"));
    }
    text
}

fn malformed_timetz() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Internal,
        false,
        "the ADBC driver returned a malformed time with time zone value",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timetz_matches_postgresql_output() {
        assert_eq!(format_timetz(36_000_000_000, -32_400), "10:00:00+09");
        assert_eq!(format_timetz(36_000_250_000, -19_800), "10:00:00.25+05:30");
        assert_eq!(format_timetz(0, 0), "00:00:00+00");
        assert_eq!(format_timetz(86_400_000_000, 3_600), "24:00:00-01");
        assert_eq!(format_timetz(1, 3_723), "00:00:00.000001-01:02:03");
    }
}
