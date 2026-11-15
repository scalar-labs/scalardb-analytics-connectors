//! The reads, source changes, and failing requests the contract expects of
//! the fixture's objects.

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal32Array, Decimal64Array,
    Decimal128Array, Decimal256Array, Float32Array, Float64Array, Int16Array, Int32Array,
    Int64Array, IntervalMonthDayNanoArray, StringArray, Time64MicrosecondArray,
    TimestampMicrosecondArray, types::IntervalMonthDayNano,
};
use arrow_buffer::i256;
use connector_sdk::{
    ErrorKind,
    conformance::{
        DriftCase, DriftOutcome, ErrorCase, Expected, ExpectedError, ExpectedPlanning,
        FailingRequest, ReadCase, ReadResult, plan::*,
    },
};
use substrait::proto::{Expression, Plan, expression::literal::LiteralType};

use super::{OUTSIDER, READER, SCHEMA, options, server};

pub fn read_cases() -> Vec<ReadCase> {
    let mut cases = vec![
        ReadCase {
            name: "every covered rule reads its boundary values and NULL",
            offered: value_types().build(),
            expected: remote(rows(value_type_rows())),
        },
        ReadCase {
            name: "a NaN numeric cannot be carried",
            offered: special().project(vec![(0, "id"), (1, "amount")]).build(),
            expected: remote(error(ErrorKind::Data)),
        },
        ReadCase {
            name: "time 24:00:00 cannot be carried",
            offered: special().project(vec![(0, "id"), (2, "clock")]).build(),
            expected: remote(error(ErrorKind::Data)),
        },
        ReadCase {
            name: "values that cannot be carried do not fail a read that skips them",
            offered: special()
                .filter(call("equal", vec![column(0), int(3)]))
                .build(),
            expected: remote(rows(vec![
                Arc::new(Int32Array::from(vec![3])),
                decimal_array(vec![Some(-1)], 10, 2),
                Arc::new(Time64MicrosecondArray::from(vec![0])),
            ])),
        },
        ReadCase {
            name: "a timestamp beyond the Arrow range cannot be carried",
            offered: ReadPlan::new(
                [SCHEMA, "beyond"],
                vec![("id", i32(false)), ("stamp", timestamp(true))],
            )
            .build(),
            expected: remote(error(ErrorKind::Data)),
        },
        ReadCase {
            name: "an interval whose time does not fit in nanoseconds cannot be carried",
            offered: ReadPlan::new(
                [SCHEMA, "long_span"],
                vec![("id", i32(false)), ("span", interval(true))],
            )
            .build(),
            expected: ExpectedPlanning::Planned {
                local_work: false,
                result: Expected::KnownDeviation {
                    contract: error(ErrorKind::Data),
                    observed: error(ErrorKind::DataSource),
                    reason: "the ADBC driver fails the read before the connector sees the value",
                },
            },
        },
    ];
    // `infinity` and `-infinity` of each type.
    for (name, column, ty) in [
        ("an infinite date cannot be carried", "day", date(true)),
        (
            "an infinite timestamp cannot be carried",
            "stamp",
            timestamp(true),
        ),
        (
            "an infinite timestamptz cannot be carried",
            "instant",
            timestamp_tz(true),
        ),
    ] {
        cases.push(ReadCase {
            name,
            offered: ReadPlan::new([SCHEMA, "infinite"], vec![("id", i32(false)), (column, ty)])
                .build(),
            expected: remote(error(ErrorKind::Data)),
        });
    }
    cases.extend(filter_cases());
    cases
}

