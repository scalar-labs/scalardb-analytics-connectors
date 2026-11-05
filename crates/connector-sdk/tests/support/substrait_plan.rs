use connector_sdk::{
    BOOLEAN_FUNCTIONS_URI, COMPARISON_FUNCTIONS_URI, LOGICAL_TYPE_URI, LogicalPlan,
};
use substrait::proto::{
    Expression, FilterRel, FunctionArgument, NamedStruct, Plan, PlanRel, ProjectRel, ReadRel, Rel,
    RelCommon, RelRoot, Type, Version, expression,
    extensions::{SimpleExtensionDeclaration, SimpleExtensionUri, simple_extension_declaration},
    function_argument, plan_rel, read_rel, rel, rel_common, r#type,
};

const COMPARISON_URI: u32 = 1;
const BOOLEAN_URI: u32 = 2;
const LOGICAL_URI: u32 = 3;
pub(crate) const EQUAL: u32 = 1;
pub(crate) const OR: u32 = 2;
const LOGICAL: u32 = 10;

pub(crate) fn requested_plan() -> Plan {
    let read = ReadRel {
        base_schema: Some(NamedStruct {
            names: vec!["id".to_owned(), "name".to_owned(), "external_id".to_owned()],
            r#struct: Some(r#type::Struct {
                types: vec![i64_type(false), string_type(true), uuid_type(false)],
                nullability: required(),
                ..Default::default()
            }),
        }),
        read_type: Some(read_rel::ReadType::NamedTable(read_rel::NamedTable {
            names: vec!["catalog".to_owned(), "users".to_owned()],
            advanced_extension: None,
        })),
        ..Default::default()
    };
    Plan {
        version: Some(Version {
            major_number: 0,
            minor_number: 69,
            patch_number: 0,
            producer: "connector-sdk-tests".to_owned(),
            ..Default::default()
        }),
        extension_uris: vec![
            extension_uri(COMPARISON_URI, COMPARISON_FUNCTIONS_URI),
            extension_uri(BOOLEAN_URI, BOOLEAN_FUNCTIONS_URI),
            extension_uri(LOGICAL_URI, LOGICAL_TYPE_URI),
        ],
        extensions: vec![
            function_decl(COMPARISON_URI, EQUAL, "equal:any1_any1"),
            function_decl(BOOLEAN_URI, OR, "or:bool_bool"),
            type_decl(LOGICAL_URI, LOGICAL, "logical"),
        ],
        relations: vec![PlanRel {
            rel_type: Some(plan_rel::RelType::Root(RelRoot {
                input: Some(Rel {
                    rel_type: Some(rel::RelType::Project(Box::new(ProjectRel {
                        common: Some(RelCommon {
                            emit_kind: Some(rel_common::EmitKind::Emit(rel_common::Emit {
                                output_mapping: vec![3, 4],
                            })),
                            ..Default::default()
                        }),
                        input: Some(Box::new(Rel {
                            rel_type: Some(rel::RelType::Filter(Box::new(FilterRel {
                                input: Some(Box::new(Rel {
                                    rel_type: Some(rel::RelType::Read(Box::new(read))),
                                })),
                                condition: Some(Box::new(scalar(
                                    EQUAL,
                                    vec![column(0), i64_literal(7)],
                                ))),
                                ..Default::default()
                            }))),
                        })),
                        expressions: vec![column(0), column(2)],
                        ..Default::default()
                    }))),
                }),
                names: vec!["user_id".to_owned(), "external_id".to_owned()],
            })),
        }],
        ..Default::default()
    }
}

fn extension_uri(anchor: u32, uri: &str) -> SimpleExtensionUri {
    SimpleExtensionUri {
        extension_uri_anchor: anchor,
        uri: uri.to_owned(),
    }
}

fn function_decl(uri: u32, anchor: u32, name: &str) -> SimpleExtensionDeclaration {
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

fn type_decl(uri: u32, anchor: u32, name: &str) -> SimpleExtensionDeclaration {
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

#[allow(deprecated)]
pub(crate) fn scalar(anchor: u32, arguments: Vec<Expression>) -> Expression {
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
                options: Vec::new(),
                output_type: Some(bool_type(false)),
                args: Vec::new(),
            },
        )),
    }
}

pub(crate) fn column(position: i32) -> Expression {
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

pub(crate) fn i64_literal(value: i64) -> Expression {
    Expression {
        rex_type: Some(expression::RexType::Literal(expression::Literal {
            literal_type: Some(expression::literal::LiteralType::I64(value)),
            ..Default::default()
        })),
    }
}

fn bool_type(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::Bool(r#type::Boolean {
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

fn i64_type(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::I64(r#type::I64 {
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

fn string_type(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::String(r#type::String {
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

fn uuid_type(nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::UserDefined(r#type::UserDefined {
            type_reference: LOGICAL,
            nullability: nullability(nullable),
            type_parameters: vec![
                string_parameter("uuid"),
                string_parameter("{}"),
                data_type_parameter(string_type(false)),
            ],
            ..Default::default()
        })),
    }
}

fn string_parameter(value: &str) -> r#type::Parameter {
    r#type::Parameter {
        parameter: Some(r#type::parameter::Parameter::String(value.to_owned())),
    }
}

fn data_type_parameter(value: Type) -> r#type::Parameter {
    r#type::Parameter {
        parameter: Some(r#type::parameter::Parameter::DataType(value)),
    }
}

fn nullability(nullable: bool) -> i32 {
    if nullable {
        r#type::Nullability::Nullable as i32
    } else {
        required()
    }
}

fn required() -> i32 {
    r#type::Nullability::Required as i32
}

#[allow(dead_code)]
pub(crate) fn root_mut(plan: &mut Plan) -> &mut RelRoot {
    match plan.relations[0].rel_type.as_mut().unwrap() {
        plan_rel::RelType::Root(root) => root,
        _ => unreachable!(),
    }
}

#[allow(dead_code)]
pub(crate) fn read_mut(plan: &mut Plan) -> &mut ReadRel {
    read_rel_mut(root_mut(plan).input.as_mut().unwrap())
}

#[allow(dead_code)]
pub(crate) fn read_ref(plan: &Plan) -> &ReadRel {
    let plan_rel::RelType::Root(root) = plan.relations[0].rel_type.as_ref().unwrap() else {
        unreachable!();
    };
    read_rel_ref(root.input.as_ref().unwrap())
}

#[allow(dead_code)]
pub(crate) fn pushed_remote(plan: &Plan) -> Plan {
    plan.clone()
}

pub(crate) fn pushed_remote_logical(plan: &LogicalPlan) -> LogicalPlan {
    plan.clone()
}

fn read_rel_mut(relation: &mut Rel) -> &mut ReadRel {
    match relation.rel_type.as_mut().unwrap() {
        rel::RelType::Read(read) => read,
        rel::RelType::Filter(filter) => read_rel_mut(filter.input.as_mut().unwrap()),
        rel::RelType::Project(project) => read_rel_mut(project.input.as_mut().unwrap()),
        _ => unreachable!(),
    }
}

fn read_rel_ref(relation: &Rel) -> &ReadRel {
    match relation.rel_type.as_ref().unwrap() {
        rel::RelType::Read(read) => read,
        rel::RelType::Filter(filter) => read_rel_ref(filter.input.as_ref().unwrap()),
        rel::RelType::Project(project) => read_rel_ref(project.input.as_ref().unwrap()),
        _ => unreachable!(),
    }
}
