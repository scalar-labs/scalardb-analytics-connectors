use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use analytics_types::{AnalyticsField, AnalyticsSchema, AnalyticsType};
use arrow_array::{ArrayRef, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use connector_sdk::{
    ConnectorError, ErrorKind, ExecutableQuery, ExecutionCancelHandle, ExecutionStream,
    PlannedPushdown, execute_planned,
};
use connector_sql::{RenderedQuery, SqlExecutor};
use futures_core::Stream;
use substrait::proto::Plan;

#[path = "support/adbc.rs"]
mod adbc;

use adbc::{Event, Fixture, FixtureExecution};

#[test]
fn public_sql_executable_query_path() {
    round_trips_sql_and_delegates_the_lazy_batch_stream();
    preserves_execution_scoped_cancellation();
    cancellation_stops_a_query_blocked_at_the_source();
    expiry_stops_a_query_blocked_at_the_source();
    preserves_adbc_execution_diagnostics();
}

fn round_trips_sql_and_delegates_the_lazy_batch_stream() {
    let sql = "select value from source where value = 7";
    let pushdown = planned_query(sql);
    assert_eq!(pushdown.executable_query.query.sql, sql);

    let (fixture, connection) = Fixture::install(FixtureExecution::Batches(vec![
        batch(&[10, 20]),
        batch(&[30]),
    ]));
    let executor = executor(connection);
    let mut stream = execute_planned(&executor, &pushdown, None).unwrap();

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
    let pushdown = planned_query("select value");
    let (cancelled_fixture, cancelled_connection) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[1])]));
    let cancelled_executor = executor(cancelled_connection);
    let mut cancelled = execute_planned(&cancelled_executor, &pushdown, None).unwrap();
    cancelled.cancel_handle().cancel().unwrap();

    let Poll::Ready(Some(Err(error))) = pull(&mut cancelled) else {
        panic!("the cancelled execution should return an error");
    };
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(cancelled_fixture.events().is_empty());

    let (next_fixture, next_connection) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[2])]));
    let next_executor = executor(next_connection);
    let mut next = execute_planned(&next_executor, &pushdown, None).unwrap();
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
    let pushdown = planned_query("select slow");
    let (fixture, connection) = Fixture::install(FixtureExecution::BlockUntilCancelled);
    let executor = executor(connection);
    let mut stream = execute_planned(&executor, &pushdown, None).unwrap();
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

fn preserves_adbc_execution_diagnostics() {
    let pushdown = planned_query("select broken");
    let (fixture, connection) = Fixture::install(FixtureExecution::Fail);
    let executor = executor(connection);
    let mut stream = execute_planned(&executor, &pushdown, None).unwrap();

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

fn planned_query(sql: &str) -> PlannedPushdown<RenderedQuery> {
    let schema = AnalyticsSchema::new(vec![AnalyticsField::new(
        "value",
        AnalyticsType::BigInt,
        false,
    )]);
    PlannedPushdown {
        remote_plan: Plan::default(),
        local_plan: None,
        executable_query: ExecutableQuery {
            query: RenderedQuery {
                sql: sql.to_owned(),
            },
        },
        remote_base_schema: schema.clone(),
        remote_output_schema: schema.clone(),
        table_bindings: Vec::new(),
        output_schema: schema,
    }
}

fn executor(connection: adbc_driver_manager::ManagedConnection) -> SqlExecutor {
    SqlExecutor::new(connection)
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
