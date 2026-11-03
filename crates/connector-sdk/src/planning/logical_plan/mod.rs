//! Logical plans and their Substrait planning boundary.

mod extensions;
mod schema;
mod types;
mod validation;

use analytics_types::AnalyticsSchema;
use prost::Message;
use prost_types::Any;
use substrait::proto::{
    DynamicParameterBinding, Expression, ExtensionLeafRel, FilterRel, NamedStruct, Plan, PlanRel,
    ProjectRel, ReadRel, Rel, RelCommon, RelRoot, Version,
    extensions::{SimpleExtensionDeclaration, SimpleExtensionUri},
    plan_rel, read_rel, rel, rel_common,
};

use crate::RequestError;

/// Substrait extension type used to encode a remote result consumed by a local plan.
pub(crate) const REMOTE_RESULT_TYPE_URL: &str =
    "urn:scalardb:analytics:substrait:remote-result-schema:v1";
pub(super) const LOGICAL_TYPE_NAME: &str = "logical";
pub(super) const LOGICAL_URI: &str = "urn:scalardb:analytics:substrait:logical";

/// An opaque table identifier carried by a Substrait `NamedTable`.
///
/// Only the Connector interprets the components. The SDK preserves them
/// without treating them as physical catalog names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableBinding {
    components: Vec<String>,
}

impl TableBinding {
    /// Creates an opaque identifier from non-empty path components.
    pub fn new(components: Vec<String>) -> Result<Self, RequestError> {
        if components.is_empty() || components.iter().any(String::is_empty) {
            return Err(RequestError::invalid(
                "a table binding must contain only non-empty components",
            ));
        }
        Ok(Self { components })
    }

    pub fn components(&self) -> &[String] {
        &self.components
    }
}

/// The Connector SDK's internal relational representation for pushdown planning.
///
/// Its relation tree represents the SDK-supported semantic profile of
/// Substrait without protobuf-only invalid states such as missing relation
/// variants, inputs, or filter conditions. Expressions, schemas, and extension
/// declarations remain Substrait payload types, with their accepted shape
/// checked at the planning boundaries, so the internal model does not define a
/// second expression or type language. Planning is a sequence of tree
/// transformations, so a plan is immutable after construction. A transform
/// chooses the replacement tree; constructing the replacement `LogicalPlan`
/// owns deriving and retaining the plan-level schema invariants.
#[derive(Clone, Debug, PartialEq)]
pub struct LogicalPlan {
    /// Substrait producer/version metadata preserved across planning.
    version: Option<Version>,
    /// Extension URI anchors referenced by declarations in this plan.
    extension_uris: Vec<SimpleExtensionUri>,
    /// Function and type declarations referenced by plan payloads.
    extensions: Vec<SimpleExtensionDeclaration>,
    /// Dynamic parameter values preserved for the remote or local plan.
    parameter_bindings: Vec<DynamicParameterBinding>,
    /// Strictly typed relational operator tree rewritten by Connector rules.
    relation: LogicalRelation,
    /// Optional names applied to the complete plan's output schema.
    output_names: Vec<String>,
    /// Schema established when this immutable plan was constructed.
    output_schema: AnalyticsSchema,
}

/// One valid relation in the SDK-supported Substrait semantic profile.
#[derive(Clone, Debug, PartialEq)]
pub enum LogicalRelation {
    /// A table read before relational operators are applied.
    ///
    /// Substrait also permits scan-local filters and projection masks inside
    /// `ReadRel`. This planning representation intentionally omits them:
    /// query decomposition keeps filtering and projection as explicit
    /// [`LogicalRelation::Filter`] and [`LogicalRelation::Project`] nodes in
    /// both remote and local plans.
    Read {
        /// Original Substrait schema retained for lossless boundary conversion.
        substrait_schema: NamedStruct,
        /// Validated schema used by internal planning and contract checks.
        base_schema: AnalyticsSchema,
        table_binding: TableBinding,
    },
    /// Applies a predicate without changing its input schema.
    Filter {
        input: Box<LogicalRelation>,
        condition: Expression,
    },
    /// Computes expressions and selects output positions using Substrait emit semantics.
    Project {
        input: Box<LogicalRelation>,
        expressions: Vec<Expression>,
        output_mapping: Vec<i32>,
    },
    /// Consumes the remote plan's output as the leaf of a local plan.
    RemoteResult {
        /// Substrait schema encoded in the boundary extension leaf.
        substrait_schema: NamedStruct,
        /// Validated schema used by internal planning and contract checks.
        output_schema: AnalyticsSchema,
    },
}

