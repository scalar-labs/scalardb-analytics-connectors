use substrait::proto::{
    Expression, FunctionArgument, NamedStruct, Plan, PlanRel, ReadRel, Rel, RelRoot, Type, Version,
    expression,
    extensions::{SimpleExtensionDeclaration, SimpleExtensionUri, simple_extension_declaration},
    function_argument, plan_rel, read_rel, rel, r#type,
};

const COMPARISON_URI: u32 = 1;
const BOOLEAN_URI: u32 = 2;
const LOGICAL_URI: u32 = 3;
pub(crate) const EQUAL: u32 = 1;
pub(crate) const OR: u32 = 2;
const LOGICAL: u32 = 10;

pub(crate) fn candidate_plan() -> Plan {
    let read = ReadRel {
        base_schema: Some(NamedStruct {
            names: vec!["id".to_owned(), "name".to_owned(), "external_id".to_owned()],
            r#struct: Some(r#type::Struct {
                types: vec![i64_type(false), string_type(true), uuid_type(false)],
                nullability: required(),
                ..Default::default()
            }),
        }),
        filter: Some(Box::new(scalar(EQUAL, vec![column(0), i64_literal(7)]))),
        projection: Some(expression::MaskExpression {
            select: Some(expression::mask_expression::StructSelect {
                struct_items: vec![
                    expression::mask_expression::StructItem {
                        field: 0,
                        child: None,
                    },
                    expression::mask_expression::StructItem {
                        field: 2,
                        child: None,
                    },
                ],
            }),
            maintain_singular_struct: false,
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
            extension_uri(
                COMPARISON_URI,
                "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_comparison.yaml",
            ),
            extension_uri(
                BOOLEAN_URI,
                "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_boolean.yaml",
            ),
            extension_uri(LOGICAL_URI, "urn:scalardb:analytics:substrait:logical"),
        ],
        extensions: vec![
            function_decl(COMPARISON_URI, EQUAL, "equal:any1_any1"),
            function_decl(BOOLEAN_URI, OR, "or:bool_bool"),
            type_decl(LOGICAL_URI, LOGICAL, "logical"),
        ],
        relations: vec![PlanRel {
            rel_type: Some(plan_rel::RelType::Root(RelRoot {
                input: Some(Rel {
                    rel_type: Some(rel::RelType::Read(Box::new(read))),
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
    match root_mut(plan)
        .input
        .as_mut()
        .unwrap()
        .rel_type
        .as_mut()
        .unwrap()
    {
        rel::RelType::Read(read) => read,
        _ => unreachable!(),
    }
}

#[allow(dead_code)]
pub(crate) fn read_ref(plan: &Plan) -> &ReadRel {
    let plan_rel::RelType::Root(root) = plan.relations[0].rel_type.as_ref().unwrap() else {
        unreachable!();
    };
    match root.input.as_ref().unwrap().rel_type.as_ref().unwrap() {
        rel::RelType::Read(read) => read,
        _ => unreachable!(),
    }
}
