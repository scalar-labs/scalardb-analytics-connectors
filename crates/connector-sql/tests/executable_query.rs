use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use analytics_types::{AnalyticsField, AnalyticsSchema, AnalyticsType};
use arrow_array::{ArrayRef, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use connector_sdk::{
    ConnectorError, ErrorKind, ExecutionCancelHandle, ExecutionStream, RemoteExecution,
    execute_planned,
};
use connector_sql::{RenderedQuery, SqlExecutor};
use futures_core::Stream;

#[path = "support/adbc.rs"]
mod adbc;
#[path = "support/adbc_connector.rs"]
mod adbc_connector;

use adbc_connector::FixtureAdbc;
use connector_adbc::AdbcConnection;

use adbc::{Event, Fixture, FixtureExecution};

#[test]
fn public_sql_executable_query_path() {
    round_trips_sql_and_delegates_the_lazy_batch_stream();
    preserves_execution_scoped_cancellation();
    cancellation_stops_a_query_blocked_at_the_source();
    expiry_stops_a_query_blocked_at_the_source();
    preserves_adbc_execution_diagnostics();
    transports_the_query_to_another_connection();
    rejects_transported_sql_that_is_not_utf8();
}

fn round_trips_sql_and_delegates_the_lazy_batch_stream() {
    let sql = "select value from source where value = 7";
    let remote = remote_query(sql);
    assert_eq!(remote.query.sql, sql);

    let (fixture, connection) = Fixture::install(FixtureExecution::Batches(vec![
        batch(&[10, 20]),
        batch(&[30]),
    ]));
    let executor = executor(connection);
    let mut stream = execute_planned(&remote.output_schema, None, || {
        executor.execute(&remote.query)
    })
    .unwrap();

    assert!(fixture.events().is_empty());
    assert_eq!(pull(&mut stream), Poll::Ready(Some(Ok(batch(&[10, 20])))));
    assert_eq!(pull(&mut stream), Poll::Ready(Some(Ok(batch(&[30])))));
    assert_eq!(pull(&mut stream), Poll::Ready(None));
    assert_eq!(
        fixture.events(),
        vec![
            Event::NewStatement,
            Event::SetSql(sql.to_owned()),
            Event::Execute,
            Event::Pull,
            Event::Pull,
            Event::Pull,
        ]
    );
}

fn preserves_execution_scoped_cancellation() {
    let remote = remote_query("select value");
    let (cancelled_fixture, cancelled_connection) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[1])]));
    let cancelled_executor = executor(cancelled_connection);
    let mut cancelled = execute_planned(&remote.output_schema, None, || {
        cancelled_executor.execute(&remote.query)
    })
    .unwrap();
    cancelled.cancel_handle().cancel().unwrap();

    let Poll::Ready(Some(Err(error))) = pull(&mut cancelled) else {
        panic!("the cancelled execution should return an error");
    };
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(cancelled_fixture.events().is_empty());

    let (next_fixture, next_connection) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[2])]));
    let next_executor = executor(next_connection);
    let mut next = execute_planned(&remote.output_schema, None, || {
        next_executor.execute(&remote.query)
    })
    .unwrap();
    assert_eq!(pull(&mut next), Poll::Ready(Some(Ok(batch(&[2])))));
    assert_eq!(
        next_fixture.events(),
        vec![
            Event::NewStatement,
            Event::SetSql("select value".to_owned()),
            Event::Execute,
            Event::Pull,
        ]
    );
}

fn cancellation_stops_a_query_blocked_at_the_source() {
    let error = stop_blocked_query(ExecutionCancelHandle::cancel);
    assert_eq!(error.kind(), ErrorKind::Cancellation);
}

fn expiry_stops_a_query_blocked_at_the_source() {
    let error = stop_blocked_query(ExecutionCancelHandle::expire);
    assert_eq!(error.kind(), ErrorKind::Timeout);
}

