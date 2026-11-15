//! Validated literal values owned by the logical-plan boundary.

use analytics_types::{AnalyticsType, DecimalType};
use substrait::proto::{Type, expression};

use super::{
    expression::parse_scalar_type,
    profile::PlanExtensions,
    types::{encode_type, logical_encoding},
};
use crate::RequestError;

/// A canonical literal value in the SDK-supported logical-plan profile.
///
/// Logical values retain both their Analytics type and validated carrier
/// instead of exposing their Substrait encoding to a Connector.
#[derive(Clone, PartialEq)]
pub enum LiteralValue {
    /// A Boolean value.
    Boolean(bool),
    /// An Analytics byte value.
    Byte(i8),
    /// An Analytics small integer value.
    SmallInt(i16),
    /// An Analytics integer value.
    Int(i32),
    /// An Analytics big integer value.
    BigInt(i64),
    /// An Analytics single-precision floating-point value.
    Float(f32),
    /// An Analytics double-precision floating-point value.
    Double(f64),
    /// A Unicode text value.
    Text(String),
    /// An opaque binary value.
    Blob(Vec<u8>),
    /// A date as days from the Unix epoch.
    Date(i32),
    /// A time of day as microseconds from midnight.
    Time(i64),
    /// A timezone-free timestamp as microseconds from the Unix epoch.
    Timestamp(i64),
    /// A UTC timestamp as microseconds from the Unix epoch.
    TimestampTz(i64),
    /// A duration in microseconds.
    Duration(i64),
    /// A calendar interval whose components have been range-checked.
    Interval {
        months: i32,
        days: i32,
        nanoseconds: i64,
    },
    /// A fixed-precision decimal represented by its signed unscaled integer.
    Decimal {
        value: i128,
        precision: u8,
        scale: i8,
    },
    /// A typed null retaining the Analytics type validated by the plan.
    Null { data_type: AnalyticsType },
    /// A logical value retaining its Analytics type and physical carrier.
    Logical {
        data_type: AnalyticsType,
        carrier: Box<LiteralValue>,
    },
}

