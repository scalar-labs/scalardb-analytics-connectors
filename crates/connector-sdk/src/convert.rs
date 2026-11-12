//! Value-preserving conversions from Data Source access output to Analytics values.
//!
//! A Connector's constant tables pick one of these building blocks per mapping
//! rule. Each conversion either reproduces every source value exactly or fails
//! with a `data` error; none rounds, truncates, or substitutes a nearby value.
//! Errors name the target type but never contain a value.

use std::sync::Arc;

use analytics_types::{AnalyticsSchema, AnalyticsType};
use arrow_array::{
    Array, ArrayRef, Decimal128Array, FixedSizeBinaryArray, RecordBatch, StringArray,
    Time64MicrosecondArray, builder::StringBuilder,
};

use crate::{ConnectorError, ErrorKind};

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Converts one batch of Data Source access output to the planned output schema.
///
/// `convert_column` turns each column into its Analytics type's storage, or
/// returns it unchanged when Data Source access already produced that storage.
/// The result must have exactly the Analytics storage type; a `Time` value
/// outside one day is a `data` error, whatever produced it.
pub fn convert_batch(
    batch: &RecordBatch,
    output: &AnalyticsSchema,
    convert_column: impl Fn(&ArrayRef, &AnalyticsType) -> Result<ArrayRef, ConnectorError>,
) -> Result<RecordBatch, ConnectorError> {
    let schema = Arc::new(output.to_arrow());
    if batch.num_columns() != schema.fields().len() {
        return Err(unexpected_output());
    }
    let columns = batch
        .columns()
        .iter()
        .zip(output.fields())
        .zip(schema.fields())
        .map(|((column, field), arrow_field)| {
            let converted = convert_column(column, field.data_type())?;
            if converted.data_type() != arrow_field.data_type() {
                return Err(unexpected_output());
            }
            if field.data_type() == &AnalyticsType::Time {
                check_time_of_day(&converted)?;
            }
            Ok(converted)
        })
        .collect::<Result<Vec<_>, _>>()?;
    RecordBatch::try_new(schema, columns).map_err(|_| unexpected_output())
}

fn check_time_of_day(column: &ArrayRef) -> Result<(), ConnectorError> {
    let times = downcast::<Time64MicrosecondArray>(column)?;
    if times
        .iter()
        .flatten()
        .any(|micros| !(0..MICROS_PER_DAY).contains(&micros))
    {
        return Err(not_carried("Time"));
    }
    Ok(())
}

/// Returns `column` as the concrete array type Data Source access produced.
pub fn downcast<T: 'static>(column: &ArrayRef) -> Result<&T, ConnectorError> {
    column
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(unexpected_output)
}

fn unexpected_output() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Internal,
        false,
        "Data Source access returned a column in an unexpected form",
    )
}

