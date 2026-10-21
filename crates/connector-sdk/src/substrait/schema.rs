//! Conversion of a Substrait schema and projection into Analytics types.

use analytics_types::{AnalyticsField, AnalyticsSchema, AnalyticsType};
use substrait::proto::{NamedStruct, expression};

use super::{extensions::ExtensionRegistry, types::parse_type};
use crate::RequestError;

pub(super) fn parse_projection(
    schema: &AnalyticsSchema,
    projection: Option<&expression::MaskExpression>,
) -> Result<AnalyticsSchema, RequestError> {
    let Some(projection) = projection else {
        return Ok(schema.clone());
    };
    if projection.maintain_singular_struct {
        return Err(RequestError::undeclared(
            "maintain_singular_struct projections are not accepted",
        ));
    }
    let select = projection
        .select
        .as_ref()
        .ok_or_else(|| RequestError::invalid("the projection selection is missing"))?;
    let mut fields = Vec::with_capacity(select.struct_items.len());
    for item in &select.struct_items {
        if item.child.is_some() {
            return Err(RequestError::undeclared(
                "nested projection masks are not accepted",
            ));
        }
        let position = usize::try_from(item.field)
            .map_err(|_| RequestError::invalid("a projection field position is negative"))?;
        let field = schema.fields().get(position).ok_or_else(|| {
            RequestError::invalid(format!(
                "projection field position {position} is outside the base schema"
            ))
        })?;
        fields.push(field.clone());
    }
    Ok(AnalyticsSchema::new(fields))
}

pub(super) fn parse_schema(
    schema: &NamedStruct,
    extensions: &ExtensionRegistry,
) -> Result<AnalyticsSchema, RequestError> {
    let struct_type = schema
        .r#struct
        .as_ref()
        .ok_or_else(|| RequestError::invalid("the named schema has no struct type"))?;
    let mut names = schema.names.iter();
    let mut fields = Vec::with_capacity(struct_type.types.len());
    for data_type in &struct_type.types {
        let name = next_schema_name(&mut names)?;
        let (data_type, nullable) = parse_type(data_type, &mut names, extensions)?;
        fields.push(AnalyticsField::new(name, data_type, nullable));
    }
    if names.next().is_some() {
        return Err(RequestError::invalid(
            "the named schema contains unused field names",
        ));
    }
    Ok(AnalyticsSchema::new(fields))
}

pub(super) fn apply_root_names(
    schema: AnalyticsSchema,
    names: &[String],
) -> Result<AnalyticsSchema, RequestError> {
    if names.is_empty() {
        return Ok(schema);
    }
    let mut names = names.iter();
    let mut fields = Vec::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let name = next_schema_name(&mut names)?;
        let data_type = rename_nested_fields(field.data_type().clone(), &mut names)?;
        fields.push(AnalyticsField::new(name, data_type, field.is_nullable()));
    }
    if names.next().is_some() {
        return Err(RequestError::invalid(
            "the root relation contains unused output names",
        ));
    }
    Ok(AnalyticsSchema::new(fields))
}

fn next_schema_name<'a>(
    names: &mut impl Iterator<Item = &'a String>,
) -> Result<String, RequestError> {
    let name = names
        .next()
        .ok_or_else(|| RequestError::invalid("the schema does not contain enough field names"))?;
    if name.is_empty() {
        return Err(RequestError::invalid("schema field names may not be empty"));
    }
    Ok(name.clone())
}

fn rename_nested_fields<'a>(
    data_type: AnalyticsType,
    names: &mut impl Iterator<Item = &'a String>,
) -> Result<AnalyticsType, RequestError> {
    match data_type {
        AnalyticsType::Array(array) => Ok(AnalyticsType::Array(analytics_types::ArrayType::new(
            rename_nested_fields(array.element_type().clone(), names)?,
            array.is_element_nullable(),
        ))),
        AnalyticsType::Map(map) => Ok(AnalyticsType::Map(analytics_types::MapType::new(
            rename_nested_fields(map.key_type().clone(), names)?,
            rename_nested_fields(map.value_type().clone(), names)?,
            map.is_value_nullable(),
        ))),
        AnalyticsType::Struct(struct_type) => {
            let mut fields = Vec::with_capacity(struct_type.fields().len());
            for field in struct_type.fields() {
                let name = next_schema_name(names)?;
                fields.push(AnalyticsField::new(
                    name,
                    rename_nested_fields(field.data_type().clone(), names)?,
                    field.is_nullable(),
                ));
            }
            Ok(AnalyticsType::Struct(analytics_types::StructType::new(
                fields,
            )))
        }
        other => Ok(other),
    }
}

pub(super) fn analytics_type_complexity(data_type: &AnalyticsType) -> usize {
    match data_type {
        AnalyticsType::Array(array) => 1 + analytics_type_complexity(array.element_type()),
        AnalyticsType::Map(map) => {
            1 + analytics_type_complexity(map.key_type())
                + analytics_type_complexity(map.value_type())
        }
        AnalyticsType::Struct(struct_type) => {
            1 + struct_type
                .fields()
                .iter()
                .map(|field| analytics_type_complexity(field.data_type()))
                .sum::<usize>()
        }
        _ => 1,
    }
}
