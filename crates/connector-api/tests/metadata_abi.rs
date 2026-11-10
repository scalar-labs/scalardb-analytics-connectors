//! Metadata requests through the C boundary: name filters, the Arrow table
//! input, the rows as a C caller imports them, and the calling conventions
//! they share with reads.
//!
//! The row contents are tested on the runtime in connector-sdk; these tests
//! check that they survive the C representation.

mod support;

use std::{
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

use analytics_types::{AnalyticsField, AnalyticsType, DecimalType};
use arrow_array::{
    ArrayRef, Int64Array, RecordBatch, RecordBatchIterator, cast::AsArray,
    ffi_stream::FFI_ArrowArrayStream,
};
use connector_api::abi::*;
use support::*;

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached in time");
        thread::sleep(Duration::from_millis(5));
    }
}

fn batches(stream: &mut Stream) -> Vec<RecordBatch> {
    std::iter::from_fn(|| stream.next_batch()).collect()
}

fn state(batch: &RecordBatch) -> String {
    batch
        .column_by_name("state")
        .unwrap()
        .as_string::<i32>()
        .value(0)
        .to_owned()
}

fn kind(error: Error) -> ScalardbAnalyticsConnectorErrorKind {
    error.kind
}

#[test]
fn each_request_returns_its_schema_before_the_source_is_queried() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();

    let has_field = |started: Result<Read, Error>, field: &str| {
        let Read {
            operation,
            mut stream,
        } = started.unwrap();
        assert!(stream.schema().field_with_name(field).is_ok());
        stream.release();
        operation.close();
    };
    has_field(session.namespace_list(None, 0), "namespace");
    has_field(session.table_list(None, None), "kind");
    has_field(session.table_metadata(&["orders"], 0), "source_metadata");

    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
}

#[test]
fn name_filters_match_exact_names_and_null_matches_every_name() {
    initialize();
    let (data_source, _) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let count = |namespaces: Option<&[&str]>| {
        let Read {
            operation,
            mut stream,
        } = session.namespace_list(namespaces, 0).unwrap();
        let count = batches(&mut stream).len();
        stream.release();
        operation.close();
        count
    };

    assert_eq!(count(None), 3);
    assert_eq!(count(Some(&["public", "sales"])), 2);
    assert_eq!(count(Some(&["Public"])), 0);
    assert_eq!(count(Some(&[])), 0);

    let Read {
        operation,
        mut stream,
    } = session
        .table_list(Some(&["public"]), Some(&["orders"]))
        .unwrap();
    let rows = batches(&mut stream);
    assert_eq!(rows.len(), 1);
    assert_eq!(state(&rows[0]), "listed");
    stream.release();
    operation.close();
}

#[test]
fn table_metadata_rows_reach_a_c_caller_with_their_analytics_types() {
    initialize();
    let (data_source, _) = DataSource::open_probed();
    let session = data_source.session().unwrap();

    let Read {
        operation,
        mut stream,
    } = session
        .table_metadata(&["orders", "events", "missing", "secret"], 0)
        .unwrap();
    let rows = batches(&mut stream);

    let states: Vec<_> = rows.iter().map(state).collect();
    assert_eq!(
        states,
        [
            "complete",
            "incomplete",
            "does_not_exist",
            "could_not_be_read"
        ]
    );
    let columns = rows[0]
        .column_by_name("columns")
        .unwrap()
        .as_list::<i32>()
        .value(0);
    let types = columns
        .as_struct()
        .column_by_name("type")
        .unwrap()
        .as_struct()
        .column_by_name("analytics_type")
        .unwrap()
        .as_binary::<i32>()
        .value(1)
        .to_vec();
    let schema = arrow_ipc::convert::try_schema_from_ipc_buffer(&types).unwrap();
    assert_eq!(
        AnalyticsField::try_from_arrow(schema.field(0)).unwrap(),
        AnalyticsField::new(
            "",
            AnalyticsType::Decimal(DecimalType::new(10, 2).unwrap()),
            true
        )
    );
    let library_version = rows[0]
        .column_by_name("retrieval")
        .unwrap()
        .as_struct()
        .column_by_name("library_version")
        .unwrap()
        .as_string::<i32>()
        .value(0)
        .to_owned();
    assert_eq!(library_version, env!("CARGO_PKG_VERSION"));
    assert!(operation.error().is_none());
    stream.release();
    operation.close();
}

#[test]
fn a_malformed_table_input_is_an_invalid_request_and_is_still_released() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let wrong_type: ArrayRef = Arc::new(Int64Array::from(vec![1]));
    let batch = RecordBatch::try_from_iter([("table", wrong_type)]).unwrap();
    let schema = batch.schema();
    let input = FFI_ArrowArrayStream::new(Box::new(RecordBatchIterator::new([Ok(batch)], schema)));

    let error = session.table_metadata_from(input, 0).err().unwrap();

    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidRequest
    );
    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
    assert!(session.table_metadata(&["orders"], 0).is_ok());
}

#[test]
fn a_failure_partway_ends_the_stream_after_complete_rows_with_the_structured_error() {
    initialize();
    let (data_source, _) = DataSource::open_probed();
    let session = data_source.session().unwrap();

    let Read {
        operation,
        mut stream,
    } = session.table_metadata(&["orders", "fail"], 0).unwrap();

    assert_eq!(stream.pull(), Pull::Batch(1));
    assert!(matches!(stream.pull(), Pull::Failed(..)));
    let error = operation.error().unwrap();
    assert_eq!(error.kind, ScalardbAnalyticsConnectorErrorKind::DataSource);
    assert_eq!(stream.pull(), Pull::End);
    stream.release();
    operation.close();
}

#[test]
fn cancelling_from_another_thread_stops_a_blocked_metadata_pull() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let Read {
        operation,
        mut stream,
    } = session.namespace_list(Some(&["block"]), 0).unwrap();

    let pulled = thread::scope(|scope| {
        let puller = scope.spawn(move || {
            let pulled = stream.pull();
            stream.release();
            pulled
        });
        wait_until(|| probe.sent.load(Ordering::SeqCst) == 1);
        operation.cancel().unwrap();
        puller.join().unwrap()
    });

    assert!(matches!(pulled, Pull::Failed(..)));
    assert_eq!(
        operation.error().unwrap().kind,
        ScalardbAnalyticsConnectorErrorKind::Cancellation
    );
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
    operation.close();
}

#[test]
fn the_timeout_stops_a_blocked_metadata_pull() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let Read {
        operation,
        mut stream,
    } = session.table_metadata(&["block"], 50).unwrap();

    assert!(matches!(stream.pull(), Pull::Failed(..)));

    assert_eq!(
        operation.error().unwrap().kind,
        ScalardbAnalyticsConnectorErrorKind::Timeout
    );
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
    stream.release();
    operation.close();
}

#[test]
fn a_running_operation_rejects_others_until_its_stream_is_released() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["batches", "3"]);
    let Read {
        operation,
        mut stream,
    } = session.table_list(None, None).unwrap();
    assert_eq!(stream.pull(), Pull::Batch(1));

    let busy = [
        session.read(&remote, 0).err().map(kind),
        session.namespace_list(None, 0).err().map(kind),
    ];
    assert_eq!(
        busy,
        [Some(ScalardbAnalyticsConnectorErrorKind::InvalidState); 2]
    );

    stream.release();

    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    let Read {
        operation: next,
        stream: mut next_stream,
    } = session.table_metadata(&["orders"], 0).unwrap();
    assert_eq!(next_stream.pull(), Pull::Batch(1));
    next_stream.release();
    next.close();
    operation.close();
}
