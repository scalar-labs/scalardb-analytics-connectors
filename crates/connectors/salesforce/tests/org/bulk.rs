//! Reads of `ConnectorTestBulk__c`, whose records span several query pages:
//! `run.sh prepare` loads records whose `Seq__c` runs from 1 without gaps.

use std::{sync::Arc, time::Instant};

use arrow_array::{Array, Decimal64Array, RecordBatch};
use connector_sdk::{ErrorKind, NameFilter, conformance::plan::*, runtime::SessionState};
use substrait::proto::expression::literal::{Decimal, LiteralType};

use super::{NAMESPACE, Org, reads::plan, soql};

const BULK: &str = "ConnectorTestBulk__c";
/// How many records a query page holds when the query does not ask for
/// fewer, so a read of more records spans several pages.
const PAGE_SIZE: i64 = 2_000;

pub fn check(org: &Org) {
    let session = org.session();
    every_record_reads_across_pages(&session);
    a_filter_selects_records_remotely(&session);
    releasing_a_read_partway_leaves_the_session_usable(&session);
    a_read_cancelled_between_pages_stops_and_leaves_the_session_usable(&session);
    session.close();
}

fn every_record_reads_across_pages(session: &Arc<SessionState>) {
    // Counted by Salesforce in one query, so a read that misses later pages
    // cannot also lower the count it is compared with.
    let total = soql::count(BULK);
    assert!(
        total > PAGE_SIZE,
        "{BULK} holds {total} records; run `run.sh prepare` to load more than one page"
    );
    let started = Instant::now();
    let batches = read_all(session, seqs().build());
    let mut seqs = values(&batches);
    eprintln!(
        "{BULK}: {} of {total} records in {} batches in {:.1?}",
        seqs.len(),
        batches.len(),
        started.elapsed()
    );
    assert!(
        batches.len() > 1,
        "a read of several pages returns several batches"
    );
    seqs.sort_unstable();
    let expected: Vec<i64> = (1..=total).collect();
    assert_eq!(
        seqs, expected,
        "every one of the {total} records is read once"
    );
}

fn a_filter_selects_records_remotely(session: &Arc<SessionState>) {
    let offered = seqs()
        .filter(call(
            "lte",
            vec![
                column(0),
                literal(LiteralType::Decimal(Decimal {
                    value: 100_i128.to_le_bytes().to_vec(),
                    precision: 18,
                    scale: 0,
                })),
            ],
        ))
        .build();
    let mut seqs = values(&read_all(session, offered));
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=100).collect::<Vec<_>>(), "Seq__c <= 100");
}

fn releasing_a_read_partway_leaves_the_session_usable(session: &Arc<SessionState>) {
    let (remote, _) = plan(session, seqs().build());
    let operation = session.start_read(remote, None).expect("the read starts");
    let first = operation
        .pull()
        .unwrap_or_else(|error| panic!("the first pull fails: {error}"));
    assert!(first.is_some(), "the read returns a first batch");
    operation.release();
    operation.close();
    session
        .namespace_list(&NameFilter::None, None, None)
        .expect("the session accepts a request after a read released partway");
}

/// Cancels a read after its first page, while later pages remain at
/// Salesforce, so the cancel reaches a read in progress without depending
/// on timing. A pull already waiting when the cancel arrives is checked
/// against the mock, which can make a query wait.
fn a_read_cancelled_between_pages_stops_and_leaves_the_session_usable(session: &Arc<SessionState>) {
    let (remote, _) = plan(session, seqs().build());
    let operation = session.start_read(remote, None).expect("the read starts");
    let first = operation
        .pull()
        .unwrap_or_else(|error| panic!("the first pull fails: {error}"));
    assert!(first.is_some(), "the read returns a first batch");
    operation.cancel().expect("the operation cancels");
    let error = operation.pull().expect_err("a pull after the cancel fails");
    assert_eq!(error.kind(), ErrorKind::Cancellation, "a cancelled read");
    operation.close();
    session
        .namespace_list(&NameFilter::None, None, None)
        .expect("the session accepts a request after a cancelled read");
}

/// `Seq__c` of `ConnectorTestBulk__c`.
fn seqs() -> ReadPlan {
    ReadPlan::new([NAMESPACE, BULK], vec![("Seq__c", decimal(18, 0, true))])
}

/// Plans `offered`, which must leave no local work, and reads it to the end.
fn read_all(session: &Arc<SessionState>, offered: substrait::proto::Plan) -> Vec<RecordBatch> {
    let (remote, local_work) = plan(session, offered);
    assert!(!local_work, "the region runs remotely");
    let operation = session.start_read(remote, None).expect("the read starts");
    let mut batches = Vec::new();
    while let Some(batch) = operation
        .pull()
        .unwrap_or_else(|error| panic!("the read fails: {error}"))
    {
        batches.push(batch);
    }
    operation.close();
    batches
}

/// The `Seq__c` values of every batch.
fn values(batches: &[RecordBatch]) -> Vec<i64> {
    batches
        .iter()
        .flat_map(|batch| {
            let column = batch
                .column(0)
                .as_any()
                .downcast_ref::<Decimal64Array>()
                .expect("Seq__c is a Decimal(18, 0)");
            column
                .iter()
                .map(|value| value.expect("every record has a Seq__c"))
                .collect::<Vec<_>>()
        })
        .collect()
}
