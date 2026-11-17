//! The reads, source changes, and failing requests the contract expects of
//! the fixture's objects.

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Decimal32Array, Decimal64Array, Decimal128Array,
    Float32Array, Float64Array, StringArray, TimestampMicrosecondArray,
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

use super::{OUTSIDER, READER, SCHEMA, options, server};

/// 0001-01-01 00:00:00, in microseconds from the Unix epoch.
const FIRST_DAY: i64 = -62_135_596_800_000_000;

/// 1582-10-10 00:00:00, which Oracle's calendar skips from the Julian to the
/// Gregorian calendar, in microseconds from the Unix epoch.
const SKIPPED_DAY: i64 = -12_219_724_800_000_000;

/// Oracle's earliest date, -4712-01-01 00:00:00, as the proleptic Gregorian
/// date of the same fields, in microseconds from the Unix epoch.
const EARLIEST: i64 = -210_863_520_000_000_000;

pub fn read_cases() -> Vec<ReadCase> {
    let mut cases = vec![
        ReadCase {
            name: "every covered rule reads its boundary values and NULL",
            offered: value_types().build(),
            expected: remote(rows(value_type_rows())),
        },
        ReadCase {
            name: "a name beyond ASCII reaches Oracle unchanged",
            offered: ReadPlan::new([SCHEMA, "Ünï😀"], vec![("çol", decimal(10, 0, false))])
                .filter(call("equal", vec![column(0), decimal_literal(7, 10, 0)]))
                .build(),
            expected: remote(rows(vec![decimal_array(vec![Some(7)], 10, 0)])),
        },
    ];
    cases.extend(filter_cases());
    cases
}

/// A filter that runs remotely returns only its rows; one that stays local
/// leaves the whole table to the remote result.
fn filter_cases() -> Vec<ReadCase> {
    let remote_ids = |ids: Vec<i128>| {
        remote(rows(vec![decimal_array(
            ids.into_iter().map(Some).collect(),
            10,
            0,
        )]))
    };
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
            "NUMBER comparison runs remotely",
            call("gt", vec![column(0), decimal_literal(1, 10, 0)]),
            remote_ids(vec![2, 3]),
        ),
        case(
            "NUMBER with a scale compares remotely by its exact value",
            call("gt", vec![column(4), decimal_literal(0, 10, 2)]),
            remote_ids(vec![1, 2]),
        ),
        case(
            "TIMESTAMP comparison runs remotely",
            call(
                "gte",
                vec![column(5), timestamp_literal(1_609_459_200_000_000)],
            ),
            remote_ids(vec![2]),
        ),
        case(
            "DATE comparison runs remotely",
            call(
                "lt",
                vec![column(6), timestamp_literal(1_590_969_600_000_000)],
            ),
            remote_ids(vec![1]),
        ),
        case(
            "TIMESTAMP WITH TIME ZONE compares remotely by instant",
            call(
                "equal",
                vec![column(7), timestamp_tz_literal(1_577_836_800_000_000)],
            ),
            remote_ids(vec![1]),
        ),
        case(
            "Boolean composition and null tests run remotely",
            call(
                "or",
                vec![
                    call("is_null", vec![column(5)]),
                    call(
                        "not",
                        vec![call("equal", vec![column(0), decimal_literal(1, 10, 0)])],
                    ),
                ],
            ),
            remote_ids(vec![2, 3]),
        ),
        case(
            "NVARCHAR2 equality stays local",
            call("equal", vec![column(1), string("a")]),
            local(),
        ),
        case(
            "CHAR equality ignores trailing spaces at Oracle",
            call("equal", vec![column(2), string("a")]),
            local(),
        ),
        case(
            "floating-point comparison is not yet evaluated remotely",
            call("gt", vec![column(3), literal(LiteralType::Fp64(1.0))]),
            local(),
        ),
        case(
            "a bare BOOLEAN column is not evaluated remotely",
            column(8),
            local(),
        ),
        case(
            "a timestamp literal before year 1",
            call("gt", vec![column(5), timestamp_literal(FIRST_DAY - 1)]),
            local(),
        ),
        case(
            "a timestamp literal on a day Oracle's calendar skips",
            call("gt", vec![column(5), timestamp_literal(SKIPPED_DAY)]),
            local(),
        ),
    ]
}

