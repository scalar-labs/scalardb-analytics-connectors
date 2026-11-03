use std::cell::RefCell;

use arrow_array::{
    Array, DurationMicrosecondArray, Int64Array, IntervalMonthDayNanoArray, StringArray,
};
use arrow_schema::{DataType, IntervalUnit, TimeUnit};
use connector_sdk::{
    ConnectorPlanner, LogicalPlan, PlanningError, PushdownRequest, PushdownResult,
};
use connector_sql::{SqlDialect, TranslationError, translate};
use substrait::proto::{
    Expression, FilterRel, FunctionArgument, NamedStruct, Plan, PlanRel, ProjectRel, ReadRel, Rel,
    RelCommon, RelRoot, Type, Version, expression,
    extensions::{SimpleExtensionDeclaration, SimpleExtensionUri, simple_extension_declaration},
    function_argument, plan_rel, read_rel, rel, rel_common, r#type,
};

const COMPARISON_URI_ANCHOR: u32 = 1;
const BOOLEAN_URI_ANCHOR: u32 = 2;
const LOGICAL_URI_ANCHOR: u32 = 3;
const LOGICAL_TYPE_ANCHOR: u32 = 10;
const EQUAL: u32 = 1;
const AND: u32 = 2;
const OR: u32 = 3;
const NOT: u32 = 4;
const IS_NOT_NULL: u32 = 5;
const NOT_EQUAL: u32 = 6;
const LESS_THAN: u32 = 7;
const LESS_THAN_OR_EQUAL: u32 = 8;
const GREATER_THAN: u32 = 9;
const GREATER_THAN_OR_EQUAL: u32 = 10;
const IS_NULL: u32 = 11;

struct PostgreSqlDialect;

impl SqlDialect for PostgreSqlDialect {
    fn quote_identifier(&self, identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    fn write_placeholder(&self, position: usize, sql: &mut String) {
        sql.push('$');
        sql.push_str(&position.to_string());
    }
}

struct TranslationPlanner {
    relation: Vec<String>,
    columns: Vec<String>,
    result: RefCell<Option<Result<connector_sql::RenderedQuery, TranslationError>>>,
}

impl TranslationPlanner {
    fn new() -> Self {
        Self {
            relation: vec!["physical".to_owned(), "users".to_owned()],
            columns: vec![
                "id_col".to_owned(),
                "name_col".to_owned(),
                "score_col".to_owned(),
            ],
            result: RefCell::new(None),
        }
    }
}

impl ConnectorPlanner for TranslationPlanner {
    fn plan(&self, plan: &LogicalPlan) -> Result<PushdownResult, PlanningError> {
        self.result.replace(Some(translate(
            plan,
            &self.relation,
            &self.columns,
            PostgreSqlDialect,
        )));
        Ok(PushdownResult::Unsupported {
            reason: "test captured translation".to_owned(),
        })
    }
}

#[test]
fn translates_read() {
    let query = translate_success(read_plan());

    assert_eq!(
        query.sql,
        "SELECT \"id_col\" AS \"id\", \"name_col\" AS \"name\", \"score_col\" AS \"score\" FROM \"physical\".\"users\""
    );
    assert!(query.parameters.is_none());
}

#[test]
fn translates_project_over_read() {
    let query = translate_success(project_read_plan());

    assert_eq!(
        query.sql,
        "SELECT \"score_col\" AS \"score_alias\", \"id_col\" AS \"id_alias\" FROM \"physical\".\"users\""
    );
    assert!(query.parameters.is_none());
}

#[test]
fn translates_filter_over_read() {
    let query = translate_success(filter_read_plan(scalar(
        EQUAL,
        vec![column(0), i64_literal(7)],
    )));

    assert_eq!(
        query.sql,
        "SELECT \"id_col\" AS \"id\", \"name_col\" AS \"name\", \"score_col\" AS \"score\" FROM \"physical\".\"users\" WHERE (\"id_col\" = $1)"
    );
    let parameters = query.parameters.unwrap();
    assert_eq!(parameters.num_columns(), 1);
    assert_eq!(
        parameters
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0),
        7
    );
}

