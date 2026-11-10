//! The C boundary: handles, the Arrow C stream, error structures, and text.
//!
//! The contract's calling conventions themselves are tested on the runtime in
//! connector-sdk; these tests check that they survive the C representation.

mod support;

use std::{
    slice,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
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

#[test]
fn a_read_returns_the_planned_batches_through_the_arrow_stream() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["batches", "2"]);

    let Read {
        operation,
        mut stream,
    } = session.read(&remote, 0).unwrap();

    let schema = stream.schema();
    assert_eq!(schema.fields().len(), 1);
    assert_eq!(schema.field(0).name(), "value");
    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
    assert_eq!(stream.pull(), Pull::Batch(1));
    assert_eq!(probe.sent.load(Ordering::SeqCst), 1);
    assert_eq!(stream.pull(), Pull::Batch(1));
    assert_eq!(stream.pull(), Pull::End);
    assert!(operation.error().is_none());
    stream.release();
    operation.close();
    session.close();
    data_source.close();
}

#[test]
fn cancelling_from_another_thread_stops_a_blocked_pull() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["block"]);
    let Read {
        operation,
        mut stream,
    } = session.read(&remote, 0).unwrap();

    let pulled = thread::scope(|scope| {
        let puller = scope.spawn(move || stream.pull());
        wait_until(|| probe.sent.load(Ordering::SeqCst) == 1);
        operation.cancel().unwrap();
        puller.join().unwrap()
    });

    assert!(matches!(pulled, Pull::Failed(_, message) if message.contains("cancelled")));
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
    let error = operation.error().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::Cancellation
    );
    assert!(!error.retryable);
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert!(session.read(&remote, 0).is_ok());
}

#[test]
fn releasing_a_stream_partway_stops_the_query_and_frees_the_session() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["batches", "3"]);
    let mut read = session.read(&remote, 0).unwrap();
    assert_eq!(read.stream.pull(), Pull::Batch(1));

    read.stream.release();

    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    let mut next = session.read(&remote, 0).unwrap();
    assert_eq!(next.stream.pull(), Pull::Batch(1));
}

#[test]
fn calls_on_a_child_after_its_parent_closed_return_invalid_state() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["batches", "2"]);
    let mut read = session.read(&remote, 0).unwrap();
    assert_eq!(read.stream.pull(), Pull::Batch(1));

    data_source.close();

    let error = session.plan(&["batches", "1"]).err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );
    assert!(matches!(read.stream.pull(), Pull::Failed(..)));
    assert_eq!(
        read.operation.error().unwrap().kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    read.stream.release();
    read.operation.close();
    session.close();
}

#[test]
fn a_stream_pulled_after_its_operation_closed_returns_invalid_state() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["batches", "2"]);
    let Read {
        operation,
        mut stream,
    } = session.read(&remote, 0).unwrap();
    assert_eq!(stream.pull(), Pull::Batch(1));

    operation.close();

    assert!(matches!(stream.pull(), Pull::Failed(_, message) if message.contains("invalid state")));
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert!(session.read(&remote, 0).is_ok());
}

#[test]
fn a_failed_pull_leaves_the_structured_error_on_the_operation() {
    initialize();
    let (data_source, _probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["fail"]);
    let Read {
        operation,
        mut stream,
    } = session.read(&remote, 0).unwrap();

    let Pull::Failed(_, message) = stream.pull() else {
        panic!("the pull must fail");
    };

    assert!(message.contains("the test source failed"));
    assert!(!message.contains("could not serialize"));
    let error = operation.error().unwrap();
    assert_eq!(error.kind, ScalardbAnalyticsConnectorErrorKind::DataSource);
    assert!(error.retryable);
    assert_eq!(
        read_string(error.message).as_deref(),
        Some("the test source failed")
    );
    assert_eq!(
        read_string(error.data_source_code).as_deref(),
        Some("40001")
    );
    // Text from the Data Source keeps an embedded U+0000 through its length.
    assert_eq!(
        read_string(error.data_source_message).as_deref(),
        Some("could not serialize\0access")
    );
    // SAFETY: the error owns `diagnostic_count` diagnostics.
    let diagnostics = unsafe { slice::from_raw_parts(error.diagnostics, error.diagnostic_count) };
    let keys: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| read_string(diagnostic.key).unwrap())
        .collect();
    assert_eq!(keys, ["vendor_code", "detail", "detail"]);
    assert_eq!(
        diagnostics[0].value_type,
        ScalardbAnalyticsConnectorDiagnosticType::Int32
    );
    // SAFETY: each payload is read through the member its type names.
    unsafe {
        assert_eq!(diagnostics[0].value.int32, -7);
        assert_eq!(
            read_string(diagnostics[1].value.utf8).as_deref(),
            Some("first")
        );
        let detail = diagnostics[2].value.bytes;
        assert_eq!(slice::from_raw_parts(detail.data, detail.length), [0, 1, 2]);
    }
    assert_eq!(
        diagnostics[2].value_type,
        ScalardbAnalyticsConnectorDiagnosticType::Bytes
    );
    // The session stays usable after a Data Source error.
    assert!(session.read(&remote, 0).is_ok());
}

