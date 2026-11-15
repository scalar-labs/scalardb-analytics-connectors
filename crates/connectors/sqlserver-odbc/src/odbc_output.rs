//! Converting what ODBC reads from SQL Server to Analytics values.
//!
//! `arrow-odbc` reads most mapped types already in their Analytics Arrow
//! storage type, and those columns pass through. The rest arrive in a form
//! chosen by [`OdbcConnector::read_type`](connector_odbc::OdbcConnector::read_type)
//! or by `arrow-odbc`, and each is converted without changing any value:
//!
//! | Mapping rule | ODBC output | Conversion |
//! | --- | --- | --- |
//! | `decimal`, `numeric`, `money`, `smallmoney` | Utf8 decimal text | Text to the narrowest Arrow decimal of the precision |
//! | `datetime`, `smalldatetime`, `datetime2(p)` up to 6 digits | Timestamp in seconds, milliseconds, or microseconds | Aligned to microseconds |
//! | `datetime2(7)` | Utf8 `YYYY-MM-DD hh:mm:ss.fffffff` | Text to microseconds |
//! | `time(p)` | Utf8 `hh:mm:ss[.fffffff]` | Text to microseconds |
//! | `datetimeoffset(p)` | Utf8 `YYYY-MM-DD hh:mm:ss[.fffffff] +hh:mm` | Text to the UTC instant in microseconds |
//! | `uniqueidentifier` | Utf8 in uppercase | Lowercase canonical text |
//!
//! A time or timestamp whose seventh fractional digit is not zero holds a
//! fraction of a microsecond, which the Analytics types cannot carry; it is
//! a `data` error.

use std::sync::Arc;

use analytics_types::AnalyticsType;
use arrow_array::{
    Array, ArrayRef, StringArray, Time64MicrosecondArray, TimestampMicrosecondArray,
    builder::StringBuilder,
};
use arrow_schema::DataType;
use connector_sdk::{
    ConnectorError,
    convert::{decimal_from_text, downcast, not_carried, timestamps_to_micros},
};
use connector_sql::literal::MICROS_PER_DAY;

const MICROS_PER_SECOND: i64 = 1_000_000;

/// Converts one column of ODBC output to `target`'s Analytics storage.
pub(crate) fn convert_column(
    column: &ArrayRef,
    target: &AnalyticsType,
) -> Result<ArrayRef, ConnectorError> {
    Ok(match target {
        AnalyticsType::Decimal(decimal) => {
            decimal_from_text(downcast::<StringArray>(column)?, *decimal)?
        }
        AnalyticsType::Time => {
            let values = parse_each(column, "Time", time_micros)?;
            Arc::new(Time64MicrosecondArray::from(values))
        }
        AnalyticsType::Timestamp => match column.data_type() {
            DataType::Utf8 => Arc::new(TimestampMicrosecondArray::from(parse_each(
                column,
                "Timestamp",
                timestamp_micros,
            )?)),
            _ => Arc::new(timestamps_to_micros(column)?),
        },
        AnalyticsType::TimestampTz => {
            let values = parse_each(column, "TimestampTz", offset_timestamp_micros)?;
            Arc::new(TimestampMicrosecondArray::from(values).with_timezone("UTC"))
        }
        AnalyticsType::Uuid => Arc::new(canonical_uuids(downcast::<StringArray>(column)?)?),
        _ => Arc::clone(column),
    })
}

/// Parses every text value of `column` with `parse`; a value it rejects is a
/// `data` error naming `target`.
fn parse_each(
    column: &ArrayRef,
    target: &str,
    parse: impl Fn(&str) -> Option<i64>,
) -> Result<Vec<Option<i64>>, ConnectorError> {
    downcast::<StringArray>(column)?
        .iter()
        .map(|value| {
            value
                .map(|value| parse(value).ok_or_else(|| not_carried(target)))
                .transpose()
        })
        .collect()
}