/// Validates and normalizes one Substrait literal at the logical-plan boundary.
pub(super) fn parse(
    literal: &expression::Literal,
    extensions: &PlanExtensions,
) -> Result<(AnalyticsType, LiteralValue), RequestError> {
    use expression::literal::LiteralType;

    if literal.type_variation_reference != 0 {
        return Err(RequestError::undeclared(
            "literal type variations are not accepted",
        ));
    }
    match literal.literal_type.as_ref() {
        Some(LiteralType::Boolean(value)) => {
            Ok((AnalyticsType::Boolean, LiteralValue::Boolean(*value)))
        }
        Some(LiteralType::I8(value)) => Ok((
            AnalyticsType::Byte,
            LiteralValue::Byte(
                i8::try_from(*value)
                    .map_err(|_| RequestError::invalid("an i8 literal is out of range"))?,
            ),
        )),
        Some(LiteralType::I16(value)) => Ok((
            AnalyticsType::SmallInt,
            LiteralValue::SmallInt(
                i16::try_from(*value)
                    .map_err(|_| RequestError::invalid("an i16 literal is out of range"))?,
            ),
        )),
        Some(LiteralType::I32(value)) => Ok((AnalyticsType::Int, LiteralValue::Int(*value))),
        Some(LiteralType::I64(value)) => Ok((AnalyticsType::BigInt, LiteralValue::BigInt(*value))),
        Some(LiteralType::Fp32(value)) => Ok((AnalyticsType::Float, LiteralValue::Float(*value))),
        Some(LiteralType::Fp64(value)) => Ok((AnalyticsType::Double, LiteralValue::Double(*value))),
        Some(LiteralType::String(value)) => {
            Ok((AnalyticsType::Text, LiteralValue::Text(value.clone())))
        }
        Some(LiteralType::Binary(value)) => {
            Ok((AnalyticsType::Blob, LiteralValue::Blob(value.clone())))
        }
        Some(LiteralType::Date(value)) => Ok((AnalyticsType::Date, LiteralValue::Date(*value))),
        Some(LiteralType::PrecisionTime(value)) if value.precision == 6 => {
            if !(0..86_400_000_000).contains(&value.value) {
                return Err(RequestError::invalid("a time literal is outside one day"));
            }
            Ok((AnalyticsType::Time, LiteralValue::Time(value.value)))
        }
        Some(LiteralType::PrecisionTimestamp(value)) if value.precision == 6 => Ok((
            AnalyticsType::Timestamp,
            LiteralValue::Timestamp(value.value),
        )),
        Some(LiteralType::PrecisionTimestampTz(value)) if value.precision == 6 => Ok((
            AnalyticsType::TimestampTz,
            LiteralValue::TimestampTz(value.value),
        )),
        Some(LiteralType::IntervalDayToSecond(value)) => {
            require_interval_precision(value, 6)?;
            let micros = duration_micros(value)?;
            Ok((AnalyticsType::Duration, LiteralValue::Duration(micros)))
        }
        Some(LiteralType::IntervalCompound(value)) => {
            let day_time = value.interval_day_to_second.as_ref().ok_or_else(|| {
                RequestError::invalid("an interval compound literal requires a day-to-second part")
            })?;
            require_interval_precision(day_time, 9)?;
            let year_month = value.interval_year_to_month.as_ref().ok_or_else(|| {
                RequestError::invalid("an interval compound literal requires a year-to-month part")
            })?;
            let months = year_month
                .years
                .checked_mul(12)
                .and_then(|years| years.checked_add(year_month.months))
                .ok_or_else(|| RequestError::invalid("interval months are out of range"))?;
            let subseconds = interval_subseconds(day_time, 9)?;
            require_consistent_interval_sign(day_time.days, day_time.seconds, subseconds)?;
            let nanoseconds = i64::from(day_time.seconds)
                .checked_mul(1_000_000_000)
                .and_then(|seconds| seconds.checked_add(subseconds))
                .ok_or_else(|| RequestError::invalid("interval nanoseconds are out of range"))?;
            Ok((
                AnalyticsType::Interval,
                LiteralValue::Interval {
                    months,
                    days: day_time.days,
                    nanoseconds,
                },
            ))
        }
        Some(LiteralType::Decimal(value)) => {
            let precision = u8::try_from(value.precision)
                .map_err(|_| RequestError::invalid("literal decimal precision is invalid"))?;
            let scale = i8::try_from(value.scale)
                .map_err(|_| RequestError::invalid("literal decimal scale is invalid"))?;
            let decimal = DecimalType::new(precision, scale)
                .map_err(|error| RequestError::invalid(error.to_string()))?;
            let bytes: [u8; 16] = value.value.as_slice().try_into().map_err(|_| {
                RequestError::invalid("a decimal literal must contain exactly 16 bytes")
            })?;
            let unscaled = i128::from_le_bytes(bytes);
            let limit = 10_u128.pow(u32::from(precision));
            if unscaled.unsigned_abs() >= limit {
                return Err(RequestError::invalid(
                    "a decimal literal exceeds its declared precision",
                ));
            }
            Ok((
                AnalyticsType::Decimal(decimal),
                LiteralValue::Decimal {
                    value: unscaled,
                    precision,
                    scale,
                },
            ))
        }
        Some(LiteralType::Null(data_type)) => {
            let (data_type, nullable) = parse_scalar_type(data_type, extensions)?;
            if !nullable {
                return Err(RequestError::invalid(
                    "a typed null must declare a nullable type",
                ));
            }
            Ok((data_type.clone(), LiteralValue::Null { data_type }))
        }
        Some(LiteralType::UserDefined(value)) => {
            let Some(expression::literal::user_defined::Val::Struct(struct_value)) =
                value.val.as_ref()
            else {
                return Err(RequestError::undeclared(
                    "a logical literal must use the declared struct representation",
                ));
            };
            let [carrier] = struct_value.fields.as_slice() else {
                return Err(RequestError::invalid(
                    "a logical literal must contain exactly one carrier value",
                ));
            };
            let data_type = Type {
                kind: Some(substrait::proto::r#type::Kind::UserDefined(
                    substrait::proto::r#type::UserDefined {
                        type_reference: value.type_reference,
                        type_variation_reference: literal.type_variation_reference,
                        nullability: substrait::proto::r#type::Nullability::Required as i32,
                        type_parameters: value.type_parameters.clone(),
                    },
                )),
            };
            let (logical, _) = parse_scalar_type(&data_type, extensions)?;
            let (carrier_type, carrier) = parse(carrier, extensions)?;
            if logical_encoding(&logical).map(|(_, carrier)| carrier) != Some(carrier_type) {
                return Err(RequestError::invalid(
                    "a logical literal carrier does not match its declared logical type",
                ));
            }
            if !carrier_holds_value(&logical, &carrier) {
                return Err(RequestError::invalid(
                    "a logical literal carrier holds no value of its declared logical type",
                ));
            }
            Ok((
                logical.clone(),
                LiteralValue::Logical {
                    data_type: logical,
                    carrier: Box::new(carrier),
                },
            ))
        }
        Some(_) => Err(RequestError::undeclared(
            "the literal uses a type outside the initial SQL translation profile",
        )),
        None => Err(RequestError::invalid(
            "a literal is missing its value and type",
        )),
    }
}