#[test]
fn translates_each_supported_predicate() {
    let comparisons = [
        (EQUAL, "(\"id_col\" = $1)"),
        (NOT_EQUAL, "(\"id_col\" <> $1)"),
        (LESS_THAN, "(\"id_col\" < $1)"),
        (LESS_THAN_OR_EQUAL, "(\"id_col\" <= $1)"),
        (GREATER_THAN, "(\"id_col\" > $1)"),
        (GREATER_THAN_OR_EQUAL, "(\"id_col\" >= $1)"),
    ];
    for (function, expected) in comparisons {
        assert_predicate_sql(scalar(function, vec![column(0), i64_literal(7)]), expected);
    }

    assert_predicate_sql(
        scalar(
            AND,
            vec![
                scalar(EQUAL, vec![column(0), i64_literal(1)]),
                scalar(EQUAL, vec![column(2), i64_literal(2)]),
            ],
        ),
        "((\"id_col\" = $1) AND (\"score_col\" = $2))",
    );
    assert_predicate_sql(
        scalar(
            OR,
            vec![
                scalar(EQUAL, vec![column(0), i64_literal(1)]),
                scalar(EQUAL, vec![column(2), i64_literal(2)]),
            ],
        ),
        "((\"id_col\" = $1) OR (\"score_col\" = $2))",
    );
    assert_predicate_sql(
        scalar(NOT, vec![scalar(EQUAL, vec![column(0), i64_literal(7)])]),
        "(NOT (\"id_col\" = $1))",
    );
    assert_predicate_sql(scalar(IS_NULL, vec![column(1)]), "(\"name_col\" IS NULL)");
    assert_predicate_sql(
        scalar(IS_NOT_NULL, vec![column(1)]),
        "(\"name_col\" IS NOT NULL)",
    );
}

#[test]
fn translates_projection_filter_and_typed_parameters_in_depth_first_order() {
    let planner = TranslationPlanner::new();

    PushdownRequest {
        offered_plan: requested_plan("equal:any1_any1", column(1)),
    }
    .plan(&planner)
    .unwrap();
    let query = planner.result.take().unwrap().unwrap();

    assert_eq!(
        query.sql,
        "SELECT \"score_col\" AS \"score_alias\", \"id_col\" AS \"id_alias\" FROM \"physical\".\"users\" WHERE (((NOT (\"name_col\" = $1)) OR (\"score_col\" IS NOT NULL)) AND (\"id_col\" = $2))"
    );
    let parameters = query.parameters.unwrap();
    assert_eq!(parameters.num_rows(), 1);
    assert_eq!(parameters.num_columns(), 2);
    assert_eq!(parameters.schema().field(0).data_type(), &DataType::Utf8);
    assert!(parameters.column(0).is_null(0));
    assert_eq!(
        parameters
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        parameters
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0),
        7
    );
}

#[test]
fn reports_a_declared_but_unmapped_function_as_unsupported() {
    let planner = TranslationPlanner::new();
    PushdownRequest {
        offered_plan: requested_plan("mystery:any1_any1", column(1)),
    }
    .plan(&planner)
    .unwrap();

    assert!(matches!(
        planner.result.take().unwrap(),
        Err(TranslationError::Unsupported { .. })
    ));
}

#[test]
fn rejects_an_out_of_range_filter_field_before_translation() {
    let planner = TranslationPlanner::new();
    let result = PushdownRequest {
        offered_plan: requested_plan("equal:any1_any1", column(99)),
    }
    .plan(&planner);

    assert!(matches!(result, Err(PlanningError::InvalidPlan(_))));
    assert!(planner.result.borrow().is_none());
}

#[test]
fn accepts_function_anchor_zero() {
    let planner = TranslationPlanner::new();
    let mut plan = requested_plan("equal:any1_any1", column(1));
    let declaration = plan
        .extensions
        .iter_mut()
        .find_map(|declaration| match declaration.mapping_type.as_mut() {
            Some(simple_extension_declaration::MappingType::ExtensionFunction(function))
                if function.function_anchor == EQUAL =>
            {
                Some(function)
            }
            _ => None,
        })
        .unwrap();
    declaration.function_anchor = 0;
    replace_function_anchor(filter_condition_mut(&mut plan), EQUAL, 0);

    PushdownRequest { offered_plan: plan }
        .plan(&planner)
        .unwrap();
    assert!(planner.result.take().unwrap().is_ok());
}

