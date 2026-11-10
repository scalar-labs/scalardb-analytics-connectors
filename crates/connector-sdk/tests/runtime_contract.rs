//! The contract's units and calling conventions, driven through the runtime
//! with the in-memory test Connector.

use std::{
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

use connector_sdk::{
    ConnectorError, ErrorKind, LibrarySettings, SecretString,
    runtime::{
        BuiltInConnector, DataSourceState, Library, PlanningOutcome, ReadOperation, SessionState,
        SharedRemoteExecution,
    },
    test_connector::{Probe, TestConnectorLibrary, probe},
};
use prost::Message;
use substrait::proto::{
    NamedStruct, Plan, PlanRel, ReadRel, Rel, RelRoot, Type, plan_rel, read_rel, rel, r#type,
};

fn library() -> Arc<Library> {
    Library::initialize(
        &[BuiltInConnector::of::<TestConnectorLibrary>()],
        &LibrarySettings::default(),
    )
    .unwrap()
}

fn options(pairs: &[(&str, &str)]) -> Vec<(String, SecretString)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), SecretString::new((*value).to_owned())))
        .collect()
}

/// Opens a test Data Source with a probe no other test uses.
fn open_probed(library: &Arc<Library>, label: &str) -> (Arc<DataSourceState>, Arc<Probe>) {
    let label = format!("runtime-{label}");
    let data_source = library
        .open_data_source("test", options(&[("label", &label)]), None)
        .unwrap();
    (data_source, probe(&label))
}

/// Encodes a Substrait plan that reads one `bigint` column `value` from `table`.
fn read_plan(table: &[&str]) -> Vec<u8> {
    let read = ReadRel {
        base_schema: Some(NamedStruct {
            names: vec!["value".to_owned()],
            r#struct: Some(r#type::Struct {
                types: vec![Type {
                    kind: Some(r#type::Kind::I64(r#type::I64 {
                        nullability: r#type::Nullability::Required as i32,
                        ..Default::default()
                    })),
                }],
                nullability: r#type::Nullability::Required as i32,
                ..Default::default()
            }),
        }),
        read_type: Some(read_rel::ReadType::NamedTable(read_rel::NamedTable {
            names: table.iter().map(|name| (*name).to_owned()).collect(),
            advanced_extension: None,
        })),
        ..Default::default()
    };
    Plan {
        relations: vec![PlanRel {
            rel_type: Some(plan_rel::RelType::Root(RelRoot {
                input: Some(Rel {
                    rel_type: Some(rel::RelType::Read(Box::new(read))),
                }),
                names: vec!["value".to_owned()],
            })),
        }],
        ..Default::default()
    }
    .encode_to_vec()
}

fn plan(session: &Arc<SessionState>, table: &[&str]) -> Result<PlanningOutcome, ConnectorError> {
    session.plan_pushdown(&read_plan(table))
}

fn remote(session: &Arc<SessionState>, table: &[&str]) -> SharedRemoteExecution {
    match plan(session, table).unwrap() {
        PlanningOutcome::Planned { remote, .. } => remote,
        PlanningOutcome::Unsupported { reason } => panic!("unexpectedly unsupported: {reason}"),
    }
}

fn kind<T>(result: Result<T, ConnectorError>) -> ErrorKind {
    result.err().expect("the call must fail").kind()
}

/// Pulls until the read ends and returns the number of batches.
fn pull_all(operation: &ReadOperation) -> Result<usize, ConnectorError> {
    let mut batches = 0;
    while operation.pull()?.is_some() {
        batches += 1;
    }
    Ok(batches)
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached in time");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_read_is_sent_at_its_first_pull() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "lazy");
    let session = data_source.open_session().unwrap();
    let operation = session
        .start_read(remote(&session, &["batches", "2"]), None)
        .unwrap();

    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
    assert_eq!(pull_all(&operation).unwrap(), 2);
    assert_eq!(probe.sent.load(Ordering::SeqCst), 1);
    assert!(operation.error().is_none());
}

#[test]
fn releasing_a_read_without_pulling_sends_nothing() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "unpulled");
    let session = data_source.open_session().unwrap();
    let remote = remote(&session, &["batches", "1"]);

    session.start_read(remote.clone(), None).unwrap().release();

    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
    assert!(session.start_read(remote, None).is_ok());
}

#[test]
fn a_busy_session_rejects_new_requests_and_the_running_read_continues() {
    let library = library();
    let (data_source, _probe) = open_probed(&library, "busy");
    let session = data_source.open_session().unwrap();
    let remote = remote(&session, &["batches", "2"]);
    let first = session.start_read(remote.clone(), None).unwrap();
    assert!(first.pull().unwrap().is_some());

    assert_eq!(
        kind(session.start_read(remote.clone(), None)),
        ErrorKind::InvalidState
    );
    assert_eq!(
        kind(plan(&session, &["batches", "1"])),
        ErrorKind::InvalidState
    );

    assert!(first.pull().unwrap().is_some());
    assert!(first.pull().unwrap().is_none());
    assert!(session.start_read(remote, None).is_ok());
}