#[test]
fn a_panic_in_a_pull_becomes_internal_and_makes_the_session_unusable() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["panic"]);
    let Read {
        operation,
        mut stream,
    } = session.read(&remote, 0).unwrap();

    assert!(matches!(stream.pull(), Pull::Failed(..)));

    assert_eq!(
        operation.error().unwrap().kind,
        ScalardbAnalyticsConnectorErrorKind::Internal
    );
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    let error = session.plan(&["batches", "1"]).err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidState
    );
    assert!(data_source.session().is_ok());
}

#[test]
fn a_stream_released_after_every_parent_closed_releases_the_read() {
    initialize();
    let (data_source, probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();
    let remote = session.remote(&["batches", "2"]);
    let mut read = session.read(&remote, 0).unwrap();
    assert_eq!(read.stream.pull(), Pull::Batch(1));
    drop(remote);

    session.close();
    data_source.close();
    read.operation.close();
    read.stream.release();

    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
}

#[test]
fn invalid_options_return_invalid_settings_naming_the_option() {
    initialize();
    for (options, named) in [
        (&[("token", "hunter2")][..], "label"),
        (&[("label", "a"), ("password", "hunter2")][..], "password"),
    ] {
        let error = DataSource::open(options).err().unwrap();
        assert_eq!(
            error.kind,
            ScalardbAnalyticsConnectorErrorKind::InvalidSettings
        );
        assert!(error.message.contains(named), "{}", error.message);
        assert!(!error.message.contains("hunter2"));
    }
    let unknown = text("unknown");
    let mut data_source = std::ptr::null_mut();
    // SAFETY: every input lives for the call and the output is writable.
    let error = check(unsafe {
        scalardb_analytics_connector_data_source_open(
            unknown.as_ptr(),
            std::ptr::null(),
            0,
            0,
            &mut data_source,
        )
    })
    .err()
    .unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidSettings
    );
}

#[test]
fn a_remote_execution_restored_from_bytes_reads_on_another_session() {
    initialize();
    let (planning_source, _probe) = DataSource::open_probed();
    let encoded = planning_source
        .session()
        .unwrap()
        .remote(&["batches", "1"])
        .encode();
    planning_source.close();

    let (reading_source, probe) = DataSource::open_probed();
    let session = reading_source.session().unwrap();
    let remote = session.restore(&encoded).unwrap();
    let mut read = session.read(&remote, 0).unwrap();

    assert_eq!(read.stream.pull(), Pull::Batch(1));
    assert_eq!(probe.sent.load(Ordering::SeqCst), 1);
    let error = session.restore(b"not a remote execution").err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidRequest
    );
}

#[test]
fn an_unsupported_plan_has_a_reason_and_no_remote_execution() {
    initialize();
    let (data_source, _probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();

    let result = session.plan(&["unknown-table"]).unwrap();

    // SAFETY: the result is live.
    unsafe {
        assert!(!scalardb_analytics_connector_pushdown_result_is_planned(
            result.0
        ));
        assert!(
            read_string(scalardb_analytics_connector_pushdown_result_unsupported_reason(result.0))
                .is_some()
        );
        assert!(
            scalardb_analytics_connector_pushdown_result_remote_plan(result.0)
                .data
                .is_null()
        );
    }
    let error = result.remote_execution().err().unwrap();
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidRequest
    );
}

#[test]
fn a_planned_result_carries_the_remote_plan() {
    initialize();
    let (data_source, _probe) = DataSource::open_probed();
    let session = data_source.session().unwrap();

    let result = session.plan(&["batches", "1"]).unwrap();

    // SAFETY: the result is live.
    unsafe {
        assert!(scalardb_analytics_connector_pushdown_result_is_planned(
            result.0
        ));
        let remote_plan = scalardb_analytics_connector_pushdown_result_remote_plan(result.0);
        assert!(remote_plan.length > 0);
        assert!(
            scalardb_analytics_connector_pushdown_result_local_plan(result.0)
                .data
                .is_null()
        );
    }
}