#[test]
fn translates_duration_and_compound_interval_literals() {
    let duration = expression::Literal {
        literal_type: Some(expression::literal::LiteralType::IntervalDayToSecond(
            expression::literal::IntervalDayToSecond {
                days: 1,
                seconds: 2,
                subseconds: 3,
                precision_mode: Some(
                    expression::literal::interval_day_to_second::PrecisionMode::Precision(6),
                ),
            },
        )),
        ..Default::default()
    };
    let duration_query = translate_literal_plan(
        Type {
            kind: Some(r#type::Kind::IntervalDay(r#type::IntervalDay {
                precision: Some(6),
                nullability: required(),
                ..Default::default()
            })),
        },
        duration,
    );
    let duration_parameters = duration_query.parameters.unwrap();
    assert_eq!(
        duration_parameters.schema().field(0).data_type(),
        &DataType::Duration(TimeUnit::Microsecond)
    );
    assert_eq!(
        duration_parameters
            .column(0)
            .as_any()
            .downcast_ref::<DurationMicrosecondArray>()
            .unwrap()
            .value(0),
        86_402_000_003
    );

    let interval = expression::Literal {
        literal_type: Some(expression::literal::LiteralType::IntervalCompound(
            expression::literal::IntervalCompound {
                interval_year_to_month: Some(expression::literal::IntervalYearToMonth {
                    years: 1,
                    months: 2,
                }),
                interval_day_to_second: Some(expression::literal::IntervalDayToSecond {
                    days: 3,
                    seconds: 4,
                    subseconds: 5,
                    precision_mode: Some(
                        expression::literal::interval_day_to_second::PrecisionMode::Precision(9),
                    ),
                }),
            },
        )),
        ..Default::default()
    };
    let interval_query = translate_literal_plan(
        Type {
            kind: Some(r#type::Kind::IntervalCompound(r#type::IntervalCompound {
                precision: 9,
                nullability: required(),
                ..Default::default()
            })),
        },
        interval,
    );
    let interval_parameters = interval_query.parameters.unwrap();
    assert_eq!(
        interval_parameters.schema().field(0).data_type(),
        &DataType::Interval(IntervalUnit::MonthDayNano)
    );
    let value = interval_parameters
        .column(0)
        .as_any()
        .downcast_ref::<IntervalMonthDayNanoArray>()
        .unwrap()
        .value(0);
    assert_eq!(
        (value.months, value.days, value.nanoseconds),
        (14, 3, 4_000_000_005)
    );
}

#[test]
fn translates_extension_encoded_logical_literals_through_their_carrier() {
    let parameters = vec![
        string_type_parameter("uuid"),
        string_type_parameter("{}"),
        data_type_parameter(string_type(false)),
    ];
    let logical_type = Type {
        kind: Some(r#type::Kind::UserDefined(r#type::UserDefined {
            type_reference: LOGICAL_TYPE_ANCHOR,
            nullability: required(),
            type_parameters: parameters.clone(),
            ..Default::default()
        })),
    };
    let logical_literal = expression::Literal {
        literal_type: Some(expression::literal::LiteralType::UserDefined(
            expression::literal::UserDefined {
                type_reference: LOGICAL_TYPE_ANCHOR,
                type_parameters: parameters,
                val: Some(expression::literal::user_defined::Val::Struct(
                    expression::literal::Struct {
                        fields: vec![expression::Literal {
                            literal_type: Some(expression::literal::LiteralType::String(
                                "550e8400-e29b-41d4-a716-446655440000".to_owned(),
                            )),
                            ..Default::default()
                        }],
                    },
                )),
            },
        )),
        ..Default::default()
    };

    let mut plan = requested_plan("equal:any1_any1", column(0));
    plan.extension_uris.push(extension_uri(
        LOGICAL_URI_ANCHOR,
        "urn:scalardb:analytics:substrait:logical",
    ));
    plan.extensions.push(type_decl(
        LOGICAL_URI_ANCHOR,
        LOGICAL_TYPE_ANCHOR,
        "logical",
    ));
    read_base_schema_mut(&mut plan)
        .r#struct
        .as_mut()
        .unwrap()
        .types[0] = logical_type;
    *filter_condition_mut(&mut plan) = scalar(
        EQUAL,
        vec![
            column(0),
            Expression {
                rex_type: Some(expression::RexType::Literal(logical_literal)),
            },
        ],
    );

    let planner = TranslationPlanner::new();
    PushdownRequest { offered_plan: plan }
        .plan(&planner)
        .unwrap();
    let query = planner.result.take().unwrap().unwrap();
    let parameters = query.parameters.unwrap();
    assert_eq!(parameters.schema().field(0).data_type(), &DataType::Utf8);
    assert_eq!(
        parameters
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0),
        "550e8400-e29b-41d4-a716-446655440000"
    );
}

