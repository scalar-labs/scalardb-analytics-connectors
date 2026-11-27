//! The reads, source changes, and failing requests the contract expects of
//! the fixture's objects.
//!
//! The values below are those `setup.sql` writes, in the Arrow types the
//! connector converts the driver's output to.

use std::sync::Arc;

use analytics_types::{
    AnalyticsField, AnalyticsType, ArrayType, MapType, StructType, arrow_schema::DataType,
};
use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal32Array, Decimal64Array,
    Decimal128Array, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    ListArray, MapArray, StringArray, StructArray, TimestampMicrosecondArray,
};
use arrow_buffer::{NullBuffer, OffsetBuffer};
use connector_sdk::{
    ErrorKind,
    conformance::{
        DriftCase, DriftOutcome, ErrorCase, Expected, ExpectedError, ExpectedPlanning,
        FailingRequest, ReadCase, ReadResult, plan::*,
    },
};
use substrait::proto::{
    Expression, Plan, Type,
    expression::literal::{Decimal, LiteralType, PrecisionTimestamp},
    plan_rel, rel, r#type,
};

use super::DatabricksFixture;

/// 0001-01-01 and 9999-12-31, Databricks' date range, in days since
/// 1970-01-01.
const FIRST_DAY: i32 = -719_162;
const LAST_DAY: i32 = 2_932_896;
const MICROS_PER_DAY: i64 = 86_400_000_000;
/// 2020-01-01T00:00:00 in microseconds since 1970-01-01T00:00:00.
const NEW_YEAR_2020: i64 = 18_262 * MICROS_PER_DAY;

pub fn read_cases(fixture: &DatabricksFixture) -> Vec<ReadCase> {
    let mut cases = vec![ReadCase {
        name: "every mapping rule reads its boundary values and NULL",
        offered: value_types(fixture),
        expected: remote(ReadResult::Rows(value_type_rows())),
    }];
    cases.extend(filter_cases(fixture));
    cases
}

/// A filter that runs remotely returns only its rows; one that stays local
/// leaves the whole table to the remote result.
fn filter_cases(fixture: &DatabricksFixture) -> Vec<ReadCase> {
    let remote_ids =
        |ids: Vec<i32>| remote(ReadResult::Rows(vec![Arc::new(Int32Array::from(ids))]));
    let local = || ExpectedPlanning::Planned {
        local_work: true,
        result: Expected::Contract(ReadResult::Rows(filtering_rows())),
    };
    let case = |name, filter, expected| ReadCase {
        name,
        offered: filtering(fixture, filter),
        expected,
    };
    vec![
        case(
            "integer comparison runs remotely",
            call("gt", vec![column(0), int(1)]),
            remote_ids(vec![2, 3]),
        ),
        case(
            "text equality under UTF8_BINARY runs remotely",
            call("equal", vec![column(1), string("a")]),
            remote_ids(vec![1]),
        ),
        case(
            "text ordering under UTF8_BINARY runs remotely",
            call("lt", vec![column(1), string("a!")]),
            remote_ids(vec![1, 3]),
        ),
        case(
            "text equality between UTF8_BINARY columns runs remotely",
            call("equal", vec![column(1), column(10)]),
            remote_ids(vec![1, 3]),
        ),
        case(
            "a UTF8_LCASE collation folds case",
            call("equal", vec![column(2), string("a")]),
            local(),
        ),
        case(
            "char columns pad with spaces",
            call("equal", vec![column(3), string("a")]),
            local(),
        ),
        case(
            "floating-point comparison runs remotely",
            call("gt", vec![column(4), literal(LiteralType::Fp64(1.0))]),
            remote_ids(vec![2, 3]),
        ),
        case(
            "date comparison runs remotely",
            call("gt", vec![column(5), literal(LiteralType::Date(18_414))]),
            remote_ids(vec![3]),
        ),
        case(
            "decimal comparison runs remotely",
            call(
                "lt",
                vec![
                    column(6),
                    literal(LiteralType::Decimal(Decimal {
                        value: 0_i128.to_le_bytes().to_vec(),
                        precision: 5,
                        scale: 2,
                    })),
                ],
            ),
            remote_ids(vec![2]),
        ),
        case(
            "instants compare in UTC whatever offset they were written with",
            call(
                "equal",
                vec![
                    column(7),
                    literal(LiteralType::PrecisionTimestampTz(PrecisionTimestamp {
                        precision: 6,
                        value: NEW_YEAR_2020,
                    })),
                ],
            ),
            remote_ids(vec![1, 2]),
        ),
        case(
            "timestamp comparison runs remotely",
            call(
                "gt",
                vec![
                    column(8),
                    literal(LiteralType::PrecisionTimestamp(PrecisionTimestamp {
                        precision: 6,
                        value: NEW_YEAR_2020,
                    })),
                ],
            ),
            remote_ids(vec![2]),
        ),
        case(
            "Boolean comparison runs remotely",
            call(
                "equal",
                vec![column(9), literal(LiteralType::Boolean(true))],
            ),
            remote_ids(vec![1, 3]),
        ),
        case(
            "Boolean composition and null tests run remotely",
            call(
                "or",
                vec![
                    call("is_null", vec![column(5)]),
                    call("not", vec![call("equal", vec![column(0), int(1)])]),
                ],
            ),
            remote_ids(vec![2, 3]),
        ),
        case(
            "a non-normalized tree runs remotely",
            call(
                "and",
                vec![
                    call(
                        "not",
                        vec![call("not", vec![call("gt", vec![column(0), int(1)])])],
                    ),
                    call(
                        "and",
                        vec![
                            call("lte", vec![column(0), int(2)]),
                            call("is_not_null", vec![column(0)]),
                        ],
                    ),
                ],
            ),
            remote_ids(vec![2]),
        ),
    ]
}

