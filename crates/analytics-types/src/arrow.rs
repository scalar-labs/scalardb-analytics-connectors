//! Conversion between the Analytics schema model and Arrow schemas.

use std::{collections::HashMap, sync::Arc};

use arrow_schema::{
    DataType, Field, IntervalUnit, Schema, TimeUnit,
    extension::{EXTENSION_TYPE_METADATA_KEY, EXTENSION_TYPE_NAME_KEY},
};
use serde_json::{Map, Value, json};

use crate::{
    AnalyticsField, AnalyticsSchema, AnalyticsType, ArrayType, DecimalType, EnumType, MapType,
    StructType, TypeError,
};

const EXTENSION_NAME_PREFIX: &str = "scalardb.analytics.";

impl AnalyticsSchema {
    /// Converts this Analytics schema to its Arrow representation.
    pub fn to_arrow(&self) -> Schema {
        Schema::new(
            self.fields()
                .iter()
                .map(AnalyticsField::to_arrow)
                .collect::<Vec<_>>(),
        )
    }

    /// Reconstructs an Analytics schema from a supported Arrow schema.
    pub fn try_from_arrow(schema: &Schema) -> Result<Self, TypeError> {
        let fields = schema
            .fields()
            .iter()
            .map(|field| AnalyticsField::try_from_arrow(field))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(fields))
    }
}

impl AnalyticsField {
    /// Converts this Analytics field to its canonical Arrow representation.
    pub fn to_arrow(&self) -> Field {
        let field = Field::new(
            &self.name,
            analytics_type_to_arrow(&self.data_type),
            self.nullable,
        );
        let Some((extension_name, extension_metadata)) = arrow_extension_encoding(&self.data_type)
        else {
            return field;
        };
        field.with_metadata(HashMap::from([
            (
                EXTENSION_TYPE_NAME_KEY.to_owned(),
                format!("{EXTENSION_NAME_PREFIX}{extension_name}"),
            ),
            (EXTENSION_TYPE_METADATA_KEY.to_owned(), extension_metadata),
        ]))
    }

    /// Reconstructs an Analytics field from a supported Arrow field.
    pub fn try_from_arrow(field: &Field) -> Result<Self, TypeError> {
        let extension_name = field.metadata().get(EXTENSION_TYPE_NAME_KEY);
        let extension_metadata = field.metadata().get(EXTENSION_TYPE_METADATA_KEY);

        let data_type = match (extension_name, extension_metadata) {
            (None, None) => analytics_type_from_arrow(field.data_type())?,
            (None, Some(_)) => return Err(TypeError::OrphanExtensionMetadata),
            (Some(name), metadata) => {
                let analytics_type =
                    analytics_type_from_extension(name, metadata.map(String::as_str))?;
                let expected = analytics_type_to_arrow(&analytics_type);
                if field.data_type() != &expected {
                    return Err(TypeError::ExtensionStorageMismatch {
                        name: name.clone(),
                        expected: format!("{expected:?}"),
                        actual: format!("{:?}", field.data_type()),
                    });
                }
                analytics_type
            }
        };

        Ok(Self::new(
            field.name().clone(),
            data_type,
            field.is_nullable(),
        ))
    }
}

fn arrow_extension_encoding(data_type: &AnalyticsType) -> Option<(&'static str, String)> {
    let encoding = match data_type {
        AnalyticsType::Uuid => ("uuid", "{}".to_owned()),
        AnalyticsType::Json => ("json", "{}".to_owned()),
        AnalyticsType::Enum(enum_type) => (
            "enum",
            json!({ "members": enum_type.members() }).to_string(),
        ),
        AnalyticsType::Year => ("year", "{}".to_owned()),
        AnalyticsType::TimeWithTimeZone => ("timetz", "{}".to_owned()),
        _ => return None,
    };
    Some(encoding)
}