#[test]
fn rejects_scalar_literals_that_do_not_fit_their_declared_type() {
    assert_invalid_literal(
        Type {
            kind: Some(r#type::Kind::I8(r#type::I8 {
                nullability: required(),
                ..Default::default()
            })),
        },
        expression::Literal {
            literal_type: Some(expression::literal::LiteralType::I8(128)),
            ..Default::default()
        },
    );
    assert_invalid_literal(
        Type {
            kind: Some(r#type::Kind::Decimal(r#type::Decimal {
                precision: 2,
                scale: 0,
                nullability: required(),
                ..Default::default()
            })),
        },
        expression::Literal {
            literal_type: Some(expression::literal::LiteralType::Decimal(
                expression::literal::Decimal {
                    value: vec![100; 16],
                    precision: 2,
                    scale: 0,
                },
            )),
            ..Default::default()
        },
    );
    assert_invalid_literal(
        Type {
            kind: Some(r#type::Kind::PrecisionTime(r#type::PrecisionTime {
                precision: 6,
                nullability: required(),
                ..Default::default()
            })),
        },
        expression::Literal {
            literal_type: Some(expression::literal::LiteralType::PrecisionTime(
                expression::literal::PrecisionTime {
                    precision: 6,
                    value: 86_400_000_000,
                },
            )),
            ..Default::default()
        },
    );
    assert_invalid_literal(
        Type {
            kind: Some(r#type::Kind::IntervalDay(r#type::IntervalDay {
                precision: Some(6),
                nullability: required(),
                ..Default::default()
            })),
        },
        expression::Literal {
            literal_type: Some(expression::literal::LiteralType::IntervalDayToSecond(
                expression::literal::IntervalDayToSecond {
                    days: i32::MAX,
                    seconds: 0,
                    subseconds: 0,
                    precision_mode: Some(
                        expression::literal::interval_day_to_second::PrecisionMode::Precision(6),
                    ),
                },
            )),
            ..Default::default()
        },
    );
    assert_invalid_literal(
        Type {
            kind: Some(r#type::Kind::IntervalCompound(r#type::IntervalCompound {
                precision: 9,
                nullability: required(),
                ..Default::default()
            })),
        },
        expression::Literal {
            literal_type: Some(expression::literal::LiteralType::IntervalCompound(
                expression::literal::IntervalCompound {
                    interval_year_to_month: Some(expression::literal::IntervalYearToMonth {
                        years: i32::MAX,
                        months: 0,
                    }),
                    interval_day_to_second: Some(expression::literal::IntervalDayToSecond {
                        days: 0,
                        seconds: 0,
                        subseconds: 0,
                        precision_mode: Some(
                            expression::literal::interval_day_to_second::PrecisionMode::Precision(
                                9,
                            ),
                        ),
                    }),
                },
            )),
            ..Default::default()
        },
    );
}

fn translate_success(plan: Plan) -> connector_sql::RenderedQuery {
    let planner = TranslationPlanner::new();
    PushdownRequest { offered_plan: plan }
        .plan(&planner)
        .unwrap();
    planner.result.into_inner().unwrap().unwrap()
}

fn assert_predicate_sql(condition: Expression, expected_predicate: &str) {
    let query = translate_success(filter_read_plan(condition));
    assert_eq!(
        query.sql,
        format!(
            "SELECT \"id_col\" AS \"id\", \"name_col\" AS \"name\", \"score_col\" AS \"score\" FROM \"physical\".\"users\" WHERE {expected_predicate}"
        )
    );
}

fn read_plan() -> Plan {
    let mut plan = requested_plan("equal:any1_any1", column(0));
    remove_filter(&mut plan);
    remove_project(&mut plan);
    plan
}

fn project_read_plan() -> Plan {
    let mut plan = requested_plan("equal:any1_any1", column(0));
    remove_filter(&mut plan);
    plan
}

fn filter_read_plan(condition: Expression) -> Plan {
    let mut plan = requested_plan("equal:any1_any1", column(0));
    *filter_condition_mut(&mut plan) = condition;
    remove_project(&mut plan);
    plan
}