/// Encodes an internal logical plan as the Substrait boundary representation.
impl From<LogicalPlan> for Plan {
    fn from(plan: LogicalPlan) -> Self {
        let expected_type_urls = if plan.relation.contains_remote_result() {
            vec![REMOTE_RESULT_TYPE_URL.to_owned()]
        } else {
            Vec::new()
        };

        Self {
            version: plan.version,
            extension_uris: plan.extension_uris,
            extensions: plan.extensions,
            relations: vec![PlanRel {
                rel_type: Some(plan_rel::RelType::Root(RelRoot {
                    input: Some(plan.relation.into()),
                    names: plan.output_names,
                })),
            }],
            advanced_extensions: None,
            expected_type_urls,
            parameter_bindings: plan.parameter_bindings,
        }
    }
}

impl LogicalPlan {
    pub fn version(&self) -> Option<&Version> {
        self.version.as_ref()
    }

    pub fn extension_uris(&self) -> &[SimpleExtensionUri] {
        &self.extension_uris
    }

    pub fn extensions(&self) -> &[SimpleExtensionDeclaration] {
        &self.extensions
    }

    pub fn parameter_bindings(&self) -> &[DynamicParameterBinding] {
        &self.parameter_bindings
    }

    pub fn relation(&self) -> &LogicalRelation {
        &self.relation
    }

    pub fn output_names(&self) -> &[String] {
        &self.output_names
    }

    pub fn output_schema(&self) -> &AnalyticsSchema {
        &self.output_schema
    }

    /// Returns a plan with replacement producer/version metadata.
    pub fn with_version(self, version: Option<Version>) -> Self {
        Self { version, ..self }
    }

    /// Constructs a plan for a transformed relational root.
    ///
    /// A tree transform supplies only the new root and output names. This
    /// operation owns deriving the corresponding output schema, so individual
    /// transforms do not maintain plan-level derived state. The supplied tree
    /// must be schema-derivable; a violation is a Connector rewrite bug and
    /// causes an assertion failure.
    pub fn with_root(self, relation: LogicalRelation, output_names: Vec<String>) -> Self {
        let output_schema = schema::derive_output_schema(&relation, &output_names)
            .expect("a Connector-produced logical plan must have a derivable output schema");
        Self {
            relation,
            output_names,
            output_schema,
            ..self
        }
    }

    /// Returns the leaf relation that supplies rows to this plan's operator tree.
    pub(crate) fn terminal_relation(&self) -> &LogicalRelation {
        self.relation.terminal()
    }
}

impl LogicalRelation {
    /// Walks through unary operators to the relation that supplies their rows.
    fn terminal(&self) -> &Self {
        match self {
            Self::Filter { input, .. } | Self::Project { input, .. } => input.terminal(),
            Self::Read { .. } | Self::RemoteResult { .. } => self,
        }
    }

    /// Reports whether conversion must declare the reserved `RemoteResult` type URL.
    fn contains_remote_result(&self) -> bool {
        match self {
            Self::Read { .. } => false,
            Self::Filter { input, .. } | Self::Project { input, .. } => {
                input.contains_remote_result()
            }
            Self::RemoteResult { .. } => true,
        }
    }
}

/// Encodes one valid internal relation as its Substrait relation node.
impl From<LogicalRelation> for Rel {
    fn from(relation: LogicalRelation) -> Self {
        let rel_type = match relation {
            LogicalRelation::Read {
                substrait_schema,
                table_binding,
                ..
            } => rel::RelType::Read(Box::new(ReadRel {
                base_schema: Some(substrait_schema),
                read_type: Some(read_rel::ReadType::NamedTable(read_rel::NamedTable {
                    names: table_binding.components().to_vec(),
                    advanced_extension: None,
                })),
                ..Default::default()
            })),
            LogicalRelation::Filter { input, condition } => {
                rel::RelType::Filter(Box::new(FilterRel {
                    input: Some(Box::new((*input).into())),
                    condition: Some(Box::new(condition)),
                    ..Default::default()
                }))
            }
            LogicalRelation::Project {
                input,
                expressions,
                output_mapping,
            } => rel::RelType::Project(Box::new(ProjectRel {
                common: Some(RelCommon {
                    emit_kind: Some(rel_common::EmitKind::Emit(rel_common::Emit {
                        output_mapping,
                    })),
                    ..Default::default()
                }),
                input: Some(Box::new((*input).into())),
                expressions,
                ..Default::default()
            })),
            LogicalRelation::RemoteResult {
                substrait_schema, ..
            } => rel::RelType::ExtensionLeaf(ExtensionLeafRel {
                detail: Some(Any {
                    type_url: REMOTE_RESULT_TYPE_URL.to_owned(),
                    value: substrait_schema.encode_to_vec(),
                }),
                ..Default::default()
            }),
        };
        Self {
            rel_type: Some(rel_type),
        }
    }
}
