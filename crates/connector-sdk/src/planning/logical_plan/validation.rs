//! Validation of the accepted Substrait plan shapes.

use analytics_types::AnalyticsSchema;
use substrait::proto::{Plan, ProjectRel, ReadRel, RelRoot, plan_rel, read_rel, rel, rel_common};

use super::{
    LogicalPlan, LogicalRelation, TableBinding,
    extensions::ExtensionRegistry,
    schema::{apply_root_names, derive_project_schema, parse_schema},
};
use crate::RequestError;

/// One relation and its schema while an offered plan is being validated.
///
/// This is construction state, not a second result beside `LogicalPlan`.
/// Each operator-validation stage consumes and produces the same unit so its
/// schema effect stays attached to the relation being constructed.
struct ValidatedRelation {
    relation: LogicalRelation,
    output_schema: AnalyticsSchema,
}

/// Validates the logical plan offered to Connector pushdown planning.
///
/// Validation follows the pushdown operations in the initial offered subset:
///
/// ```text
/// plan boundary
/// `-- projection pushdown shape: ProjectRel?
///     `-- filter pushdown shape: FilterRel?
///         `-- read shape: ReadRel
/// ```
///
/// Each operation validates its own relation fields and schema effect. This
/// keeps another pushdown operation, such as aggregation, addable as one
/// validation stage without combining its rules with projection or filter.
impl LogicalPlan {
    /// Validates an offered Substrait plan while constructing its logical form.
    ///
    /// A successful return is the complete validation result; no validation
    /// sidecar is required by later planning stages.
    pub(crate) fn validate(plan: Plan) -> Result<Self, RequestError> {
        let (root, extensions) = validate_plan_container(&plan)?;
        let input = root
            .input
            .as_ref()
            .ok_or_else(|| RequestError::invalid("the offered root relation has no input"))?;
        let validated = validate_projection_pushdown(input, &extensions)?;
        let output_schema = apply_root_names(validated.output_schema, &root.names)?;
        let output_names = root.names.clone();

        Ok(Self {
            version: plan.version,
            extension_uris: plan.extension_uris,
            extensions: plan.extensions,
            parameter_bindings: plan.parameter_bindings,
            relation: validated.relation,
            output_names,
            output_schema,
        })
    }
}

/// Validates the optional `ProjectRel` that requests projection pushdown.
///
/// A project changes the current output schema. Without a project, the schema
/// produced by the next pushdown stage passes through unchanged.
fn validate_projection_pushdown(
    relation: &substrait::proto::Rel,
    extensions: &ExtensionRegistry,
) -> Result<ValidatedRelation, RequestError> {
    let Some(rel::RelType::Project(project)) = relation.rel_type.as_ref() else {
        return validate_filter_pushdown(relation, extensions);
    };
    let input = project
        .input
        .as_deref()
        .ok_or_else(|| RequestError::invalid("the offered ProjectRel has no input"))?;
    let input = validate_filter_pushdown(input, extensions)?;
    let (output_schema, output_mapping) = validate_project_schema(project, &input.output_schema)?;
    Ok(ValidatedRelation {
        relation: LogicalRelation::Project {
            input: Box::new(input.relation),
            expressions: project.expressions.clone(),
            output_mapping,
        },
        output_schema,
    })
}

/// Validates the optional `FilterRel` that requests filter pushdown.
///
/// A filter must contain its predicate and preserves the schema produced by
/// its input read. Without a filter, the read schema passes through unchanged.
fn validate_filter_pushdown(
    relation: &substrait::proto::Rel,
    extensions: &ExtensionRegistry,
) -> Result<ValidatedRelation, RequestError> {
    let Some(rel::RelType::Filter(filter)) = relation.rel_type.as_ref() else {
        return validate_read(relation, extensions);
    };
    if filter.common.is_some() || filter.advanced_extension.is_some() {
        return Err(RequestError::undeclared(
            "offered FilterRel common and advanced fields are not accepted",
        ));
    }
    let condition = filter
        .condition
        .as_deref()
        .ok_or_else(|| RequestError::invalid("the offered FilterRel has no condition"))?;
    let input = filter
        .input
        .as_deref()
        .ok_or_else(|| RequestError::invalid("the offered FilterRel has no input"))?;
    let input = validate_read(input, extensions)?;
    Ok(ValidatedRelation {
        relation: LogicalRelation::Filter {
            input: Box::new(input.relation),
            condition: condition.clone(),
        },
        output_schema: input.output_schema,
    })
}

