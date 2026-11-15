//! Value-preserving conversions from Data Source access output to Analytics values.
//!
//! A Connector's constant tables pick one of these building blocks per mapping
//! rule. Each conversion either reproduces every source value exactly or fails
//! with a `data` error; none rounds, truncates, or substitutes a nearby value.
//! Errors name the target type but never contain a value.

use std::sync::Arc;

use analytics_types::{
    AnalyticsSchema, AnalyticsType, DecimalType as AnalyticsDecimal,
    arrow_schema::{DataType, TimeUnit},
};
use arrow_array::{
    Array, ArrayRef, FixedSizeBinaryArray, ListArray, MapArray, PrimitiveArray, RecordBatch,
    StringArray, StructArray, Time64MicrosecondArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray,
    builder::StringBuilder,
    types::{Decimal32Type, Decimal64Type, Decimal128Type, Decimal256Type, DecimalType},
};
use arrow_buffer::i256;

use crate::{ConnectorError, ErrorKind};

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Converts one batch of Data Source access output to the planned output schema.
///
/// `convert_column` turns each column into its Analytics type's storage, or
/// returns it unchanged when Data Source access already produced that storage.
/// The result must have exactly the Analytics storage type; a `Time` value
/// outside one day, at any depth of an array, map, or struct, is a `data`
/// error, whatever produced it.
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
            if contains_time(arrow_field.data_type()) {
                check_time_of_day(converted.as_ref())?;
            }
            Ok(converted)
        })
        .collect::<Result<Vec<_>, _>>()?;
    RecordBatch::try_new(schema, columns).map_err(|_| unexpected_output())
}

/// Whether `Time` storage appears anywhere in `data_type`.
fn contains_time(data_type: &DataType) -> bool {
    match data_type {
        DataType::Time64(TimeUnit::Microsecond) => true,
        DataType::List(element) => contains_time(element.data_type()),
        DataType::Map(entries, _) => contains_time(entries.data_type()),
        DataType::Struct(fields) => fields.iter().any(|field| contains_time(field.data_type())),
        _ => false,
    }
}

/// The rows of `array` that are not null.
fn valid_rows(array: &dyn Array) -> impl Iterator<Item = usize> + '_ {
    (0..array.len()).filter(move |row| array.is_valid(*row))
}

