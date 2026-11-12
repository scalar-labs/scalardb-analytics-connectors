//! Catalog statements run in order as one stoppable source read.

use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch};
use connector_sdk::{ConnectorError, ErrorKind, start_source_read};
use connector_sql::catalog::{catalog_read, int64};

#[path = "support/adbc.rs"]
mod adbc;
#[path = "support/connector.rs"]
mod connector;

use adbc::{Event, Fixture, FixtureExecution};
use connector_adbc::AdbcConnection;

connector::sql_test_connector!(FixtureSql, runs_remotely = false);

#[test]
fn catalog_reads_run_their_statements_in_order() {
    statements_run_in_order_and_their_rows_are_assembled_once();
    a_cancelled_read_sends_no_statement();
    cancelling_a_blocked_statement_stops_it_and_sends_no_later_statement();
}

fn batch(values: &[i64]) -> RecordBatch {
    let column: ArrayRef = Arc::new(Int64Array::from(values.to_vec()));
    RecordBatch::try_from_iter([("value", column)]).unwrap()
}

fn statements_run_in_order_and_their_rows_are_assembled_once() {
    let (fixture, connection): (_, AdbcConnection<FixtureSql>) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[1, 2])]));
    let output = catalog_read(
        &connection,
        Box::new(|catalog| {
            let sum = |batches: Vec<RecordBatch>| -> Result<i64, ConnectorError> {
                let mut sum = 0;
                for batch in &batches {
                    sum += int64(batch, 0)?.values().iter().sum::<i64>();
                }
                Ok(sum)
            };
            let first = sum(catalog.query("select first")?)?;
            let second = sum(catalog.query("select second")?)?;
            Ok(vec![first, second])
        }),
    );
    assert!(fixture.events().is_empty());
    assert_eq!(output.read_to_end(), Ok(vec![3, 3]));
    let statements: Vec<Event> = fixture
        .events()
        .into_iter()
        .filter(|event| matches!(event, Event::SetSql(_)))
        .collect();
    assert_eq!(
        statements,
        [
            Event::SetSql("select first".to_owned()),
            Event::SetSql("select second".to_owned()),
        ]
    );
}

fn a_cancelled_read_sends_no_statement() {
    let (fixture, connection): (_, AdbcConnection<FixtureSql>) =
        Fixture::install(FixtureExecution::Batches(vec![batch(&[1])]));
    let read = start_source_read(None, || {
        Ok(catalog_read(
            &connection,
            Box::new(|catalog| {
                catalog.query("select first")?;
                Ok(vec![()])
            }),
        ))
    })
    .unwrap();
    read.cancel_handle().cancel().unwrap();
    let mut read = read;
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let first = futures_core::Stream::poll_next(std::pin::Pin::new(&mut read), &mut context);
    assert!(matches!(
        first,
        std::task::Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Cancellation
    ));
    assert!(fixture.events().is_empty());
}

/// Cancels while the first statement is blocked at the source: the cancel
/// reaches that statement, its error ends the read, and the second statement
/// is never sent.
fn cancelling_a_blocked_statement_stops_it_and_sends_no_later_statement() {
    let (fixture, connection): (_, AdbcConnection<FixtureSql>) =
        Fixture::install(FixtureExecution::BlockUntilCancelled);
    let (mut stream, cancel) = catalog_read(
        &connection,
        Box::new(|catalog| {
            catalog.query("select first")?;
            catalog.query("select second")?;
            Ok(vec![()])
        }),
    )
    .into_parts();

    let puller = std::thread::spawn(move || {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        futures_core::Stream::poll_next(stream.as_mut(), &mut context)
            .map(|item| item.map(|result| result.map(|_| ())))
    });
    fixture.wait_until_executing();
    cancel.cancel().unwrap();
    let pulled = puller.join().unwrap();

    assert!(matches!(pulled, std::task::Poll::Ready(Some(Err(_)))));
    assert_eq!(
        fixture.events(),
        [
            Event::NewStatement,
            Event::SetSql("select first".to_owned()),
            Event::Execute,
            Event::Cancel,
        ]
    );
}
