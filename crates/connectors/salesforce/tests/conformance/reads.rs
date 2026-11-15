//! The reads, source changes, and failing requests the contract expects of
//! the fixture's objects.

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Decimal32Array, Decimal64Array, Decimal128Array,
    Int32Array, Int64Array, StringArray, Time64MicrosecondArray, TimestampMicrosecondArray,
};
use connector_sdk::{
    ErrorKind,
    conformance::{
        DriftCase, DriftOutcome, ErrorCase, Expected, ExpectedError, ExpectedPlanning,
        FailingRequest, ReadCase, ReadResult, plan::*,
    },
};
use substrait::proto::{
    Expression,
    expression::literal::{Decimal, LiteralType, PrecisionTimestamp},
};

use super::{NAMESPACE, OUTSIDER, READER, REVOKED, SalesforceFixture, id, mock::Mock};

const MICROS_PER_DAY: i64 = 86_400_000_000;
/// 1700-01-01 and 4000-12-31, the first and last day Salesforce holds, as
/// days since 1970-01-01.
const FIRST_DAY: i32 = -98_615;
const LAST_DAY: i32 = 741_807;

fn ids(rows: &[u32]) -> ArrayRef {
    Arc::new(StringArray::from(
        rows.iter()
            .map(|row| id(*row).trim_matches('"').to_owned())
            .collect::<Vec<_>>(),
    ))
}

fn texts(values: Vec<Option<&str>>) -> ArrayRef {
    Arc::new(StringArray::from(values))
}