pub fn drift_cases<'a>(fixture: &'a DatabricksFixture) -> Vec<DriftCase<'a>> {
    let plan = |table: &str, value_nullable: Option<bool>| {
        let mut columns = vec![("id", i32(false))];
        columns.extend(value_nullable.map(|nullable| ("value", i32(nullable))));
        ReadPlan::new(fixture.table(table).each_ref().map(String::as_str), columns).build()
    };
    let change = |sql: &'static str| -> Box<dyn Fn() + 'a> { Box::new(move || fixture.sql(sql)) };
    vec![
        DriftCase {
            name: "a changed column type",
            offered: plan("retyped", Some(false)),
            change: change(
                "CREATE OR REPLACE TABLE ${schema}.retyped (id INT NOT NULL, value BIGINT NOT NULL)",
            ),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a dropped column",
            offered: plan("narrowed", Some(false)),
            change: change("CREATE OR REPLACE TABLE ${schema}.narrowed (id INT NOT NULL)"),
            expected: DriftOutcome::Fails(ErrorKind::DoesNotExist),
        },
        DriftCase {
            name: "a dropped table, which Unity Catalog cannot tell from a hidden one",
            offered: plan("removed", None),
            change: change("DROP TABLE ${schema}.removed"),
            expected: DriftOutcome::Fails(ErrorKind::NotFound),
        },
        DriftCase {
            name: "a NOT NULL column made nullable",
            offered: plan("relaxed", Some(false)),
            change: change("ALTER TABLE ${schema}.relaxed ALTER COLUMN value DROP NOT NULL"),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a nullable column made NOT NULL",
            offered: plan("tightened", Some(true)),
            change: change("ALTER TABLE ${schema}.tightened ALTER COLUMN value SET NOT NULL"),
            expected: DriftOutcome::Reads,
        },
    ]
}

/// The failures one credential can show: a rejected credential, and a table
/// that is not found. Permission denied needs a second principal, which the
/// fixture does not have.
pub fn error_cases(fixture: &DatabricksFixture) -> Vec<ErrorCase> {
    let failing = |kind| {
        Expected::Contract(ExpectedError {
            kind,
            retryable: false,
        })
    };
    vec![
        ErrorCase {
            name: "a rejected credential",
            options: fixture.rejected_options(),
            request: FailingRequest::OpenSession,
            expected: failing(ErrorKind::Authentication),
        },
        ErrorCase {
            name: "reading a table that is not found",
            options: fixture.credential_options(),
            request: FailingRequest::Read(Box::new(
                ReadPlan::new(
                    fixture.table("missing").each_ref().map(String::as_str),
                    vec![("id", i32(false)), ("label", text(true))],
                )
                .filter(call(
                    "equal",
                    vec![column(1), string("conformance-literal")],
                ))
                .build(),
            )),
            expected: failing(ErrorKind::NotFound),
        },
    ]
}

fn remote(result: ReadResult) -> ExpectedPlanning {
    ExpectedPlanning::Planned {
        local_work: false,
        result: Expected::Contract(result),
    }
}

fn int(value: i32) -> Expression {
    literal(LiteralType::I32(value))
}

fn string(value: &str) -> Expression {
    literal(LiteralType::String(value.to_owned()))
}

fn nullability(nullable: bool) -> i32 {
    if nullable {
        r#type::Nullability::Nullable as i32
    } else {
        r#type::Nullability::Required as i32
    }
}