fn remove_project(plan: &mut Plan) {
    let root = match plan.relations[0].rel_type.as_mut().unwrap() {
        plan_rel::RelType::Root(root) => root,
        _ => panic!("expected root"),
    };
    let project = match root.input.take().unwrap().rel_type.unwrap() {
        rel::RelType::Project(project) => project,
        _ => panic!("expected project"),
    };
    root.input = project.input.map(|input| *input);
    root.names = vec!["id".to_owned(), "name".to_owned(), "score".to_owned()];
}

fn remove_filter(plan: &mut Plan) {
    let root = match plan.relations[0].rel_type.as_mut().unwrap() {
        plan_rel::RelType::Root(root) => root,
        _ => panic!("expected root"),
    };
    let project = match root.input.as_mut().unwrap().rel_type.as_mut().unwrap() {
        rel::RelType::Project(project) => project,
        _ => panic!("expected project"),
    };
    let filter = match project.input.take().unwrap().rel_type.unwrap() {
        rel::RelType::Filter(filter) => filter,
        _ => panic!("expected filter"),
    };
    project.input = filter.input;
}

fn translate_literal_plan(
    data_type: Type,
    literal: expression::Literal,
) -> connector_sql::RenderedQuery {
    let planner = TranslationPlanner::new();
    let mut plan = requested_plan("equal:any1_any1", column(0));
    read_base_schema_mut(&mut plan)
        .r#struct
        .as_mut()
        .unwrap()
        .types[0] = data_type;
    *filter_condition_mut(&mut plan) = scalar(
        EQUAL,
        vec![
            column(0),
            Expression {
                rex_type: Some(expression::RexType::Literal(literal)),
            },
        ],
    );
    PushdownRequest { offered_plan: plan }
        .plan(&planner)
        .unwrap();
    planner.result.take().unwrap().unwrap()
}

fn assert_invalid_literal(data_type: Type, literal: expression::Literal) {
    let planner = TranslationPlanner::new();
    let mut plan = requested_plan("equal:any1_any1", column(0));
    read_base_schema_mut(&mut plan)
        .r#struct
        .as_mut()
        .unwrap()
        .types[0] = data_type;
    *filter_condition_mut(&mut plan) = scalar(
        EQUAL,
        vec![
            column(0),
            Expression {
                rex_type: Some(expression::RexType::Literal(literal)),
            },
        ],
    );
    assert!(matches!(
        PushdownRequest { offered_plan: plan }.plan(&planner),
        Err(PlanningError::InvalidPlan(_))
    ));
    assert!(planner.result.borrow().is_none());
}

fn read_base_schema_mut(plan: &mut Plan) -> &mut NamedStruct {
    let root = match plan.relations[0].rel_type.as_mut().unwrap() {
        plan_rel::RelType::Root(root) => root,
        _ => panic!("expected root"),
    };
    let project = match root.input.as_mut().unwrap().rel_type.as_mut().unwrap() {
        rel::RelType::Project(project) => project,
        _ => panic!("expected project"),
    };
    let filter = match project.input.as_mut().unwrap().rel_type.as_mut().unwrap() {
        rel::RelType::Filter(filter) => filter,
        _ => panic!("expected filter"),
    };
    let read = match filter.input.as_mut().unwrap().rel_type.as_mut().unwrap() {
        rel::RelType::Read(read) => read,
        _ => panic!("expected read"),
    };
    read.base_schema.as_mut().unwrap()
}

fn filter_condition_mut(plan: &mut Plan) -> &mut Expression {
    let root = match plan.relations[0].rel_type.as_mut().unwrap() {
        plan_rel::RelType::Root(root) => root,
        _ => panic!("expected root"),
    };
    let project = match root.input.as_mut().unwrap().rel_type.as_mut().unwrap() {
        rel::RelType::Project(project) => project,
        _ => panic!("expected project"),
    };
    let filter = match project.input.as_mut().unwrap().rel_type.as_mut().unwrap() {
        rel::RelType::Filter(filter) => filter,
        _ => panic!("expected filter"),
    };
    filter.condition.as_deref_mut().unwrap()
}

fn replace_function_anchor(expression: &mut Expression, from: u32, to: u32) {
    let Some(expression::RexType::ScalarFunction(function)) = expression.rex_type.as_mut() else {
        return;
    };
    if function.function_reference == from {
        function.function_reference = to;
    }
    for argument in &mut function.arguments {
        if let Some(function_argument::ArgType::Value(value)) = argument.arg_type.as_mut() {
            replace_function_anchor(value, from, to);
        }
    }
}

