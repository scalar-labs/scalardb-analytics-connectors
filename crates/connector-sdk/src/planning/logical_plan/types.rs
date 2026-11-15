//! Mapping from Substrait types to the Analytics type system.

use analytics_types::{
    AnalyticsField, AnalyticsType, ArrayType, DecimalType, EnumType, MapType, StructType,
};
use serde_json::Value;
use substrait::proto::{Type, r#type};

use super::profile::PlanExtensions;
use crate::RequestError;

/// The largest precision of Substrait's own decimal type. A wider Analytics
/// decimal is the logical `decimal` type over decimal text.
const SUBSTRAIT_DECIMAL_PRECISION: u8 = 38;

/// The logical type name of an Analytics type that Substrait has no type for,
/// and the carrier type that holds its values: a string for text-like types,
/// the next wider signed integer for unsigned integers up to 32 bits,
/// `decimal(20, 0)` for unsigned 64-bit integers, and decimal text for
/// decimals wider than Substrait's.
pub(super) fn logical_encoding(data_type: &AnalyticsType) -> Option<(&'static str, AnalyticsType)> {
    let encoding = match data_type {
        AnalyticsType::Uuid => ("uuid", AnalyticsType::Text),
        AnalyticsType::Json => ("json", AnalyticsType::Text),
        AnalyticsType::Enum(_) => ("enum", AnalyticsType::Text),
        AnalyticsType::TimeWithTimeZone => ("timetz", AnalyticsType::Text),
        AnalyticsType::Year => ("year", AnalyticsType::SmallInt),
        AnalyticsType::UnsignedByte => ("unsigned_byte", AnalyticsType::SmallInt),
        AnalyticsType::UnsignedSmallInt => ("unsigned_smallint", AnalyticsType::Int),
        AnalyticsType::UnsignedInt => ("unsigned_int", AnalyticsType::BigInt),
        AnalyticsType::UnsignedBigInt => (
            "unsigned_bigint",
            AnalyticsType::Decimal(DecimalType::new(20, 0).expect("decimal(20, 0) is valid")),
        ),
        AnalyticsType::Decimal(decimal) if decimal.precision() > SUBSTRAIT_DECIMAL_PRECISION => {
            ("decimal", AnalyticsType::Text)
        }
        _ => return None,
    };
    Some(encoding)
}

/// Converts one Substrait type and its nullability to the Analytics type model.
///
/// Nested struct names are consumed from the same depth-first name stream used
/// by the enclosing `NamedStruct`. User-defined logical types are resolved
/// through the validated extension registry.
pub(super) fn parse_type<'a>(
    data_type: &Type,
    names: &mut impl Iterator<Item = &'a String>,
    extensions: &PlanExtensions,
) -> Result<(AnalyticsType, bool), RequestError> {
    let kind = data_type
        .kind
        .as_ref()
        .ok_or_else(|| RequestError::invalid("a schema type is missing its kind"))?;
    let (data_type, nullability) = match kind {
        r#type::Kind::Bool(value) => (AnalyticsType::Boolean, value.nullability),
        r#type::Kind::I8(value) => (AnalyticsType::Byte, value.nullability),
        r#type::Kind::I16(value) => (AnalyticsType::SmallInt, value.nullability),
        r#type::Kind::I32(value) => (AnalyticsType::Int, value.nullability),
        r#type::Kind::I64(value) => (AnalyticsType::BigInt, value.nullability),
        r#type::Kind::Fp32(value) => (AnalyticsType::Float, value.nullability),
        r#type::Kind::Fp64(value) => (AnalyticsType::Double, value.nullability),
        r#type::Kind::String(value) => (AnalyticsType::Text, value.nullability),
        r#type::Kind::Binary(value) => (AnalyticsType::Blob, value.nullability),
        r#type::Kind::Date(value) => (AnalyticsType::Date, value.nullability),
        r#type::Kind::PrecisionTime(value) if value.precision == 6 => {
            (AnalyticsType::Time, value.nullability)
        }
        r#type::Kind::PrecisionTimestamp(value) if value.precision == 6 => {
            (AnalyticsType::Timestamp, value.nullability)
        }
        r#type::Kind::PrecisionTimestampTz(value) if value.precision == 6 => {
            (AnalyticsType::TimestampTz, value.nullability)
        }
        r#type::Kind::Decimal(value) => {
            let precision = u8::try_from(value.precision)
                .ok()
                .filter(|precision| *precision <= SUBSTRAIT_DECIMAL_PRECISION)
                .ok_or_else(|| {
                    RequestError::invalid(
                        "decimal precision is outside Substrait's decimal range; \
                         wider decimals use the logical decimal type",
                    )
                })?;
            let scale = i8::try_from(value.scale)
                .map_err(|_| RequestError::invalid("decimal scale is outside the valid range"))?;
            let decimal = DecimalType::new(precision, scale)
                .map_err(|error| RequestError::invalid(error.to_string()))?;
            (AnalyticsType::Decimal(decimal), value.nullability)
        }
        r#type::Kind::IntervalDay(value) if value.precision.unwrap_or(6) == 6 => {
            (AnalyticsType::Duration, value.nullability)
        }
        r#type::Kind::IntervalCompound(value) if value.precision == 9 => {
            (AnalyticsType::Interval, value.nullability)
        }
        r#type::Kind::Struct(value) => {
            let mut fields = Vec::with_capacity(value.types.len());
            for child in &value.types {
                let name = next_name(names)?;
                let (child, nullable) = parse_type(child, names, extensions)?;
                fields.push(AnalyticsField::new(name, child, nullable));
            }
            (
                AnalyticsType::Struct(StructType::new(fields)),
                value.nullability,
            )
        }
        r#type::Kind::List(value) => {
            let child = value
                .r#type
                .as_ref()
                .ok_or_else(|| RequestError::invalid("a list type has no element type"))?;
            let (child, child_nullable) = parse_type(child, names, extensions)?;
            (
                AnalyticsType::Array(ArrayType::new(child, child_nullable)),
                value.nullability,
            )
        }
        r#type::Kind::Map(value) => {
            let key = value
                .key
                .as_ref()
                .ok_or_else(|| RequestError::invalid("a map type has no key type"))?;
            let value_type = value
                .value
                .as_ref()
                .ok_or_else(|| RequestError::invalid("a map type has no value type"))?;
            let (key, key_nullable) = parse_type(key, names, extensions)?;
            if key_nullable {
                return Err(RequestError::invalid("map keys may not be nullable"));
            }
            let (value_type, value_nullable) = parse_type(value_type, names, extensions)?;
            (
                AnalyticsType::Map(MapType::new(key, value_type, value_nullable)),
                value.nullability,
            )
        }
        r#type::Kind::UserDefined(value) => {
            return parse_logical_type(value, names, extensions);
        }
        _ => {
            return Err(RequestError::undeclared(
                "the schema contains a type outside the declared contract",
            ));
        }
    };
    Ok((data_type, parse_nullability(nullability)?))
}