/// Parses `digits` decimal digits exactly, with no sign.
fn number(digits: &str) -> Option<i64> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Parses SQL Server's `hh:mm:ss[.fffffff]` as microseconds since midnight.
///
/// A fraction with a nonzero digit beyond the sixth is not carried.
fn time_micros(text: &str) -> Option<i64> {
    let (clock, fraction) = text.split_once('.').unwrap_or((text, ""));
    let mut parts = clock.split(':');
    let (hour, minute, second) = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 || fraction.len() > 7 {
        return None;
    }
    let mut micros = 0;
    if !fraction.is_empty() {
        let padded = format!("{fraction:0<7}");
        let ticks = number(&padded)?;
        if ticks % 10 != 0 {
            return None;
        }
        micros = ticks / 10;
    }
    Some(((hour * 60 + minute) * 60 + second) * MICROS_PER_SECOND + micros)
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// Parses SQL Server's `YYYY-MM-DD hh:mm:ss[.fffffff]` as microseconds since
/// the Unix epoch.
fn timestamp_micros(text: &str) -> Option<i64> {
    let (date, time) = text.split_once(' ')?;
    let mut parts = date.split('-');
    let (year, month, day) = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    if parts.next().is_some() {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    Some(days * MICROS_PER_DAY + time_micros(time)?)
}

/// Parses SQL Server's `YYYY-MM-DD hh:mm:ss[.fffffff] ±hh:mm` as the UTC
/// instant in microseconds since the Unix epoch.
fn offset_timestamp_micros(text: &str) -> Option<i64> {
    let (local, offset) = text.rsplit_once(' ')?;
    let (sign, offset) = match offset.as_bytes().first()? {
        b'+' => (1, &offset[1..]),
        b'-' => (-1, &offset[1..]),
        _ => return None,
    };
    let (hours, minutes) = offset.split_once(':')?;
    let (hours, minutes) = (number(hours)?, number(minutes)?);
    if hours > 14 || minutes > 59 {
        return None;
    }
    let offset = sign * (hours * 60 + minutes) * 60 * MICROS_PER_SECOND;
    Some(timestamp_micros(local)? - offset)
}

/// Writes `uniqueidentifier` text, which SQL Server spells in uppercase, in
/// the canonical lowercase form.
fn canonical_uuids(values: &StringArray) -> Result<StringArray, ConnectorError> {
    let mut builder = StringBuilder::with_capacity(values.len(), values.len() * 36);
    for value in values {
        match value {
            Some(value) => {
                let canonical = value.len() == 36
                    && value.bytes().enumerate().all(|(index, byte)| match index {
                        8 | 13 | 18 | 23 => byte == b'-',
                        _ => byte.is_ascii_hexdigit(),
                    });
                if !canonical {
                    return Err(not_carried("Uuid"));
                }
                builder.append_value(value.to_ascii_lowercase());
            }
            None => builder.append_null(),
        }
    }
    Ok(builder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_column(values: &[Option<&str>]) -> ArrayRef {
        Arc::new(StringArray::from(values.to_vec()))
    }

    #[test]
    fn temporal_text_converts_to_microseconds_exactly() {
        let times = convert_column(
            &text_column(&[Some("23:59:59.9999990"), Some("00:00:00"), None]),
            &AnalyticsType::Time,
        )
        .unwrap();
        let times = times
            .as_any()
            .downcast_ref::<Time64MicrosecondArray>()
            .unwrap();
        assert_eq!(times.value(0), MICROS_PER_DAY - 1);
        assert_eq!(times.value(1), 0);
        assert!(times.is_null(2));

        let timestamps = convert_column(
            &text_column(&[
                Some("0001-01-01 00:00:00.0000000"),
                Some("9999-12-31 23:59:59.9999990"),
            ]),
            &AnalyticsType::Timestamp,
        )
        .unwrap();
        let timestamps = timestamps
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(timestamps.value(0), -62_135_596_800_000_000);
        assert_eq!(timestamps.value(1), 253_402_300_799_999_999);

        let instants = convert_column(
            &text_column(&[Some("2020-01-01 09:00:00.0000010 +09:00")]),
            &AnalyticsType::TimestampTz,
        )
        .unwrap();
        let instants = instants
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(instants.value(0), 1_577_836_800_000_001);
        assert_eq!(instants.timezone(), Some("UTC"));
    }

    #[test]
    fn fractions_of_a_microsecond_are_not_carried() {
        for (value, target) in [
            ("12:00:00.0000001", AnalyticsType::Time),
            ("2020-01-01 00:00:00.1234567", AnalyticsType::Timestamp),
            (
                "2020-01-01 00:00:00.1234567 +00:00",
                AnalyticsType::TimestampTz,
            ),
        ] {
            let error = convert_column(&text_column(&[Some(value)]), &target).unwrap_err();
            assert_eq!(error.kind(), connector_sdk::ErrorKind::Data, "{value}");
        }
    }

    #[test]
    fn uniqueidentifiers_are_written_in_canonical_form() {
        let uuids = convert_column(
            &text_column(&[Some("A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11")]),
            &AnalyticsType::Uuid,
        )
        .unwrap();
        let uuids = uuids.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(uuids.value(0), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
    }
}
