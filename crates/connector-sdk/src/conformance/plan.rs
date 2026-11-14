//! Offered Substrait plans in the connector profile, as an Adapter would build
//! them, for fixtures to describe their read requests.
//!
//! A plan reads one named table, optionally filters it, and optionally
//! projects plain columns:
//!
//! ```ignore
//! use connector_sdk::conformance::plan::*;
//! use substrait::proto::expression::literal::LiteralType;
//!
//! let offered = ReadPlan::new(["schema", "table"], vec![("id", i32(false)), ("label", text(true))])
//!     .filter(call("equal", vec![column(1), literal(LiteralType::String("a".to_owned()))]))
//!     .project(vec![(0, "id")])
//!     .build();
//! ```
//!
//! Every plan declares the comparison and Boolean functions [`call`] accepts
//! and the logical type of registered extension-encoded types, so any plan
//! built here passes common validation when its columns match the table.

use crate::{BOOLEAN_FUNCTIONS_URI, COMPARISON_FUNCTIONS_URI, LOGICAL_TYPE_URI};
use substrait::proto::{
    Expression, FilterRel, FunctionArgument, NamedStruct, Plan, PlanRel, ProjectRel, ReadRel, Rel,
    RelCommon, RelRoot, Type, expression,
    extensions::{SimpleExtensionDeclaration, SimpleExtensionUri, simple_extension_declaration},
    function_argument, plan_rel, read_rel, rel, rel_common, r#type,
};

const COMPARISON_URI: u32 = 1;
const BOOLEAN_URI: u32 = 2;
const LOGICAL_URI: u32 = 3;
const LOGICAL: u32 = 100;

/// Function anchors declared by every plan.
const FUNCTIONS: [(u32, &str, &str); 11] = [
    (COMPARISON_URI, "equal", "equal:any1_any1"),
    (COMPARISON_URI, "not_equal", "not_equal:any1_any1"),
    (COMPARISON_URI, "lt", "lt:any1_any1"),
    (COMPARISON_URI, "lte", "lte:any1_any1"),
    (COMPARISON_URI, "gt", "gt:any1_any1"),
    (COMPARISON_URI, "gte", "gte:any1_any1"),
    (COMPARISON_URI, "is_null", "is_null:any"),
    (COMPARISON_URI, "is_not_null", "is_not_null:any"),
    (BOOLEAN_URI, "and", "and:bool_bool"),
    (BOOLEAN_URI, "or", "or:bool_bool"),
    (BOOLEAN_URI, "not", "not:bool"),
];

