//! The reads, source changes, and failing requests the contract expects of
//! the fixture's tables.

use std::{net::TcpListener, sync::Arc};

use arrow_array::{ArrayRef, BinaryArray, StringArray};
use aws_sdk_dynamodb::types::ScalarAttributeType;
use connector_dynamodb::NAMESPACE;
use connector_sdk::{
    ErrorKind,
    conformance::{
        DriftCase, DriftOutcome, ErrorCase, Expected, ExpectedError, ExpectedPlanning,
        FailingRequest, ReadCase, ReadResult, plan::*,
    },
};
use substrait::proto::{Expression, expression::literal::LiteralType};

use super::{
    DynamoDbFixture, Policy, options,
    tables::{DOCUMENT_JSON, PARTITION, blob_keys, text_keys},
};

pub fn read_cases() -> Vec<ReadCase> {
    let texts = text_keys();
    let blobs = blob_keys();
    let partition =
        |count: usize| -> ArrayRef { Arc::new(StringArray::from(vec![PARTITION; count])) };
    let text_items: Vec<String> = texts
        .iter()
        .map(|key| {
            format!(
                r#"{{"pk":{{"S":"{PARTITION}"}},"sk":{{"S":{}}}}}"#,
                serde_json::to_string(key).unwrap()
            )
        })
        .collect();
    vec![
        ReadCase {
            name: "a partition key equality queries S keys in UTF-8 byte order with their items",
            offered: keyed("texts", text(false))
                .filter(call("equal", vec![column(0), string(PARTITION)]))
                .build(),
            expected: remote(ReadResult::Rows(vec![
                partition(texts.len()),
                Arc::new(StringArray::from(texts.clone())),
                Arc::new(StringArray::from(text_items)),
            ])),
        },
        ReadCase {
            name: "a sort key range queries B keys in unsigned byte order",
            offered: keyed("blobs", binary(false))
                .filter(call(
                    "and",
                    vec![
                        call("equal", vec![column(0), string(PARTITION)]),
                        call("gte", vec![column(1), bytes(&[0x01])]),
                    ],
                ))
                .project(vec![(1, "sk")])
                .build(),
            expected: remote(ReadResult::Rows(vec![Arc::new(BinaryArray::from(
                blobs[2..].iter().map(Vec::as_slice).collect::<Vec<_>>(),
            ))])),
        },
        ReadCase {
            name: "item holds every DynamoDB type as DynamoDB JSON",
            offered: ReadPlan::new(
                [NAMESPACE, "documents"],
                vec![("pk", text(false)), ("item", json())],
            )
            .filter(call("equal", vec![column(0), string("doc")]))
            .project(vec![(1, "item")])
            .build(),
            expected: remote(ReadResult::Rows(vec![Arc::new(StringArray::from(vec![
                DOCUMENT_JSON,
            ]))])),
        },
        ReadCase {
            name: "a table keyed by a number is read through item",
            offered: ReadPlan::new([NAMESPACE, "numbers"], vec![("item", json())]).build(),
            expected: remote(ReadResult::Rows(vec![Arc::new(StringArray::from(vec![
                r#"{"id":{"N":"1"}}"#,
            ]))])),
        },
        ReadCase {
            name: "a filter without the partition key stays local over a scan",
            offered: ReadPlan::new(
                [NAMESPACE, "texts"],
                vec![("pk", text(false)), ("sk", text(false))],
            )
            .filter(call("equal", vec![column(1), string("a")]))
            .project(vec![(0, "pk")])
            .build(),
            // One partition, so the scan returns its items in sort key order.
            expected: ExpectedPlanning::Planned {
                local_work: true,
                result: Expected::Contract(ReadResult::Rows(vec![
                    partition(texts.len()),
                    Arc::new(StringArray::from(texts)),
                ])),
            },
        },
    ]
}

pub fn drift_cases(fixture: &DynamoDbFixture) -> Vec<DriftCase<'_>> {
    let read = |table| ReadPlan::new([NAMESPACE, table], vec![("pk", text(false))]).build();
    vec![
        DriftCase {
            name: "a table re-created with a number key",
            offered: read("drift_type"),
            change: Box::new(|| {
                fixture.admin(async |admin| {
                    admin.delete("drift_type").await;
                    admin
                        .create("drift_type", ("pk", ScalarAttributeType::N), None)
                        .await;
                });
            }),
            expected: DriftOutcome::Fails(ErrorKind::SchemaMismatch),
        },
        DriftCase {
            name: "a deleted table",
            offered: read("drift_dropped"),
            change: Box::new(|| {
                fixture.admin(async |admin| admin.delete("drift_dropped").await);
            }),
            expected: DriftOutcome::Fails(ErrorKind::DoesNotExist),
        },
        DriftCase {
            name: "a table re-created with the same key",
            offered: read("drift_same"),
            change: Box::new(|| {
                fixture.admin(async |admin| {
                    admin.delete("drift_same").await;
                    admin
                        .create("drift_same", ("pk", ScalarAttributeType::S), None)
                        .await;
                });
            }),
            expected: DriftOutcome::Reads,
        },
    ]
}

pub fn error_cases(fixture: &DynamoDbFixture) -> Vec<ErrorCase> {
    let failing = |kind, retryable| Expected::Contract(ExpectedError { kind, retryable });
    let literal_read = |table| {
        FailingRequest::Read(Box::new(
            ReadPlan::new([NAMESPACE, table], vec![("pk", text(false))])
                .filter(call(
                    "equal",
                    vec![column(0), string("conformance-literal")],
                ))
                .build(),
        ))
    };
    // A port nothing listens on.
    let closed = TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a local port is free");
    vec![
        ErrorCase {
            name: "reading a table the credentials may not describe",
            options: fixture.options(Policy::Outsider),
            request: literal_read("locked"),
            expected: failing(ErrorKind::NotFound, false),
        },
        ErrorCase {
            name: "querying a table the credentials may describe but not read",
            options: fixture.options(Policy::Outsider),
            request: literal_read("texts"),
            expected: failing(ErrorKind::PermissionDenied, false),
        },
        ErrorCase {
            name: "credentials DynamoDB does not recognize",
            options: fixture.options(Policy::Rejected),
            request: FailingRequest::OpenSession,
            expected: failing(ErrorKind::Authentication, false),
        },
        ErrorCase {
            name: "an endpoint that refuses connections",
            options: options(&format!("http://{closed}"), Policy::Reader),
            request: FailingRequest::OpenSession,
            expected: failing(ErrorKind::Connection, true),
        },
    ]
}

/// A read of `table`, keyed by the string `pk` and the sort key `sk` of
/// `sort`, with its `item` column.
fn keyed(table: &str, sort: substrait::proto::Type) -> ReadPlan {
    ReadPlan::new(
        [NAMESPACE, table],
        vec![("pk", text(false)), ("sk", sort), ("item", json())],
    )
}

fn json() -> substrait::proto::Type {
    logical("json", "{}", false)
}

fn remote(result: ReadResult) -> ExpectedPlanning {
    ExpectedPlanning::Planned {
        local_work: false,
        result: Expected::Contract(result),
    }
}

fn string(value: &str) -> Expression {
    literal(LiteralType::String(value.to_owned()))
}

fn bytes(value: &[u8]) -> Expression {
    literal(LiteralType::Binary(value.to_vec()))
}