pub fn drift_cases<'a>() -> Vec<DriftCase<'a>> {
    let two_columns = |table: &'static str| {
        ReadPlan::new(
            [SCHEMA, table],
            vec![
                ("ID", decimal(10, 0, false)),
                ("VALUE", decimal(10, 0, false)),
            ],
        )
        .build()
    };
    let change = |sql: &'static str| -> Box<dyn Fn() + 'a> {
        Box::new(move || {
            server().sqlplus(sql);
        })
    };
    vec![
        DriftCase {
            name: "a changed column type",
            offered: two_columns("RETYPED"),
            change: change("ALTER TABLE conformance.retyped MODIFY (value NUMBER(19));"),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a dropped column",
            offered: two_columns("NARROWED"),
            change: change("ALTER TABLE conformance.narrowed DROP COLUMN value;"),
            expected: DriftOutcome::Fails(ErrorKind::DoesNotExist),
        },
        DriftCase {
            name: "a dropped table, which Oracle reports as a hidden one",
            offered: ReadPlan::new([SCHEMA, "REMOVED"], vec![("ID", decimal(10, 0, false))])
                .build(),
            change: change("DROP TABLE conformance.removed PURGE;"),
            expected: DriftOutcome::Fails(ErrorKind::NotFound),
        },
        DriftCase {
            name: "a NOT NULL column made nullable",
            offered: two_columns("RELAXED"),
            change: change("ALTER TABLE conformance.relaxed MODIFY (value NULL);"),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a nullable column made NOT NULL",
            offered: ReadPlan::new(
                [SCHEMA, "TIGHTENED"],
                vec![
                    ("ID", decimal(10, 0, false)),
                    ("VALUE", decimal(10, 0, true)),
                ],
            )
            .build(),
            change: change("ALTER TABLE conformance.tightened MODIFY (value NOT NULL);"),
            expected: DriftOutcome::Reads,
        },
    ]
}