fn requested_plan(equal_name: &str, first_comparison_column: Expression) -> Plan {
    let read = ReadRel {
        base_schema: Some(NamedStruct {
            names: vec!["id".to_owned(), "name".to_owned(), "score".to_owned()],
            r#struct: Some(r#type::Struct {
                types: vec![i64_type(false), string_type(true), i64_type(true)],
                nullability: required(),
                ..Default::default()
            }),
        }),
        read_type: Some(read_rel::ReadType::NamedTable(read_rel::NamedTable {
            names: vec!["binding".to_owned(), "users".to_owned()],
            advanced_extension: None,
        })),
        ..Default::default()
    };
    let condition = scalar(
        AND,
        vec![
            scalar(
                OR,
                vec![
                    scalar(
                        NOT,
                        vec![scalar(
                            EQUAL,
                            vec![first_comparison_column, typed_null(string_type(true))],
                        )],
                    ),
                    scalar(IS_NOT_NULL, vec![column(2)]),
                ],
            ),
            scalar(EQUAL, vec![column(0), i64_literal(7)]),
        ],
    );
    Plan {
        version: Some(Version {
            major_number: 0,
            minor_number: 69,
            patch_number: 0,
            producer: "connector-sql-tests".to_owned(),
            ..Default::default()
        }),
        extension_uris: vec![
            extension_uri(
                COMPARISON_URI_ANCHOR,
                "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_comparison.yaml",
            ),
            extension_uri(
                BOOLEAN_URI_ANCHOR,
                "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_boolean.yaml",
            ),
        ],
        extensions: vec![
            function_decl(COMPARISON_URI_ANCHOR, EQUAL, equal_name),
            function_decl(BOOLEAN_URI_ANCHOR, AND, "and:bool_bool"),
            function_decl(BOOLEAN_URI_ANCHOR, OR, "or:bool_bool"),
            function_decl(BOOLEAN_URI_ANCHOR, NOT, "not:bool"),
            function_decl(COMPARISON_URI_ANCHOR, IS_NOT_NULL, "is_not_null:any"),
            function_decl(COMPARISON_URI_ANCHOR, NOT_EQUAL, "not_equal:any1_any1"),
            function_decl(COMPARISON_URI_ANCHOR, LESS_THAN, "lt:any1_any1"),
            function_decl(COMPARISON_URI_ANCHOR, LESS_THAN_OR_EQUAL, "lte:any1_any1"),
            function_decl(COMPARISON_URI_ANCHOR, GREATER_THAN, "gt:any1_any1"),
            function_decl(
                COMPARISON_URI_ANCHOR,
                GREATER_THAN_OR_EQUAL,
                "gte:any1_any1",
            ),
            function_decl(COMPARISON_URI_ANCHOR, IS_NULL, "is_null:any"),
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
                                condition: Some(Box::new(condition)),
                                ..Default::default()
                            }))),
                        })),
                        expressions: vec![column(2), column(0)],
                        ..Default::default()
                    }))),
                }),
                names: vec!["score_alias".to_owned(), "id_alias".to_owned()],
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

fn string_type_parameter(value: &str) -> r#type::Parameter {
    r#type::Parameter {
        parameter: Some(r#type::parameter::Parameter::String(value.to_owned())),
    }
}

fn data_type_parameter(value: Type) -> r#type::Parameter {
    r#type::Parameter {
        parameter: Some(r#type::parameter::Parameter::DataType(value)),
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

#[allow(deprecated)]
fn scalar(anchor: u32, arguments: Vec<Expression>) -> Expression {
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
                output_type: Some(bool_type(true)),
                args: Vec::new(),
            },
        )),
    }
}

fn column(position: i32) -> Expression {
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

fn i64_literal(value: i64) -> Expression {
    Expression {
        rex_type: Some(expression::RexType::Literal(expression::Literal {
            literal_type: Some(expression::literal::LiteralType::I64(value)),
            ..Default::default()
        })),
    }
}

fn typed_null(data_type: Type) -> Expression {
    Expression {
        rex_type: Some(expression::RexType::Literal(expression::Literal {
            literal_type: Some(expression::literal::LiteralType::Null(data_type)),
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