/// A read of one named table with an optional filter and projection.
pub struct ReadPlan {
    /// The table's names, namespace first.
    pub table: Vec<String>,
    /// The base schema: every column the read binds, in table order, with
    /// its Substrait type. The connector checks them against the table.
    pub columns: Vec<(&'static str, Type)>,
    /// A Boolean condition over base-schema positions ([`column()`]).
    pub filter: Option<Expression>,
    /// Output base-schema positions and their names; without it the read
    /// returns every base-schema column.
    pub project: Option<Vec<(i32, &'static str)>>,
}

impl ReadPlan {
    /// A read of `table` that returns every column of `columns`.
    pub fn new<const N: usize>(table: [&str; N], columns: Vec<(&'static str, Type)>) -> Self {
        Self {
            table: table.map(str::to_owned).to_vec(),
            columns,
            filter: None,
            project: None,
        }
    }

    /// Keeps only the rows for which `condition` is true.
    pub fn filter(mut self, condition: Expression) -> Self {
        self.filter = Some(condition);
        self
    }

    /// Returns only the given base-schema positions, under the given names.
    pub fn project(mut self, columns: Vec<(i32, &'static str)>) -> Self {
        self.project = Some(columns);
        self
    }

    /// The offered plan: the read, then the filter, then the projection.
    pub fn build(self) -> Plan {
        let names: Vec<String> = self
            .columns
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        let read = Rel {
            rel_type: Some(rel::RelType::Read(Box::new(ReadRel {
                base_schema: Some(NamedStruct {
                    names: names.clone(),
                    r#struct: Some(r#type::Struct {
                        types: self.columns.into_iter().map(|(_, ty)| ty).collect(),
                        nullability: r#type::Nullability::Required as i32,
                        ..Default::default()
                    }),
                }),
                read_type: Some(read_rel::ReadType::NamedTable(read_rel::NamedTable {
                    names: self.table,
                    advanced_extension: None,
                })),
                ..Default::default()
            }))),
        };
        let mut relation = read;
        if let Some(condition) = self.filter {
            relation = Rel {
                rel_type: Some(rel::RelType::Filter(Box::new(FilterRel {
                    input: Some(Box::new(relation)),
                    condition: Some(Box::new(condition)),
                    ..Default::default()
                }))),
            };
        }
        let output_names = match self.project {
            Some(project) => {
                let base = i32::try_from(names.len()).unwrap();
                let output_mapping = (base..base + i32::try_from(project.len()).unwrap()).collect();
                let output_names = project.iter().map(|(_, name)| (*name).to_owned()).collect();
                relation = Rel {
                    rel_type: Some(rel::RelType::Project(Box::new(ProjectRel {
                        common: Some(RelCommon {
                            emit_kind: Some(rel_common::EmitKind::Emit(rel_common::Emit {
                                output_mapping,
                            })),
                            ..Default::default()
                        }),
                        input: Some(Box::new(relation)),
                        expressions: project
                            .iter()
                            .map(|(position, _)| column(*position))
                            .collect(),
                        ..Default::default()
                    }))),
                };
                output_names
            }
            None => names,
        };
        Plan {
            extension_uris: vec![
                extension_uri(COMPARISON_URI, COMPARISON_FUNCTIONS_URI),
                extension_uri(BOOLEAN_URI, BOOLEAN_FUNCTIONS_URI),
                extension_uri(LOGICAL_URI, LOGICAL_TYPE_URI),
            ],
            extensions: FUNCTIONS
                .iter()
                .zip(1..)
                .map(|((uri, _, name), anchor)| function_declaration(*uri, anchor, name))
                .chain([type_declaration(LOGICAL_URI, LOGICAL, "logical")])
                .collect(),
            relations: vec![PlanRel {
                rel_type: Some(plan_rel::RelType::Root(RelRoot {
                    input: Some(relation),
                    names: output_names,
                })),
            }],
            ..Default::default()
        }
    }
}

fn extension_uri(anchor: u32, uri: &str) -> SimpleExtensionUri {
    SimpleExtensionUri {
        extension_uri_anchor: anchor,
        uri: uri.to_owned(),
    }
}

fn function_declaration(uri: u32, anchor: u32, name: &str) -> SimpleExtensionDeclaration {
    SimpleExtensionDeclaration {
        mapping_type: Some(
            simple_extension_declaration::MappingType::ExtensionFunction(
                simple_extension_declaration::ExtensionFunction {
                    extension_uri_reference: uri,
                    function_anchor: anchor,
                    name: name.to_owned(),
                },
            ),
        ),
    }
}

fn type_declaration(uri: u32, anchor: u32, name: &str) -> SimpleExtensionDeclaration {
    SimpleExtensionDeclaration {
        mapping_type: Some(simple_extension_declaration::MappingType::ExtensionType(
            simple_extension_declaration::ExtensionType {
                extension_uri_reference: uri,
                type_anchor: anchor,
                name: name.to_owned(),
            },
        )),
    }
}

/// Calls a declared function with a Boolean result: `equal`, `not_equal`,
/// `lt`, `lte`, `gt`, `gte`, `is_null`, `is_not_null`, `and`, `or`, or `not`.
///
/// It panics for any other name.
#[allow(deprecated)]
pub fn call(function: &str, arguments: Vec<Expression>) -> Expression {
    let anchor = FUNCTIONS
        .iter()
        .zip(1..)
        .find(|((_, short, _), _)| *short == function)
        .map(|(_, anchor)| anchor)
        .unwrap_or_else(|| panic!("{function} is not declared"));
    Expression {
        rex_type: Some(expression::RexType::ScalarFunction(
            expression::ScalarFunction {
                function_reference: anchor,
                arguments: arguments
                    .into_iter()
                    .map(|value| FunctionArgument {
                        arg_type: Some(function_argument::ArgType::Value(value)),
                    })
                    .collect(),
                output_type: Some(boolean(true)),
                ..Default::default()
            },
        )),
    }
}

/// A reference to the base-schema column at `position`, counted from 0.
pub fn column(position: i32) -> Expression {
    Expression {
        rex_type: Some(expression::RexType::Selection(Box::new(
            expression::FieldReference {
                reference_type: Some(expression::field_reference::ReferenceType::DirectReference(
                    expression::ReferenceSegment {
                        reference_type: Some(
                            expression::reference_segment::ReferenceType::StructField(Box::new(
                                expression::reference_segment::StructField {
                                    field: position,
                                    child: None,
                                },
                            )),
                        ),
                    },
                )),
                root_type: Some(expression::field_reference::RootType::RootReference(
                    expression::field_reference::RootReference {},
                )),
            },
        ))),
    }
}

/// A literal of a Substrait built-in type, such as
/// `LiteralType::I32(1)` or `LiteralType::String(..)`.
pub fn literal(value: expression::literal::LiteralType) -> Expression {
    Expression {
        rex_type: Some(expression::RexType::Literal(expression::Literal {
            literal_type: Some(value),
            ..Default::default()
        })),
    }
}

/// A logical literal of a registered extension-encoded type over a string
/// carrier, such as `logical_text("uuid", "{}", "a0eebc99-...")`.
pub fn logical_text(name: &str, parameters: &str, value: &str) -> Expression {
    use expression::literal::{LiteralType, Struct, UserDefined, user_defined::Val};
    literal(LiteralType::UserDefined(UserDefined {
        type_reference: LOGICAL,
        type_parameters: logical_parameters(name, parameters),
        val: Some(Val::Struct(Struct {
            fields: vec![expression::Literal {
                literal_type: Some(LiteralType::String(value.to_owned())),
                ..Default::default()
            }],
        })),
    }))
}

fn logical_parameters(name: &str, parameters: &str) -> Vec<r#type::Parameter> {
    let string = |value: &str| r#type::Parameter {
        parameter: Some(r#type::parameter::Parameter::String(value.to_owned())),
    };
    vec![
        string(name),
        string(parameters),
        r#type::Parameter {
            parameter: Some(r#type::parameter::Parameter::DataType(text(false))),
        },
    ]
}

fn nullability(nullable: bool) -> i32 {
    if nullable {
        r#type::Nullability::Nullable as i32
    } else {
        r#type::Nullability::Required as i32
    }
}

macro_rules! simple_type {
    ($name:ident, $kind:ident, $message:ident) => {
        #[doc = concat!("The Substrait `", stringify!($message), "` type.")]
        pub fn $name(nullable: bool) -> Type {
            Type {
                kind: Some(r#type::Kind::$kind(r#type::$message {
                    nullability: nullability(nullable),
                    ..Default::default()
                })),
            }
        }
    };
}

