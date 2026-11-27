//! Key conditions: which filters a `Query` reads as its key condition, the
//! requests it sends, and the filters that stay local over a `Scan`.

use arrow_array::{Array, RecordBatch, StringArray};
use connector_dynamodb::NAMESPACE;
use connector_sdk::{
    conformance::plan::{ReadPlan, binary, call, column, literal, logical, text},
    runtime::{PlanningOutcome, SessionState},
};
use prost::Message;
use serde_json::{Value, json};
use std::sync::Arc;
use substrait::proto::{Expression, expression::literal::LiteralType};

use super::{MockDynamoDb, metadata::orders, ok, session};

fn string(value: &str) -> Expression {
    literal(LiteralType::String(value.to_owned()))
}

fn bytes(value: &[u8]) -> Expression {
    literal(LiteralType::Binary(value.to_vec()))
}

/// A read of `orders` filtered by `filter`, projected to its partition key.
fn filtered(filter: Expression) -> Vec<u8> {
    ReadPlan::new(
        [NAMESPACE, "orders"],
        vec![
            ("pk", text(false)),
            ("sk", binary(false)),
            ("item", logical("json", "{}", false)),
        ],
    )
    .filter(filter)
    .project(vec![(0, "pk")])
    .build()
    .encode_to_vec()
}

/// Plans `plan`, reads its remote execution to the end, and returns whether
/// local work remained and the batches read.
fn read(session: &Arc<SessionState>, plan: &[u8]) -> (bool, Vec<RecordBatch>) {
    let PlanningOutcome::Planned {
        remote, local_plan, ..
    } = session.plan_pushdown(plan).unwrap()
    else {
        panic!("not planned");
    };
    let operation = session.start_read(remote, None).unwrap();
    let mut batches = Vec::new();
    while let Some(batch) = operation.pull().unwrap() {
        batches.push(batch);
    }
    (local_plan.is_some(), batches)
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

/// A mock with `orders` described, whose `Query` and `Scan` both answer
/// with one empty page.
fn orders_mock() -> MockDynamoDb {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "orders" })),
        ok(orders(1)),
    );
    let empty = || ok(json!({ "Items": [], "Count": 0, "ScannedCount": 0 }));
    mock.on("Query", None, empty());
    mock.on("Scan", None, empty());
    mock
}

/// The bodies of the `Query` and `Scan` requests `filter` sends, and whether
/// local work remained.
fn requests(filter: Expression) -> (bool, Vec<Value>, Vec<Value>) {
    let mock = orders_mock();
    let session = session(&mock);
    let (local_work, _) = read(&session, &filtered(filter));
    (local_work, mock.bodies("Query"), mock.bodies("Scan"))
}

#[test]
fn a_partition_key_equality_queries_its_partition_page_by_page() {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "orders" })),
        ok(orders(1)),
    );
    let key = |sk: &str| json!({ "pk": { "S": "a" }, "sk": { "B": sk } });
    mock.on(
        "Query",
        Some(json!({ "ExclusiveStartKey": key("AQ==") })),
        ok(json!({ "Items": [{ "pk": { "S": "a" } }], "Count": 1, "ScannedCount": 1 })),
    );
    mock.on(
        "Query",
        None,
        ok(json!({
            "Items": [{ "pk": { "S": "a" } }],
            "Count": 1,
            "ScannedCount": 1,
            "LastEvaluatedKey": key("AQ==")
        })),
    );
    let session = session(&mock);
    let (local_work, batches) = read(
        &session,
        &filtered(call("equal", vec![column(0), string("a")])),
    );
    assert!(!local_work);
    assert_eq!(batches.len(), 2);
    assert_eq!(texts(&batches[0], 0), ["a"]);
    assert_eq!(texts(&batches[1], 0), ["a"]);

    let queries = mock.bodies("Query");
    assert_eq!(
        queries[0],
        json!({
            "TableName": "orders",
            "KeyConditionExpression": "#k0 = :k0",
            "ProjectionExpression": "#a0",
            "ExpressionAttributeNames": { "#a0": "pk", "#k0": "pk" },
            "ExpressionAttributeValues": { ":k0": { "S": "a" } },
            "ConsistentRead": false
        })
    );
    assert_eq!(queries[1]["ExclusiveStartKey"], key("AQ=="));
    assert!(mock.bodies("Scan").is_empty());
}

#[test]
fn one_sort_key_comparison_joins_the_key_condition_and_the_rest_stays_local() {
    let (local_work, queries, scans) = requests(call(
        "and",
        vec![
            call(
                "and",
                vec![
                    // The literal on the left is compared with its sides swapped.
                    call("lt", vec![bytes(&[0x01]), column(1)]),
                    call("equal", vec![column(0), string("a")]),
                ],
            ),
            call(
                "and",
                vec![
                    call("lte", vec![column(1), bytes(&[0xff])]),
                    call("equal", vec![column(0), string("b")]),
                ],
            ),
        ],
    ));
    assert!(local_work);
    assert!(scans.is_empty());
    let [query] = queries.as_slice() else {
        panic!("{queries:?}");
    };
    assert_eq!(query["KeyConditionExpression"], "#k0 = :k0 AND #k1 > :k1");
    assert_eq!(
        query["ExpressionAttributeNames"],
        json!({ "#k0": "pk", "#k1": "sk" })
    );
    assert_eq!(
        query["ExpressionAttributeValues"],
        json!({ ":k0": { "S": "a" }, ":k1": { "B": "AQ==" } })
    );
    // The local filter reads `sk` and the projection stays local above it,
    // so whole items are read.
    assert!(query.get("ProjectionExpression").is_none());
}

#[test]
fn filters_a_key_condition_cannot_evaluate_scan_and_stay_local() {
    let cases = [
        // A sort key comparison without the partition key.
        call("equal", vec![column(1), bytes(&[0x01])]),
        // The partition key may only be compared for equality.
        call("gt", vec![column(0), string("a")]),
        call("not_equal", vec![column(0), string("a")]),
        // DynamoDB refuses an empty key value.
        call("equal", vec![column(0), string("")]),
        // DynamoDB refuses a partition key value over 2048 bytes.
        call("equal", vec![column(0), string(&"x".repeat(2049))]),
        // A disjunction is no key condition.
        call(
            "or",
            vec![
                call("equal", vec![column(0), string("a")]),
                call("equal", vec![column(0), string("b")]),
            ],
        ),
        call("not", vec![call("equal", vec![column(0), string("a")])]),
    ];
    for filter in cases {
        let (local_work, queries, scans) = requests(filter);
        assert!(local_work);
        assert!(queries.is_empty());
        assert_eq!(scans.len(), 1);
    }
}

#[test]
fn a_query_of_the_item_column_reads_whole_items() {
    let mock = orders_mock();
    let session = session(&mock);
    let plan = ReadPlan::new(
        [NAMESPACE, "orders"],
        vec![
            ("pk", text(false)),
            ("sk", binary(false)),
            ("item", logical("json", "{}", false)),
        ],
    )
    .filter(call("equal", vec![column(0), string("a")]))
    .project(vec![(2, "item")])
    .build()
    .encode_to_vec();
    let (local_work, _) = read(&session, &plan);
    assert!(!local_work);
    assert_eq!(
        mock.bodies("Query"),
        [json!({
            "TableName": "orders",
            "KeyConditionExpression": "#k0 = :k0",
            "ExpressionAttributeNames": { "#k0": "pk" },
            "ExpressionAttributeValues": { ":k0": { "S": "a" } },
            "ConsistentRead": false
        })]
    );
}
