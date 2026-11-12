use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use analytics_types::{AnalyticsField, AnalyticsSchema, AnalyticsType};
use arrow_array::{ArrayRef, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use connector_sdk::{
    ConnectorError, ErrorKind, ExecutionCancelHandle, ExecutionStream, Executor, RemoteExecution,
    execute_planned,
};
use connector_sql::{SqlExecutor, SqlRead};
use futures_core::Stream;

#[path = "support/adbc.rs"]
mod adbc;
#[path = "support/connector.rs"]
mod connector;

use adbc::{Event, Fixture, FixtureExecution};
connector::sql_test_connector!(FixtureSql, runs_remotely = false);

#[test]
fn public_sql_executable_query_path() {
    round_trips_sql_and_delegates_the_lazy_batch_stream();
    preserves_execution_scoped_cancellation();
    cancellation_stops_a_query_blocked_at_the_source();
    expiry_stops_a_query_blocked_at_the_source();
    preserves_adbc_execution_diagnostics();
    transports_the_query_to_another_connection();
    rejects_transported_bytes_that_are_not_a_statement();
}

fn round_trips_sql_and_delegates_the_lazy_batch_stream() {
    let sql = "select value from source where value = 7";
    let remote = remote_query(sql);
    assert_eq!(remote.query.sql, sql);

    let (fixture, connection) = Fixture::install(FixtureExecution::Batches(vec![
        batch(&[10, 20]),
        batch(&[30]),
    ]));
    let mut stream = execute(connection, &remote);

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
    let mut cancelled = execute(cancelled_connection, &remote);
    cancelled.cancel_handle().cancel().unwrap();

    let Poll::Ready(Some(Err(error))) = pull(&mut cancelled) else {
        panic!("the cancelled execution should return an error");
    };
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(cancelled_fixture.events().is_empty());

    let (next_fixture, next_connection) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[2])]));
    let mut next = execute(next_connection, &remote);
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
    let mut stream = execute(connection, &remote);
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
    let restored = RemoteExecution::<SqlRead>::from_bytes(&bytes).unwrap();
    let mut stream = execute(connection, &restored);

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

fn rejects_transported_bytes_that_are_not_a_statement() {
    let mut bytes = remote_query("select value").to_bytes();
    bytes.push(0xff);

    let error = RemoteExecution::<SqlRead>::from_bytes(&bytes)
        .err()
        .unwrap();
    assert_eq!(error.kind(), ErrorKind::InvalidRequest);
}

fn preserves_adbc_execution_diagnostics() {
    let remote = remote_query("select broken");
    let (fixture, connection) = Fixture::install(FixtureExecution::Fail);
    let mut stream = execute(connection, &remote);

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

fn value_schema() -> AnalyticsSchema {
    AnalyticsSchema::new(vec![AnalyticsField::new(
        "value",
        AnalyticsType::BigInt,
        false,
    )])
}

fn remote_query(sql: &str) -> RemoteExecution<SqlRead> {
    RemoteExecution {
        query: SqlRead {
            sql: sql.to_owned(),
        },
        output_schema: value_schema(),
    }
}

/// Starts the statement on a fixture session, as a planned read's query step.
fn execute(
    connection: connector_adbc::AdbcConnection<FixtureSql>,
    remote: &RemoteExecution<SqlRead>,
) -> ExecutionStream {
    let session = connector::session(connection);
    execute_planned(&remote.output_schema, None, || {
        SqlExecutor::<FixtureSql>::run(&session, &remote.query)
    })
    .unwrap()
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