/// The Substrait list type of `element`.
fn list(element: Type, nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::List(Box::new(r#type::List {
            r#type: Some(Box::new(element)),
            nullability: nullability(nullable),
            ..Default::default()
        }))),
    }
}

/// The Substrait map type of `key` to `value`.
fn map(key: Type, value: Type, nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::Map(Box::new(r#type::Map {
            key: Some(Box::new(key)),
            value: Some(Box::new(value)),
            nullability: nullability(nullable),
            ..Default::default()
        }))),
    }
}

/// The Substrait struct type of `fields`, whose names the read's base schema
/// lists after the struct column's ([`with_struct_field_names`]).
fn structure(fields: Vec<Type>, nullable: bool) -> Type {
    Type {
        kind: Some(r#type::Kind::Struct(r#type::Struct {
            types: fields,
            nullability: nullability(nullable),
            ..Default::default()
        })),
    }
}

/// Inserts struct field names into the base schema of a plain read, after
/// the name of the column whose struct they belong to: Substrait lists a
/// base schema's names depth-first, and [`ReadPlan`] lists only the columns'.
fn with_struct_field_names(mut plan: Plan, column: &str, fields: &[&str]) -> Plan {
    let Some(plan_rel::RelType::Root(root)) = plan.relations[0].rel_type.as_mut() else {
        unreachable!("a read plan has a root")
    };
    let Some(rel::RelType::Read(read)) = root
        .input
        .as_mut()
        .and_then(|input| input.rel_type.as_mut())
    else {
        unreachable!("the plan reads without a filter or projection")
    };
    let names = &mut read.base_schema.as_mut().unwrap().names;
    let position = names.iter().position(|name| name == column).unwrap() + 1;
    names.splice(
        position..position,
        fields.iter().map(|name| (*name).to_owned()),
    );
    plan
}

/// Every mapped column of `value_types` but the collated string, which the
/// filters cover.
fn value_types(fixture: &DatabricksFixture) -> Plan {
    let plan = ReadPlan::new(
        fixture.table("value_types").each_ref().map(String::as_str),
        vec![
            ("id", i32(false)),
            ("tiny", i8(true)),
            ("small", i16(true)),
            ("regular", i32(true)),
            ("big", i64(true)),
            ("single", fp32(true)),
            ("ratio", fp64(true)),
            ("narrow", decimal(5, 2, true)),
            ("medium", decimal(18, 4, true)),
            ("widest", decimal(38, 4, true)),
            ("label", text(true)),
            ("bounded", text(true)),
            ("fixed", text(true)),
            ("bytes", binary(true)),
            ("day", date(true)),
            ("stamp", timestamp(true)),
            ("instant", timestamp_tz(true)),
            ("flag", boolean(true)),
            ("tags", list(i32(true), true)),
            ("attributes", map(text(false), i32(true), true)),
            ("point", structure(vec![i32(true), text(true)], true)),
        ],
    )
    .build();
    with_struct_field_names(plan, "point", &["x", "y"])
}

