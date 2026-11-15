//! Decoding query records into Arrow typed by the caller's schema.
//!
//! Each JSON value is read from its own text, so numbers never pass through a
//! binary float, and a value is converted only when the conversion keeps it
//! unchanged. A value that would change is a [`Error::Value`].
//!
//! What a value of each Salesforce type means, such as a dateTime being
//! transferred in UTC, is in
//! [Primitive Data Types](https://developer.salesforce.com/docs/atlas.en-us.api.meta/api/primitive_data_types.htm).
//! The textual forms read here are those each function below lists.

use std::{borrow::Cow, collections::HashMap, sync::Arc};

use arrow_array::{
    ArrayRef, RecordBatch,
    builder::{
        BooleanBuilder, Date32Builder, Float64Builder, Int32Builder, Int64Builder,
        PrimitiveBuilder, StringBuilder, Time64MicrosecondBuilder, TimestampMicrosecondBuilder,
    },
    types::{Decimal32Type, Decimal64Type, Decimal128Type, DecimalType},
};
use arrow_schema::{DataType, Field, SchemaRef, TimeUnit};
use serde_json::value::RawValue;

use crate::error::{Error, Result};

/// One record of a query page: field name to the field's JSON value.
pub(crate) type Record<'a> = HashMap<Cow<'a, str>, &'a RawValue>;

const MICROS_PER_SECOND: i64 = 1_000_000;
const SECONDS_PER_DAY: i64 = 86_400;

/// Checks that every field of `schema` has a type the decoder produces.
pub(crate) fn check_schema(schema: &SchemaRef) -> Result<()> {
    for field in schema.fields() {
        let supported = match field.data_type() {
            DataType::Utf8
            | DataType::Boolean
            | DataType::Int32
            | DataType::Int64
            | DataType::Float64
            | DataType::Decimal32(_, _)
            | DataType::Decimal64(_, _)
            | DataType::Decimal128(_, _)
            | DataType::Date32
            | DataType::Time64(TimeUnit::Microsecond) => true,
            DataType::Timestamp(TimeUnit::Microsecond, Some(zone)) => zone.as_ref() == "UTC",
            _ => false,
        };
        if !supported {
            return Err(Error::Config(format!(
                "field {} has type {}, which the client cannot produce",
                field.name(),
                field.data_type()
            )));
        }
    }
    Ok(())
}

