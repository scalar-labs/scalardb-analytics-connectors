//! Conversion from validated Substrait literals to Arrow SQL parameters.

use std::sync::Arc;

use analytics_types::AnalyticsField;
use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal128Array, DurationMicrosecondArray,
    Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    IntervalMonthDayNanoArray, StringArray, Time64MicrosecondArray, TimestampMicrosecondArray,
    new_null_array, types::IntervalMonthDayNano,
};
use connector_sdk::LogicalPlan;
use substrait::proto::expression;

/// Converts one validated scalar literal to the single-value Arrow parameter
/// representation retained by `Fragment` until final rendering.
pub(super) fn literal_array(plan: &LogicalPlan, literal: &expression::Literal) -> ArrayRef {
    use expression::literal::LiteralType;

    let array: ArrayRef = match literal.literal_type.as_ref() {
        Some(LiteralType::Boolean(value)) => Arc::new(BooleanArray::from(vec![Some(*value)])),
        Some(LiteralType::I8(value)) => Arc::new(Int8Array::from(vec![Some(
            i8::try_from(*value).expect("validated i8 literals are in range"),
        )])),
        Some(LiteralType::I16(value)) => Arc::new(Int16Array::from(vec![Some(
            i16::try_from(*value).expect("validated i16 literals are in range"),
        )])),
        Some(LiteralType::I32(value)) => Arc::new(Int32Array::from(vec![Some(*value)])),
        Some(LiteralType::I64(value)) => Arc::new(Int64Array::from(vec![Some(*value)])),
        Some(LiteralType::Fp32(value)) => Arc::new(Float32Array::from(vec![Some(*value)])),
        Some(LiteralType::Fp64(value)) => Arc::new(Float64Array::from(vec![Some(*value)])),
        Some(LiteralType::String(value)) => Arc::new(StringArray::from(vec![Some(value.as_str())])),
        Some(LiteralType::Binary(value)) => {
            Arc::new(BinaryArray::from(vec![Some(value.as_slice())]))
        }
        Some(LiteralType::Date(value)) => Arc::new(Date32Array::from(vec![Some(*value)])),
        Some(LiteralType::PrecisionTime(value)) if value.precision == 6 => {
            Arc::new(Time64MicrosecondArray::from(vec![Some(value.value)]))
        }
        Some(LiteralType::PrecisionTimestamp(value)) if value.precision == 6 => {
            Arc::new(TimestampMicrosecondArray::from(vec![Some(value.value)]))
        }
        Some(LiteralType::PrecisionTimestampTz(value)) if value.precision == 6 => {
            Arc::new(TimestampMicrosecondArray::from(vec![Some(value.value)]).with_timezone_utc())
        }
        Some(LiteralType::IntervalDayToSecond(value)) => Arc::new(DurationMicrosecondArray::from(
            vec![Some(duration_micros(value))],
        )),
        Some(LiteralType::IntervalCompound(value)) => {
            let year_month = value
                .interval_year_to_month
                .as_ref()
                .expect("validated compound intervals have a year-to-month part");
            let day_time = value
                .interval_day_to_second
                .as_ref()
                .expect("validated compound intervals have a day-to-second part");
            let months = year_month
                .years
                .checked_mul(12)
                .and_then(|years| years.checked_add(year_month.months))
                .expect("validated interval months are in range");
            let nanoseconds = interval_subseconds(day_time, 9)
                .checked_add(
                    i64::from(day_time.seconds)
                        .checked_mul(1_000_000_000)
                        .expect("validated interval seconds are in range"),
                )
                .expect("validated interval nanoseconds are in range");
            Arc::new(IntervalMonthDayNanoArray::from(vec![Some(
                IntervalMonthDayNano::new(months, day_time.days, nanoseconds),
            )]))
        }
        Some(LiteralType::Decimal(value)) => {
            let bytes: [u8; 16] = value
                .value
                .as_slice()
                .try_into()
                .expect("validated decimal literals contain 16 bytes");
            let precision =
                u8::try_from(value.precision).expect("validated decimal precision is in range");
            let scale = i8::try_from(value.scale).expect("validated decimal scale is in range");
            let values = Decimal128Array::from(vec![Some(i128::from_le_bytes(bytes))])
                .with_precision_and_scale(precision, scale)
                .expect("validated decimal precision and scale are compatible");
            Arc::new(values)
        }
        Some(LiteralType::Null(data_type)) => {
            let (data_type, _) = plan
                .analytics_type(data_type)
                .expect("validated null literal types resolve");
            let field = AnalyticsField::new("parameter", data_type, true).to_arrow();
            new_null_array(field.data_type(), 1)
        }
        Some(LiteralType::UserDefined(value)) => {
            let Some(expression::literal::user_defined::Val::Struct(struct_value)) =
                value.val.as_ref()
            else {
                unreachable!("validated logical literals use the struct representation")
            };
            let [carrier] = struct_value.fields.as_slice() else {
                unreachable!("validated logical literals contain one carrier value")
            };
            literal_array(plan, carrier)
        }
        Some(_) => unreachable!("the SDK validates the SQL literal profile"),
        None => unreachable!("validated literals have a value and type"),
    };
    array
}

/// Converts a validated Substrait day-to-second interval to microseconds.
fn duration_micros(value: &expression::literal::IntervalDayToSecond) -> i64 {
    let subseconds = interval_subseconds(value, 6);
    i64::from(value.days)
        .checked_mul(86_400_000_000)
        .and_then(|days| {
            i64::from(value.seconds)
                .checked_mul(1_000_000)
                .and_then(|seconds| days.checked_add(seconds))
        })
        .and_then(|whole| whole.checked_add(subseconds))
        .expect("validated duration literals are in range")
}

/// Reads either legacy microseconds or precision-tagged subseconds exactly.
fn interval_subseconds(
    value: &expression::literal::IntervalDayToSecond,
    expected_precision: i32,
) -> i64 {
    use expression::literal::interval_day_to_second::PrecisionMode;
    match value.precision_mode {
        Some(PrecisionMode::Microseconds(micros)) if expected_precision == 6 => i64::from(micros),
        Some(PrecisionMode::Precision(precision)) if precision == expected_precision => {
            value.subseconds
        }
        _ => unreachable!("the SDK validates interval precision"),
    }
}