fn require_interval_precision(
    value: &expression::literal::IntervalDayToSecond,
    expected: i32,
) -> Result<(), RequestError> {
    use expression::literal::interval_day_to_second::PrecisionMode;
    let precision = match value.precision_mode {
        Some(PrecisionMode::Microseconds(_)) => 6,
        Some(PrecisionMode::Precision(precision)) => precision,
        None => {
            return Err(RequestError::invalid(
                "an interval literal has no precision",
            ));
        }
    };
    if precision != expected {
        return Err(RequestError::undeclared(format!(
            "an interval literal must use precision {expected}"
        )));
    }
    Ok(())
}

fn duration_micros(value: &expression::literal::IntervalDayToSecond) -> Result<i64, RequestError> {
    let subseconds = interval_subseconds(value, 6)?;
    require_consistent_interval_sign(value.days, value.seconds, subseconds)?;
    i64::from(value.days)
        .checked_mul(86_400_000_000)
        .and_then(|days| {
            i64::from(value.seconds)
                .checked_mul(1_000_000)
                .and_then(|seconds| days.checked_add(seconds))
        })
        .and_then(|whole| whole.checked_add(subseconds))
        .ok_or_else(|| RequestError::invalid("a duration literal is out of range"))
}

fn interval_subseconds(
    value: &expression::literal::IntervalDayToSecond,
    expected_precision: i32,
) -> Result<i64, RequestError> {
    use expression::literal::interval_day_to_second::PrecisionMode;
    match value.precision_mode {
        Some(PrecisionMode::Microseconds(micros)) if expected_precision == 6 => {
            Ok(i64::from(micros))
        }
        Some(PrecisionMode::Precision(precision)) if precision == expected_precision => {
            Ok(value.subseconds)
        }
        _ => Err(RequestError::invalid(
            "interval literal precision is invalid",
        )),
    }
}

fn require_consistent_interval_sign(
    first: i32,
    second: i32,
    third: i64,
) -> Result<(), RequestError> {
    let has_positive = first > 0 || second > 0 || third > 0;
    let has_negative = first < 0 || second < 0 || third < 0;
    if has_positive && has_negative {
        return Err(RequestError::invalid(
            "an interval literal cannot mix positive and negative components",
        ));
    }
    Ok(())
}

/// Whether a carrier value is a value of its logical type: an unsigned
/// integer within its range, or a wide decimal's canonical text. A carrier of
/// another logical type holds any value of the carrier type.
fn carrier_holds_value(logical: &AnalyticsType, carrier: &LiteralValue) -> bool {
    match (logical, carrier) {
        (AnalyticsType::UnsignedByte, LiteralValue::SmallInt(value)) => {
            u8::try_from(*value).is_ok()
        }
        (AnalyticsType::UnsignedSmallInt, LiteralValue::Int(value)) => {
            u16::try_from(*value).is_ok()
        }
        (AnalyticsType::UnsignedInt, LiteralValue::BigInt(value)) => u32::try_from(*value).is_ok(),
        (AnalyticsType::UnsignedBigInt, LiteralValue::Decimal { value, .. }) => {
            u64::try_from(*value).is_ok()
        }
        (AnalyticsType::Decimal(decimal), LiteralValue::Text(text)) => {
            canonical_decimal_text(text, *decimal)
        }
        (
            AnalyticsType::UnsignedByte
            | AnalyticsType::UnsignedSmallInt
            | AnalyticsType::UnsignedInt
            | AnalyticsType::UnsignedBigInt
            | AnalyticsType::Decimal(_),
            _,
        ) => false,
        _ => true,
    }
}

/// Whether `text` is a value of `decimal` in canonical form: an optional `-`,
/// integer digits without leading zeros, and, for a positive scale, a point
/// followed by exactly `scale` digits. With a negative scale, the digits the
/// scale removes are zero. At most `precision` digits are significant.
fn canonical_decimal_text(text: &str, decimal: DecimalType) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let scale = decimal.scale();
    let (integer, fraction) = match unsigned.split_once('.') {
        Some((integer, fraction)) if scale > 0 => (integer, fraction),
        None if scale <= 0 => (unsigned, ""),
        _ => return false,
    };
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if integer.is_empty()
        || !digits(integer)
        || !digits(fraction)
        || (integer.len() > 1 && integer.starts_with('0'))
        || fraction.len() != usize::try_from(scale.max(0)).unwrap_or(usize::MAX)
    {
        return false;
    }
    let removed = usize::from(scale.min(0).unsigned_abs());
    let significant = integer.trim_start_matches('0').len() + fraction.len();
    let zero = integer == "0" && fraction.bytes().all(|byte| byte == b'0');
    if zero {
        return text == unsigned;
    }
    integer.len() > removed
        && integer[integer.len() - removed..]
            .bytes()
            .all(|byte| byte == b'0')
        && significant.saturating_sub(removed) <= usize::from(decimal.precision())
}

