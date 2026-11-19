//! Reads: planning, the `Scan` pages a read sends, decoding items and the
//! `item` column, the schema check before the first page, and failures while
//! scanning.

use arrow_array::{Array, BinaryArray, RecordBatch, StringArray};
use connector_dynamodb::NAMESPACE;
use connector_sdk::{
    ConnectorError, ErrorKind,
    conformance::plan::{ReadPlan, binary, logical, text},
    runtime::{PlanningOutcome, SessionState},
};
use prost::Message;
use serde_json::{Value, json};
use std::sync::Arc;

use super::{MockDynamoDb, error, metadata::orders, ok, session};

/// A read of both keys of `orders`, projected to `project` when given.
fn plan(project: Option<Vec<(i32, &'static str)>>) -> Vec<u8> {
    let mut plan = ReadPlan::new(
        [NAMESPACE, "orders"],
        vec![("pk", text(false)), ("sk", binary(false))],
    );
    if let Some(columns) = project {
        plan = plan.project(columns);
    }
    plan.build().encode_to_vec()
}

/// Plans `plan` and pulls the read to its end.
fn read(session: &Arc<SessionState>, plan: &[u8]) -> Result<Vec<RecordBatch>, ConnectorError> {
    let PlanningOutcome::Planned {
        remote, local_plan, ..
    } = session.plan_pushdown(plan)?
    else {
        panic!("not planned");
    };
    assert!(local_plan.is_none(), "work was left local");
    let operation = session.start_read(remote, None)?;
    let mut batches = Vec::new();
    while let Some(batch) = operation.pull()? {
        batches.push(batch);
    }
    Ok(batches)
}

fn item(pk: &str, sk: &str) -> Value {
    json!({ "pk": { "S": pk }, "sk": { "B": sk } })
}

fn texts(batch: &RecordBatch, column: usize) -> Vec<String> {
    let array = batch
        .column(column)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    (0..array.len())
        .map(|row| array.value(row).to_owned())
        .collect()
}

fn blobs(batch: &RecordBatch, column: usize) -> Vec<Vec<u8>> {
    let array = batch
        .column(column)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .unwrap();
    (0..array.len())
        .map(|row| array.value(row).to_vec())
        .collect()
}

/// A mock with `orders` described and planned on.
fn orders_mock() -> MockDynamoDb {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "orders" })),
        ok(orders(1)),
    );
    mock
}

#[test]
fn a_read_scans_every_page_after_its_last_evaluated_key() {
    let mock = orders_mock();
    let after = |key: &str| json!({ "pk": { "S": key }, "sk": { "B": "AA==" } });
    mock.on(
        "Scan",
        Some(json!({ "ExclusiveStartKey": after("b") })),
        ok(json!({ "Items": [], "Count": 0, "ScannedCount": 0, "LastEvaluatedKey": after("c") })),
    );
    mock.on(
        "Scan",
        Some(json!({ "ExclusiveStartKey": after("c") })),
        ok(json!({ "Items": [item("\u{e9}t\u{e9}", "")], "Count": 1, "ScannedCount": 1 })),
    );
    mock.on(
        "Scan",
        None,
        ok(json!({
            "Items": [item("a", "AAE="), item("b", "/w==")],
            "Count": 2,
            "ScannedCount": 2,
            "LastEvaluatedKey": after("b")
        })),
    );
    let session = session(&mock);
    let batches = read(&session, &plan(None)).unwrap();

    // The empty page in between yields no batch.
    assert_eq!(batches.len(), 2);
    assert_eq!(texts(&batches[0], 0), ["a", "b"]);
    assert_eq!(blobs(&batches[0], 1), [vec![0, 1], vec![255]]);
    assert_eq!(texts(&batches[1], 0), ["\u{e9}t\u{e9}"]);
    assert_eq!(blobs(&batches[1], 1), [Vec::<u8>::new()]);

    let scans = mock.bodies("Scan");
    assert_eq!(scans.len(), 3);
    assert_eq!(
        scans[0],
        json!({
            "TableName": "orders",
            "ProjectionExpression": "#a0, #a1",
            "ExpressionAttributeNames": { "#a0": "pk", "#a1": "sk" },
            "ConsistentRead": false
        })
    );
    assert_eq!(scans[1]["ExclusiveStartKey"], after("b"));
    assert_eq!(scans[2]["ExclusiveStartKey"], after("c"));
}