/// Decodes the records of one page into a batch of `schema`.
///
/// Every record must hold every field of `schema`.
pub(crate) fn decode(schema: &SchemaRef, records: &[Record<'_>]) -> Result<RecordBatch> {
    let columns = schema
        .fields()
        .iter()
        .map(|field| decode_column(field, records))
        .collect::<Result<Vec<_>>>()?;
    RecordBatch::try_new_with_options(
        schema.clone(),
        columns,
        &arrow_array::RecordBatchOptions::new().with_row_count(Some(records.len())),
    )
    .map_err(|error| Error::InvalidResponse(format!("the records cannot form a batch: {error}")))
}

fn decode_column(field: &Field, records: &[Record<'_>]) -> Result<ArrayRef> {
    let values = records
        .iter()
        .map(|record| {
            let value = record.get(field.name().as_str()).ok_or_else(|| {
                Error::InvalidResponse(format!("a record lacks field {}", field.name()))
            })?;
            let text = value.get();
            if text == "null" {
                if !field.is_nullable() {
                    return Err(value_error(field, "null in a field that is not nullable"));
                }
                return Ok(None);
            }
            Ok(Some(text))
        })
        .collect::<Result<Vec<_>>>()?;
    let error = |reason| value_error(field, reason);
    Ok(match field.data_type() {
        DataType::Utf8 => {
            let mut builder = StringBuilder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| string(text).ok_or_else(|| error("expected a string")))
                        .transpose()?,
                );
            }
            Arc::new(builder.finish())
        }
        DataType::Boolean => {
            let mut builder = BooleanBuilder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| match text {
                            "true" => Ok(true),
                            "false" => Ok(false),
                            _ => Err(error("expected a Boolean")),
                        })
                        .transpose()?,
                );
            }
            Arc::new(builder.finish())
        }
        DataType::Int32 => {
            let mut builder = Int32Builder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| {
                            integer(text)
                                .and_then(|value| i32::try_from(value).ok())
                                .ok_or_else(|| error("expected an integer within the type's range"))
                        })
                        .transpose()?,
                );
            }
            Arc::new(builder.finish())
        }
        DataType::Int64 => {
            let mut builder = Int64Builder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| {
                            integer(text)
                                .ok_or_else(|| error("expected an integer within the type's range"))
                        })
                        .transpose()?,
                );
            }
            Arc::new(builder.finish())
        }
        DataType::Float64 => {
            let mut builder = Float64Builder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| float(text).ok_or_else(|| error("expected a finite number")))
                        .transpose()?,
                );
            }
            Arc::new(builder.finish())
        }
        DataType::Decimal32(precision, scale) => {
            decimal_column::<Decimal32Type>(&values, *precision, *scale, error)?
        }
        DataType::Decimal64(precision, scale) => {
            decimal_column::<Decimal64Type>(&values, *precision, *scale, error)?
        }
        DataType::Decimal128(precision, scale) => {
            decimal_column::<Decimal128Type>(&values, *precision, *scale, error)?
        }
        DataType::Date32 => {
            let mut builder = Date32Builder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| {
                            string(text)
                                .as_deref()
                                .and_then(date)
                                .and_then(|days| i32::try_from(days).ok())
                                .ok_or_else(|| error("expected a date as YYYY-MM-DD"))
                        })
                        .transpose()?,
                );
            }
            Arc::new(builder.finish())
        }
        DataType::Time64(TimeUnit::Microsecond) => {
            let mut builder = Time64MicrosecondBuilder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| {
                            string(text)
                                .as_deref()
                                .and_then(|text| time(text.strip_suffix('Z').unwrap_or(text)))
                                .ok_or_else(|| error("expected a time as HH:MM:SS.sssZ"))
                        })
                        .transpose()?,
                );
            }
            Arc::new(builder.finish())
        }
        DataType::Timestamp(TimeUnit::Microsecond, Some(zone)) => {
            let mut builder = TimestampMicrosecondBuilder::new();
            for value in values {
                builder.append_option(
                    value
                        .map(|text| {
                            string(text)
                                .as_deref()
                                .and_then(date_time)
                                .ok_or_else(|| error("expected a date-time with a UTC offset"))
                        })
                        .transpose()?,
                );
            }
            Arc::new(builder.finish().with_timezone(zone.clone()))
        }
        other => {
            return Err(Error::Config(format!(
                "field {} has type {other}, which the client cannot produce",
                field.name()
            )));
        }
    })
}

/// Decodes JSON numbers into a decimal column of `precision` and `scale`,
/// stored in the Arrow decimal type `T`.
///
/// A number that does not fit the precision and scale is an error; one that
/// fits the precision fits `T`'s storage, which holds that many digits.
fn decimal_column<T>(
    values: &[Option<&str>],
    precision: u8,
    scale: i8,
    error: impl Fn(&'static str) -> Error,
) -> Result<ArrayRef>
where
    T: DecimalType,
    T::Native: TryFrom<i128>,
{
    let mut builder = PrimitiveBuilder::<T>::with_capacity(values.len());
    for value in values {
        builder.append_option(
            value
                .map(|text| {
                    decimal(text, precision, scale)
                        .and_then(|unscaled| T::Native::try_from(unscaled).ok())
                        .ok_or_else(|| error("expected a number that fits the precision and scale"))
                })
                .transpose()?,
        );
    }
    Ok(Arc::new(
        builder
            .finish()
            .with_precision_and_scale(precision, scale)
            .map_err(|_| error("the precision and scale are not valid"))?,
    ))
}

fn value_error(field: &Field, reason: &'static str) -> Error {
    Error::Value {
        field: field.name().clone(),
        data_type: field.data_type().clone(),
        reason,
    }
}

/// Reads a JSON string value, or `None` for any other JSON value.
fn string(text: &str) -> Option<Cow<'_, str>> {
    if !text.starts_with('"') {
        return None;
    }
    serde_json::from_str(text).ok()
}