pub fn error_cases() -> Vec<ErrorCase> {
    let with_password = |password: &str| {
        let mut options = options(READER);
        for (key, value) in &mut options {
            if key == "password" {
                *value = SecretString::new(password.to_owned());
            }
        }
        options
    };
    vec![
        ErrorCase {
            name: "reading a table without SELECT",
            options: options(OUTSIDER),
            request: FailingRequest::Read(Box::new(
                ReadPlan::new(
                    [SCHEMA, "HIDDEN"],
                    vec![("ID", decimal(10, 0, false)), ("LABEL", text(true))],
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
            name: "reading a table the credential cannot see",
            options: options(OUTSIDER),
            request: FailingRequest::Read(Box::new(
                ReadPlan::new([SCHEMA, "INVISIBLE"], vec![("ID", decimal(10, 0, false))]).build(),
            )),
            expected: Expected::Contract(ExpectedError {
                kind: ErrorKind::NotFound,
                retryable: false,
            }),
        },
        ErrorCase {
            name: "a rejected password",
            options: with_password("Conformance-wrong-secret1"),
            request: FailingRequest::OpenSession,
            expected: Expected::Contract(ExpectedError {
                kind: ErrorKind::Authentication,
                retryable: false,
            }),
        },
        ErrorCase {
            name: "a password the ODBC driver cannot take",
            options: with_password("Conformance;secret1"),
            request: FailingRequest::OpenSession,
            expected: Expected::Contract(ExpectedError {
                kind: ErrorKind::InvalidSettings,
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

fn timestamp_tz_literal(micros: i64) -> Expression {
    literal(LiteralType::PrecisionTimestampTz(
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
        [SCHEMA, "VALUE_TYPES"],
        vec![
            ("ID", decimal(10, 0, false)),
            ("WHOLE", decimal(38, 0, true)),
            ("FRACTION", decimal(38, 38, true)),
            ("SMALL", decimal(5, 2, true)),
            ("MIDDLE", decimal(18, 4, true)),
            ("SINGLE", fp32(true)),
            ("RATIO", fp64(true)),
            ("FLAG", boolean(true)),
            ("FIXED", text(true)),
            ("UNICODE_FIXED", text(true)),
            ("BOUNDED", text(true)),
            ("WIDE", text(true)),
            ("BYTES", binary(true)),
            ("DAY", timestamp(true)),
            ("STAMP", timestamp(true)),
            ("STAMP3", timestamp(true)),
            ("STAMP0", timestamp(true)),
            ("INSTANT", timestamp_tz(true)),
            ("LOCAL_INSTANT", timestamp_tz(true)),
        ],
    )
}

/// The three rows of `VALUE_TYPES`: maxima, NULL, and minima.
fn value_type_rows() -> Vec<ArrayRef> {
    let nines = 99_999_999_999_999_999_999_999_999_999_999_999_999_i128;
    let text =
        |values: [Option<&str>; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    let timestamps = |values: [Option<i64>; 3]| -> ArrayRef {
        Arc::new(TimestampMicrosecondArray::from(values.to_vec()))
    };
    let instants = |values: [Option<i64>; 3]| -> ArrayRef {
        Arc::new(TimestampMicrosecondArray::from(values.to_vec()).with_timezone("UTC"))
    };
    let last_second = 253_402_300_799_000_000;
    vec![
        decimal_array(vec![Some(1), Some(2), Some(3)], 10, 0),
        decimal_array(vec![Some(nines), None, Some(-nines)], 38, 0),
        decimal_array(vec![Some(nines), None, Some(-nines)], 38, 38),
        decimal_array(vec![Some(99_999), None, Some(-99_999)], 5, 2),
        decimal_array(
            vec![
                Some(999_999_999_999_999_999),
                None,
                Some(-999_999_999_999_999_999),
            ],
            18,
            4,
        ),
        Arc::new(Float32Array::from(vec![
            Some(f32::MAX),
            None,
            Some(f32::MIN),
        ])),
        Arc::new(Float64Array::from(vec![
            Some(f64::INFINITY),
            None,
            Some(f64::MIN),
        ])),
        Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])),
        text([Some("ab   "), None, Some("abcde")]),
        text([Some("é    "), None, Some("abcde")]),
        text([Some("bounded"), None, Some("a")]),
        text([Some("héllo 😀"), None, Some("a")]),
        Arc::new(BinaryArray::from(vec![
            Some([0x00, 0xff].as_slice()),
            None,
            Some([0x01, 0x02].as_slice()),
        ])),
        timestamps([Some(last_second), None, Some(EARLIEST)]),
        timestamps([Some(last_second + 999_999), None, Some(EARLIEST)]),
        timestamps([Some(last_second + 999_000), None, Some(EARLIEST)]),
        timestamps([Some(last_second), None, Some(EARLIEST)]),
        instants([
            Some(1_577_836_800_000_001),
            None,
            Some(1_593_619_200_000_000),
        ]),
        instants([Some(1_577_836_800_000_001), None, Some(FIRST_DAY)]),
    ]
}

fn filtering(filter: Expression) -> Plan {
    ReadPlan::new(
        [SCHEMA, "FILTERING"],
        vec![
            ("ID", decimal(10, 0, false)),
            ("LABEL", text(true)),
            ("PADDED", text(true)),
            ("SCORE", fp64(true)),
            ("AMOUNT", decimal(10, 2, true)),
            ("STAMP", timestamp(true)),
            ("DAY", timestamp(true)),
            ("INSTANT", timestamp_tz(true)),
            ("FLAG", boolean(true)),
        ],
    )
    .filter(filter)
    .project(vec![(0, "ID")])
    .build()
}

/// Every row of `FILTERING`, as a remote result that leaves the filter local.
fn filtering_rows() -> Vec<ArrayRef> {
    vec![
        decimal_array(vec![Some(1), Some(2), Some(3)], 10, 0),
        Arc::new(StringArray::from(vec!["a", "b", "a "])),
        Arc::new(StringArray::from(vec!["a  ", "b  ", "a  "])),
        Arc::new(Float64Array::from(vec![Some(0.5), None, Some(1.5)])),
        decimal_array(vec![Some(150), Some(250), Some(-100)], 10, 2),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(1_577_836_800_000_000),
            Some(1_609_459_200_000_000),
            None,
        ])),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(1_577_836_800_000_000),
            None,
            Some(1_609_459_200_000_000),
        ])),
        Arc::new(
            TimestampMicrosecondArray::from(vec![
                Some(1_577_836_800_000_000),
                Some(1_609_459_200_000_000),
                None,
            ])
            .with_timezone("UTC"),
        ),
        Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
    ]
}
