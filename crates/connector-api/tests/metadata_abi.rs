//! Metadata requests through the C ABI with the test Connector: results as C
//! structures, name filters, cancellation, the timeout, and session rules.

mod support;

use std::{sync::atomic::Ordering, thread, time::Duration};

use connector_api::abi::*;
use support::*;

fn kind(error: Error) -> ScalardbAnalyticsConnectorErrorKind {
    error.kind
}

#[test]
fn name_filters_match_exact_names_and_null_matches_every_name() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();

    let all = session.namespace_list(None, 0, None).unwrap();
    // SAFETY: the library returns `entry_count` entries.
    let entries = unsafe { items(all.entries, all.entry_count) };
    let listed: Vec<_> = entries
        .iter()
        .map(|entry| match entry.state {
            ScalardbAnalyticsConnectorListedState::ITEM => read_names(&entry.namespace),
            _ => read_names(&entry.scope),
        })
        .collect();
    assert_eq!(
        listed,
        [
            ["test", "public"],
            ["test", "sales"],
            ["test", "restricted"]
        ]
    );
    assert_eq!(
        entries[2].state,
        ScalardbAnalyticsConnectorListedState::UNREADABLE
    );
    assert_eq!(
        read_string(entries[2].reason).as_deref(),
        Some("permission denied for the namespace")
    );

    let none = session.namespace_list(Some(&["Sales"]), 0, None).unwrap();
    assert_eq!(none.entry_count, 0, "names match without case folding");

    let tables = session
        .table_list(Some(&["sales"]), Some(&["events"]))
        .unwrap();
    // SAFETY: the library returns `entry_count` entries.
    let entries = unsafe { items(tables.entries, tables.entry_count) };
    assert_eq!(entries.len(), 1);
    assert_eq!(read_names(&entries[0].table), ["test", "sales", "events"]);
    assert_eq!(entries[0].kind, ScalardbAnalyticsConnectorTableKind::OTHER);
    assert_eq!(
        read_string(entries[0].other_kind).as_deref(),
        Some("partitioned table")
    );
}

#[test]
fn table_records_reach_a_c_caller_with_their_table_metadata_and_source_metadata() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();

    let result = session
        .table_metadata(&["orders", "events", "missing", "secret"], 0, None)
        .unwrap();

    let records = result.records();
    let states: Vec<_> = records.iter().map(|record| record.state).collect();
    assert_eq!(
        states,
        [
            ScalardbAnalyticsConnectorTableRecordState::COMPLETE,
            ScalardbAnalyticsConnectorTableRecordState::INCOMPLETE,
            ScalardbAnalyticsConnectorTableRecordState::DOES_NOT_EXIST,
            ScalardbAnalyticsConnectorTableRecordState::COULD_NOT_BE_READ,
        ]
    );
    for record in &records[2..] {
        assert!(record.table_metadata.is_null() && record.source_metadata.is_null());
    }
    assert_eq!(
        read_string(records[3].reason).as_deref(),
        Some("permission denied for the table")
    );

    let orders = result.table_metadata();
    assert_eq!(read_names(&orders.table), ["test", "public", "orders"]);
    assert_eq!(orders.kind, ScalardbAnalyticsConnectorTableKind::TABLE);
    assert_eq!(
        read_string(orders.source_id.value).as_deref(),
        Some("16384")
    );
    // SAFETY: the library returns `column_count` columns.
    let columns = unsafe { items(orders.columns, orders.column_count) };
    let amount = &columns[1];
    assert_eq!(read_string(amount.name).as_deref(), Some("amount"));
    assert_eq!(
        amount.type_state,
        ScalardbAnalyticsConnectorColumnTypeState::MAPPED
    );
    assert_eq!(
        amount.analytics_type.kind,
        ScalardbAnalyticsConnectorAnalyticsTypeKind::DECIMAL
    );
    assert_eq!(
        (
            amount.analytics_type.decimal_precision,
            amount.analytics_type.decimal_scale
        ),
        (10, 2)
    );
    let tags = &columns[3].analytics_type;
    assert_eq!(
        tags.kind,
        ScalardbAnalyticsConnectorAnalyticsTypeKind::ARRAY
    );
    // SAFETY: an array type points to its element type.
    let element = unsafe { &*tags.element_type };
    assert_eq!(
        element.kind,
        ScalardbAnalyticsConnectorAnalyticsTypeKind::TEXT
    );
    assert_eq!(
        columns[4].unsupported_reason,
        ScalardbAnalyticsConnectorUnsupportedReason::NOT_MAPPED
    );
    assert_eq!(
        orders.constraints_state,
        ScalardbAnalyticsConnectorReportedState::VALUE
    );
    assert_eq!(orders.constraint_count, 4);

    let source_metadata = result.source_metadata();
    assert_eq!(
        read_string(source_metadata.connector).as_deref(),
        Some("test")
    );
    let document: serde_json::Value =
        serde_json::from_str(&read_string(source_metadata.document.text).unwrap()).unwrap();
    assert_eq!(document["fixture"], "orders");

    assert_eq!(
        read_string(result.retrieval.connector.name).as_deref(),
        Some("test")
    );
    assert!(result.retrieval.adbc_driver.name.data.is_null());
    assert_eq!(
        read_string(result.retrieval.library_version).as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn a_requested_table_with_no_names_is_an_invalid_request() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();

    let error = session
        .table_metadata_of(&[&["test", "public", "orders"], &[]], 0, None)
        .err()
        .unwrap();

    assert_eq!(
        kind(error),
        ScalardbAnalyticsConnectorErrorKind::InvalidRequest
    );
}