/// Converts Substrait's required/nullable enum to an Analytics nullable flag.
fn parse_nullability(nullability: i32) -> Result<bool, RequestError> {
    match r#type::Nullability::try_from(nullability) {
        Ok(r#type::Nullability::Required) => Ok(false),
        Ok(r#type::Nullability::Nullable) => Ok(true),
        _ => Err(RequestError::invalid(
            "schema types must declare required or nullable nullability",
        )),
    }
}

/// Decodes a ScalarDB Analytics logical type and validates its carrier type.
///
/// Logical types use three Substrait parameters: the logical name, JSON
/// parameters, and a required physical carrier type.
fn parse_logical_type<'a>(
    data_type: &r#type::UserDefined,
    names: &mut impl Iterator<Item = &'a String>,
    extensions: &PlanExtensions,
) -> Result<(AnalyticsType, bool), RequestError> {
    if !extensions.is_logical_type(data_type.type_reference) {
        return Err(RequestError::undeclared(
            "user-defined types must reference the declared logical type",
        ));
    }
    if data_type.type_parameters.len() != 3 {
        return Err(RequestError::invalid(
            "logical types require name, JSON parameters, and carrier type parameters",
        ));
    }
    let name = string_parameter(&data_type.type_parameters[0], "logical type name")?;
    let parameters = string_parameter(&data_type.type_parameters[1], "logical type parameters")?;
    let carrier = match data_type.type_parameters[2].parameter.as_ref() {
        Some(r#type::parameter::Parameter::DataType(carrier)) => carrier,
        _ => {
            return Err(RequestError::invalid(
                "the logical carrier must be a data type",
            ));
        }
    };
    let (carrier, carrier_nullable) = parse_type(carrier, names, extensions)?;
    if carrier_nullable {
        return Err(RequestError::invalid(
            "the logical carrier type must be required",
        ));
    }

    let logical = match name {
        "enum" => parse_enum(parameters, carrier)?,
        "decimal" => parse_wide_decimal(parameters, carrier)?,
        _ => {
            let logical = PARAMETERLESS_LOGICAL_TYPES
                .iter()
                .find(|logical| logical_encoding(logical).is_some_and(|(known, _)| known == name))
                .ok_or_else(|| {
                    RequestError::undeclared(format!("logical type {name:?} is not supported"))
                })?;
            logical_without_parameters(parameters, carrier, logical.clone())?
        }
    };
    Ok((logical, parse_nullability(data_type.nullability)?))
}

/// The logical types without parameters, found by their names.
const PARAMETERLESS_LOGICAL_TYPES: [AnalyticsType; 8] = [
    AnalyticsType::Uuid,
    AnalyticsType::Json,
    AnalyticsType::Year,
    AnalyticsType::TimeWithTimeZone,
    AnalyticsType::UnsignedByte,
    AnalyticsType::UnsignedSmallInt,
    AnalyticsType::UnsignedInt,
    AnalyticsType::UnsignedBigInt,
];

/// Decodes a decimal wider than Substrait's from the logical type's JSON
/// parameters, `{"precision": p, "scale": s}`, over decimal text.
fn parse_wide_decimal(
    parameters: &str,
    carrier: AnalyticsType,
) -> Result<AnalyticsType, RequestError> {
    if carrier != AnalyticsType::Text {
        return Err(RequestError::invalid(
            "the decimal carrier type must be string",
        ));
    }
    let value: Value = serde_json::from_str(parameters)
        .map_err(|error| RequestError::invalid(format!("invalid decimal parameters: {error}")))?;
    let parameter = |key: &str| value.get(key).and_then(Value::as_i64);
    let (Some(precision), Some(scale)) = (parameter("precision"), parameter("scale")) else {
        return Err(RequestError::invalid(
            "decimal parameters must hold an integer precision and scale",
        ));
    };
    if value.as_object().is_none_or(|object| object.len() != 2) {
        return Err(RequestError::invalid(
            "decimal parameters must contain only precision and scale",
        ));
    }
    let (Ok(precision), Ok(scale)) = (u8::try_from(precision), i8::try_from(scale)) else {
        return Err(RequestError::invalid("decimal parameters are out of range"));
    };
    if precision <= SUBSTRAIT_DECIMAL_PRECISION {
        return Err(RequestError::invalid(
            "a decimal within Substrait's decimal range uses Substrait's decimal type",
        ));
    }
    let decimal = DecimalType::new(precision, scale)
        .map_err(|error| RequestError::invalid(error.to_string()))?;
    Ok(AnalyticsType::Decimal(decimal))
}

/// Decodes enum members from the logical type's JSON parameters.
fn parse_enum(parameters: &str, carrier: AnalyticsType) -> Result<AnalyticsType, RequestError> {
    if carrier != AnalyticsType::Text {
        return Err(RequestError::invalid(
            "the enum carrier type must be string",
        ));
    }
    let value: Value = serde_json::from_str(parameters)
        .map_err(|error| RequestError::invalid(format!("invalid enum parameters: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| RequestError::invalid("enum parameters must be a JSON object"))?;
    if object.len() != 1 {
        return Err(RequestError::invalid(
            "enum parameters must contain only the members property",
        ));
    }
    let members = object
        .get("members")
        .and_then(Value::as_array)
        .ok_or_else(|| RequestError::invalid("enum members must be a JSON array"))?;
    let members = members
        .iter()
        .map(|member| {
            member
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| RequestError::invalid("enum members must be strings"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AnalyticsType::Enum(EnumType::new(members)))
}

/// Reads one logical-type parameter that is required to be a string.
fn string_parameter<'a>(
    parameter: &'a r#type::Parameter,
    description: &str,
) -> Result<&'a str, RequestError> {
    match parameter.parameter.as_ref() {
        Some(r#type::parameter::Parameter::String(value)) => Ok(value),
        _ => Err(RequestError::invalid(format!(
            "the {description} must be a string"
        ))),
    }
}

/// Builds a parameterless logical type after checking its physical carrier.
fn logical_without_parameters(
    parameters: &str,
    carrier: AnalyticsType,
    logical: AnalyticsType,
) -> Result<AnalyticsType, RequestError> {
    if parameters != "{}" {
        return Err(RequestError::invalid(
            "this logical type does not accept parameters",
        ));
    }
    if logical_encoding(&logical).map(|(_, expected)| expected) != Some(carrier) {
        return Err(RequestError::invalid(
            "the logical type uses an unexpected carrier type",
        ));
    }
    Ok(logical)
}

/// Consumes one required non-empty nested field name.
fn next_name<'a>(names: &mut impl Iterator<Item = &'a String>) -> Result<String, RequestError> {
    let name = names
        .next()
        .ok_or_else(|| RequestError::invalid("the schema does not contain enough field names"))?;
    if name.is_empty() {
        return Err(RequestError::invalid("schema field names may not be empty"));
    }
    Ok(name.clone())
}

/// Encodes a connector-owned Analytics type for the Substrait boundary.
pub(super) fn encode_type(
    data_type: &AnalyticsType,
    nullable: bool,
    extensions: &PlanExtensions,
) -> Type {
    let nullability = if nullable {
        r#type::Nullability::Nullable as i32
    } else {
        r#type::Nullability::Required as i32
    };
    let kind = match data_type {
        AnalyticsType::Boolean => r#type::Kind::Bool(r#type::Boolean {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Byte => r#type::Kind::I8(r#type::I8 {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::SmallInt => r#type::Kind::I16(r#type::I16 {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Int => r#type::Kind::I32(r#type::I32 {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::BigInt => r#type::Kind::I64(r#type::I64 {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::UnsignedByte
        | AnalyticsType::UnsignedSmallInt
        | AnalyticsType::UnsignedInt
        | AnalyticsType::UnsignedBigInt => logical_type(data_type, nullability, extensions),
        AnalyticsType::Float => r#type::Kind::Fp32(r#type::Fp32 {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Double => r#type::Kind::Fp64(r#type::Fp64 {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Decimal(decimal) if decimal.precision() > SUBSTRAIT_DECIMAL_PRECISION => {
            logical_type(data_type, nullability, extensions)
        }
        AnalyticsType::Decimal(decimal) => r#type::Kind::Decimal(r#type::Decimal {
            precision: i32::from(decimal.precision()),
            scale: i32::from(decimal.scale()),
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Text => r#type::Kind::String(r#type::String {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Blob => r#type::Kind::Binary(r#type::Binary {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Date => r#type::Kind::Date(r#type::Date {
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Time => r#type::Kind::PrecisionTime(r#type::PrecisionTime {
            precision: 6,
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Timestamp => r#type::Kind::PrecisionTimestamp(r#type::PrecisionTimestamp {
            precision: 6,
            nullability,
            ..Default::default()
        }),
        AnalyticsType::TimestampTz => {
            r#type::Kind::PrecisionTimestampTz(r#type::PrecisionTimestampTz {
                precision: 6,
                nullability,
                ..Default::default()
            })
        }
        AnalyticsType::Duration => r#type::Kind::IntervalDay(r#type::IntervalDay {
            precision: Some(6),
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Interval => r#type::Kind::IntervalCompound(r#type::IntervalCompound {
            precision: 9,
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Array(array) => r#type::Kind::List(Box::new(r#type::List {
            r#type: Some(Box::new(encode_type(
                array.element_type(),
                array.is_element_nullable(),
                extensions,
            ))),
            nullability,
            ..Default::default()
        })),
        AnalyticsType::Map(map) => r#type::Kind::Map(Box::new(r#type::Map {
            key: Some(Box::new(encode_type(map.key_type(), false, extensions))),
            value: Some(Box::new(encode_type(
                map.value_type(),
                map.is_value_nullable(),
                extensions,
            ))),
            nullability,
            ..Default::default()
        })),
        AnalyticsType::Struct(struct_type) => r#type::Kind::Struct(r#type::Struct {
            types: struct_type
                .fields()
                .iter()
                .map(|field| encode_type(field.data_type(), field.is_nullable(), extensions))
                .collect(),
            nullability,
            ..Default::default()
        }),
        AnalyticsType::Uuid
        | AnalyticsType::Json
        | AnalyticsType::Enum(_)
        | AnalyticsType::Year
        | AnalyticsType::TimeWithTimeZone => logical_type(data_type, nullability, extensions),
    };
    Type { kind: Some(kind) }
}