#[test]
fn cancelling_before_the_first_pull_sends_nothing() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "early-cancel");
    let session = data_source.open_session().unwrap();
    let operation = session
        .start_read(remote(&session, &["batches", "1"]), None)
        .unwrap();

    thread::scope(|scope| {
        scope.spawn(|| operation.cancel().unwrap());
    });

    assert_eq!(kind(operation.pull()), ErrorKind::Cancellation);
    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
    assert_eq!(operation.error().unwrap().kind(), ErrorKind::Cancellation);
    assert!(operation.pull().unwrap().is_none());
}

#[test]
fn cancelling_from_another_thread_stops_a_blocked_pull() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "cancel");
    let session = data_source.open_session().unwrap();
    let remote = remote(&session, &["block"]);
    let operation = session.start_read(remote.clone(), None).unwrap();

    let pulled = thread::scope(|scope| {
        let puller = scope.spawn(|| operation.pull());
        wait_until(|| probe.sent.load(Ordering::SeqCst) == 1);
        operation.cancel().unwrap();
        puller.join().unwrap()
    });

    let error = pulled.err().unwrap();
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(!error.retryable());
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert!(session.start_read(remote, None).is_ok());
}

#[test]
fn the_timeout_counts_from_the_first_pull_and_stops_a_blocked_pull() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "timeout");
    let session = data_source.open_session().unwrap();
    let operation = session
        .start_read(
            remote(&session, &["block"]),
            Some(Duration::from_millis(50)),
        )
        .unwrap();

    thread::sleep(Duration::from_millis(100));
    let error = operation.pull().err().unwrap();

    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(error.retryable());
    assert_eq!(probe.sent.load(Ordering::SeqCst), 1);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
}

#[test]
fn releasing_a_read_partway_stops_it_and_frees_the_session() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "partway");
    let session = data_source.open_session().unwrap();
    let remote = remote(&session, &["batches", "3"]);
    let operation = session.start_read(remote.clone(), None).unwrap();
    assert!(operation.pull().unwrap().is_some());

    operation.release();

    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    let next = session.start_read(remote, None).unwrap();
    assert!(next.pull().unwrap().is_some());
}

#[test]
fn calls_on_a_child_after_its_parent_closed_return_invalid_state() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "parent-closed");
    let session = data_source.open_session().unwrap();
    let operation = session
        .start_read(remote(&session, &["batches", "2"]), None)
        .unwrap();
    assert!(operation.pull().unwrap().is_some());

    data_source.close();

    assert_eq!(
        kind(plan(&session, &["batches", "1"])),
        ErrorKind::InvalidState
    );
    assert_eq!(kind(operation.pull()), ErrorKind::InvalidState);
    assert_eq!(operation.error().unwrap().kind(), ErrorKind::InvalidState);
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert_eq!(kind(data_source.open_session()), ErrorKind::InvalidState);
}

#[test]
fn a_pull_after_its_operation_closed_returns_invalid_state() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "operation-closed");
    let session = data_source.open_session().unwrap();
    let remote = remote(&session, &["batches", "2"]);
    let operation = session.start_read(remote.clone(), None).unwrap();
    assert!(operation.pull().unwrap().is_some());

    operation.close();

    assert_eq!(kind(operation.pull()), ErrorKind::InvalidState);
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert!(session.start_read(remote, None).is_ok());
}

#[test]
fn a_data_source_error_stays_retrievable_and_the_session_stays_usable() {
    let library = library();
    let (data_source, _probe) = open_probed(&library, "data-source-error");
    let session = data_source.open_session().unwrap();
    let remote = remote(&session, &["fail"]);
    let operation = session.start_read(remote.clone(), None).unwrap();

    assert_eq!(kind(operation.pull()), ErrorKind::DataSource);

    let error = operation.error().unwrap();
    assert!(error.retryable());
    assert_eq!(error.data_source_code(), Some("40001"));
    assert_eq!(error.diagnostics().len(), 3);
    assert!(operation.pull().unwrap().is_none());
    assert!(session.start_read(remote, None).is_ok());
}

#[test]
fn a_panic_in_a_pull_becomes_internal_and_makes_the_session_unusable() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "pull-panic");
    let session = data_source.open_session().unwrap();
    let operation = session
        .start_read(remote(&session, &["panic"]), None)
        .unwrap();

    assert_eq!(kind(operation.pull()), ErrorKind::Internal);

    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert_eq!(
        kind(plan(&session, &["batches", "1"])),
        ErrorKind::InvalidState
    );
    assert!(data_source.open_session().is_ok());
}