/// A filter that runs remotely returns only its rows; one that stays local
/// leaves the whole table to the remote result.
fn filter_cases() -> Vec<ReadCase> {
    let remote_ids = |ids: Vec<i32>| remote(rows(vec![Arc::new(Int32Array::from(ids))]));
    let local = || ExpectedPlanning::Planned {
        local_work: true,
        result: Expected::Contract(rows(filtering_rows())),
    };
    let case = |name, filter, expected| ReadCase {
        name,
        offered: filtering(filter),
        expected,
    };
    vec![
        case(
            "integer comparison runs remotely",
            call("gt", vec![column(0), int(1)]),
            remote_ids(vec![2, 3]),
        ),
        case(
            "text equality on text runs remotely",
            call("equal", vec![column(1), string("a")]),
            remote_ids(vec![1]),
        ),
        case(
            "UUID equality runs remotely",
            call(
                "equal",
                vec![
                    column(5),
                    logical_text("uuid", "{}", "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
                ],
            ),
            remote_ids(vec![1]),
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
        case(
            "text ordering depends on the collation",
            call("lt", vec![column(1), string("b")]),
            local(),
        ),
        case(
            "character(n) comparison ignores trailing spaces",
            call("equal", vec![column(2), string("a")]),
            local(),
        ),
        case(
            "a nondeterministic collation ignores letter case",
            call("equal", vec![column(3), string("a")]),
            local(),
        ),
        case(
            "floating-point comparison differs for NaN",
            call("gt", vec![column(4), literal(LiteralType::Fp64(1.0))]),
            local(),
        ),
        case(
            "text equality between columns of one collation runs remotely",
            call("equal", vec![column(6), column(6)]),
            remote_ids(vec![1, 2, 3]),
        ),
        case(
            "text equality between columns of different collations",
            call("equal", vec![column(6), column(7)]),
            local(),
        ),
        case(
            "a date literal before PostgreSQL's range",
            call("gt", vec![column(8), literal(LiteralType::Date(i32::MIN))]),
            local(),
        ),
    ]
}

pub fn drift_cases<'a>() -> Vec<DriftCase<'a>> {
    let two_columns = |table: &'static str| {
        ReadPlan::new(
            [SCHEMA, table],
            vec![("id", i32(false)), ("value", i32(false))],
        )
        .build()
    };
    let change = |sql: &'static str| -> Box<dyn Fn() + 'a> {
        Box::new(move || {
            server().psql(sql);
        })
    };
    vec![
        DriftCase {
            name: "a changed column type",
            offered: two_columns("retyped"),
            change: change("ALTER TABLE conformance.retyped ALTER COLUMN value TYPE bigint"),
            expected: DriftOutcome::StaleRegistration,
        },
        DriftCase {
            name: "a dropped column",
            offered: two_columns("narrowed"),
            change: change("ALTER TABLE conformance.narrowed DROP COLUMN value"),
            expected: DriftOutcome::StaleRegistration,
        },
        DriftCase {
            name: "a dropped table",
            offered: ReadPlan::new([SCHEMA, "removed"], vec![("id", i32(false))]).build(),
            change: change("DROP TABLE conformance.removed"),
            expected: DriftOutcome::StaleRegistration,
        },
        DriftCase {
            name: "a NOT NULL column made nullable",
            offered: two_columns("relaxed"),
            change: change("ALTER TABLE conformance.relaxed ALTER COLUMN value DROP NOT NULL"),
            expected: DriftOutcome::StaleRegistration,
        },
        DriftCase {
            name: "a nullable column made NOT NULL",
            offered: ReadPlan::new(
                [SCHEMA, "tightened"],
                vec![("id", i32(false)), ("value", i32(true))],
            )
            .build(),
            change: change("ALTER TABLE conformance.tightened ALTER COLUMN value SET NOT NULL"),
            expected: DriftOutcome::Reads,
        },
    ]
}

pub fn error_cases() -> Vec<ErrorCase> {
    let mut wrong_password = options(READER);
    for (key, value) in &mut wrong_password {
        if key == "password" {
            *value = connector_sdk::SecretString::new("conformance-wrong-secret".to_owned());
        }
    }
    vec![
        ErrorCase {
            name: "reading a table without privilege",
            options: options(OUTSIDER),
            request: FailingRequest::Read(Box::new(
                ReadPlan::new(
                    [SCHEMA, "hidden"],
                    vec![("id", i32(false)), ("label", text(true))],
                )
                .filter(call(
                    "equal",
                    vec![column(1), string("conformance-literal")],
                ))
                .build(),
            )),
            expected: Expected::Contract(ExpectedError {
                kind: ErrorKind::Permission,
                retryable: false,
            }),
        },
        ErrorCase {
            name: "a rejected password",
            options: wrong_password,
            request: FailingRequest::OpenSession,
            expected: Expected::KnownDeviation {
                contract: ExpectedError {
                    kind: ErrorKind::Authentication,
                    retryable: false,
                },
                observed: ExpectedError {
                    kind: ErrorKind::Connection,
                    retryable: false,
                },
                reason: "libpq does not expose the SQLSTATE of a failed connection, so a rejected credential cannot be told apart from an unreachable server",
            },
        },
    ]
}

fn remote(result: ReadResult) -> ExpectedPlanning {
    ExpectedPlanning::Planned {
        local_work: false,
        result: Expected::Contract(result),
    }
}

fn rows(columns: Vec<ArrayRef>) -> ReadResult {
    ReadResult::Rows(columns)
}

fn error(kind: ErrorKind) -> ReadResult {
    ReadResult::Error(ExpectedError {
        kind,
        retryable: false,
    })
}

fn int(value: i32) -> Expression {
    literal(LiteralType::I32(value))
}

fn string(value: &str) -> Expression {
    literal(LiteralType::String(value.to_owned()))
}

/// A decimal array in the Analytics storage of `Decimal(precision, scale)`,
/// the narrowest Arrow decimal that holds the precision.
fn decimal_array(values: Vec<Option<i128>>, precision: u8, scale: i8) -> ArrayRef {
    match precision {
        ..=9 => Arc::new(
            Decimal32Array::from_iter(
                values
                    .iter()
                    .map(|value| value.map(|value| i32::try_from(value).unwrap())),
            )
            .with_precision_and_scale(precision, scale)
            .unwrap(),
        ),
        10..=18 => Arc::new(
            Decimal64Array::from_iter(
                values
                    .iter()
                    .map(|value| value.map(|value| i64::try_from(value).unwrap())),
            )
            .with_precision_and_scale(precision, scale)
            .unwrap(),
        ),
        _ => Arc::new(
            Decimal128Array::from(values)
                .with_precision_and_scale(precision, scale)
                .unwrap(),
        ),
    }
}

/// A `Decimal256` array of decimal text without a point, such as `-12345`.
fn decimal256_array(values: Vec<Option<String>>, precision: u8, scale: i8) -> ArrayRef {
    Arc::new(
        Decimal256Array::from(
            values
                .into_iter()
                .map(|value| value.map(|value| i256::from_string(&value).unwrap()))
                .collect::<Vec<_>>(),
        )
        .with_precision_and_scale(precision, scale)
        .unwrap(),
    )
}

fn value_types() -> ReadPlan {
    ReadPlan::new(
        [SCHEMA, "value_types"],
        vec![
            ("id", i32(false)),
            ("flag", boolean(true)),
            ("small", i16(true)),
            ("regular", i32(true)),
            ("big", i64(true)),
            ("single", fp32(true)),
            ("ratio", fp64(true)),
            ("exact", decimal(38, 4, true)),
            ("free_text", text(true)),
            ("bounded", text(true)),
            ("padded", text(true)),
            ("bytes", binary(true)),
            ("day", date(true)),
            ("clock", time(true)),
            ("stamp", timestamp(true)),
            ("instant", timestamp_tz(true)),
            ("span", interval(true)),
            ("external_id", logical("uuid", "{}", true)),
            ("document", logical("json", "{}", true)),
            ("binary_document", logical("json", "{}", true)),
            (
                "feeling",
                logical("enum", r#"{"members":["sad","ok","happy"]}"#, true),
            ),
            ("zoned", logical("timetz", "{}", true)),
            ("count", i32(true)),
            ("narrow", decimal(9, 2, true)),
            (
                "widest",
                logical("decimal", r#"{"precision":76,"scale":10}"#, true),
            ),
        ],
    )
}

/// The three rows of `value_types`: maxima, NULL, and minima.
fn value_type_rows() -> Vec<ArrayRef> {
    let nines = 99_999_999_999_999_999_999_999_999_999_999_999_999_i128;
    let text =
        |values: [Option<&str>; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    vec![
        Arc::new(Int32Array::from(vec![1, 2, 3])),
        Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])),
        Arc::new(Int16Array::from(vec![Some(i16::MAX), None, Some(i16::MIN)])),
        Arc::new(Int32Array::from(vec![Some(i32::MAX), None, Some(i32::MIN)])),
        Arc::new(Int64Array::from(vec![Some(i64::MIN), None, Some(i64::MAX)])),
        Arc::new(Float32Array::from(vec![Some(0.5), None, Some(f32::MIN)])),
        Arc::new(Float64Array::from(vec![Some(0.25), None, Some(f64::MAX)])),
        decimal_array(vec![Some(nines), None, Some(-nines)], 38, 4),
        text([Some("héllo"), None, Some("")]),
        text([Some("bounded"), None, Some("ten chars.")]),
        text([Some("ab   "), None, Some("abcde")]),
        Arc::new(BinaryArray::from(vec![
            Some([0x00, 0xff].as_slice()),
            None,
            Some([].as_slice()),
        ])),
        // 1 BC is year 0 of the proleptic Gregorian calendar.
        Arc::new(Date32Array::from(vec![
            Some(-719_528),
            None,
            Some(2_145_042_905),
        ])),
        Arc::new(Time64MicrosecondArray::from(vec![
            Some(86_399_999_999),
            None,
            Some(0),
        ])),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(1),
            None,
            Some(i64::MAX),
        ])),
        Arc::new(
            TimestampMicrosecondArray::from(vec![
                Some(1_577_804_400_000_000),
                None,
                Some(1_577_865_600_000_000),
            ])
            .with_timezone("UTC"),
        ),
        Arc::new(IntervalMonthDayNanoArray::from(vec![
            Some(IntervalMonthDayNano::new(14, 3, 14_706_789_000_000)),
            None,
            Some(IntervalMonthDayNano::new(-12, -2, -1_000_000_000)),
        ])),
        text([
            Some("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            None,
            Some("00000000-0000-0000-0000-000000000000"),
        ]),
        text([Some(r#"{"a": 1}"#), None, Some("[]")]),
        text([Some(r#"{"b": [1, 2]}"#), None, Some("{}")]),
        text([Some("ok"), None, Some("sad")]),
        text([Some("10:00:00.25+05:30"), None, Some("00:00:00+00")]),
        Arc::new(Int32Array::from(vec![Some(1), None, Some(i32::MAX)])),
        decimal_array(vec![Some(999_999_999), None, Some(-999_999_999)], 9, 2),
        decimal256_array(
            vec![
                Some("9".repeat(76)),
                None,
                Some(format!("-{}", "9".repeat(76))),
            ],
            76,
            10,
        ),
    ]
}

fn special() -> ReadPlan {
    ReadPlan::new(
        [SCHEMA, "special"],
        vec![
            ("id", i32(false)),
            ("amount", decimal(10, 2, true)),
            ("clock", time(true)),
        ],
    )
}

fn filtering(filter: Expression) -> Plan {
    ReadPlan::new(
        [SCHEMA, "filtering"],
        vec![
            ("id", i32(false)),
            ("label", text(true)),
            ("code", text(true)),
            ("folded", text(true)),
            ("score", fp64(true)),
            ("external_id", logical("uuid", "{}", true)),
            ("c_label", text(true)),
            ("posix_label", text(true)),
            ("day", date(true)),
        ],
    )
    .filter(filter)
    .project(vec![(0, "id")])
    .build()
}

/// Every row of `filtering`, as a remote result that leaves the filter local.
fn filtering_rows() -> Vec<ArrayRef> {
    let text = |values: [&str; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    vec![
        Arc::new(Int32Array::from(vec![1, 2, 3])),
        text(["a", "b", "a "]),
        text(["a  ", "b  ", "a  "]),
        text(["A", "b", "a"]),
        Arc::new(Float64Array::from(vec![0.5, f64::NAN, 1.5])),
        Arc::new(StringArray::from(vec![
            Some("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            None,
            None,
        ])),
        text(["a", "b", "a"]),
        text(["a", "c", "b"]),
        Arc::new(Date32Array::from(vec![Some(18_262), None, Some(18_628)])),
    ]
}
