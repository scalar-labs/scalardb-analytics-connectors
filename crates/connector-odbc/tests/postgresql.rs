//! ODBC access against PostgreSQL through the PostgreSQL ODBC driver, used by
//! a SQL-speaking Connector through `connector-sql`.

mod support;

use std::{
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

use arrow_array::{
    Array, Date32Array, Decimal128Array, Float64Array, Int32Array, Int64Array, RecordBatch,
    StringArray, TimestampMicrosecondArray,
};
use arrow_schema::{DataType, TimeUnit};
use connector_sdk::{
    ConnectorError, ErrorKind, ExecutionOutput, Executor, LibrarySettings, Session,
};
use connector_sql::{SqlExecutor, SqlRead, catalog::catalog_read};
use support::{OdbcTestConnector, PASSWORD, open_session, server, session, session_as};

/// Runs `sql` through the Connector's executor and reads its whole result.
fn run(
    session: &Session<OdbcTestConnector>,
    sql: &str,
) -> Result<Vec<RecordBatch>, ConnectorError> {
    execute(session, sql).read_to_end()
}

fn execute(session: &Session<OdbcTestConnector>, sql: &str) -> ExecutionOutput {
    SqlExecutor::run(
        session,
        &SqlRead {
            sql: sql.to_owned(),
        },
    )
    .expect("the read starts")
}

/// Waits until a query containing `marker` is active at the server.
fn wait_until_active(marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while active_queries(marker) == 0 {
        assert!(Instant::now() < deadline, "the query did not start");
        thread::sleep(Duration::from_millis(50));
    }
}

/// The number of active queries at the server that contain `marker`.
fn active_queries(marker: &str) -> usize {
    server()
        .psql(&format!(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE state = 'active' AND query LIKE '%{marker}%' AND pid <> pg_backend_pid()"
        ))
        .trim()
        .parse()
        .expect("the count is a number")
}

#[test]
fn a_session_runs_the_connectors_session_statements_when_it_opens() {
    let session = session("");
    let batches =
        run(&session, "SELECT current_setting('application_name')").expect("the read succeeds");
    let names = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("the setting is text");
    assert_eq!(names.value(0), "connector-odbc-test");
}

#[test]
fn planned_reads_return_values_as_arrow() {
    server().psql(
        "DROP TABLE IF EXISTS connector_odbc_values; \
         CREATE TABLE connector_odbc_values ( \
             i integer, b bigint, n numeric(10, 2), d double precision, \
             v varchar(20), dt date, ts timestamp(6)); \
         INSERT INTO connector_odbc_values VALUES \
             (2147483647, -9223372036854775808, -12345678.90, 0.1234567890123456789, 'héllo', \
              '2024-02-29', '2024-02-29 12:34:56.123456'), \
             (NULL, NULL, NULL, NULL, NULL, NULL, NULL);",
    );
    let session = session("");
    let batches = run(
        &session,
        "SELECT i, b, n, d, v, dt, ts FROM connector_odbc_values ORDER BY i NULLS LAST",
    )
    .expect("the read succeeds");
    let [batch] = batches.as_slice() else {
        panic!("the small result is one batch");
    };
    let types: Vec<_> = batch
        .schema()
        .fields()
        .iter()
        .map(|field| field.data_type().clone())
        .collect();
    assert_eq!(
        types,
        [
            DataType::Int32,
            DataType::Int64,
            DataType::Decimal128(10, 2),
            DataType::Float64,
            DataType::Utf8,
            DataType::Date32,
            DataType::Timestamp(TimeUnit::Microsecond, None),
        ]
    );
    let column = |index: usize| batch.column(index).as_any();
    assert_eq!(
        column(0).downcast_ref::<Int32Array>().unwrap().value(0),
        i32::MAX
    );
    assert_eq!(
        column(1).downcast_ref::<Int64Array>().unwrap().value(0),
        i64::MIN
    );
    assert_eq!(
        column(2)
            .downcast_ref::<Decimal128Array>()
            .unwrap()
            .value(0),
        -1_234_567_890
    );
    assert_eq!(
        column(3).downcast_ref::<Float64Array>().unwrap().value(0),
        0.123_456_789_012_345_68
    );
    assert_eq!(
        column(4).downcast_ref::<StringArray>().unwrap().value(0),
        "héllo"
    );
    // 2024-02-29 is day 19782 after the Unix epoch.
    assert_eq!(
        column(5).downcast_ref::<Date32Array>().unwrap().value(0),
        19_782
    );
    assert_eq!(
        column(6)
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap()
            .value(0),
        1_709_210_096_123_456
    );
    for index in 0..batch.num_columns() {
        assert!(batch.column(index).is_null(1), "column {index} is NULL");
    }
}

#[test]
fn catalog_reads_run_their_statements_in_order() {
    let session = session("");
    let read = catalog_read::<OdbcTestConnector, _>(
        session.access(),
        Box::new(|catalog| {
            let mut values = Vec::new();
            for sql in ["SELECT 1::int8", "SELECT 2::int8"] {
                for batch in catalog.query(sql)? {
                    let column = batch
                        .column(0)
                        .as_any()
                        .downcast_ref::<Int64Array>()
                        .unwrap();
                    values.extend(column.values().iter().copied());
                }
            }
            Ok(values)
        }),
    );
    assert_eq!(
        read.read_to_end().expect("the catalog read succeeds"),
        [1, 2]
    );
}

#[test]
fn nothing_is_sent_before_the_first_pull() {
    server().psql(
        "DROP TABLE IF EXISTS connector_odbc_lazy; CREATE TABLE connector_odbc_lazy (x int);",
    );
    let session = session("");
    drop(execute(
        &session,
        "INSERT INTO connector_odbc_lazy VALUES (1)",
    ));
    assert_eq!(
        server()
            .psql("SELECT count(*) FROM connector_odbc_lazy")
            .trim(),
        "0"
    );

    let batches = run(&session, "INSERT INTO connector_odbc_lazy VALUES (1)")
        .expect("a statement without a result set succeeds");
    assert!(batches.is_empty());
    assert_eq!(
        server()
            .psql("SELECT count(*) FROM connector_odbc_lazy")
            .trim(),
        "1"
    );
}

#[test]
fn a_value_longer_than_its_column_size_is_a_data_error() {
    // The PostgreSQL ODBC driver reports an unbounded varchar with the
    // configured maximum size.
    let session = session("MaxVarcharSize=10;");
    let error = run(&session, "SELECT repeat('x', 100)::varchar")
        .expect_err("the value does not fit its reported size");
    assert_eq!(error.kind(), ErrorKind::Data, "unexpected error: {error}");
}

#[test]
fn a_library_without_an_odbc_driver_directory_opens_no_data_source() {
    let Err(error) = open_session(&LibrarySettings::default(), "postgres", PASSWORD, "") else {
        panic!("a data source opened without an ODBC driver directory");
    };
    assert_eq!(
        error.kind(),
        ErrorKind::InvalidSettings,
        "unexpected error: {error}"
    );
}

#[test]
fn source_errors_are_classified_by_sqlstate() {
    let session = session("");
    let error = run(&session, "SELECT * FROM connector_odbc_missing_table")
        .expect_err("the table does not exist");
    assert_eq!(
        error.data_source_code(),
        Some("42P01"),
        "unexpected error: {error}"
    );
    assert_eq!(error.kind(), ErrorKind::Internal);
}

#[test]
fn credentials_are_taken_literally_and_kept_out_of_errors() {
    let password = "p;w}d{x";
    server().psql(&format!(
        "DROP ROLE IF EXISTS connector_odbc_reader; \
         CREATE ROLE connector_odbc_reader LOGIN PASSWORD '{password}';"
    ));
    let session =
        session_as("connector_odbc_reader", password, "").expect("the password is taken literally");
    let batches = run(&session, "SELECT current_user::varchar(64)").expect("the read succeeds");
    let user = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(0)
        .to_owned();
    assert_eq!(user, "connector_odbc_reader");

    let Err(error) = session_as("connector_odbc_reader", "wrong-password", "") else {
        panic!("a wrong password is rejected");
    };
    let shown = format!("{error} {error:?} {:?}", error.data_source_message());
    assert!(
        !shown.contains("wrong-password") && !shown.contains(PASSWORD),
        "the error does not show a password: {shown}"
    );
}

#[test]
fn cancelling_a_running_statement_stops_it_at_the_source() {
    let session = session("");
    let (mut stream, cancel) = execute(
        &session,
        "SELECT pg_sleep(60), 'connector_odbc_cancel_execute'",
    )
    .into_parts();
    let canceller = thread::spawn(move || {
        wait_until_active("connector_odbc_cancel_execute");
        cancel.cancel()
    });
    let started = Instant::now();
    let mut context = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Err(error))) = stream.as_mut().poll_next(&mut context) else {
        panic!("the cancelled statement fails");
    };
    canceller
        .join()
        .expect("the canceller finishes")
        .expect("the cancellation is accepted");
    assert_eq!(
        error.kind(),
        ErrorKind::Cancellation,
        "unexpected error: {error}"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(active_queries("connector_odbc_cancel_execute"), 0);
}

#[test]
fn cancelling_between_reads_fails_the_next_read() {
    // Declare-fetch makes the ODBC driver read the result from a server-side
    // cursor as the batches are fetched.
    let session = session("UseDeclareFetch=1;Fetch=1000;");
    let (mut stream, cancel) =
        execute(&session, "SELECT x FROM generate_series(1, 10000000) AS x").into_parts();
    let mut context = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Ok(_))) = stream.as_mut().poll_next(&mut context) else {
        panic!("the first batch is read");
    };
    cancel.cancel().expect("the cancellation is accepted");
    let Poll::Ready(Some(Err(error))) = stream.as_mut().poll_next(&mut context) else {
        panic!("the read after cancellation fails");
    };
    assert_eq!(
        error.kind(),
        ErrorKind::Cancellation,
        "unexpected error: {error}"
    );
}