/// Whether `text` is a JSON number, rather than a string or another value.
fn is_number(text: &str) -> bool {
    text.starts_with(|first: char| first == '-' || first.is_ascii_digit())
}

/// Reads a JSON number written as an integer, without a fraction or exponent.
fn integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Reads a JSON number as the nearest double to the decimal number it
/// writes.
fn float(text: &str) -> Option<f64> {
    if !is_number(text) {
        return None;
    }
    text.parse().ok().filter(|value: &f64| value.is_finite())
}

/// Reads a JSON number as the unscaled value of a decimal of `precision` and
/// `scale`.
///
/// Fails when the number has non-zero digits beyond `scale` or more
/// significant digits than `precision`.
fn decimal(text: &str, precision: u8, scale: i8) -> Option<i128> {
    if !is_number(text) {
        return None;
    }
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (&text[..index], text[index + 1..].parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !(whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    let digits = digits.trim_start_matches('0');
    // The value is `digits * 10^shift` in units of 10^-scale.
    let shift = exponent
        .checked_sub(i32::try_from(fraction.len()).ok()?)?
        .checked_add(i32::from(scale))?;
    let digits = if shift >= 0 {
        if digits.is_empty() {
            String::new()
        } else {
            let zeros = usize::try_from(shift).ok()?;
            // Checked before building the digits, so a large exponent cannot
            // allocate a long string.
            if digits.len().checked_add(zeros)? > usize::from(precision) {
                return None;
            }
            format!("{digits}{}", "0".repeat(zeros))
        }
    } else {
        let dropped = usize::try_from(shift.checked_neg()?).ok()?;
        let kept = digits.len().saturating_sub(dropped);
        if !digits[kept..].bytes().all(|byte| byte == b'0') {
            return None;
        }
        digits[..kept].to_owned()
    };
    if digits.len() > usize::from(precision) {
        return None;
    }
    let unscaled: i128 = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    Some(if negative { -unscaled } else { unscaled })
}

/// Reads `YYYY-MM-DD` as days since 1970-01-01.
fn date(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = number(&text[0..4])?;
    let month = number(&text[5..7])?;
    let day = number(&text[8..10])?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Reads `HH:MM:SS` with an optional fraction as microseconds since midnight.
fn time(text: &str) -> Option<i64> {
    let (clock, fraction) = match text.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (text, None),
    };
    let bytes = clock.as_bytes();
    if bytes.len() != 8 || bytes[2] != b':' || bytes[5] != b':' {
        return None;
    }
    let hour = number(&clock[0..2])?;
    let minute = number(&clock[3..5])?;
    let second = number(&clock[6..8])?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let micros = match fraction {
        Some(fraction) => fraction_micros(fraction)?,
        None => 0,
    };
    Some(((hour * 60 + minute) * 60 + second) * MICROS_PER_SECOND + micros)
}

/// Reads `YYYY-MM-DDTHH:MM:SS[.fraction]` followed by `Z`, `±HH:MM`, or
/// `±HHMM` as microseconds since the Unix epoch in UTC.
fn date_time(text: &str) -> Option<i64> {
    let (day, rest) = text.split_once('T')?;
    let days = date(day)?;
    let (clock, offset_seconds) = if let Some(clock) = rest.strip_suffix('Z') {
        (clock, 0)
    } else {
        let index = rest.rfind(['+', '-'])?;
        let (clock, offset) = rest.split_at(index);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let offset = offset[1..].replace(':', "");
        if offset.len() != 4 {
            return None;
        }
        let hours = number(&offset[0..2])?;
        let minutes = number(&offset[2..4])?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        (clock, sign * (hours * 3600 + minutes * 60))
    };
    let micros = time(clock)?;
    (days * SECONDS_PER_DAY - offset_seconds)
        .checked_mul(MICROS_PER_SECOND)?
        .checked_add(micros)
}

/// Reads a fraction of a second as microseconds; digits beyond the sixth must
/// be zero.
fn fraction_micros(fraction: &str) -> Option<i64> {
    if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (kept, rest) = fraction.split_at(fraction.len().min(6));
    if !rest.bytes().all(|byte| byte == b'0') {
        return None;
    }
    Some(kept.parse::<i64>().ok()? * 10_i64.pow(6 - u32::try_from(kept.len()).ok()?))
}

fn number(text: &str) -> Option<i64> {
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use arrow_array::{
        Array, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int32Array, StringArray,
        Time64MicrosecondArray, TimestampMicrosecondArray,
    };
    use arrow_schema::Schema;

    use super::*;

    fn records(body: &str) -> Vec<Record<'_>> {
        serde_json::from_str(body).unwrap()
    }

    fn schema(fields: Vec<Field>) -> SchemaRef {
        Arc::new(Schema::new(fields))
    }

    fn decode_one(data_type: DataType, value: &str) -> Result<RecordBatch> {
        let body = format!(r#"[{{"F": {value}}}]"#);
        let records = records(&body);
        // `records` borrows `body`; decode before both are dropped.
        decode(&schema(vec![Field::new("F", data_type, true)]), &records)
    }

    #[test]
    fn decodes_each_supported_type() {
        let body = r#"[
            {"attributes": {"type": "Account"}, "Name": "Acme \"Inc\"", "Active": true,
             "Count": 42, "Ratio": 0.1, "Amount": 1234.5, "Since": "2024-02-29",
             "At": "12:34:56.789Z", "Updated": "2024-01-31T12:34:56.000+0000"},
            {"attributes": {"type": "Account"}, "Name": null, "Active": null,
             "Count": null, "Ratio": null, "Amount": null, "Since": null,
             "At": null, "Updated": null}
        ]"#;
        let schema = schema(vec![
            Field::new("Name", DataType::Utf8, true),
            Field::new("Active", DataType::Boolean, true),
            Field::new("Count", DataType::Int32, true),
            Field::new("Ratio", DataType::Float64, true),
            Field::new("Amount", DataType::Decimal128(18, 2), true),
            Field::new("Since", DataType::Date32, true),
            Field::new("At", DataType::Time64(TimeUnit::Microsecond), true),
            Field::new(
                "Updated",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                true,
            ),
        ]);
        let batch = decode(&schema, &records(body)).unwrap();

        let column = |index: usize| batch.column(index).clone();
        let name = column(0);
        let name = name.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(name.value(0), r#"Acme "Inc""#);
        assert!(name.is_null(1));
        assert!(
            column(1)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .value(0)
        );
        assert_eq!(
            column(2)
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .value(0),
            42
        );
        assert_eq!(
            column(3)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(0),
            0.1
        );
        assert_eq!(
            column(4)
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .unwrap()
                .value(0),
            123_450
        );
        assert_eq!(
            column(5)
                .as_any()
                .downcast_ref::<Date32Array>()
                .unwrap()
                .value(0),
            19_782
        );
        assert_eq!(
            column(6)
                .as_any()
                .downcast_ref::<Time64MicrosecondArray>()
                .unwrap()
                .value(0),
            45_296_789_000
        );
        assert_eq!(
            column(7)
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .unwrap()
                .value(0),
            1_706_704_496_000_000
        );
        for index in 0..batch.num_columns() {
            assert!(
                batch.column(index).is_null(1),
                "column {index} is null in the second row"
            );
        }
    }

    #[test]
    fn decimals_keep_every_digit_or_fail() {
        let read = |text, precision, scale| {
            decode_one(DataType::Decimal128(precision, scale), text).map(|batch| {
                batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Decimal128Array>()
                    .unwrap()
                    .value(0)
            })
        };
        assert_eq!(read("0.30", 18, 2).unwrap(), 30);
        assert_eq!(read("-12.5", 18, 2).unwrap(), -1250);
        assert_eq!(read("1.5E+3", 18, 2).unwrap(), 150_000);
        assert_eq!(read("1.2500e-1", 18, 4).unwrap(), 1250);
        assert_eq!(read("0", 18, 2).unwrap(), 0);
        assert_eq!(
            read("99999999999999999999999999999999999999", 38, 0).unwrap(),
            99_999_999_999_999_999_999_999_999_999_999_999_999
        );
        // A digit beyond the scale would be rounded away.
        assert!(matches!(read("0.125", 18, 2), Err(Error::Value { .. })));
        // More digits than the precision holds.
        assert!(matches!(read("1234.5", 5, 2), Err(Error::Value { .. })));
        // A string is not a number.
        assert!(matches!(read(r#""1.5""#, 18, 2), Err(Error::Value { .. })));
    }

    #[test]
    fn integers_and_floats_are_read_from_their_text() {
        assert!(matches!(
            decode_one(DataType::Int32, "1.0"),
            Err(Error::Value { .. })
        ));
        assert!(matches!(
            decode_one(DataType::Int32, "2147483648"),
            Err(Error::Value { .. })
        ));
        let batch = decode_one(DataType::Int64, "-9223372036854775808").unwrap();
        assert_eq!(
            batch
                .column(0)
                .as_any()
                .downcast_ref::<arrow_array::Int64Array>()
                .unwrap()
                .value(0),
            i64::MIN
        );
        let batch = decode_one(DataType::Float64, "1.7976931348623157E308").unwrap();
        assert_eq!(
            batch
                .column(0)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(0),
            f64::MAX
        );
        assert!(matches!(
            decode_one(DataType::Float64, "true"),
            Err(Error::Value { .. })
        ));
    }

    #[test]
    fn date_times_convert_offsets_and_keep_microseconds() {
        assert_eq!(date_time("1970-01-01T00:00:00.000+0000"), Some(0));
        assert_eq!(date_time("1970-01-01T09:00:00+09:00"), Some(0));
        assert_eq!(date_time("1969-12-31T23:59:59.999999Z"), Some(-1));
        assert_eq!(
            date_time("2000-01-01T00:00:00.1234560Z"),
            Some(946_684_800_123_456)
        );
        // A seventh non-zero digit would be truncated.
        assert_eq!(date_time("2000-01-01T00:00:00.1234567Z"), None);
        assert_eq!(date_time("2023-02-29T00:00:00Z"), None);
        assert_eq!(date_time("2024-01-31 12:34:56Z"), None);
        assert_eq!(date("1600-03-01"), Some(-135_080));
        assert_eq!(time("24:00:00"), None);
    }

    #[test]
    fn nulls_in_fields_that_are_not_nullable_fail() {
        let body = r#"[{"F": null}]"#;
        let schema = schema(vec![Field::new("F", DataType::Utf8, false)]);
        assert!(matches!(
            decode(&schema, &records(body)),
            Err(Error::Value { .. })
        ));
    }

    #[test]
    fn a_record_without_a_requested_field_is_an_invalid_response() {
        let body = r#"[{"Other": 1}]"#;
        let schema = schema(vec![Field::new("F", DataType::Int32, true)]);
        assert!(matches!(
            decode(&schema, &records(body)),
            Err(Error::InvalidResponse(_))
        ));
    }

    #[test]
    fn schemas_with_types_the_decoder_cannot_produce_are_rejected() {
        let accepted = schema(vec![Field::new(
            "F",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        )]);
        assert!(check_schema(&accepted).is_ok());
        for data_type in [
            DataType::Float32,
            DataType::Timestamp(TimeUnit::Microsecond, None),
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            DataType::Binary,
        ] {
            let rejected = schema(vec![Field::new("F", data_type, true)]);
            assert!(matches!(check_schema(&rejected), Err(Error::Config(_))));
        }
    }
}