/// Validates the terminal read on which pushdown operations are requested.
fn validate_read(
    relation: &substrait::proto::Rel,
    extensions: &ExtensionRegistry,
) -> Result<ValidatedRelation, RequestError> {
    let read = match relation.rel_type.as_ref() {
        Some(rel::RelType::Read(read)) => read.as_ref(),
        _ => {
            return Err(RequestError::undeclared(
                "the offered shape must be RelRoot -> ProjectRel? -> FilterRel? -> ReadRel",
            ));
        }
    };
    if read.filter.is_some() || read.best_effort_filter.is_some() || read.projection.is_some() {
        return Err(RequestError::undeclared(
            "an offered ReadRel must not contain scan-local filters or a scan projection",
        ));
    }
    validate_read_base(read, extensions)
}

/// Validates project metadata and derives the schema selected by its emit mapping.
fn validate_project_schema(
    project: &ProjectRel,
    input_schema: &AnalyticsSchema,
) -> Result<(AnalyticsSchema, Vec<i32>), RequestError> {
    if project.advanced_extension.is_some() {
        return Err(RequestError::undeclared(
            "offered ProjectRel advanced fields are not accepted",
        ));
    }
    let common = project
        .common
        .as_ref()
        .ok_or_else(|| RequestError::invalid("the offered ProjectRel has no common fields"))?;
    if common.hint.is_some() || common.advanced_extension.is_some() {
        return Err(RequestError::undeclared(
            "offered ProjectRel hints and common advanced fields are not accepted",
        ));
    }
    let mapping = match common.emit_kind.as_ref() {
        Some(rel_common::EmitKind::Emit(emit)) => &emit.output_mapping,
        _ => {
            return Err(RequestError::invalid(
                "the offered ProjectRel must use an emit mapping",
            ));
        }
    };

    let output_schema =
        derive_project_schema(input_schema, &project.expressions, mapping, "offered")?;
    Ok((output_schema, mapping.clone()))
}

/// Validates the plan-level envelope and builds its extension registry.
///
/// The offered profile contains exactly one `RelRoot` and does not accept
/// plan-level advanced extensions or expected extension type URLs.
fn validate_plan_container(plan: &Plan) -> Result<(&RelRoot, ExtensionRegistry), RequestError> {
    if plan.advanced_extensions.is_some() || !plan.expected_type_urls.is_empty() {
        return Err(RequestError::undeclared(
            "advanced offered plan extensions and expected type URLs are not accepted",
        ));
    }
    if plan.relations.len() != 1 {
        return Err(RequestError::undeclared(
            "an offered plan must contain exactly one root relation",
        ));
    }
    let extensions = ExtensionRegistry::new(&plan.extension_uris, &plan.extensions)?;
    let root = match plan.relations[0].rel_type.as_ref() {
        Some(plan_rel::RelType::Root(root)) => root,
        _ => {
            return Err(RequestError::undeclared(
                "the offered plan relation must be a RelRoot",
            ));
        }
    };
    Ok((root, extensions))
}

/// Validates a bare named-table read and constructs its internal `Read` node.
///
/// The returned output schema starts as the read's base schema; outer
/// validation stages update it as operators are applied.
fn validate_read_base(
    read: &ReadRel,
    extensions: &ExtensionRegistry,
) -> Result<ValidatedRelation, RequestError> {
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

    let logical_base_schema = read
        .base_schema
        .as_ref()
        .ok_or_else(|| RequestError::invalid("the ReadRel base schema is missing"))?;
    let base_schema = parse_schema(logical_base_schema, extensions)?;
    let table_binding = TableBinding::new(table.names.clone())?;
    Ok(ValidatedRelation {
        relation: LogicalRelation::Read {
            substrait_schema: logical_base_schema.clone(),
            base_schema: base_schema.clone(),
            table_binding,
        },
        output_schema: base_schema,
    })
}

/// Validates the opaque non-empty identifier carried by a named-table read.
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
