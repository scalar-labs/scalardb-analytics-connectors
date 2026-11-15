//! Converting the ADBC MySQL driver's output to Analytics values.
//!
//! The driver returns most mapped types already in their Analytics Arrow
//! storage type, and those columns pass through: signed and unsigned integers
//! in their own widths, and decimals in the narrowest Arrow decimal of their
//! precision. The rest arrive in a form the driver chose, and each is
//! converted without changing any value:
//!
//! | Mapping rule | Driver output | Conversion |
//! | --- | --- | --- |
//! | `datetime(fsp)`, `timestamp(fsp)` with fsp 0 to 3 | Timestamp in seconds for fsp 0 and milliseconds for 1 to 3; `timestamp` in UTC | Aligned to microseconds |
//! | `year` | Utf8 four-digit text | Text to Int16; the zero year `0000` is a `data` error |
//! | `tinyint(1)` with `tinyint1_as_boolean` | Int8 | 0 and 1 to Boolean; another value is a `data` error |
//! | `bit(1)` | Binary of one byte | The byte 0 or 1 to Boolean |
//! | `enum(...)` | Utf8 member text | Unchanged; a value that is no member, the error value `''`, is a `data` error |
//!
//! `json` arrives as Utf8, which is its storage.

use std::sync::Arc;

use analytics_types::{
    AnalyticsType,
    arrow_schema::{DataType, TimeUnit},
};
use arrow_array::{ArrayRef, BinaryArray, BooleanArray, Int8Array, Int16Array, StringArray};
use connector_sdk::{
    ConnectorError,
    convert::{downcast, not_carried, timestamps_to_micros},
};

/// Converts one column of the driver's output to `target`'s Analytics storage.
pub(crate) fn convert_column(
    column: &ArrayRef,
    target: &AnalyticsType,
) -> Result<ArrayRef, ConnectorError> {
    Ok(match (column.data_type(), target) {
        (DataType::Timestamp(unit, _), AnalyticsType::Timestamp | AnalyticsType::TimestampTz)
            if *unit != TimeUnit::Microsecond =>
        {
            Arc::new(timestamps_to_micros(column)?)
        }
        (DataType::Utf8, AnalyticsType::Year) => Arc::new(years(downcast::<StringArray>(column)?)?),
        (DataType::Int8, AnalyticsType::Boolean) => Arc::new(booleans(
            downcast::<Int8Array>(column)?
                .iter()
                .map(|value| value.map(i16::from)),
        )?),
        (DataType::Binary, AnalyticsType::Boolean) => Arc::new(booleans(
            downcast::<BinaryArray>(column)?
                .iter()
                .map(|value| value.map(|bytes| bits(bytes).unwrap_or(-1))),
        )?),
        (DataType::Utf8, AnalyticsType::Enum(enum_type)) => {
            check_members(downcast::<StringArray>(column)?, enum_type.members())?;
            Arc::clone(column)
        }
        _ => Arc::clone(column),
    })
}

/// Checks that every enum value is one of `members`. A non-strict `sql_mode`
/// stores an invalid value as the error value `''`, which is no member unless
/// the enum declares `''`, and then reads the same as that member.
fn check_members(values: &StringArray, members: &[String]) -> Result<(), ConnectorError> {
    let is_member = |value: &str| members.iter().any(|member| member == value);
    if values.iter().flatten().all(is_member) {
        Ok(())
    } else {
        Err(not_carried("Enum"))
    }
}

/// Reads booleans from integers that must be 0 or 1.
fn booleans(values: impl Iterator<Item = Option<i16>>) -> Result<BooleanArray, ConnectorError> {
    values
        .map(|value| {
            value
                .map(|value| match value {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(not_carried("Boolean")),
                })
                .transpose()
        })
        .collect()
}

/// The value of a `bit(1)` the driver returns as its one byte.
fn bits(bytes: &[u8]) -> Option<i16> {
    match bytes {
        [byte] => Some(i16::from(*byte)),
        _ => None,
    }
}

/// Reads `YEAR` values, which the driver returns as their text. MySQL's
/// years run from 1901 to 2155; its zero year `0000` is no year.
fn years(text: &StringArray) -> Result<Int16Array, ConnectorError> {
    text.iter()
        .map(|value| {
            value
                .map(|value| {
                    value
                        .parse::<i16>()
                        .ok()
                        .filter(|year| *year != 0)
                        .ok_or_else(|| not_carried("Year"))
                })
                .transpose()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use connector_sdk::ErrorKind;

    use super::*;

    #[test]
    fn years_are_read_from_their_text() {
        let column: ArrayRef = Arc::new(StringArray::from(vec![Some("1901"), None, Some("2155")]));
        let years = convert_column(&column, &AnalyticsType::Year).unwrap();
        assert_eq!(
            years.as_ref(),
            &Int16Array::from(vec![Some(1901), None, Some(2155)])
        );
        let zero: ArrayRef = Arc::new(StringArray::from(vec!["0000"]));
        let error = convert_column(&zero, &AnalyticsType::Year).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Data);
    }

    #[test]
    fn booleans_are_read_from_zero_and_one() {
        let tinyint: ArrayRef = Arc::new(Int8Array::from(vec![Some(1), None, Some(0)]));
        let bit: ArrayRef = Arc::new(BinaryArray::from(vec![
            Some([1u8].as_slice()),
            None,
            Some([0u8].as_slice()),
        ]));
        for column in [tinyint, bit] {
            assert_eq!(
                convert_column(&column, &AnalyticsType::Boolean)
                    .unwrap()
                    .as_ref(),
                &BooleanArray::from(vec![Some(true), None, Some(false)])
            );
        }
        let two: ArrayRef = Arc::new(Int8Array::from(vec![2]));
        let error = convert_column(&two, &AnalyticsType::Boolean).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Data);
    }
}
