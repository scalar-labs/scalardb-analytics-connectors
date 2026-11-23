//! Reads through the connector: boundary values, filters that run remotely,
//! and cancelling a read while its query waits at Salesforce.

use std::{sync::Arc, thread, time::Duration};

use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, Decimal32Array, Decimal64Array, RecordBatch,
    StringArray, Time64MicrosecondArray, TimestampMicrosecondArray,
};
use connector_sdk::{
    ErrorKind, NameFilter,
    conformance::plan::*,
    runtime::{PlanningOutcome, SessionState, SharedRemoteExecution},
};
use prost::Message;
use substrait::proto::{
    Expression, Plan,
    expression::literal::{Decimal, LiteralType, PrecisionTimestamp},
};

use super::{NAMESPACE, OBJECT, Org};

const MICROS_PER_DAY: i64 = 86_400_000_000;
/// 1700-01-01 and 4000-12-31, the first and last day Salesforce holds, as
/// days since 1970-01-01.
const FIRST_DAY: i32 = -98_615;
const LAST_DAY: i32 = 741_807;

pub fn check(org: &Org) {
    let session = org.session();
    every_field_reads_its_boundary_values_and_null(&session);
    filters_run_remotely_with_the_offered_result(&session);
    a_cancelled_read_stops_and_leaves_the_session_usable(&session);
    session.close();
}

/// The values of `Encryptedstring__c` that `sfdx/data` loads.
const ENCRYPTED: [Option<&str>; 3] = [Some("secret-1234"), Some("9"), None];

fn every_field_reads_its_boundary_values_and_null(session: &Arc<SessionState>) {
    let columns = read(session, values().build());
    let expected = value_rows(&first_bulk_record_id(session));
    let actual_keys = keys(&columns[0]);
    for (row, key) in ["min", "max", "null"].iter().enumerate() {
        let position = actual_keys
            .iter()
            .position(|actual| actual == key)
            .unwrap_or_else(|| panic!("no record with Key__c {key}; run `run.sh prepare`"));
        for (field, (actual, expected)) in VALUE_FIELDS.iter().zip(columns.iter().zip(&expected)) {
            let actual = actual.slice(position, 1);
            let readable = *field == "Encryptedstring__c"
                && actual.as_ref() == &StringArray::from(vec![ENCRYPTED[row]]) as &dyn Array;
            if !readable {
                assert_eq!(
                    actual.as_ref(),
                    expected.slice(row, 1).as_ref(),
                    "{key}.{field}"
                );
            }
        }
    }
}

/// The Id of the `ConnectorTestBulk__c` record whose `Seq__c` is 1, which
/// `min` refers to.
fn first_bulk_record_id(session: &Arc<SessionState>) -> String {
    let offered = ReadPlan::new(
        [NAMESPACE, "ConnectorTestBulk__c"],
        vec![("Id", text(false)), ("Seq__c", decimal(18, 0, true))],
    )
    .filter(call(
        "equal",
        vec![
            column(1),
            literal(LiteralType::Decimal(Decimal {
                value: 1_i128.to_le_bytes().to_vec(),
                precision: 18,
                scale: 0,
            })),
        ],
    ))
    .project(vec![(0, "Id")])
    .build();
    let ids = keys(&read(session, offered)[0]);
    let [id] = ids.as_slice() else {
        panic!("one ConnectorTestBulk__c record has Seq__c 1: {ids:?}");
    };
    id.clone()
}

fn filters_run_remotely_with_the_offered_result(session: &Arc<SessionState>) {
    let cases: Vec<(&str, Expression, &[&str])> = vec![
        (
            "not-equal excludes NULL as the offered filter does",
            call("not_equal", vec![column(4), rate(0)]),
            &["max", "min"],
        ),
        ("a null test", call("is_null", vec![column(4)]), &["null"]),
        (
            "a Boolean equality",
            call(
                "equal",
                vec![column(2), literal(LiteralType::Boolean(true))],
            ),
            &["min"],
        ),
        (
            "a decimal comparison at the field's precision",
            call("gt", vec![column(3), amount(0)]),
            &["max"],
        ),
        (
            "a date comparison with the first day",
            call(
                "lte",
                vec![column(5), literal(LiteralType::Date(FIRST_DAY))],
            ),
            &["min"],
        ),
        (
            "a date-time comparison with the last day",
            call(
                "gte",
                vec![column(6), instant(i64::from(LAST_DAY) * MICROS_PER_DAY)],
            ),
            &["max"],
        ),
        (
            "a tree of AND and OR with a null test",
            call(
                "or",
                vec![
                    call(
                        "and",
                        vec![
                            call("gt", vec![column(4), rate(0)]),
                            call("lte", vec![column(4), rate(99_999)]),
                        ],
                    ),
                    call("is_null", vec![column(4)]),
                ],
            ),
            &["max", "null"],
        ),
    ];
    for (name, condition, expected) in cases {
        let offered = filtered().filter(condition).project(vec![(0, "Key__c")]);
        let (remote, local_work) = plan(session, offered.build());
        assert!(!local_work, "{name}: local work remains");
        let mut actual = keys(&execute(session, &remote)[0]);
        actual.sort();
        assert_eq!(actual, expected, "{name}");
    }
}

