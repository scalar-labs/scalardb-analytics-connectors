//! Mapping from Substrait types to the Analytics type system.

use analytics_types::{
    AnalyticsField, AnalyticsType, ArrayType, DecimalType, EnumType, MapType, StructType,
};
use serde_json::Value;
use substrait::proto::{Type, r#type};

use super::{LOGICAL_TYPE_NAME, extensions::ExtensionRegistry};
use crate::RequestError;

pub(super) fn parse_type<'a>(
    data_type: &Type,
    names: &mut impl Iterator<Item = &'a String>,
    extensions: &ExtensionRegistry,
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
            let precision = u8::try_from(value.precision).map_err(|_| {
                RequestError::invalid("decimal precision is outside the valid range")
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

fn parse_nullability(nullability: i32) -> Result<bool, RequestError> {
    match r#type::Nullability::try_from(nullability) {
        Ok(r#type::Nullability::Required) => Ok(false),
        Ok(r#type::Nullability::Nullable) => Ok(true),
        _ => Err(RequestError::invalid(
            "schema types must declare required or nullable nullability",
        )),
    }
}

fn parse_logical_type<'a>(
    data_type: &r#type::UserDefined,
    names: &mut impl Iterator<Item = &'a String>,
    extensions: &ExtensionRegistry,
) -> Result<(AnalyticsType, bool), RequestError> {
    if extensions
        .types
        .get(&data_type.type_reference)
        .map(String::as_str)
        != Some(LOGICAL_TYPE_NAME)
    {
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
        "uuid" => logical_without_parameters(
            parameters,
            carrier,
            AnalyticsType::Text,
            AnalyticsType::Uuid,
        )?,
        "json" => logical_without_parameters(
            parameters,
            carrier,
            AnalyticsType::Text,
            AnalyticsType::Json,
        )?,
        "year" => logical_without_parameters(
            parameters,
            carrier,
            AnalyticsType::SmallInt,
            AnalyticsType::Year,
        )?,
        "timetz" => logical_without_parameters(
            parameters,
            carrier,
            AnalyticsType::Text,
            AnalyticsType::TimeWithTimeZone,
        )?,
        "enum" => parse_enum(parameters, carrier)?,
        _ => {
            return Err(RequestError::undeclared(format!(
                "logical type {name:?} is not supported"
            )));
        }
    };
    Ok((logical, parse_nullability(data_type.nullability)?))
}

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

fn logical_without_parameters(
    parameters: &str,
    carrier: AnalyticsType,
    expected_carrier: AnalyticsType,
    logical: AnalyticsType,
) -> Result<AnalyticsType, RequestError> {
    if parameters != "{}" {
        return Err(RequestError::invalid(
            "this logical type does not accept parameters",
        ));
    }
    if carrier != expected_carrier {
        return Err(RequestError::invalid(
            "the logical type uses an unexpected carrier type",
        ));
    }
    Ok(logical)
}

fn next_name<'a>(names: &mut impl Iterator<Item = &'a String>) -> Result<String, RequestError> {
    let name = names
        .next()
        .ok_or_else(|| RequestError::invalid("the schema does not contain enough field names"))?;
    if name.is_empty() {
        return Err(RequestError::invalid("schema field names may not be empty"));
    }
    Ok(name.clone())
}