fn analytics_type_from_extension(
    name: &str,
    metadata: Option<&str>,
) -> Result<AnalyticsType, TypeError> {
    let Some(short_name) = name.strip_prefix(EXTENSION_NAME_PREFIX) else {
        return Err(TypeError::UnknownExtensionName {
            name: name.to_owned(),
        });
    };
    let metadata = metadata.ok_or_else(|| TypeError::InvalidExtensionMetadata {
        name: name.to_owned(),
        reason: "missing ARROW:extension:metadata".to_owned(),
    })?;
    let value: Value =
        serde_json::from_str(metadata).map_err(|error| TypeError::InvalidExtensionMetadata {
            name: name.to_owned(),
            reason: error.to_string(),
        })?;
    let object = value
        .as_object()
        .ok_or_else(|| TypeError::InvalidExtensionMetadata {
            name: name.to_owned(),
            reason: "expected a JSON object".to_owned(),
        })?;

    match short_name {
        "uuid" => no_parameter_type(name, object, AnalyticsType::Uuid),
        "json" => no_parameter_type(name, object, AnalyticsType::Json),
        "year" => no_parameter_type(name, object, AnalyticsType::Year),
        "timetz" => no_parameter_type(name, object, AnalyticsType::TimeWithTimeZone),
        "enum" => {
            if object.len() != 1 {
                return Err(invalid_extension_metadata(
                    name,
                    "expected only the members parameter",
                ));
            }
            let members = object
                .get("members")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_extension_metadata(name, "members must be an array"))?
                .iter()
                .map(|member| {
                    member.as_str().map(str::to_owned).ok_or_else(|| {
                        invalid_extension_metadata(name, "every enum member must be a string")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(AnalyticsType::Enum(EnumType::new(members)))
        }
        _ => Err(TypeError::UnknownExtensionName {
            name: name.to_owned(),
        }),
    }
}

fn no_parameter_type(
    name: &str,
    object: &Map<String, Value>,
    data_type: AnalyticsType,
) -> Result<AnalyticsType, TypeError> {
    if object.is_empty() {
        Ok(data_type)
    } else {
        Err(invalid_extension_metadata(
            name,
            "this Analytics type has no parameters",
        ))
    }
}

fn invalid_extension_metadata(name: &str, reason: &str) -> TypeError {
    TypeError::InvalidExtensionMetadata {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

fn analytics_type_to_arrow(data_type: &AnalyticsType) -> DataType {
    match data_type {
        AnalyticsType::Boolean => DataType::Boolean,
        AnalyticsType::Byte => DataType::Int8,
        AnalyticsType::SmallInt => DataType::Int16,
        AnalyticsType::Int => DataType::Int32,
        AnalyticsType::BigInt => DataType::Int64,
        AnalyticsType::Float => DataType::Float32,
        AnalyticsType::Double => DataType::Float64,
        AnalyticsType::Decimal(decimal) => {
            DataType::Decimal128(decimal.precision(), decimal.scale())
        }
        AnalyticsType::Text | AnalyticsType::Uuid | AnalyticsType::Json => DataType::Utf8,
        AnalyticsType::Enum(_) | AnalyticsType::TimeWithTimeZone => DataType::Utf8,
        AnalyticsType::Year => DataType::Int16,
        AnalyticsType::Blob => DataType::Binary,
        AnalyticsType::Date => DataType::Date32,
        AnalyticsType::Time => DataType::Time64(TimeUnit::Microsecond),
        AnalyticsType::Timestamp => DataType::Timestamp(TimeUnit::Microsecond, None),
        AnalyticsType::TimestampTz => {
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
        }
        AnalyticsType::Duration => DataType::Duration(TimeUnit::Microsecond),
        AnalyticsType::Interval => DataType::Interval(IntervalUnit::MonthDayNano),
        AnalyticsType::Array(array) => DataType::List(Arc::new(
            AnalyticsField::new(
                Field::LIST_FIELD_DEFAULT_NAME,
                (*array.element_type).clone(),
                array.element_nullable,
            )
            .to_arrow(),
        )),
        AnalyticsType::Map(map) => {
            let key = AnalyticsField::new("key", (*map.key_type).clone(), false).to_arrow();
            let value = AnalyticsField::new("value", (*map.value_type).clone(), map.value_nullable)
                .to_arrow();
            let entries = Field::new("entries", DataType::Struct(vec![key, value].into()), false);
            DataType::Map(Arc::new(entries), false)
        }
        AnalyticsType::Struct(struct_type) => DataType::Struct(
            struct_type
                .fields
                .iter()
                .map(AnalyticsField::to_arrow)
                .collect::<Vec<_>>()
                .into(),
        ),
    }
}

fn analytics_type_from_arrow(data_type: &DataType) -> Result<AnalyticsType, TypeError> {
    let analytics_type = match data_type {
        DataType::Boolean => AnalyticsType::Boolean,
        DataType::Int8 => AnalyticsType::Byte,
        DataType::Int16 => AnalyticsType::SmallInt,
        DataType::Int32 => AnalyticsType::Int,
        DataType::Int64 => AnalyticsType::BigInt,
        DataType::Float32 => AnalyticsType::Float,
        DataType::Float64 => AnalyticsType::Double,
        DataType::Decimal128(precision, scale) => {
            AnalyticsType::Decimal(DecimalType::new(*precision, *scale)?)
        }
        DataType::Utf8 => AnalyticsType::Text,
        DataType::Binary => AnalyticsType::Blob,
        DataType::Date32 => AnalyticsType::Date,
        DataType::Time64(TimeUnit::Microsecond) => AnalyticsType::Time,
        DataType::Timestamp(TimeUnit::Microsecond, None) => AnalyticsType::Timestamp,
        DataType::Timestamp(TimeUnit::Microsecond, Some(timezone))
            if timezone.as_ref() == "UTC" =>
        {
            AnalyticsType::TimestampTz
        }
        DataType::Duration(TimeUnit::Microsecond) => AnalyticsType::Duration,
        DataType::Interval(IntervalUnit::MonthDayNano) => AnalyticsType::Interval,
        DataType::List(element) => {
            require_field_shape(element, "list item", "item", None)?;
            let element = AnalyticsField::try_from_arrow(element)?;
            AnalyticsType::Array(ArrayType::new(element.data_type, element.nullable))
        }
        DataType::Map(entries, false) => AnalyticsType::Map(map_from_arrow(entries)?),
        DataType::Map(_, true) => {
            return Err(TypeError::InvalidArrowShape {
                container: "map",
                reason: "sorted maps are not part of the Analytics type set".to_owned(),
            });
        }
        DataType::Struct(fields) => AnalyticsType::Struct(StructType::new(
            fields
                .iter()
                .map(|field| AnalyticsField::try_from_arrow(field))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        unsupported => {
            return Err(TypeError::UnsupportedArrowType {
                data_type: format!("{unsupported:?}"),
            });
        }
    };
    Ok(analytics_type)
}

fn map_from_arrow(entries: &Field) -> Result<MapType, TypeError> {
    require_field_shape(entries, "map entries", "entries", Some(false))?;
    let DataType::Struct(fields) = entries.data_type() else {
        return Err(TypeError::InvalidArrowShape {
            container: "map",
            reason: "entries must be a struct".to_owned(),
        });
    };
    if fields.len() != 2 {
        return Err(TypeError::InvalidArrowShape {
            container: "map",
            reason: "entries struct must contain exactly key and value fields".to_owned(),
        });
    }
    let key = &fields[0];
    let value = &fields[1];
    require_field_shape(key, "map key", "key", Some(false))?;
    require_field_shape(value, "map value", "value", None)?;
    let key = AnalyticsField::try_from_arrow(key)?;
    let value = AnalyticsField::try_from_arrow(value)?;
    Ok(MapType::new(key.data_type, value.data_type, value.nullable))
}

fn require_field_shape(
    field: &Field,
    role: &'static str,
    expected_name: &str,
    expected_nullable: Option<bool>,
) -> Result<(), TypeError> {
    if field.name() != expected_name {
        return Err(TypeError::InvalidArrowShape {
            container: role,
            reason: format!("field must be named {expected_name:?}"),
        });
    }
    if let Some(expected) = expected_nullable
        && field.is_nullable() != expected
    {
        return Err(TypeError::InvalidArrowShape {
            container: role,
            reason: format!("nullability must be {expected}"),
        });
    }
    Ok(())
}
