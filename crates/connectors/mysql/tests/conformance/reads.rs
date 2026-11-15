//! The reads, source changes, and failing requests the contract expects of
//! the fixture's objects.

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal32Array, Decimal64Array,
    Decimal128Array, Decimal256Array, Float32Array, Float64Array, Int8Array, Int16Array,
    Int32Array, Int64Array, StringArray, TimestampMicrosecondArray, UInt8Array, UInt16Array,
    UInt32Array, UInt64Array,
};
use arrow_buffer::i256;
use connector_sdk::{
    ErrorKind, SecretString,
    conformance::{
        DriftCase, DriftOutcome, ErrorCase, Expected, ExpectedError, ExpectedPlanning,
        FailingRequest, ReadCase, ReadResult, plan::*,
    },
};
use substrait::proto::{
    Expression, Plan,
    expression::literal::{Decimal, LiteralType},
};

use super::{OUTSIDER, READER, SCHEMA, options, server};

/// 1000-01-01 and 9999-12-31, MySQL's documented date range, in days since
/// 1970-01-01.
const FIRST_DAY: i32 = -354_285;
const LAST_DAY: i32 = 2_932_896;
const MICROS_PER_DAY: i64 = 86_400_000_000;

pub fn read_cases() -> Vec<ReadCase> {
    let mut cases = vec![
        ReadCase {
            name: "every covered rule reads its boundary values and NULL",
            offered: value_types().build(),
            expected: remote(rows(value_type_rows())),
        },
        ReadCase {
            name: "a zero date cannot be carried",
            offered: special().project(vec![(0, "id"), (1, "day")]).build(),
            expected: ExpectedPlanning::Planned {
                local_work: false,
                result: Expected::KnownDeviation {
                    contract: error(ErrorKind::Data),
                    observed: error(ErrorKind::DataSource),
                    reason: "the ADBC driver fails the read before the connector sees the value",
                },
            },
        },
        ReadCase {
            name: "the zero year cannot be carried",
            offered: special()
                .project(vec![(0, "id"), (2, "year_value")])
                .build(),
            expected: remote(error(ErrorKind::Data)),
        },
        ReadCase {
            name: "values that cannot be carried do not fail a read that skips them",
            offered: special()
                .filter(call("equal", vec![column(0), int(3)]))
                .build(),
            expected: remote(rows(vec![
                Arc::new(Int32Array::from(vec![3])),
                Arc::new(Date32Array::from(vec![18_263])),
                Arc::new(Int16Array::from(vec![2001])),
                Arc::new(BooleanArray::from(vec![true])),
                Arc::new(StringArray::from(vec!["ok"])),
            ])),
        },
        ReadCase {
            name: "an enum's error value cannot be carried",
            offered: special().project(vec![(0, "id"), (4, "feeling")]).build(),
            expected: remote(error(ErrorKind::Data)),
        },
        ReadCase {
            name: "a tinyint(1) other than 0 and 1 cannot be carried as Boolean",
            offered: special().project(vec![(0, "id"), (3, "flag")]).build(),
            expected: remote(error(ErrorKind::Data)),
        },
        ReadCase {
            name: "a Boolean filter skips a tinyint(1) it cannot carry",
            offered: special()
                .filter(call(
                    "equal",
                    vec![column(3), literal(LiteralType::Boolean(true))],
                ))
                .project(vec![(0, "id")])
                .build(),
            expected: remote(rows(vec![Arc::new(Int32Array::from(vec![1, 2, 3]))])),
        },
    ];
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
            "text equality under utf8mb4_0900_bin runs remotely",
            call("equal", vec![column(1), string("a")]),
            remote_ids(vec![1]),
        ),
        case(
            "text ordering under utf8mb4_0900_bin runs remotely",
            call("lt", vec![column(1), string("a!")]),
            remote_ids(vec![1, 3]),
        ),
        case(
            "text equality between columns of utf8mb4_0900_bin runs remotely",
            call("equal", vec![column(1), column(9)]),
            remote_ids(vec![1, 3]),
        ),
        case(
            "a PAD SPACE collation ignores trailing spaces",
            call("equal", vec![column(2), string("a")]),
            local(),
        ),
        case(
            "an accent- and case-insensitive collation folds letters",
            call("equal", vec![column(3), string("a")]),
            local(),
        ),
        case(
            "char columns drop trailing spaces",
            call("equal", vec![column(4), string("a")]),
            local(),
        ),
        case(
            "floating-point comparison stays local",
            call("gt", vec![column(5), literal(LiteralType::Fp64(1.0))]),
            local(),
        ),
        case(
            "date comparison runs remotely",
            call("gt", vec![column(6), literal(LiteralType::Date(18_414))]),
            remote_ids(vec![3]),
        ),
        case(
            "a date literal outside MySQL's years",
            call("gt", vec![column(6), literal(LiteralType::Date(i32::MIN))]),
            local(),
        ),
        case(
            "decimal comparison runs remotely",
            call("lt", vec![column(7), decimal_literal(0, 5, 2)]),
            remote_ids(vec![2]),
        ),
        case(
            "unsigned comparison above the signed range runs remotely",
            call(
                "gt",
                vec![
                    column(10),
                    unsigned_literal(
                        "unsigned_bigint",
                        LiteralType::Decimal(Decimal {
                            value: i128::from(i64::MAX).to_le_bytes().to_vec(),
                            precision: 20,
                            scale: 0,
                        }),
                    ),
                ],
            ),
            remote_ids(vec![2, 3]),
        ),
        case(
            "comparison of decimals wider than Substrait's runs remotely",
            call(
                "lt",
                vec![
                    column(11),
                    logical_text("decimal", r#"{"precision":65,"scale":0}"#, "0"),
                ],
            ),
            remote_ids(vec![3]),
        ),
        case(
            "Boolean comparison on bit(1) runs remotely",
            call(
                "equal",
                vec![column(12), literal(LiteralType::Boolean(true))],
            ),
            remote_ids(vec![1, 3]),
        ),
        case(
            "enum comparison stays local",
            call(
                "equal",
                vec![
                    column(8),
                    logical_text("enum", r#"{"members":["sad","ok","happy"]}"#, "ok"),
                ],
            ),
            local(),
        ),
        case(
            "Boolean composition and null tests run remotely",
            call(
                "or",
                vec![
                    call("is_null", vec![column(6)]),
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
            server().mysql(sql);
        })
    };
    vec![
        DriftCase {
            name: "a changed column type",
            offered: two_columns("retyped"),
            change: change("ALTER TABLE conformance.retyped MODIFY value bigint NOT NULL"),
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
            change: change("ALTER TABLE conformance.relaxed MODIFY value int NULL"),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a nullable column made NOT NULL",
            offered: ReadPlan::new(
                [SCHEMA, "tightened"],
                vec![("id", i32(false)), ("value", i32(true))],
            )
            .build(),
            change: change("ALTER TABLE conformance.tightened MODIFY value int NOT NULL"),
            expected: DriftOutcome::Reads,
        },
    ]
}

pub fn error_cases() -> Vec<ErrorCase> {
    let mut wrong_password = options(READER);
    for (key, value) in &mut wrong_password {
        if key == "password" {
            *value = SecretString::new("conformance-wrong-secret".to_owned());
        }
    }
    let failing = |kind| {
        Expected::Contract(ExpectedError {
            kind,
            retryable: false,
        })
    };
    let literal_read = |table: &'static str| {
        FailingRequest::Read(Box::new(
            ReadPlan::new(
                [SCHEMA, table],
                vec![("id", i32(false)), ("label", text(true))],
            )
            .filter(call(
                "equal",
                vec![column(1), string("conformance-literal")],
            ))
            .build(),
        ))
    };
    vec![
        ErrorCase {
            name: "reading a table without SELECT",
            options: options(OUTSIDER),
            request: literal_read("writable"),
            expected: failing(ErrorKind::PermissionDenied),
        },
        ErrorCase {
            name: "reading a column the account cannot see",
            options: options(OUTSIDER),
            request: literal_read("hidden"),
            expected: failing(ErrorKind::NotFound),
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

fn decimal_literal(unscaled: i128, precision: i32, scale: i32) -> Expression {
    literal(LiteralType::Decimal(Decimal {
        value: unscaled.to_le_bytes().to_vec(),
        precision,
        scale,
    }))
}

/// A decimal array in the Analytics storage of `Decimal(precision, scale)`,
/// the narrowest Arrow decimal that holds the precision; values are decimal
/// text of the unscaled integers.
fn decimal_array(values: Vec<Option<&str>>, precision: u8, scale: i8) -> ArrayRef {
    let unscaled = |value: &str| i256::from_string(value).unwrap();
    let narrow = |value: &str| unscaled(value).to_i128().unwrap();
    match precision {
        ..=9 => Arc::new(
            Decimal32Array::from_iter(
                values
                    .iter()
                    .map(|value| value.map(|value| i32::try_from(narrow(value)).unwrap())),
            )
            .with_precision_and_scale(precision, scale)
            .unwrap(),
        ),
        10..=18 => Arc::new(
            Decimal64Array::from_iter(
                values
                    .iter()
                    .map(|value| value.map(|value| i64::try_from(narrow(value)).unwrap())),
            )
            .with_precision_and_scale(precision, scale)
            .unwrap(),
        ),
        19..=38 => Arc::new(
            Decimal128Array::from_iter(values.iter().map(|value| value.map(narrow)))
                .with_precision_and_scale(precision, scale)
                .unwrap(),
        ),
        _ => Arc::new(
            Decimal256Array::from_iter(values.iter().map(|value| value.map(unscaled)))
                .with_precision_and_scale(precision, scale)
                .unwrap(),
        ),
    }
}

/// `count` nines, negated when `negative`.
fn nines(count: usize, negative: bool) -> String {
    format!("{}{}", if negative { "-" } else { "" }, "9".repeat(count))
}

/// A literal of an unsigned integer type over its carrier.
fn unsigned_literal(name: &str, carrier: LiteralType) -> Expression {
    logical_literal(name, "{}", carrier)
}

fn value_types() -> ReadPlan {
    ReadPlan::new(
        [SCHEMA, "value_types"],
        vec![
            ("id", i32(false)),
            ("tiny", i8(true)),
            ("small", i16(true)),
            ("medium", i32(true)),
            ("regular", i32(true)),
            ("big", i64(true)),
            ("tiny_unsigned", logical("unsigned_byte", "{}", true)),
            ("small_unsigned", logical("unsigned_smallint", "{}", true)),
            ("medium_unsigned", logical("unsigned_int", "{}", true)),
            ("regular_unsigned", logical("unsigned_int", "{}", true)),
            ("single", fp32(true)),
            ("ratio", fp64(true)),
            ("narrow", decimal(5, 2, true)),
            ("exact", decimal(38, 4, true)),
            ("fixed", text(true)),
            ("bounded", text(true)),
            ("free_text", text(true)),
            ("fixed_bytes", binary(true)),
            ("bytes", binary(true)),
            ("day", date(true)),
            ("stamp", timestamp(true)),
            ("precise", timestamp(true)),
            ("instant", timestamp_tz(true)),
            ("year_value", logical("year", "{}", true)),
            (
                "feeling",
                logical("enum", r#"{"members":["sad","ok","happy"]}"#, true),
            ),
            ("document", logical("json", "{}", true)),
            ("big_unsigned", logical("unsigned_bigint", "{}", true)),
            ("medium_exact", decimal(18, 4, true)),
            (
                "widest",
                logical("decimal", r#"{"precision":65,"scale":30}"#, true),
            ),
            ("flag", boolean(true)),
            ("bit_flag", boolean(true)),
        ],
    )
}

/// The three rows of `value_types`: maxima, NULL, and minima.
fn value_type_rows() -> Vec<ArrayRef> {
    let text =
        |values: [Option<&str>; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    let last_second = i64::from(LAST_DAY) * MICROS_PER_DAY + 86_399_000_000;
    vec![
        Arc::new(Int32Array::from(vec![1, 2, 3])),
        Arc::new(Int8Array::from(vec![Some(i8::MAX), None, Some(i8::MIN)])),
        Arc::new(Int16Array::from(vec![Some(i16::MAX), None, Some(i16::MIN)])),
        Arc::new(Int32Array::from(vec![
            Some(8_388_607),
            None,
            Some(-8_388_608),
        ])),
        Arc::new(Int32Array::from(vec![Some(i32::MAX), None, Some(i32::MIN)])),
        Arc::new(Int64Array::from(vec![Some(i64::MIN), None, Some(i64::MAX)])),
        Arc::new(UInt8Array::from(vec![Some(u8::MAX), None, Some(0)])),
        Arc::new(UInt16Array::from(vec![Some(u16::MAX), None, Some(0)])),
        Arc::new(UInt32Array::from(vec![Some(16_777_215), None, Some(0)])),
        Arc::new(UInt32Array::from(vec![Some(u32::MAX), None, Some(0)])),
        Arc::new(Float32Array::from(vec![Some(0.5), None, Some(-3.40282e38)])),
        Arc::new(Float64Array::from(vec![Some(0.25), None, Some(f64::MAX)])),
        decimal_array(vec![Some("99999"), None, Some("-99999")], 5, 2),
        decimal_array(
            vec![Some(&nines(38, false)), None, Some(&nines(38, true))],
            38,
            4,
        ),
        // MySQL drops a `char` value's trailing spaces when it reads it.
        text([Some("ab"), None, Some("abcde")]),
        text([Some("héllo"), None, Some("ten chars.")]),
        text([Some("日本語"), None, Some("")]),
        // `binary(n)` pads with zero bytes.
        Arc::new(BinaryArray::from(vec![
            Some([0x00, 0xff, 0x00].as_slice()),
            None,
            Some([0x00, 0x00, 0x00].as_slice()),
        ])),
        Arc::new(BinaryArray::from(vec![
            Some([0x00, 0xff].as_slice()),
            None,
            Some([].as_slice()),
        ])),
        Arc::new(Date32Array::from(vec![
            Some(FIRST_DAY),
            None,
            Some(LAST_DAY),
        ])),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(i64::from(FIRST_DAY) * MICROS_PER_DAY),
            None,
            Some(last_second),
        ])),
        Arc::new(TimestampMicrosecondArray::from(vec![
            Some(1),
            None,
            Some(last_second + 999_999),
        ])),
        Arc::new(
            TimestampMicrosecondArray::from(vec![
                Some(1_000_000),
                None,
                Some(2_147_483_647_999_999),
            ])
            .with_timezone("UTC"),
        ),
        Arc::new(Int16Array::from(vec![Some(1901), None, Some(2155)])),
        text([Some("ok"), None, Some("sad")]),
        text([Some(r#"{"a": 1}"#), None, Some("[]")]),
        Arc::new(UInt64Array::from(vec![Some(u64::MAX), None, Some(0)])),
        decimal_array(
            vec![Some(&nines(18, false)), None, Some(&nines(18, true))],
            18,
            4,
        ),
        decimal_array(
            vec![Some(&nines(65, false)), None, Some(&nines(65, true))],
            65,
            30,
        ),
        Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])),
        Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])),
    ]
}