/// Converts decimal text, such as `-12.50`, to `Decimal128(precision, scale)`.
///
/// The text is an optional `-` or `+`, digits, and an optional `.` followed by
/// digits. Fractional digits beyond `scale` must be zero; with a negative
/// scale, the integer digits it removes must be zero. A value outside
/// `precision`, or text in any other form, such as `NaN` or `Infinity`, is a
/// `data` error.
pub fn decimal128_from_text(
    text: &StringArray,
    precision: u8,
    scale: i8,
) -> Result<Decimal128Array, ConnectorError> {
    let values = text
        .iter()
        .map(|value| {
            value
                .map(|value| {
                    parse_decimal(value, precision, scale)
                        .ok_or_else(|| not_carried(&format!("Decimal({precision}, {scale})")))
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Decimal128Array::from(values)
        .with_precision_and_scale(precision, scale)
        .map_err(|_| {
            ConnectorError::new(
                ErrorKind::Internal,
                false,
                "a decimal conversion targeted an invalid decimal type",
            )
        })
}

/// Returns the unscaled value of `text` at `scale`, if it is exact and within `precision`.
fn parse_decimal(text: &str, precision: u8, scale: i8) -> Option<i128> {
    let (negative, unsigned) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    if !integer
        .bytes()
        .chain(fraction.bytes())
        .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    // Every digit of the value at the target scale, most significant first.
    let mut digits: Vec<u8> = integer.bytes().chain(fraction.bytes()).collect();
    let fraction_digits = i32::try_from(fraction.len()).ok()?;
    let excess = fraction_digits - i32::from(scale);
    if excess > 0 {
        let kept = digits.len().checked_sub(usize::try_from(excess).ok()?)?;
        if digits[kept..].iter().any(|digit| *digit != b'0') {
            return None;
        }
        digits.truncate(kept);
    } else {
        digits.extend(std::iter::repeat_n(b'0', usize::try_from(-excess).ok()?));
    }

    let significant = digits
        .iter()
        .position(|digit| *digit != b'0')
        .map_or(&digits[digits.len()..], |start| &digits[start..]);
    if significant.len() > usize::from(precision) {
        return None;
    }
    let mut value: i128 = 0;
    for digit in significant {
        value = value * 10 + i128::from(digit - b'0');
    }
    Some(if negative { -value } else { value })
}

/// Converts 16-byte UUIDs to their 36-character canonical lowercase form.
pub fn uuid_text_from_bytes(bytes: &FixedSizeBinaryArray) -> Result<StringArray, ConnectorError> {
    if bytes.value_length() != 16 {
        return Err(not_carried("Uuid"));
    }
    let mut builder = StringBuilder::with_capacity(bytes.len(), bytes.len() * 36);
    for value in bytes.iter() {
        match value {
            Some(value) => builder.append_value(uuid_text(value)),
            None => builder.append_null(),
        }
    }
    Ok(builder.finish())
}

fn uuid_text(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// The `data` error for a source value that the target type cannot carry exactly.
pub fn not_carried(target: &str) -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Data,
        false,
        format!("a source value cannot be carried exactly as {target}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimals(
        values: &[Option<&str>],
        precision: u8,
        scale: i8,
    ) -> Result<Vec<Option<i128>>, ErrorKind> {
        decimal128_from_text(&StringArray::from(values.to_vec()), precision, scale)
            .map(|array| array.iter().collect())
            .map_err(|error| error.kind())
    }

    #[test]
    fn decimal_text_converts_exactly() {
        assert_eq!(
            decimals(
                &[Some("12.50"), Some("-0.01"), None, Some("7"), Some("+3.1")],
                5,
                2
            ),
            Ok(vec![Some(1250), Some(-1), None, Some(700), Some(310)])
        );
        assert_eq!(
            decimals(&[Some("99999999999999999999999999999999999999")], 38, 0),
            Ok(vec![Some(
                99_999_999_999_999_999_999_999_999_999_999_999_999
            )])
        );
        assert_eq!(decimals(&[Some("12300")], 5, -2), Ok(vec![Some(123)]));
        assert_eq!(decimals(&[Some("1.2000")], 3, 1), Ok(vec![Some(12)]));
    }

    #[test]
    fn decimal_text_that_cannot_be_carried_is_a_data_error() {
        for (text, precision, scale) in [
            ("nan", 5, 2),
            ("inf", 5, 2),
            ("-inf", 5, 2),
            ("1.234", 5, 2),
            ("1000", 5, 2),
            ("12345", 5, -2),
            ("", 5, 2),
            ("-", 5, 2),
            ("1e5", 5, 0),
        ] {
            assert_eq!(
                decimals(&[Some(text)], precision, scale),
                Err(ErrorKind::Data),
                "{text}"
            );
        }
    }

    fn time_schema() -> AnalyticsSchema {
        AnalyticsSchema::new(vec![analytics_types::AnalyticsField::new(
            "clock",
            AnalyticsType::Time,
            true,
        )])
    }

    fn time_batch(values: Vec<Option<i64>>) -> RecordBatch {
        let column: ArrayRef = Arc::new(Time64MicrosecondArray::from(values));
        RecordBatch::try_from_iter([("clock", column)]).unwrap()
    }

    #[test]
    fn batches_take_the_output_schema_and_times_stay_within_a_day() {
        let unchanged = |column: &ArrayRef, _: &AnalyticsType| Ok(Arc::clone(column));
        let converted =
            convert_batch(&time_batch(vec![Some(0), None]), &time_schema(), unchanged).unwrap();
        assert_eq!(converted.schema().as_ref(), &time_schema().to_arrow());

        let error = convert_batch(
            &time_batch(vec![Some(MICROS_PER_DAY)]),
            &time_schema(),
            unchanged,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Data);
    }

    #[test]
    fn a_column_left_in_another_storage_type_is_internal() {
        let text: ArrayRef = Arc::new(StringArray::from(vec!["12:00"]));
        let batch = RecordBatch::try_from_iter([("clock", text)]).unwrap();
        let error =
            convert_batch(&batch, &time_schema(), |column, _| Ok(Arc::clone(column))).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
    }

    #[test]
    fn uuid_bytes_become_canonical_text() {
        let bytes = FixedSizeBinaryArray::try_from_sparse_iter_with_size(
            [
                Some((0u8..16).collect::<Vec<_>>()),
                None,
                Some(vec![0xff; 16]),
            ]
            .into_iter(),
            16,
        )
        .unwrap();
        let text = uuid_text_from_bytes(&bytes).unwrap();
        assert_eq!(
            text.iter().collect::<Vec<_>>(),
            [
                Some("00010203-0405-0607-0809-0a0b0c0d0e0f"),
                None,
                Some("ffffffff-ffff-ffff-ffff-ffffffffffff"),
            ]
        );
    }
}