/// Encodes a canonical literal for the Substrait boundary.
pub(super) fn encode(
    value: LiteralValue,
    nullable: bool,
    extensions: &PlanExtensions,
) -> expression::Literal {
    use expression::literal::LiteralType;

    let literal_type = match value {
        LiteralValue::Boolean(value) => LiteralType::Boolean(value),
        LiteralValue::Byte(value) => LiteralType::I8(i32::from(value)),
        LiteralValue::SmallInt(value) => LiteralType::I16(i32::from(value)),
        LiteralValue::Int(value) => LiteralType::I32(value),
        LiteralValue::BigInt(value) => LiteralType::I64(value),
        LiteralValue::Float(value) => LiteralType::Fp32(value),
        LiteralValue::Double(value) => LiteralType::Fp64(value),
        LiteralValue::Text(value) => LiteralType::String(value),
        LiteralValue::Blob(value) => LiteralType::Binary(value),
        LiteralValue::Date(value) => LiteralType::Date(value),
        LiteralValue::Time(value) => {
            LiteralType::PrecisionTime(expression::literal::PrecisionTime {
                precision: 6,
                value,
            })
        }
        LiteralValue::Timestamp(value) => {
            LiteralType::PrecisionTimestamp(expression::literal::PrecisionTimestamp {
                precision: 6,
                value,
            })
        }
        LiteralValue::TimestampTz(value) => {
            LiteralType::PrecisionTimestampTz(expression::literal::PrecisionTimestamp {
                precision: 6,
                value,
            })
        }
        LiteralValue::Duration(value) => {
            let days = value / 86_400_000_000;
            let within_day = value % 86_400_000_000;
            let seconds = within_day / 1_000_000;
            let micros = within_day % 1_000_000;
            LiteralType::IntervalDayToSecond(expression::literal::IntervalDayToSecond {
                days: i32::try_from(days).expect("validated duration days fit in i32"),
                seconds: i32::try_from(seconds).expect("duration seconds within a day fit in i32"),
                subseconds: 0,
                precision_mode: Some(
                    expression::literal::interval_day_to_second::PrecisionMode::Microseconds(
                        i32::try_from(micros).expect("duration microseconds fit in i32"),
                    ),
                ),
            })
        }
        LiteralValue::Interval {
            months,
            days,
            nanoseconds,
        } => {
            let seconds = nanoseconds / 1_000_000_000;
            let subseconds = nanoseconds % 1_000_000_000;
            LiteralType::IntervalCompound(expression::literal::IntervalCompound {
                interval_year_to_month: Some(expression::literal::IntervalYearToMonth {
                    years: months / 12,
                    months: months % 12,
                }),
                interval_day_to_second: Some(expression::literal::IntervalDayToSecond {
                    days,
                    seconds: i32::try_from(seconds).expect("validated interval seconds fit in i32"),
                    subseconds,
                    precision_mode: Some(
                        expression::literal::interval_day_to_second::PrecisionMode::Precision(9),
                    ),
                }),
            })
        }
        LiteralValue::Decimal {
            value,
            precision,
            scale,
        } => LiteralType::Decimal(expression::literal::Decimal {
            value: value.to_le_bytes().to_vec(),
            precision: i32::from(precision),
            scale: i32::from(scale),
        }),
        LiteralValue::Null { data_type } => {
            LiteralType::Null(encode_type(&data_type, true, extensions))
        }
        LiteralValue::Logical { data_type, carrier } => {
            let Type {
                kind: Some(substrait::proto::r#type::Kind::UserDefined(data_type)),
            } = encode_type(&data_type, false, extensions)
            else {
                unreachable!("logical Analytics types encode as user-defined types")
            };
            LiteralType::UserDefined(expression::literal::UserDefined {
                type_reference: data_type.type_reference,
                type_parameters: data_type.type_parameters,
                val: Some(expression::literal::user_defined::Val::Struct(
                    expression::literal::Struct {
                        fields: vec![encode(*carrier, false, extensions)],
                    },
                )),
            })
        }
    };
    expression::Literal {
        literal_type: Some(literal_type),
        nullable,
        ..Default::default()
    }
}