/// Checks that every reachable non-null `Time` value lies within one day.
///
/// Values under a null list, map, or struct are not reachable and are not
/// checked.
fn check_time_of_day(array: &dyn Array) -> Result<(), ConnectorError> {
    match array.data_type() {
        DataType::Time64(TimeUnit::Microsecond) => {
            let times = array
                .as_any()
                .downcast_ref::<Time64MicrosecondArray>()
                .ok_or_else(unexpected_output)?;
            if times
                .iter()
                .flatten()
                .any(|micros| !(0..MICROS_PER_DAY).contains(&micros))
            {
                return Err(not_carried("Time"));
            }
        }
        DataType::List(_) => {
            let lists = array
                .as_any()
                .downcast_ref::<ListArray>()
                .ok_or_else(unexpected_output)?;
            for row in valid_rows(lists) {
                check_time_of_day(lists.value(row).as_ref())?;
            }
        }
        DataType::Map(..) => {
            let maps = array
                .as_any()
                .downcast_ref::<MapArray>()
                .ok_or_else(unexpected_output)?;
            for row in valid_rows(maps) {
                check_time_of_day(&maps.value(row))?;
            }
        }
        DataType::Struct(_) => {
            let structs = array
                .as_any()
                .downcast_ref::<StructArray>()
                .ok_or_else(unexpected_output)?;
            for column in structs.columns() {
                if structs.null_count() == 0 {
                    check_time_of_day(column.as_ref())?;
                } else {
                    for row in valid_rows(structs) {
                        check_time_of_day(column.slice(row, 1).as_ref())?;
                    }
                }
            }
        }
        _ => {}
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

/// Converts decimal text, such as `-12.50`, to `decimal`'s Analytics storage,
/// the narrowest Arrow decimal that holds its precision.
///
/// The text is an optional `-` or `+`, digits, and an optional `.` followed by
/// digits. Fractional digits beyond the scale must be zero; with a negative
/// scale, the integer digits it removes must be zero. A value outside the
/// precision, or text in any other form, such as `NaN` or `Infinity`, is a
/// `data` error.
pub fn decimal_from_text(
    text: &StringArray,
    decimal: AnalyticsDecimal,
) -> Result<ArrayRef, ConnectorError> {
    let (precision, scale) = (decimal.precision(), decimal.scale());
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
    let to_i128 = |value: i256| value.to_i128();
    match AnalyticsType::Decimal(decimal).storage_type() {
        DataType::Decimal32(..) => decimal_array::<Decimal32Type>(&values, decimal, |value| {
            to_i128(value).and_then(|value| i32::try_from(value).ok())
        }),
        DataType::Decimal64(..) => decimal_array::<Decimal64Type>(&values, decimal, |value| {
            to_i128(value).and_then(|value| i64::try_from(value).ok())
        }),
        DataType::Decimal128(..) => decimal_array::<Decimal128Type>(&values, decimal, to_i128),
        _ => decimal_array::<Decimal256Type>(&values, decimal, Some),
    }
}

/// Builds a decimal array of width `T` from unscaled values that have at most
/// `decimal`'s precision of digits, which the width holds.
fn decimal_array<T: DecimalType>(
    values: &[Option<i256>],
    decimal: AnalyticsDecimal,
    narrow: impl Fn(i256) -> Option<T::Native>,
) -> Result<ArrayRef, ConnectorError> {
    let values = values
        .iter()
        .map(|value| {
            value
                .map(|value| narrow(value).ok_or_else(unexpected_output))
                .transpose()
        })
        .collect::<Result<PrimitiveArray<T>, _>>()?;
    let array = values
        .with_precision_and_scale(decimal.precision(), decimal.scale())
        .map_err(|_| unexpected_output())?;
    Ok(Arc::new(array))
}

/// Returns the unscaled value of `text` at `scale`, if it is exact and within `precision`.
fn parse_decimal(text: &str, precision: u8, scale: i8) -> Option<i256> {
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
        // Digits below the target scale must all be zero; a value whose every
        // digit is dropped, such as `0` at a negative scale, is zero.
        let kept = digits.len().saturating_sub(usize::try_from(excess).ok()?);
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
    // At most 76 digits, which 256 bits hold.
    let mut value = i256::ZERO;
    for digit in significant {
        value = value
            .checked_mul(i256::from(10_i8))?
            .checked_add(i256::from(i16::from(digit - b'0')))?;
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

/// Aligns timestamps of any unit to microseconds, keeping their time zone.
///
/// Seconds and milliseconds are multiplied; nanoseconds must be whole
/// microseconds. A value outside the microsecond range, or with a fraction of
/// a microsecond, is a `data` error.
pub fn timestamps_to_micros(
    values: &ArrayRef,
) -> Result<TimestampMicrosecondArray, ConnectorError> {
    let DataType::Timestamp(unit, timezone) = values.data_type() else {
        return Err(unexpected_output());
    };
    let to_micros = |value: i64| -> Option<i64> {
        match unit {
            TimeUnit::Second => value.checked_mul(1_000_000),
            TimeUnit::Millisecond => value.checked_mul(1_000),
            TimeUnit::Microsecond => Some(value),
            TimeUnit::Nanosecond => (value % 1_000 == 0).then_some(value / 1_000),
        }
    };
    let raw: Vec<Option<i64>> = match unit {
        TimeUnit::Second => downcast::<TimestampSecondArray>(values)?.iter().collect(),
        TimeUnit::Millisecond => downcast::<TimestampMillisecondArray>(values)?
            .iter()
            .collect(),
        TimeUnit::Microsecond => downcast::<TimestampMicrosecondArray>(values)?
            .iter()
            .collect(),
        TimeUnit::Nanosecond => downcast::<TimestampNanosecondArray>(values)?
            .iter()
            .collect(),
    };
    let micros = raw
        .into_iter()
        .map(|value| {
            value
                .map(|value| to_micros(value).ok_or_else(|| not_carried("a microsecond timestamp")))
                .transpose()
        })
        .collect::<Result<TimestampMicrosecondArray, _>>()?;
    Ok(micros.with_timezone_opt(timezone.clone()))
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

    /// Converts decimal text and returns the unscaled values, as 256-bit
    /// integers whatever the storage width.
    fn decimals(
        values: &[Option<&str>],
        precision: u8,
        scale: i8,
    ) -> Result<Vec<Option<i128>>, ErrorKind> {
        let decimal = AnalyticsDecimal::new(precision, scale).unwrap();
        let array = decimal_from_text(&StringArray::from(values.to_vec()), decimal)
            .map_err(|error| error.kind())?;
        assert_eq!(
            array.data_type(),
            &AnalyticsType::Decimal(decimal).storage_type()
        );
        let cast = |column: &ArrayRef| -> Vec<Option<i128>> {
            use arrow_array::{Decimal32Array, Decimal64Array, Decimal128Array, Decimal256Array};
            let any = column.as_any();
            if let Some(array) = any.downcast_ref::<Decimal32Array>() {
                array.iter().map(|value| value.map(i128::from)).collect()
            } else if let Some(array) = any.downcast_ref::<Decimal64Array>() {
                array.iter().map(|value| value.map(i128::from)).collect()
            } else if let Some(array) = any.downcast_ref::<Decimal128Array>() {
                array.iter().collect()
            } else {
                let array = any.downcast_ref::<Decimal256Array>().unwrap();
                array
                    .iter()
                    .map(|value| value.map(|value| value.to_i128().unwrap()))
                    .collect()
            }
        };
        Ok(cast(&array))
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
        assert_eq!(
            decimals(&[Some("0"), Some("0.00"), Some("-0")], 5, -2),
            Ok(vec![Some(0), Some(0), Some(0)])
        );
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

    #[test]
    fn decimal_text_takes_the_storage_width_of_its_precision() {
        for precision in [9, 18, 38] {
            assert_eq!(
                decimals(&[Some("-1.25"), None], precision, 2),
                Ok(vec![Some(-125), None]),
                "{precision}"
            );
        }
        let widest = format!("-{}", "9".repeat(76));
        let decimal = AnalyticsDecimal::new(76, 0).unwrap();
        let array = decimal_from_text(&StringArray::from(vec![widest.as_str()]), decimal).unwrap();
        let array = array
            .as_any()
            .downcast_ref::<arrow_array::Decimal256Array>()
            .unwrap();
        assert_eq!(array.value(0).to_string(), widest);
        assert_eq!(
            decimals(&[Some(&"9".repeat(77))], 76, 0),
            Err(ErrorKind::Data)
        );
    }

    #[test]
    fn timestamps_align_to_microseconds_with_their_time_zone() {
        let seconds: ArrayRef =
            Arc::new(TimestampSecondArray::from(vec![Some(1), None]).with_timezone("UTC"));
        let micros = timestamps_to_micros(&seconds).unwrap();
        assert_eq!(
            micros.data_type(),
            &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
        );
        assert_eq!(micros.iter().collect::<Vec<_>>(), [Some(1_000_000), None]);
        let millis: ArrayRef = Arc::new(TimestampMillisecondArray::from(vec![-1]));
        assert_eq!(timestamps_to_micros(&millis).unwrap().value(0), -1_000);
        for overflowing in [
            Arc::new(TimestampSecondArray::from(vec![i64::MAX])) as ArrayRef,
            Arc::new(TimestampNanosecondArray::from(vec![1_001])),
        ] {
            let error = timestamps_to_micros(&overflowing).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Data);
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
    fn times_nested_in_lists_and_structs_stay_within_a_day() {
        let unchanged = |column: &ArrayRef, _: &AnalyticsType| Ok(Arc::clone(column));
        let element = analytics_types::ArrayType::new(AnalyticsType::Time, true);
        let schema = AnalyticsSchema::new(vec![analytics_types::AnalyticsField::new(
            "clocks",
            AnalyticsType::Array(element),
            true,
        )]);
        let arrow = schema.to_arrow();
        let DataType::List(item) = arrow.field(0).data_type() else {
            panic!("an array is stored as a list");
        };
        let list = |micros: i64, valid: bool| -> ArrayRef {
            Arc::new(ListArray::new(
                Arc::clone(item),
                arrow_buffer::OffsetBuffer::from_lengths([1]),
                Arc::new(Time64MicrosecondArray::from(vec![micros])),
                Some(vec![valid].into()),
            ))
        };
        let batch =
            |column: ArrayRef| RecordBatch::try_new(Arc::new(arrow.clone()), vec![column]).unwrap();

        assert!(convert_batch(&batch(list(0, true)), &schema, unchanged).is_ok());
        let error =
            convert_batch(&batch(list(MICROS_PER_DAY, true)), &schema, unchanged).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Data);
        // A value under a null list is not reachable.
        assert!(convert_batch(&batch(list(MICROS_PER_DAY, false)), &schema, unchanged).is_ok());
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
