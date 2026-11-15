//! The reads, source changes, and failing requests the contract expects of
//! the fixture's objects.

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal32Array, Decimal64Array,
    Decimal128Array, Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, StringArray,
    Time64MicrosecondArray, TimestampMicrosecondArray, UInt8Array,
};
use connector_sdk::{
    ErrorKind, SecretString,
    conformance::{
        DriftCase, DriftOutcome, ErrorCase, Expected, ExpectedError, ExpectedPlanning,
        FailingRequest, ReadCase, ReadResult, plan::*,
    },
};
use substrait::proto::{
    Expression, Plan,
    expression::literal::{self, LiteralType},
};

use super::{OUTSIDER, READER, SCHEMA, options, sqlcmd};

pub fn read_cases() -> Vec<ReadCase> {
    let mut cases = vec![
        ReadCase {
            name: "every covered rule reads its boundary values and NULL",
            offered: value_types().build(),
            expected: remote(rows(value_type_rows())),
        },
        ReadCase {
            name: "a datetime of a third of a millisecond cannot be carried",
            offered: ReadPlan::new(
                [SCHEMA, "value_types"],
                vec![("id", i32(false)), ("legacy", timestamp(true))],
            )
            .build(),
            expected: ExpectedPlanning::Planned {
                local_work: false,
                result: Expected::KnownDeviation {
                    contract: error(ErrorKind::Data),
                    observed: rows(vec![
                        Arc::new(Int32Array::from(vec![1, 2, 3])),
                        Arc::new(TimestampMicrosecondArray::from(vec![
                            Some(253_402_300_799_997_000),
                            None,
                            Some(-6_847_804_800_000_000),
                        ])),
                    ]),
                    reason: "the ODBC driver returns datetime values rounded to the millisecond",
                },
            },
        },
    ];
    for (name, column, ty) in [
        (
            "a time with a fraction of a microsecond cannot be carried",
            "clock",
            time(true),
        ),
        (
            "a datetime2 with a fraction of a microsecond cannot be carried",
            "stamp",
            timestamp(true),
        ),
        (
            "a datetimeoffset with a fraction of a microsecond cannot be carried",
            "instant",
            timestamp_tz(true),
        ),
    ] {
        cases.push(ReadCase {
            name,
            offered: ReadPlan::new(
                [SCHEMA, "fractions"],
                vec![("id", i32(false)), (column, ty)],
            )
            .build(),
            expected: remote(error(ErrorKind::Data)),
        });
    }
    cases.push(ReadCase {
        name: "values that cannot be carried do not fail a read that skips them",
        offered: ReadPlan::new(
            [SCHEMA, "fractions"],
            vec![
                ("id", i32(false)),
                ("clock", time(true)),
                ("stamp", timestamp(true)),
                ("instant", timestamp_tz(true)),
            ],
        )
        .filter(call("equal", vec![column(0), int(2)]))
        .build(),
        expected: remote(rows(vec![
            Arc::new(Int32Array::from(vec![2])),
            Arc::new(Time64MicrosecondArray::from(vec![1])),
            Arc::new(TimestampMicrosecondArray::from(vec![1_577_836_800_000_001])),
            Arc::new(
                TimestampMicrosecondArray::from(vec![1_577_836_800_000_001]).with_timezone("UTC"),
            ),
        ])),
    });
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
            "UUID equality runs remotely",
            call(
                "equal",
                vec![
                    column(3),
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
                    call("is_null", vec![column(3)]),
                    call("not", vec![call("equal", vec![column(0), int(1)])]),
                ],
            ),
            remote_ids(vec![2, 3]),
        ),
        case(
            "datetime2 comparison runs remotely",
            call(
                "gte",
                vec![column(4), timestamp_literal(1_609_459_200_000_000)],
            ),
            remote_ids(vec![2]),
        ),
        case(
            "bit equality runs remotely",
            call(
                "equal",
                vec![column(6), literal(LiteralType::Boolean(true))],
            ),
            remote_ids(vec![1]),
        ),
        case(
            "tinyint comparison runs remotely",
            call(
                "gt",
                vec![
                    column(7),
                    logical_literal("unsigned_byte", "{}", LiteralType::I16(100)),
                ],
            ),
            remote_ids(vec![2, 3]),
        ),
        case(
            "decimal comparison runs remotely",
            call("gt", vec![column(8), decimal_literal(0, 10, 2)]),
            remote_ids(vec![1, 2]),
        ),
        case(
            "date comparison runs remotely",
            call("lt", vec![column(9), literal(LiteralType::Date(18_628))]),
            remote_ids(vec![1]),
        ),
        case(
            "text equality ignores trailing spaces at SQL Server",
            call("equal", vec![column(1), string("a")]),
            local(),
        ),
        case(
            "floating-point comparison is not yet evaluated remotely",
            call("gt", vec![column(2), literal(LiteralType::Fp64(1.0))]),
            local(),
        ),
        case(
            "datetime is compared at SQL Server in 1/300 seconds",
            call(
                "equal",
                vec![column(5), timestamp_literal(1_577_836_800_003_000)],
            ),
            local(),
        ),
        case(
            "a bare bit column is no condition at SQL Server",
            column(6),
            local(),
        ),
        case(
            "a date literal before SQL Server's range",
            call("gt", vec![column(9), literal(LiteralType::Date(i32::MIN))]),
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
            sqlcmd(sql);
        })
    };
    vec![
        DriftCase {
            name: "a changed column type",
            offered: two_columns("retyped"),
            change: change("ALTER TABLE conformance.retyped ALTER COLUMN value bigint NOT NULL"),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a dropped column",
            offered: two_columns("narrowed"),
            change: change("ALTER TABLE conformance.narrowed DROP COLUMN value"),
            expected: DriftOutcome::Fails(ErrorKind::DoesNotExist),
        },
        DriftCase {
            name: "a dropped table",
            offered: ReadPlan::new([SCHEMA, "removed"], vec![("id", i32(false))]).build(),
            change: change("DROP TABLE conformance.removed"),
            expected: DriftOutcome::Fails(ErrorKind::DoesNotExist),
        },
        DriftCase {
            name: "a NOT NULL column made nullable",
            offered: two_columns("relaxed"),
            change: change("ALTER TABLE conformance.relaxed ALTER COLUMN value int NULL"),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a nullable column made NOT NULL",
            offered: ReadPlan::new(
                [SCHEMA, "tightened"],
                vec![("id", i32(false)), ("value", i32(true))],
            )
            .build(),
            change: change("ALTER TABLE conformance.tightened ALTER COLUMN value int NOT NULL"),
            expected: DriftOutcome::Reads,
        },
    ]
}