#[test]
fn a_panic_in_planning_becomes_internal_and_makes_the_session_unusable() {
    let library = library();
    let (data_source, _probe) = open_probed(&library, "planning-panic");
    let session = data_source.open_session().unwrap();

    assert_eq!(
        kind(plan(&session, &["panic-planning"])),
        ErrorKind::Internal
    );
    assert_eq!(
        kind(plan(&session, &["batches", "1"])),
        ErrorKind::InvalidState
    );
}

#[test]
fn a_broken_connection_makes_the_session_unusable() {
    let library = library();
    let (data_source, _probe) = open_probed(&library, "broken-connection");

    let reading = data_source.open_session().unwrap();
    let remote = remote(&reading, &["break-connection"]);
    let operation = reading.start_read(remote.clone(), None).unwrap();
    assert_eq!(kind(operation.pull()), ErrorKind::Connection);
    assert_eq!(
        kind(reading.start_read(remote, None)),
        ErrorKind::InvalidState
    );

    let planning = data_source.open_session().unwrap();
    assert_eq!(
        kind(plan(&planning, &["break-connection-planning"])),
        ErrorKind::Connection
    );
    assert_eq!(
        kind(plan(&planning, &["batches", "1"])),
        ErrorKind::InvalidState
    );
}

#[test]
fn a_remote_execution_restored_from_bytes_reads_on_another_session() {
    let library = library();
    let (planning_source, _probe) = open_probed(&library, "planning-process");
    let encoded = remote(&planning_source.open_session().unwrap(), &["batches", "1"]).to_bytes();
    planning_source.close();

    let (reading_source, probe) = open_probed(&library, "reading-process");
    let session = reading_source.open_session().unwrap();
    let operation = session
        .start_read(session.restore_remote_execution(&encoded).unwrap(), None)
        .unwrap();

    assert_eq!(pull_all(&operation).unwrap(), 1);
    assert_eq!(probe.sent.load(Ordering::SeqCst), 1);
    assert_eq!(
        kind(session.restore_remote_execution(b"not a remote execution")),
        ErrorKind::InvalidRequest
    );
}

#[test]
fn planning_reports_unsupported_regions_and_rejects_non_substrait_bytes() {
    let library = library();
    let (data_source, _probe) = open_probed(&library, "planning");
    let session = data_source.open_session().unwrap();

    assert!(matches!(
        plan(&session, &["unknown-table"]).unwrap(),
        PlanningOutcome::Unsupported { .. }
    ));
    assert!(matches!(
        plan(&session, &["batches", "1"]).unwrap(),
        PlanningOutcome::Planned {
            local_plan: None,
            ..
        }
    ));
    assert_eq!(
        kind(session.plan_pushdown(b"\xff\xff not substrait")),
        ErrorKind::InvalidRequest
    );
    assert!(plan(&session, &["batches", "1"]).is_ok());
}

#[test]
fn opening_a_data_source_validates_the_connector_and_its_options() {
    let library = library();
    for (pairs, named) in [
        (&[("token", "hunter2")][..], "label"),
        (&[("label", "a"), ("password", "hunter2")][..], "password"),
    ] {
        let error = library
            .open_data_source("test", options(pairs), None)
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidSettings);
        assert!(error.safe_message().contains(named));
        assert!(!error.safe_message().contains("hunter2"));
    }
    assert_eq!(
        kind(library.open_data_source("unknown", Vec::new(), None)),
        ErrorKind::InvalidSettings
    );
}

#[test]
fn shutting_down_closes_every_unit_but_running_reads_keep_their_deadline() {
    let library = library();
    let (data_source, probe) = open_probed(&library, "shutdown");
    let session = data_source.open_session().unwrap();
    let operation = session
        .start_read(
            remote(&session, &["block"]),
            Some(Duration::from_millis(50)),
        )
        .unwrap();

    let pulled = thread::scope(|scope| {
        let puller = scope.spawn(|| operation.pull());
        wait_until(|| probe.sent.load(Ordering::SeqCst) == 1);
        library.shut_down();
        puller.join().unwrap()
    });

    assert_eq!(kind(pulled), ErrorKind::Timeout);
    assert_eq!(
        kind(plan(&session, &["batches", "1"])),
        ErrorKind::InvalidState
    );
    assert_eq!(kind(data_source.open_session()), ErrorKind::InvalidState);
    assert_eq!(
        kind(library.open_data_source("test", options(&[("label", "late")]), None)),
        ErrorKind::InvalidState
    );
}
