//! Validation of the accepted Substrait plan shape.

use analytics_types::AnalyticsSchema;
use substrait::proto::{Plan, ReadRel, plan_rel, read_rel, rel};

use super::{
    MAX_OUTPUT_COLUMNS, MAX_OUTPUT_COMPLEXITY, MAX_SCHEMA_COMPLEXITY, SUBSTRAIT_VERSION,
    extensions::ExtensionRegistry,
    schema::{analytics_type_complexity, apply_root_names, parse_projection, parse_schema},
};
use crate::{RequestError, TableReference, ValidatedCandidate};

pub(super) fn validate_plan(plan: Plan) -> Result<ValidatedCandidate, RequestError> {
    validate_version(&plan)?;
    if plan.advanced_extensions.is_some() || !plan.expected_type_urls.is_empty() {
        return Err(RequestError::undeclared(
            "advanced plan extensions and expected type URLs are not accepted",
        ));
    }
    if plan.relations.len() != 1 {
        return Err(RequestError::undeclared(
            "a candidate must contain exactly one root relation",
        ));
    }

    let extensions = ExtensionRegistry::new(&plan.extension_uris, &plan.extensions)?;
    let root = match plan.relations[0].rel_type.as_ref() {
        Some(plan_rel::RelType::Root(root)) => root,
        _ => {
            return Err(RequestError::undeclared(
                "the candidate relation must be a RelRoot",
            ));
        }
    };
    let input = root
        .input
        .as_ref()
        .ok_or_else(|| RequestError::invalid("the root relation has no input"))?;
    let read = match input.rel_type.as_ref() {
        Some(rel::RelType::Read(read)) => read.as_ref(),
        _ => {
            return Err(RequestError::undeclared(
                "only a ReadRel candidate is currently accepted",
            ));
        }
    };
    let (output_schema, table_reference) = validate_read(read, &root.names, &extensions)?;
    let requires_retained_operator = read.best_effort_filter.is_some();

    Ok(ValidatedCandidate::new(
        plan,
        output_schema,
        vec![table_reference],
        requires_retained_operator,
    ))
}

fn validate_version(plan: &Plan) -> Result<(), RequestError> {
    let version = plan
        .version
        .as_ref()
        .ok_or_else(|| RequestError::invalid("the Substrait version is missing"))?;
    let actual = (
        version.major_number,
        version.minor_number,
        version.patch_number,
    );
    if actual != SUBSTRAIT_VERSION {
        return Err(RequestError::undeclared(format!(
            "Substrait version {}.{}.{} is not supported; expected {}.{}.{}",
            actual.0,
            actual.1,
            actual.2,
            SUBSTRAIT_VERSION.0,
            SUBSTRAIT_VERSION.1,
            SUBSTRAIT_VERSION.2,
        )));
    }
    Ok(())
}

fn validate_read(
    read: &ReadRel,
    root_names: &[String],
    extensions: &ExtensionRegistry,
) -> Result<(AnalyticsSchema, TableReference), RequestError> {
    if read.common.is_some() || read.advanced_extension.is_some() {
        return Err(RequestError::undeclared(
            "common and advanced ReadRel extensions are not accepted",
        ));
    }
    let table = match read.read_type.as_ref() {
        Some(read_rel::ReadType::NamedTable(table)) => table,
        _ => {
            return Err(RequestError::undeclared(
                "only named-table reads are currently accepted",
            ));
        }
    };
    validate_table(table)?;

    let base_schema = read
        .base_schema
        .as_ref()
        .ok_or_else(|| RequestError::invalid("the ReadRel base schema is missing"))?;
    let base_schema = parse_schema(base_schema, extensions)?;
    if base_schema
        .fields()
        .iter()
        .map(|field| analytics_type_complexity(field.data_type()))
        .sum::<usize>()
        > MAX_SCHEMA_COMPLEXITY
    {
        return Err(RequestError::undeclared(format!(
            "the base schema exceeds the maximum complexity of {MAX_SCHEMA_COMPLEXITY}"
        )));
    }

    let projected = parse_projection(&base_schema, read.projection.as_ref())?;
    if projected.fields().len() > MAX_OUTPUT_COLUMNS {
        return Err(RequestError::undeclared(format!(
            "the output schema may not contain more than {MAX_OUTPUT_COLUMNS} columns"
        )));
    }
    if projected
        .fields()
        .iter()
        .map(|field| analytics_type_complexity(field.data_type()))
        .sum::<usize>()
        > MAX_OUTPUT_COMPLEXITY
    {
        return Err(RequestError::undeclared(format!(
            "the output schema exceeds the maximum complexity of {MAX_OUTPUT_COMPLEXITY}"
        )));
    }
    let output_schema = apply_root_names(projected, root_names)?;

    Ok((output_schema, TableReference::new(table.names.clone())))
}

fn validate_table(table: &read_rel::NamedTable) -> Result<(), RequestError> {
    if table.names.is_empty() || table.names.iter().any(String::is_empty) {
        return Err(RequestError::invalid(
            "a named table must contain a non-empty path",
        ));
    }
    if table.advanced_extension.is_some() {
        return Err(RequestError::undeclared(
            "named-table advanced extensions are not accepted",
        ));
    }
    Ok(())
}