fn special() -> ReadPlan {
    ReadPlan::new(
        [SCHEMA, "special"],
        vec![
            ("id", i32(false)),
            ("day", date(true)),
            ("year_value", logical("year", "{}", true)),
            ("flag", boolean(true)),
            (
                "feeling",
                logical("enum", r#"{"members":["sad","ok","happy"]}"#, true),
            ),
        ],
    )
}

fn filtering(filter: Expression) -> Plan {
    ReadPlan::new(
        [SCHEMA, "filtering"],
        vec![
            ("id", i32(false)),
            ("label", text(true)),
            ("padded", text(true)),
            ("folded", text(true)),
            ("code", text(true)),
            ("score", fp64(true)),
            ("day", date(true)),
            ("amount", decimal(5, 2, true)),
            (
                "feeling",
                logical("enum", r#"{"members":["sad","ok","happy"]}"#, true),
            ),
            ("other_label", text(true)),
            ("counter", logical("unsigned_bigint", "{}", true)),
            (
                "huge",
                logical("decimal", r#"{"precision":65,"scale":0}"#, true),
            ),
            ("bit_flag", boolean(true)),
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
        text(["a", "b", "a "]),
        text(["A", "b", "á"]),
        text(["a", "b", "a"]),
        Arc::new(Float64Array::from(vec![0.5, 2.5, 1.5])),
        Arc::new(Date32Array::from(vec![Some(18_262), None, Some(18_628)])),
        decimal_array(vec![Some("150"), Some("-100"), None], 5, 2),
        text(["ok", "sad", "happy"]),
        text(["a", "c", "a "]),
        Arc::new(UInt64Array::from(vec![7, 1 << 63, u64::MAX])),
        decimal_array(
            vec![Some("1"), Some(&nines(65, false)), Some(&nines(65, true))],
            65,
            0,
        ),
        Arc::new(BooleanArray::from(vec![true, false, true])),
    ]
}