#[test]
fn a_failure_returns_only_the_error_and_the_session_accepts_the_next_request() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();

    let error = session
        .table_metadata(&["orders", "fail", "events"], 0, None)
        .err()
        .unwrap();

    assert_eq!(error.kind, ScalardbAnalyticsConnectorErrorKind::DataSource);
    assert_eq!(
        session
            .table_metadata(&["orders"], 0, None)
            .unwrap()
            .record_count,
        1
    );
}

#[test]
fn cancelling_from_another_thread_stops_a_blocked_metadata_request() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let cancellation = Cancellation::new();

    let error = thread::scope(|scope| {
        let cancellation = &cancellation;
        let request = scope.spawn(move || {
            session
                .namespace_list(Some(&["block"]), 0, Some(cancellation))
                .err()
                .unwrap()
        });
        while probe.sent.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(5));
        }
        cancellation.cancel().unwrap();
        request.join().unwrap()
    });

    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::Cancellation
    );
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
}

#[test]
fn a_cancellation_given_before_a_request_sends_nothing() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let cancellation = Cancellation::new();
    cancellation.cancel().unwrap();

    let error = session
        .table_metadata(&["orders"], 0, Some(&cancellation))
        .err()
        .unwrap();

    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::Cancellation
    );
    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
}

#[test]
fn a_null_output_is_rejected_before_the_source_is_queried() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let orders = Names::new(&["test", "public", "orders"]);
    let tables = [orders.view()];

    // SAFETY: the handle and names are live; the null output is the case
    // under test.
    let error = check(unsafe {
        (api().table_metadata)(
            session.0,
            tables.as_ptr(),
            tables.len(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    })
    .unwrap_err();

    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidRequest
    );
    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
}

#[test]
fn the_timeout_stops_a_blocked_metadata_request() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();

    let error = session
        .table_metadata(&["orders", "block"], 50, None)
        .err()
        .unwrap();

    assert_eq!(error.kind, ScalardbAnalyticsConnectorErrorKind::Timeout);
    assert!(error.retryable);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
}

#[test]
fn a_running_read_rejects_metadata_requests_until_its_stream_is_released() {
    initialize();
    let session = DataSource::open_probed().0.session().unwrap();
    let remote = session.remote(&["batches", "2"]);
    let read = session.read(&remote, 0).unwrap();

    let error = session.namespace_list(None, 0, None).err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );

    read.stream.release();
    read.operation.close();
    assert!(session.namespace_list(None, 0, None).is_ok());
}
