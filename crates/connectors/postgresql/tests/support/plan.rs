//! Offered Substrait plans in the connector profile, as an adapter would build them.

use connector_sdk::{BOOLEAN_FUNCTIONS_URI, COMPARISON_FUNCTIONS_URI, LOGICAL_TYPE_URI};
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

/// A read of `namespace.table` with an optional filter and projection.
pub struct ReadPlan {
    pub table: [&'static str; 2],
    pub columns: Vec<(&'static str, Type)>,
    pub filter: Option<Expression>,
    /// Output base-schema positions and their names.
    pub project: Option<Vec<(i32, &'static str)>>,
}

impl ReadPlan {
    pub fn new(table: [&'static str; 2], columns: Vec<(&'static str, Type)>) -> Self {
        Self {
            table,
            columns,
            filter: None,
            project: None,
        }
    }

    pub fn filter(mut self, condition: Expression) -> Self {
        self.filter = Some(condition);
        self
    }

    pub fn project(mut self, columns: Vec<(i32, &'static str)>) -> Self {
        self.project = Some(columns);
        self
    }

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
                    names: self.table.map(str::to_owned).to_vec(),
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

/// Calls a declared function such as `equal` or `and` with a Boolean result.
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

pub fn literal(value: expression::literal::LiteralType) -> Expression {
    Expression {
        rex_type: Some(expression::RexType::Literal(expression::Literal {
            literal_type: Some(value),
            ..Default::default()
        })),
    }
}

/// A logical literal of a registered extension-encoded type over a string carrier.
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

pub fn time(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::PrecisionTime(r#type::PrecisionTime {
            precision: 6,
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

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

pub fn interval(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::IntervalCompound(r#type::IntervalCompound {
            precision: 9,
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

/// A registered extension-encoded type, such as `uuid` with parameters `{}`.
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