pub fn error_cases() -> Vec<ErrorCase> {
    let mut wrong_password = options(READER);
    for (key, value) in &mut wrong_password {
        if key == "password" {
            *value = SecretString::new("Conformance-wrong-secret1".to_owned());
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
                kind: ErrorKind::PermissionDenied,
                retryable: false,
            }),
        },
        ErrorCase {
            name: "a rejected password",
            options: wrong_password,
            request: FailingRequest::OpenSession,
            expected: Expected::Contract(ExpectedError {
                kind: ErrorKind::Authentication,
                retryable: false,
            }),
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

fn timestamp_literal(micros: i64) -> Expression {
    literal(LiteralType::PrecisionTimestamp(
        literal::PrecisionTimestamp {
            precision: 6,
            value: micros,
        },
    ))
}

fn decimal_literal(unscaled: i128, precision: i32, scale: i32) -> Expression {
    literal(LiteralType::Decimal(literal::Decimal {
        value: unscaled.to_le_bytes().to_vec(),
        precision,
        scale,
    }))
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

fn value_types() -> ReadPlan {
    ReadPlan::new(
        [SCHEMA, "value_types"],
        vec![
            ("id", i32(false)),
            ("flag", boolean(true)),
            ("tiny", logical("unsigned_byte", "{}", true)),
            ("small", i16(true)),
            ("regular", i32(true)),
            ("big", i64(true)),
            ("single", fp32(true)),
            ("ratio", fp64(true)),
            ("exact", decimal(38, 4, true)),
            ("narrow", decimal(9, 2, true)),
            ("cash", decimal(19, 4, true)),
            ("small_cash", decimal(10, 4, true)),
            ("fixed", text(true)),
            ("bounded", text(true)),
            ("unicode_fixed", text(true)),
            ("wide", text(true)),
            ("bytes", binary(true)),
            ("varbytes", binary(true)),
            ("day", date(true)),
            ("clock", time(true)),
            ("stamp", timestamp(true)),
            ("stamp3", timestamp(true)),
            ("small_stamp", timestamp(true)),
            ("instant", timestamp_tz(true)),
            ("external_id", logical("uuid", "{}", true)),
            ("code", text(true)),
            ("label", text(true)),
        ],
    )
}

/// The three rows of `value_types`: maxima, NULL, and minima.
fn value_type_rows() -> Vec<ArrayRef> {
    let nines = 99_999_999_999_999_999_999_999_999_999_999_999_999_i128;
    let text =
        |values: [Option<&str>; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    let timestamps = |values: [Option<i64>; 3]| -> ArrayRef {
        Arc::new(TimestampMicrosecondArray::from(values.to_vec()))
    };
    let first_day = -62_135_596_800_000_000;
    vec![
        Arc::new(Int32Array::from(vec![1, 2, 3])),
        Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])),
        Arc::new(UInt8Array::from(vec![Some(255), None, Some(0)])),
        Arc::new(Int16Array::from(vec![Some(i16::MAX), None, Some(i16::MIN)])),
        Arc::new(Int32Array::from(vec![Some(i32::MAX), None, Some(i32::MIN)])),
        Arc::new(Int64Array::from(vec![Some(i64::MIN), None, Some(i64::MAX)])),
        Arc::new(Float32Array::from(vec![Some(0.5), None, Some(f32::MIN)])),
        Arc::new(Float64Array::from(vec![Some(0.25), None, Some(f64::MAX)])),
        decimal_array(vec![Some(nines), None, Some(-nines)], 38, 4),
        decimal_array(vec![Some(999_999_999), None, Some(-999_999_999)], 9, 2),
        decimal_array(
            vec![Some(i128::from(i64::MAX)), None, Some(i128::from(i64::MIN))],
            19,
            4,
        ),
        decimal_array(
            vec![Some(i128::from(i32::MAX)), None, Some(i128::from(i32::MIN))],
            10,
            4,
        ),
        text([Some("ab   "), None, Some("abcde")]),
        text([Some("bounded"), None, Some("")]),
        text([Some("é    "), None, Some("abcde")]),
        text([Some("héllo 😀"), None, Some("")]),
        Arc::new(BinaryArray::from(vec![
            Some([0x00, 0xff].as_slice()),
            None,
            Some([0x01, 0x02].as_slice()),
        ])),
        Arc::new(BinaryArray::from(vec![
            Some([0x00, 0xff].as_slice()),
            None,
            Some([].as_slice()),
        ])),
        Arc::new(Date32Array::from(vec![
            Some(2_932_896),
            None,
            Some(-719_162),
        ])),
        Arc::new(Time64MicrosecondArray::from(vec![
            Some(86_399_999_999),
            None,
            Some(0),
        ])),
        timestamps([Some(253_402_300_799_999_999), None, Some(first_day)]),
        timestamps([Some(253_402_300_799_999_000), None, Some(first_day)]),
        timestamps([
            Some(3_453_321_540_000_000),
            None,
            Some(-2_208_988_800_000_000),
        ]),
        Arc::new(
            TimestampMicrosecondArray::from(vec![
                Some(1_577_836_800_000_001),
                None,
                Some(first_day),
            ])
            .with_timezone("UTC"),
        ),
        text([
            Some("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            None,
            Some("00000000-0000-0000-0000-000000000000"),
        ]),
        text([Some("X1"), None, Some("")]),
        text([Some("name"), None, Some("")]),
    ]
}

fn filtering(filter: Expression) -> Plan {
    ReadPlan::new(
        [SCHEMA, "filtering"],
        vec![
            ("id", i32(false)),
            ("label", text(true)),
            ("score", fp64(true)),
            ("external_id", logical("uuid", "{}", true)),
            ("stamp", timestamp(true)),
            ("legacy", timestamp(true)),
            ("flag", boolean(true)),
            ("tiny", logical("unsigned_byte", "{}", true)),
            ("amount", decimal(10, 2, true)),
            ("day", date(true)),
        ],
    )
    .filter(filter)
    .project(vec![(0, "id")])
    .build()
}

/// Every row of `filtering`, as a remote result that leaves the filter local.
fn filtering_rows() -> Vec<ArrayRef> {
    vec![
        Arc::new(Int32Array::from(vec![1, 2, 3])),
        Arc::new(StringArray::from(vec!["a", "b", "a "])),
        Arc::new(Float64Array::from(vec![Some(0.5), None, Some(1.5)])),
        Arc::new(StringArray::from(vec![
            Some("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            None,
            None,
        ])),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(1_577_836_800_000_000),
            Some(1_609_459_200_000_000),
            None,
        ])),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(1_577_836_800_003_000),
            Some(1_609_459_200_000_000),
            None,
        ])),
        Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
        Arc::new(UInt8Array::from(vec![10, 200, 255])),
        decimal_array(vec![Some(150), Some(250), Some(-100)], 10, 2),
        Arc::new(Date32Array::from(vec![Some(18_262), None, Some(18_628)])),
    ]
}