/// The three rows of `value_types`: maxima, NULL, and minima, with the
/// nested types holding a NULL element, nothing, or NULL fields.
fn value_type_rows() -> Vec<ArrayRef> {
    let text =
        |values: [Option<&str>; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    let first_instant = i64::from(FIRST_DAY) * MICROS_PER_DAY;
    let last_instant = i64::from(LAST_DAY) * MICROS_PER_DAY + MICROS_PER_DAY - 1;
    let nines = |digits: u32| 10_i128.pow(digits) - 1;
    let present = || Some(NullBuffer::from(vec![true, false, true]));
    vec![
        Arc::new(Int32Array::from(vec![1, 2, 3])),
        Arc::new(Int8Array::from(vec![Some(i8::MAX), None, Some(i8::MIN)])),
        Arc::new(Int16Array::from(vec![Some(i16::MAX), None, Some(i16::MIN)])),
        Arc::new(Int32Array::from(vec![Some(i32::MAX), None, Some(i32::MIN)])),
        Arc::new(Int64Array::from(vec![Some(i64::MIN), None, Some(i64::MAX)])),
        Arc::new(Float32Array::from(vec![Some(0.5), None, Some(f32::MIN)])),
        Arc::new(Float64Array::from(vec![Some(0.25), None, Some(f64::MAX)])),
        Arc::new(
            Decimal32Array::from(vec![Some(99_999), None, Some(-99_999)])
                .with_precision_and_scale(5, 2)
                .unwrap(),
        ),
        Arc::new(
            Decimal64Array::from(vec![
                Some(999_999_999_999_999_999),
                None,
                Some(-999_999_999_999_999_999),
            ])
            .with_precision_and_scale(18, 4)
            .unwrap(),
        ),
        Arc::new(
            Decimal128Array::from(vec![Some(nines(38)), None, Some(-nines(38))])
                .with_precision_and_scale(38, 4)
                .unwrap(),
        ),
        text([Some("héllo"), None, Some("日本語")]),
        text([Some("ten chars."), None, Some("")]),
        // A `char(n)` value is padded with spaces to n characters.
        text([Some("ab   "), None, Some("abcde")]),
        Arc::new(BinaryArray::from(vec![
            Some([0x00, 0xff, 0x00].as_slice()),
            None,
            Some([].as_slice()),
        ])),
        Arc::new(Date32Array::from(vec![
            Some(FIRST_DAY),
            None,
            Some(LAST_DAY),
        ])),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(first_instant),
            None,
            Some(last_instant),
        ])),
        Arc::new(
            TimestampMicrosecondArray::from(vec![Some(first_instant), None, Some(last_instant)])
                .with_timezone("UTC"),
        ),
        Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])),
        {
            let target = AnalyticsType::Array(ArrayType::new(AnalyticsType::Int, true));
            let DataType::List(element) = target.storage_type() else {
                unreachable!()
            };
            Arc::new(ListArray::new(
                element,
                OffsetBuffer::from_lengths([2, 0, 0]),
                Arc::new(Int32Array::from(vec![Some(1), None])),
                present(),
            ))
        },
        {
            let target =
                AnalyticsType::Map(MapType::new(AnalyticsType::Text, AnalyticsType::Int, true));
            let DataType::Map(entries, ordered) = target.storage_type() else {
                unreachable!()
            };
            let DataType::Struct(fields) = entries.data_type().clone() else {
                unreachable!()
            };
            Arc::new(MapArray::new(
                entries,
                OffsetBuffer::from_lengths([1, 0, 0]),
                StructArray::new(
                    fields,
                    vec![
                        Arc::new(StringArray::from(vec!["a"])),
                        Arc::new(Int32Array::from(vec![1])),
                    ],
                    None,
                ),
                present(),
                ordered,
            ))
        },
        {
            let target = AnalyticsType::Struct(StructType::new(vec![
                AnalyticsField::new("x", AnalyticsType::Int, true),
                AnalyticsField::new("y", AnalyticsType::Text, true),
            ]));
            let DataType::Struct(fields) = target.storage_type() else {
                unreachable!()
            };
            Arc::new(StructArray::new(
                fields,
                vec![
                    Arc::new(Int32Array::from(vec![Some(1), None, None])),
                    text([Some("p"), None, None]),
                ],
                present(),
            ))
        },
    ]
}

fn filtering(fixture: &DatabricksFixture, filter: Expression) -> Plan {
    ReadPlan::new(
        fixture.table("filtering").each_ref().map(String::as_str),
        vec![
            ("id", i32(false)),
            ("label", text(true)),
            ("folded", text(true)),
            ("code", text(true)),
            ("score", fp64(true)),
            ("day", date(true)),
            ("amount", decimal(5, 2, true)),
            ("instant", timestamp_tz(true)),
            ("stamp", timestamp(true)),
            ("flag", boolean(true)),
            ("other_label", text(true)),
        ],
    )
    .filter(filter)
    .project(vec![(0, "id")])
    .build()
}

/// Every row of `filtering`, as a remote result that leaves the filter local.
fn filtering_rows() -> Vec<ArrayRef> {
    let text = |values: [&str; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    let new_year_2021 = 18_628 * MICROS_PER_DAY;
    vec![
        Arc::new(Int32Array::from(vec![1, 2, 3])),
        text(["a", "b", "a "]),
        text(["A", "b", "a"]),
        text(["a  ", "b  ", "a  "]),
        Arc::new(Float64Array::from(vec![0.5, 2.5, 1.5])),
        Arc::new(Date32Array::from(vec![Some(18_262), None, Some(18_628)])),
        Arc::new(
            Decimal32Array::from(vec![Some(150), Some(-100), None])
                .with_precision_and_scale(5, 2)
                .unwrap(),
        ),
        Arc::new(
            TimestampMicrosecondArray::from(vec![NEW_YEAR_2020, NEW_YEAR_2020, new_year_2021])
                .with_timezone("UTC"),
        ),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(NEW_YEAR_2020),
            Some(18_414 * MICROS_PER_DAY),
            None,
        ])),
        Arc::new(BooleanArray::from(vec![true, false, true])),
        text(["a", "c", "a "]),
    ]
}