/// Encodes an Analytics type that [`logical_encoding`] names as the logical
/// type, with its JSON parameters and carrier.
fn logical_type(
    data_type: &AnalyticsType,
    nullability: i32,
    extensions: &PlanExtensions,
) -> r#type::Kind {
    let (name, carrier) =
        logical_encoding(data_type).expect("only types with a logical encoding are encoded here");
    let parameters = match data_type {
        AnalyticsType::Enum(enum_type) => {
            serde_json::json!({ "members": enum_type.members() }).to_string()
        }
        AnalyticsType::Decimal(decimal) => serde_json::json!({
            "precision": decimal.precision(),
            "scale": decimal.scale(),
        })
        .to_string(),
        _ => "{}".to_owned(),
    };
    r#type::Kind::UserDefined(r#type::UserDefined {
        type_reference: extensions.logical_type_anchor(),
        type_parameters: vec![
            string_type_parameter(name),
            string_type_parameter(&parameters),
            r#type::Parameter {
                parameter: Some(r#type::parameter::Parameter::DataType(encode_type(
                    &carrier, false, extensions,
                ))),
            },
        ],
        nullability,
        ..Default::default()
    })
}

fn string_type_parameter(value: &str) -> r#type::Parameter {
    r#type::Parameter {
        parameter: Some(r#type::parameter::Parameter::String(value.to_owned())),
    }
}