simple_type!(boolean, Bool, Boolean);
simple_type!(i16, I16, I16);
simple_type!(i32, I32, I32);
simple_type!(i64, I64, I64);
simple_type!(fp32, Fp32, Fp32);
simple_type!(fp64, Fp64, Fp64);
simple_type!(text, String, String);
simple_type!(binary, Binary, Binary);
simple_type!(date, Date, Date);

/// The Substrait decimal type of `precision` and `scale`.
pub fn decimal(precision: i32, scale: i32, nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::Decimal(r#type::Decimal {
            precision,
            scale,
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

/// A time of day in microseconds, the precision of the Analytics `Time` type.
pub fn time(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::PrecisionTime(r#type::PrecisionTime {
            precision: 6,
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

/// A timestamp without time zone in microseconds.
pub fn timestamp(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::PrecisionTimestamp(
            r#type::PrecisionTimestamp {
                precision: 6,
                nullability: nullability(nullable),
                ..Default::default()
            },
        )),
    }
}

/// A timestamp with time zone in microseconds.
pub fn timestamp_tz(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::PrecisionTimestampTz(
            r#type::PrecisionTimestampTz {
                precision: 6,
                nullability: nullability(nullable),
                ..Default::default()
            },
        )),
    }
}

/// A compound interval of months, days, and nanoseconds.
pub fn interval(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::IntervalCompound(r#type::IntervalCompound {
            precision: 9,
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

/// A registered extension-encoded type, such as `uuid` with parameters `{}`
/// or `enum` with `{"members":[...]}`, as the Analytics type model encodes it.
pub fn logical(name: &str, parameters: &str, nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::UserDefined(r#type::UserDefined {
            type_reference: LOGICAL,
            nullability: nullability(nullable),
            type_parameters: logical_parameters(name, parameters),
            ..Default::default()
        })),
    }
}