/// A decimal column in the Arrow width the Analytics decimal of `precision`
/// takes.
fn decimals(values: Vec<Option<i128>>, precision: u8, scale: i8) -> ArrayRef {
    match precision {
        ..=9 => Arc::new(
            Decimal32Array::from(
                values
                    .iter()
                    .map(|value| value.map(|value| i32::try_from(value).unwrap()))
                    .collect::<Vec<_>>(),
            )
            .with_precision_and_scale(precision, scale)
            .unwrap(),
        ),
        10..=18 => Arc::new(
            Decimal64Array::from(
                values
                    .iter()
                    .map(|value| value.map(|value| i64::try_from(value).unwrap()))
                    .collect::<Vec<_>>(),
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

fn instants(values: Vec<Option<i64>>) -> ArrayRef {
    Arc::new(TimestampMicrosecondArray::from(values).with_timezone("UTC"))
}

/// The first and the last instant Salesforce holds.
fn first_instant() -> i64 {
    i64::from(FIRST_DAY) * MICROS_PER_DAY
}

fn last_instant() -> i64 {
    i64::from(LAST_DAY) * MICROS_PER_DAY + MICROS_PER_DAY - 1_000
}

/// Every field of `Values__c`.
fn values() -> ReadPlan {
    ReadPlan::new(
        [NAMESPACE, "Values__c"],
        vec![
            ("Id", text(false)),
            ("Name", text(true)),
            ("Notes__c", text(true)),
            ("Stage__c", text(true)),
            ("Tags__c", text(true)),
            ("Kind__c", text(true)),
            ("Email__c", text(true)),
            ("Phone__c", text(true)),
            ("Site__c", text(true)),
            ("Code__c", text(true)),
            ("Owner__c", text(true)),
            ("Flag__c", boolean(false)),
            ("Count__c", i32(true)),
            ("Big__c", i64(true)),
            ("Amount__c", decimal(18, 2, true)),
            ("Rate__c", decimal(5, 2, true)),
            ("Price__c", decimal(18, 2, true)),
            ("Day__c", date(true)),
            ("Moment__c", timestamp_tz(true)),
            ("Clock__c", time(true)),
        ],
    )
}

fn value_rows() -> Vec<ArrayRef> {
    vec![
        ids(&[1, 2, 3]),
        texts(vec![
            Some("\u{dc}n\u{ef}code \u{2713}"),
            Some("trailing space "),
            None,
        ]),
        texts(vec![Some("line one\nline two"), Some(""), None]),
        texts(vec![Some("Prospecting"), Some("Closed Won"), None]),
        texts(vec![Some("Red;Blue"), Some("Green"), None]),
        texts(vec![Some("Other"), Some("Partner"), None]),
        texts(vec![
            Some("someone@example.com"),
            Some("SOMEONE@example.com"),
            None,
        ]),
        texts(vec![Some("+81 3-0000-0000"), Some("0"), None]),
        texts(vec![
            Some("https://example.com"),
            Some("http://example.org/a?b=c"),
            None,
        ]),
        texts(vec![Some("****1234"), Some("****9999"), None]),
        texts(vec![
            Some("0010000000000AAAAA"),
            Some("0010000000000BAAAA"),
            None,
        ]),
        Arc::new(BooleanArray::from(vec![true, false, false])),
        Arc::new(Int32Array::from(vec![Some(i32::MIN), Some(i32::MAX), None])),
        Arc::new(Int64Array::from(vec![Some(i64::MIN), Some(i64::MAX), None])),
        decimals(
            vec![
                Some(-999_999_999_999_999_999),
                Some(999_999_999_999_999_999),
                None,
            ],
            18,
            2,
        ),
        decimals(vec![Some(-99_999), Some(99_999), None], 5, 2),
        decimals(vec![Some(1), Some(123_450), None], 18, 2),
        Arc::new(Date32Array::from(vec![
            Some(FIRST_DAY),
            Some(LAST_DAY),
            None,
        ])),
        instants(vec![Some(first_instant()), Some(last_instant()), None]),
        Arc::new(Time64MicrosecondArray::from(vec![
            Some(0),
            Some(MICROS_PER_DAY - 1_000),
            None,
        ])),
    ]
}

/// The fields of `Values__c` the filter cases read: `Id` 0, `Name` 1,
/// `Notes__c` 2, `Flag__c` 3, `Count__c` 4, `Big__c` 5, `Amount__c` 6,
/// `Price__c` 7, `Day__c` 8, and `Moment__c` 9.
fn filtered() -> ReadPlan {
    ReadPlan::new(
        [NAMESPACE, "Values__c"],
        vec![
            ("Id", text(false)),
            ("Name", text(true)),
            ("Notes__c", text(true)),
            ("Flag__c", boolean(false)),
            ("Count__c", i32(true)),
            ("Big__c", i64(true)),
            ("Amount__c", decimal(18, 2, true)),
            ("Price__c", decimal(18, 2, true)),
            ("Day__c", date(true)),
            ("Moment__c", timestamp_tz(true)),
        ],
    )
}

/// The records `first..=last` (1 to 3) of `Values__c` as the columns of
/// [`filtered`].
fn filtered_rows(first: u32, last: u32) -> Vec<ArrayRef> {
    let all = value_rows();
    let (offset, length) = ((first - 1) as usize, (last - first + 1) as usize);
    [0, 1, 2, 11, 12, 13, 14, 16, 17, 18]
        .iter()
        .map(|position: &usize| all[*position].slice(offset, length))
        .collect()
}

fn int(value: i32) -> Expression {
    literal(LiteralType::I32(value))
}

fn string(value: &str) -> Expression {
    literal(LiteralType::String(value.to_owned()))
}

fn money(unscaled: i128) -> Expression {
    literal(LiteralType::Decimal(Decimal {
        value: unscaled.to_le_bytes().to_vec(),
        precision: 18,
        scale: 2,
    }))
}

fn instant(micros: i64) -> Expression {
    literal(LiteralType::PrecisionTimestampTz(PrecisionTimestamp {
        precision: 6,
        value: micros,
    }))
}

fn remote(result: ReadResult) -> ExpectedPlanning {
    ExpectedPlanning::Planned {
        local_work: false,
        result: Expected::Contract(result),
    }
}

fn local(result: ReadResult) -> ExpectedPlanning {
    ExpectedPlanning::Planned {
        local_work: true,
        result: Expected::Contract(result),
    }
}

fn error(kind: ErrorKind) -> ReadResult {
    ReadResult::Error(ExpectedError {
        kind,
        retryable: false,
    })
}

/// A fully remote filter case, returning the `Id`s of the given records.
fn remote_filter(name: &'static str, condition: Expression, rows: &[u32]) -> ReadCase {
    ReadCase {
        name,
        offered: filtered()
            .filter(condition)
            .project(vec![(0, "Id")])
            .build(),
        expected: remote(ReadResult::Rows(vec![ids(rows)])),
    }
}

/// A case whose filter stays local; the remote result holds the given
/// records.
fn local_filter(name: &'static str, condition: Expression, rows: (u32, u32)) -> ReadCase {
    ReadCase {
        name,
        offered: filtered()
            .filter(condition)
            .project(vec![(0, "Id")])
            .build(),
        expected: local(ReadResult::Rows(filtered_rows(rows.0, rows.1))),
    }
}

pub fn read_cases() -> Vec<ReadCase> {
    vec![
        ReadCase {
            name: "every covered rule reads its boundary values and NULL",
            offered: values().build(),
            expected: remote(ReadResult::Rows(value_rows())),
        },
        ReadCase {
            name: "a number with more digits than its scale cannot be carried",
            offered: ReadPlan::new(
                [NAMESPACE, "Special__c"],
                vec![("Id", text(false)), ("Amount__c", decimal(18, 2, true))],
            )
            .build(),
            expected: remote(error(ErrorKind::Data)),
        },
        ReadCase {
            name: "values that cannot be carried do not fail a read that skips them",
            offered: ReadPlan::new(
                [NAMESPACE, "Special__c"],
                vec![
                    ("Id", text(false)),
                    ("Amount__c", decimal(18, 2, true)),
                    ("Count__c", i32(true)),
                ],
            )
            .project(vec![(0, "Id"), (2, "Count__c")])
            .build(),
            expected: remote(ReadResult::Rows(vec![
                ids(&[1, 2]),
                Arc::new(Int32Array::from(vec![1, 2])),
            ])),
        },
        ReadCase {
            name: "a projection renames and repeats fields",
            offered: filtered()
                .project(vec![(0, "first"), (4, "count"), (0, "again")])
                .build(),
            expected: remote(ReadResult::Rows(vec![
                ids(&[1, 2, 3]),
                Arc::new(Int32Array::from(vec![Some(i32::MIN), Some(i32::MAX), None])),
                ids(&[1, 2, 3]),
            ])),
        },
        remote_filter(
            "an integer comparison runs remotely",
            call("gt", vec![column(4), int(0)]),
            &[2],
        ),
        remote_filter(
            "a literal on the left is compared the same way",
            call("lt", vec![int(0), column(4)]),
            &[2],
        ),
        remote_filter(
            "not-equal excludes NULL remotely as it does in the offered filter",
            call("not_equal", vec![column(4), int(0)]),
            &[1, 2],
        ),
        remote_filter(
            "a null test runs remotely",
            call("is_null", vec![column(4)]),
            &[3],
        ),
        remote_filter(
            "a Boolean equality runs remotely",
            call(
                "equal",
                vec![column(3), literal(LiteralType::Boolean(true))],
            ),
            &[1],
        ),
        remote_filter(
            "a decimal comparison runs remotely",
            call("gt", vec![column(6), money(0)]),
            &[2],
        ),
        remote_filter(
            "a date comparison runs remotely",
            call(
                "lte",
                vec![column(8), literal(LiteralType::Date(FIRST_DAY))],
            ),
            &[1],
        ),
        remote_filter(
            "a date-time comparison with a whole-second literal runs remotely",
            call(
                "gte",
                vec![column(9), instant(i64::from(LAST_DAY) * MICROS_PER_DAY)],
            ),
            &[2],
        ),
        remote_filter(
            "a non-normalized tree of AND and OR runs remotely",
            call(
                "or",
                vec![
                    call(
                        "and",
                        vec![
                            call("gt", vec![column(4), int(0)]),
                            call("lt", vec![column(4), int(i32::MAX)]),
                        ],
                    ),
                    call(
                        "or",
                        vec![
                            call("is_null", vec![column(4)]),
                            call("equal", vec![column(5), literal(LiteralType::I64(0))]),
                        ],
                    ),
                ],
            ),
            &[3],
        ),
        local_filter(
            "text equality stays local: Salesforce ignores case",
            call("equal", vec![column(1), string("TRAILING SPACE ")]),
            (1, 3),
        ),
        local_filter(
            "ID equality stays local: Salesforce equates 15- and 18-character IDs",
            call("equal", vec![column(0), string("a01000000000001")]),
            (1, 3),
        ),
        local_filter(
            "a currency comparison stays local",
            call("gt", vec![column(7), money(0)]),
            (1, 3),
        ),
        local_filter(
            "NOT stays local",
            call("not", vec![call("gt", vec![column(4), int(0)])]),
            (1, 3),
        ),
        local_filter(
            "a Boolean ordering stays local",
            call("lt", vec![column(3), literal(LiteralType::Boolean(true))]),
            (1, 3),
        ),
        local_filter(
            "a date-time literal with a fraction of a second stays local",
            call("gt", vec![column(9), instant(1)]),
            (1, 3),
        ),
        local_filter(
            "a date beyond the days Salesforce holds stays local",
            call(
                "lt",
                vec![column(8), literal(LiteralType::Date(FIRST_DAY - 1))],
            ),
            (1, 3),
        ),
        local_filter(
            "a field Salesforce cannot filter on stays local",
            call("is_null", vec![column(2)]),
            (1, 3),
        ),
        local_filter(
            "a comparison of two fields stays local",
            call("equal", vec![column(4), column(4)]),
            (1, 3),
        ),
        local_filter(
            "the remote conjuncts of a filter run remotely and the rest locally",
            call(
                "and",
                vec![
                    call("gt", vec![column(4), int(0)]),
                    call("equal", vec![column(1), string("trailing space ")]),
                ],
            ),
            (2, 2),
        ),
    ]
}

/// A read of `object`'s `Id` and its second field.
fn drift_read(
    object: &str,
    field: &'static str,
    ty: substrait::proto::Type,
) -> substrait::proto::Plan {
    ReadPlan::new([NAMESPACE, object], vec![("Id", text(false)), (field, ty)]).build()
}

pub fn drift_cases(mock: &Mock) -> Vec<DriftCase<'_>> {
    vec![
        DriftCase {
            name: "a field's scale changes",
            offered: drift_read("DriftType__c", "Amount__c", decimal(18, 2, true)),
            change: Box::new(|| {
                mock.change(|org| {
                    org.objects
                        .get_mut("DriftType__c")
                        .unwrap()
                        .field_mut("Amount__c")
                        .scale = 4;
                })
            }),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a read field is deleted, which cannot be told from a hidden one",
            offered: drift_read("DriftField__c", "Gone__c", text(true)),
            change: Box::new(|| {
                mock.change(|org| {
                    org.objects
                        .get_mut("DriftField__c")
                        .unwrap()
                        .fields
                        .retain(|field| field.name != "Gone__c");
                })
            }),
            expected: DriftOutcome::Fails(ErrorKind::NotFound),
        },
        DriftCase {
            name: "the read object is deleted, which cannot be told from a hidden one",
            offered: drift_read("DriftObject__c", "Name", text(true)),
            change: Box::new(|| {
                mock.change(|org| {
                    org.objects.remove("DriftObject__c");
                })
            }),
            expected: DriftOutcome::Fails(ErrorKind::NotFound),
        },
        DriftCase {
            name: "a required field becomes nillable",
            offered: drift_read("DriftNullable__c", "Count__c", i32(false)),
            change: Box::new(|| {
                mock.change(|org| {
                    org.objects
                        .get_mut("DriftNullable__c")
                        .unwrap()
                        .field_mut("Count__c")
                        .nillable = true;
                })
            }),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a nillable field becomes required",
            offered: drift_read("DriftRequired__c", "Count__c", i32(true)),
            change: Box::new(|| {
                mock.change(|org| {
                    org.objects
                        .get_mut("DriftRequired__c")
                        .unwrap()
                        .field_mut("Count__c")
                        .nillable = false;
                })
            }),
            expected: DriftOutcome::Reads,
        },
    ]
}

fn failing(kind: ErrorKind, retryable: bool) -> Expected<ExpectedError> {
    Expected::Contract(ExpectedError { kind, retryable })
}

/// A read of `object` whose filter carries a string literal no error may
/// reveal.
fn secret_read(object: &str) -> FailingRequest {
    FailingRequest::Read(Box::new(
        ReadPlan::new(
            [NAMESPACE, object],
            vec![("Id", text(false)), ("Name", text(true))],
        )
        .filter(call("equal", vec![column(1), string("a secret literal")]))
        .build(),
    ))
}

pub fn error_cases(fixture: &SalesforceFixture) -> Vec<ErrorCase> {
    let mut wrong_flow = fixture.options(READER);
    for (key, value) in &mut wrong_flow {
        if key == "auth_flow" {
            *value = connector_sdk::SecretString::new("password".to_owned());
        }
    }
    vec![
        ErrorCase {
            name: "a rejected refresh token",
            options: fixture.options("not-a-refresh-token"),
            request: FailingRequest::OpenSession,
            expected: failing(ErrorKind::Authentication, false),
        },
        ErrorCase {
            name: "an unknown OAuth flow",
            options: wrong_flow,
            request: FailingRequest::OpenSession,
            expected: failing(ErrorKind::InvalidSettings, false),
        },
        ErrorCase {
            name: "an access token Salesforce rejects",
            options: fixture.options(REVOKED),
            request: secret_read("Restricted__c"),
            expected: failing(ErrorKind::Authentication, false),
        },
        ErrorCase {
            name: "an object the user may not see",
            options: fixture.options(OUTSIDER),
            request: secret_read("Restricted__c"),
            expected: failing(ErrorKind::NotFound, false),
        },
        ErrorCase {
            name: "an object the user may see but not read",
            options: fixture.options(OUTSIDER),
            request: secret_read("Locked__c"),
            expected: failing(ErrorKind::PermissionDenied, false),
        },
        ErrorCase {
            name: "a field the user may not see",
            options: fixture.options(OUTSIDER),
            request: FailingRequest::Read(Box::new(
                ReadPlan::new(
                    [NAMESPACE, "Contact"],
                    vec![("Id", text(false)), ("Email", text(true))],
                )
                .build(),
            )),
            expected: failing(ErrorKind::NotFound, false),
        },
        ErrorCase {
            name: "the org's API request limit is exceeded",
            options: fixture.options(READER),
            request: secret_read("Limited__c"),
            expected: failing(ErrorKind::Resource, true),
        },
    ]
}
