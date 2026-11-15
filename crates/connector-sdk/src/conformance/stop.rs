//! Stopping at the source: cancellation, timeouts, releasing a stream
//! partway, and a broken connection, observed through the fixture.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use substrait::proto::Plan;

use super::{
    Fixture, Interruption, Probe, REQUEST_TIMEOUT, SOURCE_STOP_BOUND, STOP_DELAY,
    errors::filter_literals, read::planned,
};
use crate::{
    ConnectorError, ErrorKind, NameFilter,
    runtime::{Cancellation, Operation, SessionState},
};

pub(super) fn check<F: Fixture>(probe: &Probe<'_, F>) {
    cancelling_a_waiting_pull_stops_the_source_query(probe);
    a_read_timeout_stops_the_source_query(probe);
    releasing_a_stream_partway_stops_the_source_query(probe);
    cancelling_a_waiting_metadata_request_stops_its_source_query(probe);
    a_metadata_timeout_stops_its_source_query(probe);
    a_broken_connection_makes_the_session_unusable(probe);
}

fn cancelling_a_waiting_pull_stops_the_source_query<F: Fixture>(probe: &Probe<'_, F>) {
    let session = probe.session();
    let offered = probe.fixture.waiting_read();
    let operation = start(probe, &session, &offered, None);
    let error = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(STOP_DELAY);
            operation.cancel().expect("the operation cancels");
        });
        pull_error(probe, &operation, &offered)
    });
    assert_eq!(error.kind(), ErrorKind::Cancellation, "a cancelled pull");
    operation.close();
    let stopped = source_stops(probe, Interruption::CancelledPull);
    if stopped {
        accepts_next_request(probe, &session, "a cancelled pull");
    }
}

fn a_read_timeout_stops_the_source_query<F: Fixture>(probe: &Probe<'_, F>) {
    let session = probe.session();
    let offered = probe.fixture.waiting_read();
    let operation = start(probe, &session, &offered, Some(REQUEST_TIMEOUT));
    let error = pull_error(probe, &operation, &offered);
    assert_eq!(error.kind(), ErrorKind::Timeout, "an expired read");
    operation.close();
    let stopped = source_stops(probe, Interruption::ExpiredRead);
    if stopped {
        accepts_next_request(probe, &session, "an expired read");
    }
}

fn releasing_a_stream_partway_stops_the_source_query<F: Fixture>(probe: &Probe<'_, F>) {
    let session = probe.session();
    let offered = probe.fixture.long_read();
    let operation = start(probe, &session, &offered, None);
    let first = probe
        .observed
        .result(operation.pull(), &filter_literals(&offered))
        .expect("the first pull succeeds");
    assert!(first.is_some(), "the long read returns a first batch");
    operation.release();
    let stopped = source_stops(probe, Interruption::ReleasedStream);
    if stopped {
        accepts_next_request(probe, &session, "a stream released partway");
    }
    operation.close();
}

fn cancelling_a_waiting_metadata_request_stops_its_source_query<F: Fixture>(probe: &Probe<'_, F>) {
    let session = probe.session();
    let Some((table, block)) = probe.fixture.block_metadata() else {
        return;
    };
    let cancellation = Cancellation::new();
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(STOP_DELAY);
            cancellation.cancel().expect("the cancellation cancels");
        });
        session.table_metadata(&[table], None, Some(&cancellation))
    });
    let error = probe
        .observed
        .result(result, &[])
        .expect_err("a cancelled metadata request fails");
    assert_eq!(
        error.kind(),
        ErrorKind::Cancellation,
        "a cancelled metadata request"
    );
    let stopped = source_stops(probe, Interruption::CancelledMetadataRequest);
    drop(block);
    if stopped {
        accepts_next_request(probe, &session, "a cancelled metadata request");
    }
}

fn a_metadata_timeout_stops_its_source_query<F: Fixture>(probe: &Probe<'_, F>) {
    let session = probe.session();
    let Some((table, block)) = probe.fixture.block_metadata() else {
        return;
    };
    let result = session.table_metadata(&[table], Some(REQUEST_TIMEOUT), None);
    let error = probe
        .observed
        .result(result, &[])
        .expect_err("an expired metadata request fails");
    assert_eq!(
        error.kind(),
        ErrorKind::Timeout,
        "an expired metadata request"
    );
    let stopped = source_stops(probe, Interruption::ExpiredMetadataRequest);
    drop(block);
    if stopped {
        accepts_next_request(probe, &session, "an expired metadata request");
    }
}

fn a_broken_connection_makes_the_session_unusable<F: Fixture>(probe: &Probe<'_, F>) {
    let session = probe.session();
    let offered = probe.fixture.waiting_read();
    let operation = start(probe, &session, &offered, None);
    let broken = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(STOP_DELAY);
            probe.fixture.break_connections();
        });
        pull_error(probe, &operation, &offered)
    });
    operation.close();
    let error = probe
        .observed
        .result(session.namespace_list(&NameFilter::None, None, None), &[])
        .expect_err("the session rejects requests after its connection broke");
    if known_deviation(probe, Interruption::BrokenConnection) {
        assert_ne!(
            error.kind(),
            ErrorKind::InvalidState,
            "BrokenConnection: the session now becomes unusable; remove the known deviation"
        );
    } else {
        assert_eq!(
            error.kind(),
            ErrorKind::InvalidState,
            "a request after a broken connection; the pull failed with {:?}",
            broken.kind()
        );
    }
}

/// Plans `offered` and starts reading it; nothing is sent before the first pull.
fn start<F: Fixture>(
    probe: &Probe<'_, F>,
    session: &Arc<SessionState>,
    offered: &Plan,
    timeout: Option<Duration>,
) -> Arc<Operation> {
    let remote = planned(probe, session, offered).expect("the read is planned");
    probe
        .observed
        .result(
            session.start_read(remote, timeout),
            &filter_literals(offered),
        )
        .expect("the read starts")
}

/// Pulls until the operation fails, which the request is expected to do.
fn pull_error<F: Fixture>(
    probe: &Probe<'_, F>,
    operation: &Operation,
    offered: &Plan,
) -> ConnectorError {
    let literals = filter_literals(offered);
    loop {
        match probe.observed.result(operation.pull(), &literals) {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the read ends without being stopped"),
            Err(error) => return error,
        }
    }
}

/// Whether the source query ended after `stop`, as the contract requires.
///
/// For a known deviation the query is expected to keep running until the
/// bound passes; the suite then breaks the fixture's connections so the run
/// continues on new sessions.
fn source_stops<F: Fixture>(probe: &Probe<'_, F>, stop: Interruption) -> bool {
    let deviation = known_deviation(probe, stop);
    let deadline = Instant::now() + SOURCE_STOP_BOUND;
    while !probe.fixture.source_is_idle() {
        if Instant::now() >= deadline {
            assert!(deviation, "{stop:?}: the source query is still running");
            probe.fixture.break_connections();
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !deviation,
        "{stop:?}: the source query now stops; remove the known deviation"
    );
    true
}

fn known_deviation<F: Fixture>(probe: &Probe<'_, F>, interruption: Interruption) -> bool {
    probe
        .fixture
        .known_interruption_deviations()
        .into_iter()
        .any(|(known, _)| known == interruption)
}

fn accepts_next_request<F: Fixture>(probe: &Probe<'_, F>, session: &Arc<SessionState>, case: &str) {
    probe
        .observed
        .result(session.namespace_list(&NameFilter::None, None, None), &[])
        .unwrap_or_else(|error| panic!("{case}: the session rejects the next request: {error}"));
}