fn a_cancelled_read_stops_and_leaves_the_session_usable(session: &Arc<SessionState>) {
    let (remote, _) = plan(session, values().build());
    let operation = session.start_read(remote, None).expect("the read starts");
    // The query is sent at the first pull; the cancel arrives while its
    // response is on the way, because a round trip to Salesforce takes
    // longer than the delay.
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(5));
            operation.cancel().expect("the operation cancels");
        });
        operation.pull()
    });
    match result {
        Err(error) => assert_eq!(error.kind(), ErrorKind::Cancellation, "a cancelled pull"),
        Ok(_) => panic!("the first page arrived before the cancel; the check cannot run"),
    }
    operation.close();
    session
        .table_list(&NameFilter::None, &NameFilter::None, None, None)
        .expect("the session accepts a request after a cancelled read");
}

/// The fields of `ConnectorTest__c` that [`values`] reads, in its order.
const VALUE_FIELDS: [&str; 17] = [
    "Key__c",
    "Name",
    "LongTextArea__c",
    "Picklist__c",
    "MultiPicklist__c",
    "Email__c",
    "Phone__c",
    "Url__c",
    "Encryptedstring__c",
    "Reference__c",
    "Checkbox__c",
    "Number18_2__c",
    "Percent5_2__c",
    "Currency18_2__c",
    "Date__c",
    "DateTime__c",
    "Time__c",
];

/// Every field of `ConnectorTest__c` that `sfdx/data` loads.
fn values() -> ReadPlan {
    ReadPlan::new(
        [NAMESPACE, OBJECT],
        vec![
            ("Key__c", text(true)),
            ("Name", text(true)),
            ("LongTextArea__c", text(true)),
            ("Picklist__c", text(true)),
            ("MultiPicklist__c", text(true)),
            ("Email__c", text(true)),
            ("Phone__c", text(true)),
            ("Url__c", text(true)),
            ("Encryptedstring__c", text(true)),
            ("Reference__c", text(true)),
            ("Checkbox__c", boolean(false)),
            ("Number18_2__c", decimal(18, 2, true)),
            ("Percent5_2__c", decimal(5, 2, true)),
            ("Currency18_2__c", decimal(18, 2, true)),
            ("Date__c", date(true)),
            ("DateTime__c", timestamp_tz(true)),
            ("Time__c", time(true)),
        ],
    )
}

