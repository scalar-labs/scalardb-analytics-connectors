use std::{
    pin::Pin,
    task::{Context, Poll, Waker},
};

use connector_adbc::AdbcQueryStream;
use connector_sdk::{ConnectorError, ErrorKind, ExecutableQuery};
use connector_sql::{RenderedQuery, execute};
use futures_core::Stream;

#[path = "support/adbc.rs"]
mod adbc;

use adbc::{Event, Fixture, FixtureExecution, batch};

#[test]
fn public_sql_executable_query_path() {
    round_trips_sql_and_delegates_the_lazy_batch_stream();
    preserves_execution_scoped_cancellation();
    preserves_adbc_execution_diagnostics();
}

fn round_trips_sql_and_delegates_the_lazy_batch_stream() {
    let sql = "select value from source where value = 7";
    let executable_query = executable_query(sql);
    assert_eq!(executable_query.query.sql, sql);

    let (fixture, connection) = Fixture::install(FixtureExecution::Batches(vec![
        batch(&[10, 20]),
        batch(&[30]),
    ]));
    let mut stream = execute(connection, &executable_query);

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
    let executable_query = executable_query("select value");
    let (cancelled_fixture, cancelled_connection) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[1])]));
    let mut cancelled = execute(cancelled_connection, &executable_query);
    cancelled.cancel_handle().try_cancel().unwrap();

    let Poll::Ready(Some(Err(error))) = pull(&mut cancelled) else {
        panic!("the cancelled execution should return an error");
    };
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(cancelled_fixture.events().is_empty());

    let (next_fixture, next_connection) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[2])]));
    let mut next = execute(next_connection, &executable_query);
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

fn preserves_adbc_execution_diagnostics() {
    let executable_query = executable_query("select broken");
    let (fixture, connection) = Fixture::install(FixtureExecution::Fail);
    let mut stream = execute(connection, &executable_query);

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
    stream: &mut AdbcQueryStream,
) -> Poll<Option<Result<arrow_array::RecordBatch, ConnectorError>>> {
    let mut context = Context::from_waker(Waker::noop());
    Stream::poll_next(Pin::new(stream), &mut context)
}

fn executable_query(sql: &str) -> ExecutableQuery<RenderedQuery> {
    ExecutableQuery {
        query: RenderedQuery {
            sql: sql.to_owned(),
        },
    }
}