#[test]
fn the_table_is_described_again_before_the_first_scan() {
    let mock = orders_mock();
    mock.on(
        "Scan",
        None,
        ok(json!({ "Items": [], "Count": 0, "ScannedCount": 0 })),
    );
    let session = session(&mock);
    assert!(read(&session, &plan(None)).unwrap().is_empty());
    let operations: Vec<_> = mock
        .requests()
        .into_iter()
        .map(|(operation, _)| operation)
        .filter(|operation| operation != "ListTables")
        .collect();
    // Planning describes the table, and so does the read before scanning.
    assert_eq!(operations, ["DescribeTable", "DescribeTable", "Scan"]);
}

#[test]
fn a_projection_of_key_columns_becomes_the_projection_expression() {
    let mock = orders_mock();
    mock.on(
        "Scan",
        None,
        ok(json!({ "Items": [{ "sk": { "B": "AQ==" } }], "Count": 1, "ScannedCount": 1 })),
    );
    let session = session(&mock);
    let batches = read(&session, &plan(Some(vec![(1, "sort"), (1, "again")]))).unwrap();
    assert_eq!(batches[0].schema().field(0).name(), "sort");
    assert_eq!(blobs(&batches[0], 0), [vec![1]]);
    assert_eq!(blobs(&batches[0], 1), [vec![1]]);
    let scans = mock.bodies("Scan");
    assert_eq!(scans[0]["ProjectionExpression"], "#a0");
    assert_eq!(scans[0]["ExpressionAttributeNames"], json!({ "#a0": "sk" }));
}

#[test]
fn projecting_item_scans_whole_items_and_writes_them_as_dynamodb_json() {
    let mock = orders_mock();
    mock.on(
        "Scan",
        None,
        ok(json!({
            "Items": [{
                "sk": { "B": "AA==" },
                "pk": { "S": "a" },
                "total": { "N": "12.50" },
                "tags": { "SS": ["x", "a"] },
                "lines": { "L": [{ "M": { "qty": { "N": "1" }, "id": { "B": "AQ==" } } }] }
            }],
            "Count": 1,
            "ScannedCount": 1
        })),
    );
    let session = session(&mock);
    let plan = ReadPlan::new(
        [NAMESPACE, "orders"],
        vec![
            ("pk", text(false)),
            ("sk", binary(false)),
            ("item", logical("json", "{}", false)),
        ],
    )
    .project(vec![(0, "pk"), (2, "item")])
    .build()
    .encode_to_vec();
    let batches = read(&session, &plan).unwrap();
    assert_eq!(texts(&batches[0], 0), ["a"]);
    assert_eq!(
        texts(&batches[0], 1),
        [concat!(
            r#"{"lines":{"L":[{"M":{"id":{"B":"AQ=="},"qty":{"N":"1"}}}]},"#,
            r#""pk":{"S":"a"},"sk":{"B":"AA=="},"tags":{"SS":["a","x"]},"total":{"N":"12.50"}}"#
        )]
    );
    // Whole items are read without a projection expression.
    assert_eq!(
        mock.bodies("Scan"),
        [json!({ "TableName": "orders", "ConsistentRead": false })]
    );
}

#[test]
fn a_key_whose_type_changed_since_planning_fails_before_scanning() {
    let mock = orders_mock();
    let session = session(&mock);
    let PlanningOutcome::Planned { remote, .. } = session.plan_pushdown(&plan(None)).unwrap()
    else {
        panic!("not planned");
    };
    // The table was re-created with a numeric partition key.
    let mut recreated = orders(0);
    recreated["Table"]["TableId"] = json!("a-new-table-id");
    recreated["Table"]["AttributeDefinitions"][0]["AttributeType"] = json!("N");
    mock.reset();
    mock.on("DescribeTable", None, ok(recreated));
    let operation = session.start_read(remote, None).unwrap();
    let error = operation.pull().err().unwrap();
    assert_eq!(error.kind(), ErrorKind::SchemaMismatch);
    assert!(mock.bodies("Scan").is_empty());
}

