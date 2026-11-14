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
//! | `date`, `timestamp`, `timestamp with time zone` | Their Analytics storage | Checked against PostgreSQL's range |
//!
//! `json`, `jsonb`, and enums arrive as Utf8, which is their storage.
//!
//! The driver moves dates and timestamps from PostgreSQL's 2000 epoch to the
//! Unix epoch with wrapping arithmetic, so `infinity`, `-infinity`, and
//! timestamps beyond the range of Arrow microsecond timestamps arrive as
//! wrong values. Each of them wraps to below PostgreSQL's earliest value,
//! 4714-11-24 BC, which no value the server stores can be, so a value below it
//! is a `data` error.

use std::sync::Arc;

use analytics_types::AnalyticsType;
use arrow_array::{
    Array, ArrayRef, BinaryArray, Date32Array, FixedSizeBinaryArray, StringArray,
    TimestampMicrosecondArray, builder::StringBuilder,
};
use connector_sdk::{
    ConnectorError, ErrorKind,
    convert::{decimal128_from_text, downcast, not_carried, uuid_text_from_bytes},
};

/// PostgreSQL's earliest date, 4714-11-24 BC, in days since the Unix epoch.
const EARLIEST_DATE: i32 = -2_440_588;

/// PostgreSQL's earliest timestamp, 4714-11-24 BC 00:00, in microseconds
/// since the Unix epoch.
const EARLIEST_TIMESTAMP: i64 = EARLIEST_DATE as i64 * 86_400_000_000;

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
        AnalyticsType::Date => {
            let dates = downcast::<Date32Array>(column)?;
            if dates.iter().flatten().any(|days| days < EARLIEST_DATE) {
                return Err(not_carried("Date"));
            }
            Arc::clone(column)
        }
        AnalyticsType::Timestamp | AnalyticsType::TimestampTz => {
            let timestamps = downcast::<TimestampMicrosecondArray>(column)?;
            if timestamps
                .iter()
                .flatten()
                .any(|micros| micros < EARLIEST_TIMESTAMP)
            {
                return Err(not_carried("a microsecond timestamp"));
            }
            Arc::clone(column)
        }
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
    fn dates_before_postgresql_range_are_data_errors() {
        let convert = |days: Vec<Option<i32>>| {
            let column: ArrayRef = Arc::new(Date32Array::from(days));
            convert_column(&column, &AnalyticsType::Date).map(|_| ())
        };
        assert!(convert(vec![Some(EARLIEST_DATE), None, Some(i32::MAX)]).is_ok());
        // `infinity` as the driver returns it.
        let error = convert(vec![Some(1), Some(-2_147_472_692)]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Data);
        assert!(convert(vec![Some(EARLIEST_DATE - 1)]).is_err());
    }

    #[test]
    fn timestamps_before_postgresql_range_are_data_errors() {
        for target in [AnalyticsType::Timestamp, AnalyticsType::TimestampTz] {
            let convert = |micros: Vec<Option<i64>>| {
                let column: ArrayRef = Arc::new(TimestampMicrosecondArray::from(micros));
                convert_column(&column, &target).map(|_| ())
            };
            assert!(convert(vec![Some(EARLIEST_TIMESTAMP), None, Some(i64::MAX)]).is_ok());
            // `infinity` and 294276-12-31 23:59:59.999999 as the driver returns them.
            for wrapped in [-9_222_425_352_054_775_809, -9_222_426_057_709_551_617] {
                let error = convert(vec![Some(wrapped)]).unwrap_err();
                assert_eq!(error.kind(), ErrorKind::Data);
            }
            assert!(convert(vec![Some(EARLIEST_TIMESTAMP - 1)]).is_err());
        }
    }

    #[test]
    fn timetz_matches_postgresql_output() {
        assert_eq!(format_timetz(36_000_000_000, -32_400), "10:00:00+09");
        assert_eq!(format_timetz(36_000_250_000, -19_800), "10:00:00.25+05:30");
        assert_eq!(format_timetz(0, 0), "00:00:00+00");
        assert_eq!(format_timetz(86_400_000_000, 3_600), "24:00:00-01");
        assert_eq!(format_timetz(1, 3_723), "00:00:00.000001-01:02:03");
    }
}