/// Blocks the first pull in the source's `execute`, stops it from the test
/// thread, and returns the error that the blocked pull ends with.
fn stop_blocked_query(
    stop: impl FnOnce(&ExecutionCancelHandle) -> Result<(), ConnectorError>,
) -> ConnectorError {
    let remote = remote_query("select slow");
    let (fixture, connection) = Fixture::install(FixtureExecution::BlockUntilCancelled);
    let executor = executor(connection);
    let mut stream = execute_planned(&remote.output_schema, None, || {
        executor.execute(&remote.query)
    })
    .unwrap();
    let handle = stream.cancel_handle();

    let puller = std::thread::spawn(move || pull(&mut stream));
    fixture.wait_until_executing();
    stop(&handle).unwrap();
    let result = puller.join().unwrap();

    assert_eq!(
        fixture.events(),
        vec![
            Event::NewStatement,
            Event::SetSql("select slow".to_owned()),
            Event::Execute,
            Event::Cancel,
        ]
    );
    let Poll::Ready(Some(Err(error))) = result else {
        panic!("the blocked pull should end with the stop error");
    };
    error
}

/// Plans in one process and executes in another: only the bytes cross over,
/// and the receiving side uses its own connection.
fn transports_the_query_to_another_connection() {
    let sql = "select value from source where value = 7";
    let bytes = remote_query(sql).to_bytes();

    let (fixture, connection) = Fixture::install(FixtureExecution::Batches(vec![batch(&[7])]));
    let restored = RemoteExecution::<RenderedQuery>::from_bytes(&bytes).unwrap();
    let executor = executor(connection);
    let mut stream = execute_planned(&restored.output_schema, None, || {
        executor.execute(&restored.query)
    })
    .unwrap();

    assert_eq!(pull(&mut stream), Poll::Ready(Some(Ok(batch(&[7])))));
    assert_eq!(
        fixture.events(),
        vec![
            Event::NewStatement,
            Event::SetSql(sql.to_owned()),
            Event::Execute,
            Event::Pull,
        ]
    );
}

fn rejects_transported_sql_that_is_not_utf8() {
    let mut bytes = remote_query("select value").to_bytes();
    bytes.push(0xff);

    let error = RemoteExecution::<RenderedQuery>::from_bytes(&bytes)
        .err()
        .unwrap();
    assert_eq!(error.kind(), ErrorKind::InvalidRequest);
}

fn preserves_adbc_execution_diagnostics() {
    let remote = remote_query("select broken");
    let (fixture, connection) = Fixture::install(FixtureExecution::Fail);
    let executor = executor(connection);
    let mut stream = execute_planned(&remote.output_schema, None, || {
        executor.execute(&remote.query)
    })
    .unwrap();

    let Poll::Ready(Some(Err(error))) = pull(&mut stream) else {
        panic!("the first pull should return the ADBC execution error");
    };
    assert_eq!(error.kind(), ErrorKind::Internal);
    assert_eq!(error.data_source_message(), Some("fixture execute failure"));
    assert_eq!(
        fixture.events(),
        vec![
            Event::NewStatement,
            Event::SetSql("select broken".to_owned()),
            Event::Execute,
        ]
    );
}

fn pull(
    stream: &mut ExecutionStream,
) -> Poll<Option<Result<arrow_array::RecordBatch, ConnectorError>>> {
    let mut context = Context::from_waker(Waker::noop());
    Stream::poll_next(Pin::new(stream), &mut context)
}

fn remote_query(sql: &str) -> RemoteExecution<RenderedQuery> {
    RemoteExecution {
        query: RenderedQuery {
            sql: sql.to_owned(),
        },
        output_schema: AnalyticsSchema::new(vec![AnalyticsField::new(
            "value",
            AnalyticsType::BigInt,
            false,
        )]),
    }
}

fn executor(connection: adbc_driver_manager::ManagedConnection) -> SqlExecutor<FixtureAdbc> {
    SqlExecutor::new(AdbcConnection::new(connection))
}

fn batch(values: &[i64]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int64,
        false,
    )]));
    let columns: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(values.to_vec()))];
    RecordBatch::try_new(schema, columns).unwrap()
}