#[test]
fn a_table_dropped_since_planning_does_not_exist() {
    let mock = orders_mock();
    let session = session(&mock);
    let PlanningOutcome::Planned { remote, .. } = session.plan_pushdown(&plan(None)).unwrap()
    else {
        panic!("not planned");
    };
    mock.reset();
    mock.on(
        "DescribeTable",
        None,
        error(
            400,
            "ResourceNotFoundException",
            "Requested resource not found",
        ),
    );
    let operation = session.start_read(remote, None).unwrap();
    assert_eq!(
        operation.pull().err().unwrap().kind(),
        ErrorKind::DoesNotExist
    );
    assert!(mock.bodies("Scan").is_empty());
}

#[test]
fn a_column_that_is_neither_a_key_attribute_nor_item_does_not_exist() {
    let mock = orders_mock();
    let session = session(&mock);
    let plan = ReadPlan::new([NAMESPACE, "orders"], vec![("total", text(true))])
        .build()
        .encode_to_vec();
    let error = session.plan_pushdown(&plan).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::DoesNotExist);
}

#[test]
fn a_value_of_another_type_is_a_data_error() {
    let mock = orders_mock();
    mock.on(
        "Scan",
        None,
        ok(json!({
            "Items": [{ "pk": { "N": "1" }, "sk": { "B": "AA==" } }],
            "Count": 1,
            "ScannedCount": 1
        })),
    );
    let session = session(&mock);
    let error = read(&session, &plan(None)).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::Data);
    assert!(!error.retryable());
}

#[test]
fn throttling_that_outlasts_the_retries_is_a_retryable_resource_error() {
    let mock = orders_mock();
    mock.on(
        "Scan",
        None,
        error(
            400,
            "ProvisionedThroughputExceededException",
            "The level of configured provisioned throughput for the table was exceeded.",
        ),
    );
    let session = session(&mock);
    let error = read(&session, &plan(None)).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::Resource);
    assert!(error.retryable());
    assert_eq!(
        error.data_source_code(),
        Some("ProvisionedThroughputExceededException")
    );
    // The SDK's standard retry mode makes three attempts.
    assert_eq!(mock.bodies("Scan").len(), 3);
}

#[test]
fn a_server_error_that_outlasts_the_retries_is_a_retryable_data_source_error() {
    let mock = orders_mock();
    mock.on(
        "Scan",
        None,
        error(500, "InternalServerError", "Internal server error"),
    );
    let session = session(&mock);
    let error = read(&session, &plan(None)).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::DataSource);
    assert!(error.retryable());
}

#[test]
fn a_denied_scan_is_permission_denied() {
    let mock = orders_mock();
    mock.on(
        "Scan",
        None,
        error(
            400,
            "AccessDeniedException",
            "not authorized to perform: dynamodb:Scan",
        ),
    );
    let session = session(&mock);
    let error = read(&session, &plan(None)).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    assert!(!error.retryable());
    assert_eq!(mock.bodies("Scan").len(), 1);
}

/// Plans a read of `orders` whose first `Scan` page takes ten seconds.
fn waiting_read(
    mock: &MockDynamoDb,
) -> (
    Arc<SessionState>,
    connector_sdk::runtime::SharedRemoteExecution,
) {
    mock.on(
        "Scan",
        None,
        ok(json!({ "Items": [], "Count": 0, "ScannedCount": 0 }))
            .set_delay(std::time::Duration::from_secs(10)),
    );
    let session = session(mock);
    let PlanningOutcome::Planned { remote, .. } = session.plan_pushdown(&plan(None)).unwrap()
    else {
        panic!("not planned");
    };
    (session, remote)
}

#[test]
fn a_cancel_stops_a_waiting_scan() {
    let mock = orders_mock();
    let (session, remote) = waiting_read(&mock);
    let operation = session.start_read(remote, None).unwrap();
    let started = std::time::Instant::now();
    let error = std::thread::scope(|scope| {
        let pull = scope.spawn(|| operation.pull());
        std::thread::sleep(std::time::Duration::from_millis(300));
        operation.cancel().unwrap();
        pull.join().unwrap()
    })
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn a_read_past_its_timeout_stops_its_scan() {
    let mock = orders_mock();
    let (session, remote) = waiting_read(&mock);
    let operation = session
        .start_read(remote, Some(std::time::Duration::from_millis(300)))
        .unwrap();
    let started = std::time::Instant::now();
    let error = operation.pull().err().unwrap();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}
