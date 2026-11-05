//! Schema derivation for logical plans and Substrait schema conversion.

use analytics_types::{AnalyticsField, AnalyticsSchema, AnalyticsType};
use substrait::proto::{Expression, NamedStruct};

use super::{
    LogicalExpression, LogicalExpressionKind, LogicalRelation, profile::PlanExtensions,
    types::parse_type,
};
use crate::RequestError;

pub(super) trait ExpressionRef {
    fn field_position(&self, plan_kind: &str) -> Result<usize, RequestError>;
}

impl ExpressionRef for Expression {
    fn field_position(&self, plan_kind: &str) -> Result<usize, RequestError> {
        project_field_position(self, plan_kind)
    }
}

impl ExpressionRef for LogicalExpression {
    fn field_position(&self, plan_kind: &str) -> Result<usize, RequestError> {
        match &self.kind {
            LogicalExpressionKind::Field { position } => Ok(*position),
            _ => Err(RequestError::undeclared(format!(
                "only direct field selections are accepted in a {plan_kind} ProjectRel"
            ))),
        }
    }
}

/// Derives the schema produced by a relation tree before root output names are applied.
///
/// Filters preserve their input schema. Projects apply their expressions and
/// output mapping to the recursively derived input schema.
fn derive_relation_output_schema(
    relation: &LogicalRelation,
) -> Result<AnalyticsSchema, RequestError> {
    match relation {
        LogicalRelation::Read { base_schema, .. } => Ok(base_schema.clone()),
        LogicalRelation::RemoteResult { output_schema, .. } => Ok(output_schema.clone()),
        LogicalRelation::Filter { input, condition } => {
            let input_schema = derive_relation_output_schema(input)?;
            if condition.data_type != AnalyticsType::Boolean {
                return Err(RequestError::invalid(
                    "a filter condition must produce a Boolean value",
                ));
            }
            Ok(input_schema)
        }
        LogicalRelation::Project {
            input,
            expressions,
            output_mapping,
        } => {
            let input_schema = derive_relation_output_schema(input)?;
            derive_project_schema(&input_schema, expressions, output_mapping, "logical")
        }
    }
}

/// Derives and names the schema stored by a newly constructed logical plan.
pub(super) fn derive_output_schema(
    relation: &LogicalRelation,
    output_names: &[String],
) -> Result<AnalyticsSchema, RequestError> {
    apply_root_names(derive_relation_output_schema(relation)?, output_names)
}

/// Resolves each output field to its position in the terminal relation.
pub(super) fn derive_output_field_positions(
    relation: &LogicalRelation,
) -> Result<Vec<usize>, RequestError> {
    match relation {
        LogicalRelation::Read { base_schema, .. } => Ok((0..base_schema.fields().len()).collect()),
        LogicalRelation::RemoteResult { output_schema, .. } => {
            Ok((0..output_schema.fields().len()).collect())
        }
        LogicalRelation::Filter { input, .. } => derive_output_field_positions(input),
        LogicalRelation::Project {
            input,
            expressions,
            output_mapping,
        } => {
            let input_positions = derive_output_field_positions(input)?;
            let mut project_positions = input_positions.clone();
            for expression in expressions {
                let position = expression.field_position("logical")?;
                project_positions.push(input_positions[position]);
            }
            output_mapping
                .iter()
                .map(|position| {
                    let position = usize::try_from(*position)
                        .expect("logical projection positions are validated when constructed");
                    Ok(project_positions[position])
                })
                .collect()
        }
    }
}

/// Applies a project's direct-field expressions and emit mapping to its input schema.
///
/// The initial profile accepts only direct top-level field selections. Each
/// project expression is appended after the input fields before the emit
/// mapping selects the final positions, matching Substrait project semantics.
pub(super) fn derive_project_schema(
    input_schema: &AnalyticsSchema,
    expressions: &[impl ExpressionRef],
    output_mapping: &[i32],
    plan_kind: &str,
) -> Result<AnalyticsSchema, RequestError> {
    let mut project_fields = input_schema.fields().to_vec();
    for expression in expressions {
        let position = expression.field_position(plan_kind)?;
        project_fields.push(
            input_schema
                .fields()
                .get(position)
                .cloned()
                .ok_or_else(|| {
                    RequestError::invalid(format!(
                        "{plan_kind} project expression position {position} is outside the input schema"
                    ))
                })?,
        );
    }
    let fields = output_mapping
        .iter()
        .map(|position| {
            let position = usize::try_from(*position).map_err(|_| {
                RequestError::invalid(format!("a {plan_kind} projection position is negative"))
            })?;
            project_fields.get(position).cloned().ok_or_else(|| {
                RequestError::invalid(format!(
                    "{plan_kind} projection position {position} is outside the project schema"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AnalyticsSchema::new(fields))
}

/// Extracts the top-level input position from an accepted project expression.
pub(super) fn project_field_position(
    expression: &Expression,
    plan_kind: &str,
) -> Result<usize, RequestError> {
    let Some(substrait::proto::expression::RexType::Selection(selection)) =
        expression.rex_type.as_ref()
    else {
        return Err(RequestError::undeclared(format!(
            "only direct field selections are accepted in a {plan_kind} ProjectRel"
        )));
    };
    if !matches!(
        selection.root_type,
        Some(substrait::proto::expression::field_reference::RootType::RootReference(_))
    ) {
        return Err(RequestError::undeclared(format!(
            "a {plan_kind} project selection must use the input root"
        )));
    }
    let Some(substrait::proto::expression::field_reference::ReferenceType::DirectReference(
        reference,
    )) = selection.reference_type.as_ref()
    else {
        return Err(RequestError::undeclared(format!(
            "a {plan_kind} project selection must be a direct reference"
        )));
    };
    let Some(substrait::proto::expression::reference_segment::ReferenceType::StructField(field)) =
        reference.reference_type.as_ref()
    else {
        return Err(RequestError::undeclared(format!(
            "a {plan_kind} project selection must reference a struct field"
        )));
    };
    if field.child.is_some() {
        return Err(RequestError::undeclared(format!(
            "nested {plan_kind} project selections are not accepted"
        )));
    }
    usize::try_from(field.field)
        .map_err(|_| RequestError::invalid(format!("a {plan_kind} project position is negative")))
}

/// Converts one validated Substrait named schema to the Analytics schema model.
///
/// Names are consumed in Substrait's depth-first order while nested types are
/// recursively converted.
pub(super) fn parse_schema(
    schema: &NamedStruct,
    extensions: &PlanExtensions,
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

/// Applies `RelRoot.names` to an already derived schema.
///
/// Root names replace both top-level and nested struct field names in
/// Substrait's depth-first order; an empty list preserves the derived names.
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

/// Consumes one required non-empty name from a depth-first schema-name stream.
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

/// Rebuilds a nested Analytics type with names consumed from a root-name stream.
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