/// The records `min`, `max`, and `null` of `sfdx/data` as the columns of
/// [`values`], as Salesforce stores them: it removes trailing spaces from
/// text, writes email addresses in lower case, and holds a number in a
/// double, so `99999999999999.99` is stored as `99999999999999.98`.
///
/// `Encryptedstring__c` reads masked, one `*` per character, for a user
/// without the View Encrypted Data permission; [`ENCRYPTED`] holds the
/// values a user with it reads. `reference` is the Id `min` refers to.
fn value_rows(reference: &str) -> Vec<ArrayRef> {
    let texts =
        |values: [Option<&str>; 3]| -> ArrayRef { Arc::new(StringArray::from(values.to_vec())) };
    vec![
        texts([Some("min"), Some("max"), Some("null")]),
        texts([
            Some("\u{dc}n\u{ef}code \u{2713}"),
            Some("trailing space"),
            Some("null"),
        ]),
        texts([Some("line one\nline two"), Some("x"), None]),
        texts([Some("Prospecting"), Some("Closed Won"), None]),
        texts([Some("Red;Blue"), Some("Green"), None]),
        texts([
            Some("someone@example.com"),
            Some("someone@example.com"),
            None,
        ]),
        texts([Some("+81 3-0000-0000"), Some("0"), None]),
        texts([
            Some("https://example.com"),
            Some("http://example.org/a?b=c"),
            None,
        ]),
        texts([Some("***********"), Some("*"), None]),
        texts([Some(reference), None, None]),
        Arc::new(BooleanArray::from(vec![true, false, false])),
        Arc::new(
            Decimal64Array::from(vec![
                Some(-9_999_999_999_999_998),
                Some(9_999_999_999_999_998),
                None,
            ])
            .with_precision_and_scale(18, 2)
            .unwrap(),
        ),
        Arc::new(
            Decimal32Array::from(vec![Some(-99_999), Some(99_999), None])
                .with_precision_and_scale(5, 2)
                .unwrap(),
        ),
        Arc::new(
            Decimal64Array::from(vec![Some(1), Some(123_450), None])
                .with_precision_and_scale(18, 2)
                .unwrap(),
        ),
        Arc::new(Date32Array::from(vec![
            Some(FIRST_DAY),
            Some(LAST_DAY),
            None,
        ])),
        Arc::new(
            TimestampMicrosecondArray::from(vec![
                Some(i64::from(FIRST_DAY) * MICROS_PER_DAY),
                Some(i64::from(LAST_DAY) * MICROS_PER_DAY),
                None,
            ])
            .with_timezone("UTC"),
        ),
        Arc::new(Time64MicrosecondArray::from(vec![
            Some(0),
            Some(MICROS_PER_DAY - 1_000),
            None,
        ])),
    ]
}

/// The fields the filter cases read: `Key__c` 0, `Name` 1, `Checkbox__c` 2,
/// `Number18_2__c` 3, `Percent5_2__c` 4, `Date__c` 5, and `DateTime__c` 6.
fn filtered() -> ReadPlan {
    ReadPlan::new(
        [NAMESPACE, OBJECT],
        vec![
            ("Key__c", text(true)),
            ("Name", text(true)),
            ("Checkbox__c", boolean(false)),
            ("Number18_2__c", decimal(18, 2, true)),
            ("Percent5_2__c", decimal(5, 2, true)),
            ("Date__c", date(true)),
            ("DateTime__c", timestamp_tz(true)),
        ],
    )
}

fn decimal_literal(unscaled: i128, precision: i32) -> Expression {
    literal(LiteralType::Decimal(Decimal {
        value: unscaled.to_le_bytes().to_vec(),
        precision,
        scale: 2,
    }))
}

fn amount(unscaled: i128) -> Expression {
    decimal_literal(unscaled, 18)
}

fn rate(unscaled: i128) -> Expression {
    decimal_literal(unscaled, 5)
}

fn instant(micros: i64) -> Expression {
    literal(LiteralType::PrecisionTimestampTz(PrecisionTimestamp {
        precision: 6,
        value: micros,
    }))
}

/// Plans `offered`, returning its remote execution and whether local work
/// remains.
pub fn plan(session: &Arc<SessionState>, offered: Plan) -> (SharedRemoteExecution, bool) {
    match session
        .plan_pushdown(&offered.encode_to_vec())
        .unwrap_or_else(|error| panic!("planning fails: {error}"))
    {
        PlanningOutcome::Planned {
            remote, local_plan, ..
        } => (remote, local_plan.is_some()),
        PlanningOutcome::Unsupported { reason } => panic!("the region is unsupported: {reason}"),
    }
}

pub fn read(session: &Arc<SessionState>, offered: Plan) -> Vec<ArrayRef> {
    let (remote, local_work) = plan(session, offered);
    assert!(!local_work, "a plain read leaves no local work");
    execute(session, &remote)
}

/// Reads a remote execution to the end, as output-schema columns.
///
/// The test records fit in one query page, so the read returns one batch.
fn execute(session: &Arc<SessionState>, remote: &SharedRemoteExecution) -> Vec<ArrayRef> {
    let operation = session
        .start_read(remote.clone(), None)
        .expect("the read starts");
    let mut batches: Vec<RecordBatch> = Vec::new();
    while let Some(batch) = operation
        .pull()
        .unwrap_or_else(|error| panic!("the read fails: {error}"))
    {
        batches.push(batch);
    }
    operation.close();
    assert_eq!(batches.len(), 1, "the records come in one batch");
    batches.remove(0).columns().to_vec()
}

fn keys(column: &ArrayRef) -> Vec<String> {
    let keys = column
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("Key__c is text");
    keys.iter()
        .map(|key| key.expect("every record has a Key__c").to_owned())
        .collect()
}
